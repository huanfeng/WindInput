//! 远端会话：创建同步库、用同步密钥加入、重置、回收 blob。

use crate::backend::Backend;
use crate::crypto::{CryptoError, Dek, Meta};
use crate::error::SyncError;
use crate::manifest::{self, Manifest};
use std::collections::BTreeSet;

fn read_meta(b: &dyn Backend) -> Result<Meta, SyncError> {
    let raw = b
        .get(&manifest::meta_key())?
        .ok_or(SyncError::NotInitialized)?;
    Ok(Meta::from_json(&raw)?)
}

/// 首次启用：生成同步密钥并写 meta.json。远端已有 meta 则拒绝（不覆盖）。
pub fn init_remote(b: &dyn Backend) -> Result<Dek, SyncError> {
    let dek = Dek::generate();
    init_remote_with(b, &dek)?;
    Ok(dek)
}

/// 同 [`init_remote`]，但用调用方已生成（并已在本机保存）的密钥。
pub fn init_remote_with(b: &dyn Backend, dek: &Dek) -> Result<(), SyncError> {
    if b.get(&manifest::meta_key())?.is_some() {
        return Err(SyncError::AlreadyInitialized);
    }
    let meta = Meta::new(dek).to_json();
    b.put(&manifest::meta_key(), &meta)?;
    // 并发首次初始化：后写覆盖先写。回读核对，输了就按「已初始化」处理，让用户改用同步密钥加入。
    // 同步引擎要求 meta 已存在才会上传任何 blob/清单，所以输家此前没有留下数据。
    if b.get(&manifest::meta_key())?.as_deref() != Some(meta.as_slice()) {
        return Err(SyncError::AlreadyInitialized);
    }
    Ok(())
}

/// 新设备粘贴同步密钥加入（用 meta 里的校验值核对）。
pub fn join_with_key(b: &dyn Backend, key: &str) -> Result<Dek, SyncError> {
    // 解析失败与校验失败对用户是同一件事：同步密钥不对
    let dek = Dek::from_key_string(key).map_err(|_| CryptoError::BadKey)?;
    if !read_meta(b)?.verify(&dek) {
        return Err(CryptoError::BadKey.into());
    }
    Ok(dek)
}

/// 重置远端：删光 `windinput-sync/v1/` 下所有对象，再生成新同步密钥并写 meta。
/// 之后各设备以自己的本地内容为准重新上传。调用方负责取得用户的明确确认。
pub fn reset_remote(b: &dyn Backend) -> Result<Dek, SyncError> {
    let dek = Dek::generate();
    reset_remote_with(b, &dek)?;
    Ok(dek)
}

/// 同 [`reset_remote`]，但用调用方已生成（并已在本机保存）的密钥。
pub fn reset_remote_with(b: &dyn Backend, dek: &Dek) -> Result<(), SyncError> {
    let meta = Meta::new(dek).to_json();
    // 先删 meta：中途失败时远端是「未初始化」，旧库各设备的同步轮就此停下，不会在半毁的远端上
    // 把已删的设备当作清除；新 meta 最后写，重试即可
    b.delete(&manifest::meta_key())?;
    for o in b.list(&format!("{}/", manifest::ROOT))? {
        if !manifest::safe_key(&o.key) || o.key == manifest::meta_key() {
            continue;
        }
        b.delete(&o.key)?;
    }
    b.put(&manifest::meta_key(), &meta)?;
    Ok(())
}

/// 回收：任一设备最新两代清单都不引用、且修改时间早于 `now - min_age_secs` 的 blob。
/// 返回被删的 blob 键。后端给不出修改时间的 blob 一律保留。
pub fn gc_blobs(
    b: &dyn Backend,
    dek: &Dek,
    now: u64,
    min_age_secs: u64,
) -> Result<Vec<String>, SyncError> {
    let files = dek.files_key();
    let mut gens: std::collections::BTreeMap<String, Vec<u64>> = Default::default();
    for o in b.list(&manifest::devices_prefix())? {
        if let Some((d, g)) = manifest::parse_manifest_key(&o.key) {
            gens.entry(d).or_default().push(g);
        }
    }
    let mut referenced = BTreeSet::new();
    for (dev, mut gs) in gens {
        gs.sort_unstable_by(|a, b| b.cmp(a));
        for g in gs.into_iter().take(manifest::KEEP_GENS as usize) {
            let Some(raw) = b.get(&manifest::manifest_key(&dev, g))? else {
                continue;
            };
            // 读不出来的清单：为安全起见中止回收（不能确定它引用了什么）。
            let m = Manifest::decode(&files, &dev, g, &raw)?;
            for e in m.files.values().filter(|e| !e.deleted) {
                referenced.insert(manifest::blob_key(&crate::crypto::blob_id(&files, &e.hash)));
            }
        }
    }
    let mut removed = Vec::new();
    let blobs = manifest::blobs_prefix();
    for o in b.list(&blobs)? {
        let old = o.modified.is_some_and(|t| t + min_age_secs <= now);
        if old
            && o.key.starts_with(&blobs)
            && manifest::safe_key(&o.key)
            && !referenced.contains(&o.key)
        {
            b.delete(&o.key)?;
            removed.push(o.key);
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::MemBackend;

    #[test]
    fn join_with_key_accepts_own_and_rejects_foreign() {
        let b = MemBackend::new();
        let dek = init_remote(&b).unwrap();
        assert_eq!(join_with_key(&b, &dek.to_key_string()).unwrap(), dek);
        assert!(join_with_key(&b, &Dek::generate().to_key_string()).is_err());
        assert!(join_with_key(&b, "garbage").is_err());
    }

    #[test]
    fn reset_clears_everything_then_writes_new_key_meta() {
        let b = MemBackend::new();
        let old = init_remote(&b).unwrap();
        b.raw_put("windinput-sync/v1/blobs/x", vec![1]);
        b.raw_put("other/keep", vec![1]);
        let fresh = reset_remote(&b).unwrap();
        assert_eq!(b.keys("windinput-sync/"), vec![manifest::meta_key()]);
        assert_eq!(b.keys("other/").len(), 1);
        assert_ne!(old, fresh);
        assert!(join_with_key(&b, &old.to_key_string()).is_err());
        assert_eq!(join_with_key(&b, &fresh.to_key_string()).unwrap(), fresh);
    }
}
