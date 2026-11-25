//! 一次同步的编排（设计 §7）：扫描 → 拉清单 → 裁决 → 写本地 → 传 blob → 写新清单 → 更新状态。
//!
//! 幂等性：状态库只在最后一步整体提交；任何一步失败后重跑，本地已写的内容会被识别为
//! 「两边结果相同」（领养）或再次得到同样的合并结果，blob 内容寻址、重复上传被跳过，
//! 清单只在全部 blob 就位后才写。

use crate::backend::{Backend, ObjMeta};
use crate::crypto::{self, Dek, FilesKey, ObjKind, content_hash};
use crate::error::SyncError;
use crate::local::{LocalError, LocalStore, NsLocal};
use crate::manifest::{self, Entry, Manifest};
use crate::merge::{self, Conflict};
use crate::plan::{Action, PlanInput, decide};
use crate::remote;
use crate::scope::{self, Namespace, to_local, to_remote};
use crate::state::SyncState;
use std::collections::{BTreeMap, BTreeSet};

/// blob 回收的最小年龄（设计 §4）。
pub const GC_MIN_AGE_SECS: u64 = 7 * 86400;

/// 大批量删除护栏：本轮删除数超过此值且过已同步文件数的一半时中止。
pub const MASS_DELETE_MIN: usize = 20;
/// 已存在 blob 的修改时间早于此（秒）则重传刷新。
const BLOB_REFRESH_SECS: u64 = 86400;
/// 待传 blob 数不超过它时逐个 stat 判存在，超过则一次 list。
const STAT_THRESHOLD: usize = 32;
/// 条目继承的 `absorbed` 上限（超出丢最旧）。
const ABSORBED_CAP: usize = 64;

/// 把冲突副本落到一个不覆盖他人内容的名字：同名已存在且内容不同则追加 `-2`、`-3`…
///
/// 副本名在本机路径上拼再换回远端逻辑路径：设置的副本不再是设置，拼在 `groups/<组>/` 下会映射不回本机。
fn unique_copy_name(
    local: &dyn LocalStore,
    taken: &BTreeSet<String>,
    path: &str,
    ns: &Namespace,
    device: &str,
    date: &str,
    bytes: &[u8],
) -> Result<String, SyncError> {
    let local_path = to_local(path, ns).unwrap_or_else(|| path.to_string());
    for n in 1u32.. {
        let tag = if n == 1 {
            date.to_string()
        } else {
            format!("{date}-{n}")
        };
        let name = to_remote(&merge::conflict_copy_name(&local_path, device, &tag), ns);
        if taken.contains(&name) {
            continue;
        }
        match local.read(&name).map_err(le)? {
            None => return Ok(name),
            Some(c) if c == bytes => return Ok(name), // 崩溃重跑：已写过同样的副本
            Some(_) => {}
        }
    }
    unreachable!()
}

#[derive(Debug, Clone)]
pub struct SyncCtx {
    /// 本机命名空间：设置按设备组（`groups/<组>/…`），主题按类别（`<类别>/themes/…`）。
    pub ns: Namespace,
    /// 当前 unix 秒（由调用方给，便于测试）。
    pub now: u64,
    /// 立即同步：忽略退避等待（停止态仍不绕过）。
    pub force: bool,
    /// 同步成功后顺带回收 blob（失败只记警告）。
    pub gc: bool,
    /// 用户已确认放行大批量删除（见 `SyncError::mass_deletion`）。
    pub allow_mass_delete: bool,
}

impl SyncCtx {
    pub fn new(now: u64, ns: Namespace) -> Self {
        SyncCtx {
            ns,
            now,
            force: false,
            gc: false,
            allow_mass_delete: false,
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

/// 本机新发布的条目继承旧条目已吸收的版本（连同旧版本自身），使链式更新后
/// 旧的并发版本仍被识别为已吸收。
fn inherit_absorbed(entry: &mut Entry, old: Option<&Entry>) {
    let Some(old) = old else { return };
    let mut all: Vec<String> = old.absorbed.clone();
    if !old.deleted {
        all.push(old.hash.clone());
    }
    all.append(&mut entry.absorbed);
    let mut seen = BTreeSet::new();
    // 去重保留最后出现的位置，再丢最旧的
    let mut out: Vec<String> = Vec::new();
    for h in all.into_iter().rev() {
        if h != entry.hash && seen.insert(h.clone()) {
            out.push(h);
        }
    }
    out.truncate(ABSORBED_CAP);
    out.reverse();
    entry.absorbed = out;
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
    // 引擎全程用远端逻辑路径；读写本机时由适配器映射回本机路径。
    let mut mapped = NsLocal {
        inner: local,
        ns: ctx.ns.clone(),
    };
    match run(backend, &mut mapped, state, dek, ctx) {
        Ok(r) => {
            state.retry.record_success();
            Ok(localize_report(r, &ctx.ns))
        }
        Err(e) => {
            state.retry.record_failure(&e, ctx.now);
            Err(e)
        }
    }
}

/// 报告里的路径换回本机路径（调用方展示、定位冲突副本用）。
fn localize_report(mut r: SyncReport, ns: &Namespace) -> SyncReport {
    let map = |v: &mut Vec<String>| {
        for p in v.iter_mut() {
            if let Some(l) = to_local(p, ns) {
                *p = l;
            }
        }
    };
    map(&mut r.uploaded);
    map(&mut r.downloaded);
    map(&mut r.merged);
    map(&mut r.deleted_local);
    map(&mut r.deleted_remote);
    for c in &mut r.conflicts {
        if let Some(l) = to_local(&c.path, ns) {
            c.path = l;
        }
        if let Some(l) = c.copy.as_deref().and_then(|p| to_local(p, ns)) {
            c.copy = Some(l);
        }
    }
    r
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
    dek: &FilesKey,
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

/// 本机远端最新清单与本机状态对不上，说明另有一处在用同一个设备号。
/// 仅「恰好领先一代且 prev 接得上」视为上次写出后未提交即崩溃，全新状态同样适用（同号撞入首份清单由确认身份时的 id_taken 检查）。
fn check_own_chain(me: &str, state: &SyncState, top: &Manifest) -> Result<(), SyncError> {
    let g = top.gen_;
    let ok = if g > state.gen_ {
        g == state.gen_ + 1 && top.prev == state.last_nonce
    } else if g == state.gen_ {
        g == 0 || top.nonce == state.last_nonce
    } else {
        true
    };
    if ok {
        Ok(())
    } else {
        Err(SyncError::DeviceIdConflict(me.to_string()))
    }
}

fn run(
    backend: &dyn Backend,
    local: &mut dyn LocalStore,
    state: &mut SyncState,
    root: &Dek,
    ctx: &SyncCtx,
) -> Result<SyncReport, SyncError> {
    let dek = &root.files_key();
    let mut rep = SyncReport::default();
    let me = state.device.clone();

    // 0. 远端必须已初始化且密钥匹配
    let raw = backend
        .get(&manifest::meta_key())?
        .ok_or(SyncError::NotInitialized)?;
    if !crypto::Meta::from_json(&raw)?.check_dek(root) {
        return Err(crypto::CryptoError::BadKey.into());
    }

    // 1. 扫描本地
    let in_scope = |p: &str| scope::in_scope_remote(p, &ctx.ns);
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

    // 本设备自己最新的远端清单：校验 nonce 链；崩溃于提交前时 lamport 以它为下限。
    if let Some(g) = own_gens.iter().copied().max()
        && let Some(raw) = backend.get(&manifest::manifest_key(&me, g))?
    {
        match Manifest::decode(dek, &me, g, &raw) {
            Ok(m) => {
                check_own_chain(&me, state, &m)?;
                let top = m.files.values().map(|e| e.lamport).max().unwrap_or(0);
                lamport = lamport.max(m.lamport).max(top);
                // 刚认领旧设备号（代数已对齐）：状态库里没有的路径，以本机清单里的条目当作远端变化参与裁决。
                // 不变量：稳态下状态库包含本机清单的全部路径，只有身份操作（认领、加入组）会让它缺路径，
                // 所以这个条件每轮都成立也无副作用。
                if m.gen_ == state.gen_ {
                    let mut own = m;
                    own.files.retain(|p, _| !state.files.contains_key(p));
                    if !own.files.is_empty() {
                        remotes.push(own);
                    }
                }
            }
            Err(e) => rep.warnings.push(format!("本设备清单 {g} 无法读取: {e}")),
        }
    }

    // 3. 裁决
    let refs: Vec<&Manifest> = remotes.iter().collect();
    let plan = decide(&PlanInput {
        local: &hashes,
        state: &state.files,
        remotes: &refs,
        in_scope: &in_scope,
    });

    if !ctx.allow_mass_delete {
        let deletes = plan
            .iter()
            .filter(|(_, a)| matches!(a, Action::PublishDelete))
            .count();
        let total = state.files.values().filter(|e| !e.deleted).count();
        if deletes > MASS_DELETE_MIN && deletes * 2 > total {
            return Err(SyncError::mass_deletion(deletes, total));
        }
    }

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
        exec: false,
    };
    let mut outs: Vec<Out> = Vec::new();
    // 范围外的冲突副本：只写本地，不进状态与清单
    let mut untracked: Vec<(String, Vec<u8>)> = Vec::new();
    let mut used_copies: BTreeSet<String> = BTreeSet::new();

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
                    let copy = unique_copy_name(
                        local,
                        &used_copies,
                        &path,
                        &ctx.ns,
                        &e.device,
                        &merge::date_string(ctx.now),
                        bytes,
                    )?;
                    used_copies.insert(copy.clone());
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
            let copy = unique_copy_name(
                local,
                &used_copies,
                &path,
                &ctx.ns,
                &r.device,
                &merge::date_string(ctx.now),
                &remote_bytes,
            )?;
            used_copies.insert(copy.clone());
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

    // 本机版本发布时带上本机的可执行位
    for o in outs.iter_mut() {
        if o.upload.is_some() && o.entry.device == me && !o.entry.deleted {
            o.entry.exec = local.is_exec(&o.path).map_err(le)?;
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
    // 远端条目带可执行位的补上（被重写过的本机条目也要补，原子写入会丢位）
    let execs: Vec<String> = outs
        .iter()
        .filter(|o| o.entry.exec && !o.entry.deleted && !o.delete)
        .filter(|o| o.write.is_some() || o.entry.device != me)
        .map(|o| o.path.clone())
        .collect();
    if !execs.is_empty() {
        local.set_exec(&execs).map_err(le)?;
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
        // 已存在但很旧（或时间未知）的 blob 也重传一次刷新时间：它可能无人引用、即将被回收。
        let is_fresh = |m: &ObjMeta| m.modified.is_some_and(|t| t + BLOB_REFRESH_SECS > ctx.now);
        let items: Vec<(String, String, &[u8])> = blobs
            .into_iter()
            .map(|(hash, bytes)| {
                let id = crypto::blob_id(dek, hash);
                (manifest::blob_key(&id), id, bytes)
            })
            .collect();
        // 少量逐个 stat（避免列举整个 blobs/ 目录）；很多（如首次同步）一次 list 更省请求。
        let fresh: Option<BTreeSet<String>> = if items.len() > STAT_THRESHOLD {
            Some(
                backend
                    .list(&manifest::blobs_prefix())?
                    .iter()
                    .filter(|o| is_fresh(o))
                    .map(|o| o.key.clone())
                    .collect(),
            )
        } else {
            None
        };
        for (key, id, bytes) in items {
            let skip = match &fresh {
                Some(set) => set.contains(&key),
                None => backend.stat(&key)?.is_some_and(|m| is_fresh(&m)),
            };
            if !skip {
                backend.put(&key, &crypto::seal(dek, ObjKind::Blob, &id, bytes))?;
            }
        }
    }

    // 6. 写新清单（内容无变化则跳过）
    let mut files = state.files.clone();
    for mut o in outs {
        if o.entry.device == me {
            inherit_absorbed(&mut o.entry, state.files.get(&o.path));
        }
        files.insert(o.path, o.entry);
    }
    let digest = content_hash(&serde_json::to_vec(&files).expect("条目可序列化"));
    let mut gen_ = state.gen_;
    let mut new_nonce = state.last_nonce.clone();
    if digest != state.published_digest {
        gen_ = own_gens.iter().copied().max().unwrap_or(0).max(state.gen_) + 1;
        new_nonce = data_encoding::HEXLOWER.encode(&crypto::random_bytes::<16>());
        let m = Manifest {
            device: me.clone(),
            gen_,
            lamport,
            files: files.clone(),
            nonce: new_nonce.clone(),
            prev: state.last_nonce.clone(),
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
    state.last_nonce = new_nonce;
    state.published_digest = digest;
    state.remote_cache = new_cache;
    state.pending.clear();

    if ctx.gc {
        match remote::gc_blobs(backend, root, ctx.now, GC_MIN_AGE_SECS) {
            Ok(v) => rep.gc_removed = v.len(),
            Err(e) => rep.warnings.push(format!("回收 blob 失败: {e}")),
        }
    }
    Ok(rep)
}
