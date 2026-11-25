//! 设备组身份操作：加入/退出组、认领已有设备、改设备号。
//!
//! 只播种状态库（`join_group` 的 `Theirs` 另覆写本地），不写远端；调用方随后跑一轮 `sync_once`。

use crate::backend::Backend;
use crate::crypto::{self, Dek, FilesKey, ObjKind, content_hash};
use crate::devices;
use crate::error::SyncError;
use crate::ids::valid_id;
use crate::local::{LocalError, LocalStore};
use crate::manifest::{self, Entry, Manifest};
use crate::scope::{self, Kind, Namespace, ScopeFlags};
use crate::state::{RetiredId, SyncState};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overwrite {
    /// 以目标组为准：覆写本机设置。
    Theirs,
    /// 以本机为准：目标组被本机设置覆盖。
    Mine,
}

/// 实际写入/删除的本机路径。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JoinReport {
    pub written: Vec<String>,
    pub deleted: Vec<String>,
}

fn le(e: LocalError) -> SyncError {
    SyncError::Local(e.0)
}

/// 每台设备最新一份可解密的清单（最新代解不开则退到次新代），含本机。
fn latest_manifests(b: &dyn Backend, key: &FilesKey) -> Result<Vec<Manifest>, SyncError> {
    let mut by_dev: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for o in b.list(&manifest::devices_prefix())? {
        if let Some((d, g)) = manifest::parse_manifest_key(&o.key) {
            by_dev.entry(d).or_default().push(g);
        }
    }
    let mut out = Vec::new();
    for (dev, mut gens) in by_dev {
        gens.sort_unstable_by(|a, b| b.cmp(a));
        for g in gens.into_iter().take(manifest::KEEP_GENS as usize) {
            let Some(raw) = b.get(&manifest::manifest_key(&dev, g))? else {
                continue;
            };
            if let Ok(m) = Manifest::decode(key, &dev, g, &raw) {
                out.push(m);
                break;
            }
        }
    }
    Ok(out)
}

/// 取 blob 明文并校验 hash；缺失或损坏即失败。
fn fetch_verified(b: &dyn Backend, key: &FilesKey, hash: &str) -> Result<Vec<u8>, SyncError> {
    let id = crypto::blob_id(key, hash);
    let raw = b
        .get(&manifest::blob_key(&id))?
        .ok_or_else(|| SyncError::Local(format!("远端缺少 blob {id}")))?;
    match crypto::open(key, ObjKind::Blob, &id, &raw) {
        Ok(p) if content_hash(&p) == hash => Ok(p),
        _ => Err(SyncError::Local(format!("blob {id} 损坏或被调换"))),
    }
}

pub fn join_group(
    b: &dyn Backend,
    local: &mut dyn LocalStore,
    state: &mut SyncState,
    dek: &Dek,
    ns: &Namespace,
    mode: Overwrite,
) -> Result<JoinReport, SyncError> {
    let key = dek.files_key();
    // 远端清单不可信：范围外或畸形路径（含 `..`）一律丢弃，不写本地。
    let mut g: BTreeMap<String, Entry> = BTreeMap::new();
    for m in latest_manifests(b, &key)? {
        for (p, e) in m.files {
            if !scope::is_group_setting(&p, &ns.group)
                || !scope::in_scope_remote(&p, &ScopeFlags::ALL, ns)
            {
                continue;
            }
            if g.get(&p).is_none_or(|cur| e.order() > cur.order()) {
                g.insert(p, e);
            }
        }
    }

    let mut rep = JoinReport::default();
    let mut writes: Vec<(String, Vec<u8>)> = Vec::new();
    let mut deletes: Vec<String> = Vec::new();
    if mode == Overwrite::Theirs {
        for (p, e) in &g {
            let Some(l) = scope::to_local(p, ns) else {
                continue;
            };
            let cur = local.read(&l).map_err(le)?;
            if e.deleted {
                if cur.is_some() {
                    deletes.push(l);
                }
            } else if cur.as_deref().map(content_hash).as_deref() != Some(e.hash.as_str()) {
                writes.push((l, fetch_verified(b, &key, &e.hash)?));
            }
        }
        for l in local.list().map_err(le)? {
            if scope::kind(&l) == Kind::Setting
                && scope::in_scope(&l, &ScopeFlags::ALL)
                && !g.get(&scope::to_remote(&l, ns)).is_some_and(|e| !e.deleted)
                && !deletes.contains(&l)
            {
                deletes.push(l);
            }
        }
        if !writes.is_empty() {
            local.write_batch(&writes).map_err(le)?;
        }
        if !deletes.is_empty() {
            local.delete(&deletes).map_err(le)?;
        }
        rep.written = writes.into_iter().map(|(p, _)| p).collect();
        rep.deleted = deletes;
    }

    // 状态库里的设置条目都带 `groups/` 前缀：旧组与新组的一并清掉，换成 G。
    state.files.retain(|p, _| !p.starts_with("groups/"));
    let top = g.values().map(|e| e.lamport).max().unwrap_or(0);
    state.files.extend(g);
    state.lamport = state.lamport.max(top);
    state.group = ns.group.clone();
    Ok(rep)
}

pub fn leave_group(state: &mut SyncState, new_group: &str) {
    state.group = new_group.to_string();
    state.files.retain(|p, _| !p.starts_with("groups/"));
}

pub fn claim_device(
    b: &dyn Backend,
    state: &mut SyncState,
    dek: &Dek,
    dev: &str,
) -> Result<String, SyncError> {
    if !valid_id(dev) {
        return Err(SyncError::Local(format!("设备号 {dev} 不可用")));
    }
    let t = latest_manifests(b, &dek.files_key())?
        .into_iter()
        .find(|m| m.device == dev)
        .ok_or_else(|| SyncError::Local(format!("设备 {dev} 没有可读的清单")))?;
    let info = devices::read_info(b, &dek.devinfo_key(), dev).ok();
    let mut s = SyncState::new(dev);
    s.gen_ = t.gen_;
    s.last_nonce = t.nonce;
    s.lamport = t.lamport;
    s.identity_confirmed = true;
    match info {
        Some(i) if valid_id(&i.group) => {
            s.group = i.group;
            s.group_name = i.group_name;
            s.group_name_at = i.group_name_at;
        }
        _ => s.group = dev.to_string(),
    }
    let group = s.group.clone();
    *state = s;
    Ok(group)
}

pub fn rename_device(
    b: &dyn Backend,
    state: &mut SyncState,
    new_id: &str,
) -> Result<(), SyncError> {
    let unusable = || SyncError::Local(format!("设备号 {new_id} 不可用"));
    if !valid_id(new_id) || devices::id_taken(b, new_id)? {
        return Err(unusable());
    }
    let old = std::mem::replace(&mut state.device, new_id.to_string());
    state.retired_ids.push(RetiredId {
        id: old,
        nonce: state.last_nonce.clone(),
        gen_: state.gen_,
    });
    state.gen_ = 0;
    state.last_nonce.clear();
    state.published_digest.clear();
    Ok(())
}

/// 删除改号前的旧号目录。旧号最新清单的 nonce 与记录不符（号已被别处接着用）或解不开时不删，返回 false。
pub fn retire_device(b: &dyn Backend, dek: &Dek, r: &RetiredId) -> Result<bool, SyncError> {
    let latest = b
        .list(&manifest::device_dir_prefix(&r.id))?
        .iter()
        .filter_map(|o| manifest::parse_manifest_key(&o.key))
        .filter(|(d, _)| *d == r.id)
        .map(|(_, g)| g)
        .max();
    let nonce = match latest {
        // 清单已删光只剩 info：记录过发布就当作本机删了一半继续删（号恰被别处占用只有 info 时有小概率误删）
        None if r.gen_ > 0 => {
            devices::remove_device(b, &r.id)?;
            return Ok(true);
        }
        None => String::new(),
        Some(g) => match b.get(&manifest::manifest_key(&r.id, g))? {
            None => String::new(),
            Some(raw) => match Manifest::decode(&dek.files_key(), &r.id, g, &raw) {
                Ok(m) => m.nonce,
                Err(_) => return Ok(false),
            },
        },
    };
    if nonce != r.nonce {
        return Ok(false);
    }
    devices::remove_device(b, &r.id)?;
    Ok(true)
}
