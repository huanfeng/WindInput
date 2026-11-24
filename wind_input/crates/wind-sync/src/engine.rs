//! 一次同步的编排（设计 §7）：扫描 → 拉清单 → 裁决 → 写本地 → 传 blob → 写新清单 → 更新状态。
//!
//! 幂等性：状态库只在最后一步整体提交；任何一步失败后重跑，本地已写的内容会被识别为
//! 「两边结果相同」（领养）或再次得到同样的合并结果，blob 内容寻址、重复上传被跳过，
//! 清单只在全部 blob 就位后才写。

use crate::backend::Backend;
use crate::crypto::{self, Dek, ObjKind, content_hash};
use crate::error::SyncError;
use crate::local::{LocalError, LocalStore};
use crate::manifest::{self, Entry, Manifest};
use crate::merge::{self, Conflict};
use crate::plan::{Action, PlanInput, decide};
use crate::remote;
use crate::scope::{self, ScopeFlags};
use crate::state::SyncState;
use std::collections::{BTreeMap, BTreeSet};

/// blob 回收的最小年龄（设计 §4）。
pub const GC_MIN_AGE_SECS: u64 = 7 * 86400;

#[derive(Debug, Clone)]
pub struct SyncCtx {
    pub scope: ScopeFlags,
    /// 当前 unix 秒（由调用方给，便于测试）。
    pub now: u64,
    /// 立即同步：忽略退避等待（停止态仍不绕过）。
    pub force: bool,
    /// 同步成功后顺带回收 blob（失败只记警告）。
    pub gc: bool,
}

impl SyncCtx {
    pub fn new(scope: ScopeFlags, now: u64) -> Self {
        SyncCtx {
            scope,
            now,
            force: false,
            gc: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictInfo {
    pub path: String,
    /// 不可合并文件：远端版本被保存成的冲突副本（相对路径）。
    pub copy: Option<String>,
    /// 可合并文件：同键冲突里被丢弃的值。
    pub keys: Vec<Conflict>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// 处于退避等待期，本次没有执行。
    pub skipped: bool,
    pub uploaded: Vec<String>,
    pub downloaded: Vec<String>,
    pub merged: Vec<String>,
    pub deleted_local: Vec<String>,
    pub deleted_remote: Vec<String>,
    pub conflicts: Vec<ConflictInfo>,
    pub warnings: Vec<String>,
    pub manifest_gen: Option<u64>,
    pub gc_removed: usize,
}

fn le(e: LocalError) -> SyncError {
    SyncError::Local(e.0)
}

pub fn sync_once(
    backend: &dyn Backend,
    local: &mut dyn LocalStore,
    state: &mut SyncState,
    dek: &Dek,
    ctx: &SyncCtx,
) -> Result<SyncReport, SyncError> {
    if let Some(h) = &state.retry.halted {
        return Err(SyncError::Halted(h.clone()));
    }
    if !ctx.force && !state.retry.due(ctx.now) {
        return Ok(SyncReport {
            skipped: true,
            ..Default::default()
        });
    }
    match run(backend, local, state, dek, ctx) {
        Ok(r) => {
            state.retry.record_success();
            Ok(r)
        }
        Err(e) => {
            state.retry.record_failure(&e, ctx.now);
            Err(e)
        }
    }
}

/// 一次裁决产生的本地/远端效果。
struct Out {
    path: String,
    entry: Entry,
    write: Option<Vec<u8>>,
    delete: bool,
    upload: Option<Vec<u8>>,
}

/// 取 blob 明文；缺失或损坏（被调换、hash 对不上）记警告返回 None，该文件本轮跳过。
fn fetch_blob(
    backend: &dyn Backend,
    dek: &Dek,
    hash: &str,
    warnings: &mut Vec<String>,
) -> Result<Option<Vec<u8>>, SyncError> {
    let id = crypto::blob_id(dek, hash);
    let Some(raw) = backend.get(&manifest::blob_key(&id))? else {
        warnings.push(format!("远端缺少 blob {id}"));
        return Ok(None);
    };
    match crypto::open(dek, ObjKind::Blob, &id, &raw) {
        Ok(p) if content_hash(&p) == hash => Ok(Some(p)),
        _ => {
            warnings.push(format!("blob {id} 损坏或被调换，已忽略"));
            Ok(None)
        }
    }
}

fn run(
    backend: &dyn Backend,
    local: &mut dyn LocalStore,
    state: &mut SyncState,
    dek: &Dek,
    ctx: &SyncCtx,
) -> Result<SyncReport, SyncError> {
    let mut rep = SyncReport::default();
    let me = state.device.clone();

    // 0. 远端必须已初始化且密钥匹配
    let raw = backend
        .get(&manifest::meta_key())?
        .ok_or(SyncError::NotInitialized)?;
    if !crypto::Meta::from_json(&raw)?.check_dek(dek) {
        return Err(crypto::CryptoError::BadKey.into());
    }

    // 1. 扫描本地
    let in_scope = |p: &str| scope::in_scope(p, &ctx.scope);
    let mut contents: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut hashes: BTreeMap<String, String> = BTreeMap::new();
    for p in local.list().map_err(le)? {
        if !in_scope(&p) {
            continue;
        }
        if let Some(b) = local.read(&p).map_err(le)? {
            hashes.insert(p.clone(), content_hash(&b));
            contents.insert(p, b);
        }
    }

    // 2. 拉各设备最新清单（按 gen 缓存）
    let mut by_dev: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for o in backend.list(&manifest::devices_prefix())? {
        if let Some((d, g)) = manifest::parse_manifest_key(&o.key) {
            by_dev.entry(d).or_default().push(g);
        }
    }
    let own_gens = by_dev.remove(&me).unwrap_or_default();
    let mut remotes: Vec<Manifest> = Vec::new();
    let mut new_cache = BTreeMap::new();
    for (dev, mut gens) in by_dev {
        gens.sort_unstable_by(|a, b| b.cmp(a));
        for g in gens.into_iter().take(manifest::KEEP_GENS as usize) {
            if let Some(c) = state.remote_cache.get(&dev).filter(|c| c.gen_ == g) {
                remotes.push(c.clone());
                new_cache.insert(dev.clone(), c.clone());
                break;
            }
            let Some(raw) = backend.get(&manifest::manifest_key(&dev, g))? else {
                continue;
            };
            match Manifest::decode(dek, &dev, g, &raw) {
                Ok(m) => {
                    new_cache.insert(dev.clone(), m.clone());
                    remotes.push(m);
                    break;
                }
                Err(e) => rep
                    .warnings
                    .push(format!("设备 {dev} 清单 {g} 无法读取: {e}")),
            }
        }
    }
    let mut lamport = remotes
        .iter()
        .flat_map(|m| std::iter::once(m.lamport).chain(m.files.values().map(|e| e.lamport)))
        .max()
        .unwrap_or(0)
        .max(state.lamport);

    // 3. 裁决
    let refs: Vec<&Manifest> = remotes.iter().collect();
    let plan = decide(&PlanInput {
        local: &hashes,
        state: &state.files,
        remotes: &refs,
        in_scope: &in_scope,
    });

    let base_of = |p: &str| {
        state
            .files
            .get(p)
            .and_then(Entry::value)
            .map(str::to_string)
    };
    let mk = |hash: &str, base: Option<String>, lamport: u64| Entry {
        hash: hash.to_string(),
        base,
        deleted: hash.is_empty(),
        lamport,
        device: me.clone(),
        absorbed: Vec::new(),
    };
    let mut outs: Vec<Out> = Vec::new();
    // 范围外的冲突副本：只写本地，不进状态与清单
    let mut untracked: Vec<(String, Vec<u8>)> = Vec::new();

    for (path, action) in plan {
        let base = base_of(&path);
        // 合并失败 / 不可合并 都落到冲突副本
        let conflict_with: Entry;
        match action {
            Action::Upload => {
                lamport += 1;
                outs.push(Out {
                    entry: mk(&hashes[&path], base, lamport),
                    write: None,
                    delete: false,
                    upload: Some(contents[&path].clone()),
                    path: path.clone(),
                });
                rep.uploaded.push(path);
                continue;
            }
            Action::PublishDelete => {
                lamport += 1;
                outs.push(Out {
                    entry: mk("", base, lamport),
                    write: None,
                    delete: false,
                    upload: None,
                    path: path.clone(),
                });
                rep.deleted_remote.push(path);
                continue;
            }
            Action::Download(r) => {
                let Some(bytes) = fetch_blob(backend, dek, &r.hash, &mut rep.warnings)? else {
                    continue;
                };
                outs.push(Out {
                    entry: Entry { base, ..r },
                    write: Some(bytes),
                    delete: false,
                    upload: None,
                    path: path.clone(),
                });
                rep.downloaded.push(path);
                continue;
            }
            Action::DeleteLocal(r) => {
                outs.push(Out {
                    entry: Entry { base, ..r },
                    write: None,
                    delete: true,
                    upload: None,
                    path: path.clone(),
                });
                rep.deleted_local.push(path);
                continue;
            }
            Action::Adopt(r) => {
                outs.push(Out {
                    entry: Entry { base, ..r },
                    write: None,
                    delete: false,
                    upload: None,
                    path,
                });
                continue;
            }
            Action::Merge(r) => {
                let Some(remote_bytes) = fetch_blob(backend, dek, &r.hash, &mut rep.warnings)?
                else {
                    continue;
                };
                let base_bytes = match &base {
                    Some(h) => fetch_blob(backend, dek, h, &mut rep.warnings)?,
                    None => None,
                };
                match merge::merge(
                    &path,
                    base_bytes.as_deref(),
                    &contents[&path],
                    &remote_bytes,
                ) {
                    Some(m) => {
                        lamport += 1;
                        let write = (m.content != contents[&path]).then(|| m.content.clone());
                        let mut entry = mk(&content_hash(&m.content), base, lamport);
                        entry.absorbed = vec![r.hash.clone()];
                        outs.push(Out {
                            entry,
                            write,
                            delete: false,
                            upload: Some(m.content),
                            path: path.clone(),
                        });
                        if !m.conflicts.is_empty() {
                            rep.conflicts.push(ConflictInfo {
                                path: path.clone(),
                                copy: None,
                                keys: m.conflicts,
                            });
                        }
                        rep.merged.push(path);
                        continue;
                    }
                    None => conflict_with = r,
                }
            }
            Action::Multi(cands) => {
                let local_bytes = contents.get(&path);
                let local_changed = hashes.get(&path).map(String::as_str) != base.as_deref();
                if cands.iter().all(|c| c.deleted) {
                    if local_bytes.is_some() && local_changed {
                        lamport += 1;
                        outs.push(Out {
                            entry: mk(&hashes[&path], base, lamport),
                            write: None,
                            delete: false,
                            upload: Some(contents[&path].clone()),
                            path: path.clone(),
                        });
                        rep.uploaded.push(path);
                    } else if local_bytes.is_some() {
                        outs.push(Out {
                            entry: Entry {
                                base,
                                ..cands[0].clone()
                            },
                            write: None,
                            delete: true,
                            upload: None,
                            path: path.clone(),
                        });
                        rep.deleted_local.push(path);
                    }
                    continue;
                }
                // 一删一改：忽略删除，保留各个修改（cands 已按新到旧）
                let mut live: Vec<(Entry, Vec<u8>)> = Vec::new();
                for c in cands.iter().filter(|c| !c.deleted) {
                    if let Some(b) = fetch_blob(backend, dek, &c.hash, &mut rep.warnings)? {
                        live.push((c.clone(), b));
                    }
                }
                if live.is_empty() {
                    continue;
                }
                let (mut acc, others): (Vec<u8>, &[(Entry, Vec<u8>)]) = match local_bytes {
                    Some(l) if local_changed => (l.clone(), &live[..]),
                    _ => (live[0].1.clone(), &live[1..]),
                };
                let base_bytes = match &base {
                    Some(h) => fetch_blob(backend, dek, h, &mut rep.warnings)?,
                    None => None,
                };
                let mut keys = Vec::new();
                let mut copies: Vec<(&Entry, &Vec<u8>)> = Vec::new();
                let mut merged_any = false;
                for (e, bytes) in others {
                    if content_hash(bytes) == content_hash(&acc) {
                        continue;
                    }
                    match merge::merge(&path, base_bytes.as_deref(), &acc, bytes) {
                        Some(m) => {
                            merged_any = true;
                            keys.extend(m.conflicts);
                            acc = m.content;
                        }
                        None => copies.push((e, bytes)),
                    }
                }
                let acc_hash = content_hash(&acc);
                let changed_locally = local_bytes != Some(&acc);
                if !merged_any && copies.is_empty() && acc_hash == live[0].0.hash {
                    // 结果就是最新的那个远端版本：领养它
                    outs.push(Out {
                        entry: Entry {
                            base,
                            ..live[0].0.clone()
                        },
                        write: changed_locally.then(|| acc.clone()),
                        delete: false,
                        upload: None,
                        path: path.clone(),
                    });
                    rep.downloaded.push(path);
                    continue;
                }
                lamport += 1;
                let mut entry = mk(&acc_hash, base, lamport);
                entry.absorbed = live.iter().map(|(e, _)| e.hash.clone()).collect();
                outs.push(Out {
                    entry,
                    write: changed_locally.then(|| acc.clone()),
                    delete: false,
                    upload: Some(acc),
                    path: path.clone(),
                });
                if merged_any {
                    rep.merged.push(path.clone());
                } else {
                    rep.uploaded.push(path.clone());
                }
                if !keys.is_empty() {
                    rep.conflicts.push(ConflictInfo {
                        path: path.clone(),
                        copy: None,
                        keys,
                    });
                }
                for (e, bytes) in copies {
                    let copy =
                        merge::conflict_copy_name(&path, &e.device, &merge::date_string(ctx.now));
                    let tracked = in_scope(&copy);
                    if tracked {
                        lamport += 1;
                    }
                    if tracked {
                        outs.push(Out {
                            entry: mk(&content_hash(bytes), None, lamport),
                            write: Some(bytes.clone()),
                            delete: false,
                            upload: Some(bytes.clone()),
                            path: copy.clone(),
                        });
                    } else {
                        untracked.push((copy.clone(), bytes.clone()));
                    }
                    rep.conflicts.push(ConflictInfo {
                        path: path.clone(),
                        copy: Some(copy),
                        keys: vec![],
                    });
                }
                continue;
            }
            Action::ConflictCopy(r) => conflict_with = r,
        }
        let r = conflict_with;
        {
            let Some(remote_bytes) = fetch_blob(backend, dek, &r.hash, &mut rep.warnings)? else {
                continue;
            };
            let copy = merge::conflict_copy_name(&path, &r.device, &merge::date_string(ctx.now));
            lamport += 1;
            // 本地版本胜出，重新以更高 lamport 发布
            let mut entry = mk(&hashes[&path], base, lamport);
            entry.absorbed = vec![r.hash.clone()];
            outs.push(Out {
                entry,
                write: None,
                delete: false,
                upload: Some(contents[&path].clone()),
                path: path.clone(),
            });
            let tracked = in_scope(&copy);
            if tracked {
                lamport += 1;
            }
            if tracked {
                outs.push(Out {
                    entry: mk(&content_hash(&remote_bytes), None, lamport),
                    write: Some(remote_bytes.clone()),
                    delete: false,
                    upload: Some(remote_bytes),
                    path: copy.clone(),
                });
            } else {
                untracked.push((copy.clone(), remote_bytes));
            }
            rep.uploaded.push(path.clone());
            rep.conflicts.push(ConflictInfo {
                path,
                copy: Some(copy),
                keys: vec![],
            });
        }
    }

    // 4. 写本地（一批写入、一批删除）
    let mut writes: Vec<(String, Vec<u8>)> = outs
        .iter()
        .filter_map(|o| o.write.clone().map(|w| (o.path.clone(), w)))
        .collect();
    writes.extend(untracked);
    let dels: Vec<String> = outs
        .iter()
        .filter(|o| o.delete)
        .map(|o| o.path.clone())
        .collect();
    if !writes.is_empty() {
        local.write_batch(&writes).map_err(le)?;
    }
    if !dels.is_empty() {
        local.delete(&dels).map_err(le)?;
    }

    // 5. 传 blob（先于清单）
    state.pending = outs
        .iter()
        .filter(|o| o.upload.is_some())
        .map(|o| o.path.clone())
        .collect();
    let mut blobs: BTreeMap<&str, &[u8]> = BTreeMap::new();
    for o in &outs {
        if let Some(b) = &o.upload {
            blobs.insert(&o.entry.hash, b);
        }
    }
    if !blobs.is_empty() {
        let existing: BTreeSet<String> = backend
            .list(&manifest::blobs_prefix())?
            .into_iter()
            .map(|o| o.key)
            .collect();
        for (hash, bytes) in blobs {
            let id = crypto::blob_id(dek, hash);
            let key = manifest::blob_key(&id);
            if !existing.contains(&key) {
                backend.put(&key, &crypto::seal(dek, ObjKind::Blob, &id, bytes))?;
            }
        }
    }

    // 6. 写新清单（内容无变化则跳过）
    let mut files = state.files.clone();
    for o in outs {
        files.insert(o.path, o.entry);
    }
    let digest = content_hash(&serde_json::to_vec(&files).expect("条目可序列化"));
    let mut gen_ = state.gen_;
    if digest != state.published_digest {
        gen_ = own_gens.iter().copied().max().unwrap_or(0).max(state.gen_) + 1;
        let m = Manifest {
            device: me.clone(),
            gen_,
            lamport,
            files: files.clone(),
        };
        backend.put(&manifest::manifest_key(&me, gen_), &m.encode(dek))?;
        rep.manifest_gen = Some(gen_);
        for g in own_gens
            .iter()
            .filter(|g| **g + manifest::KEEP_GENS <= gen_)
        {
            if let Err(e) = backend.delete(&manifest::manifest_key(&me, *g)) {
                rep.warnings.push(format!("清理旧清单 {g} 失败: {e}"));
            }
        }
    }

    // 7. 提交状态
    state.files = files;
    state.lamport = lamport;
    state.gen_ = gen_;
    state.published_digest = digest;
    state.remote_cache = new_cache;
    state.pending.clear();

    if ctx.gc {
        match remote::gc_blobs(backend, dek, ctx.now, GC_MIN_AGE_SECS) {
            Ok(v) => rep.gc_removed = v.len(),
            Err(e) => rep.warnings.push(format!("回收 blob 失败: {e}")),
        }
    }
    Ok(rep)
}
