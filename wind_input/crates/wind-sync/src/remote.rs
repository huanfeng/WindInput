//! 远端会话：初始化、用口令/密钥加入、改口令、回收 blob。

use crate::backend::Backend;
use crate::crypto::{Dek, KdfParams, Meta};
use crate::error::SyncError;
use crate::manifest::{self, Manifest};
use std::collections::BTreeSet;

fn read_meta(b: &dyn Backend) -> Result<Meta, SyncError> {
    let raw = b
        .get(&manifest::meta_key())?
        .ok_or(SyncError::NotInitialized)?;
    Ok(Meta::from_json(&raw)?)
}

/// 首次启用：生成 DEK，口令封装后写 meta.json。远端已有 meta 则拒绝（不覆盖）。
pub fn init_remote(b: &dyn Backend, pass: &str, kdf: KdfParams) -> Result<Dek, SyncError> {
    if b.get(&manifest::meta_key())?.is_some() {
        return Err(SyncError::AlreadyInitialized);
    }
    let dek = Dek::generate();
    b.put(
        &manifest::meta_key(),
        &Meta::new(&dek, pass, kdf)?.to_json(),
    )?;
    Ok(dek)
}

/// 新设备用口令加入。
pub fn join_with_passphrase(b: &dyn Backend, pass: &str) -> Result<Dek, SyncError> {
    Ok(read_meta(b)?.unlock(pass)?)
}

/// 新设备粘贴导出的密钥加入（用 meta 里的校验值核对）。
pub fn join_with_key(b: &dyn Backend, exported: &str) -> Result<Dek, SyncError> {
    let dek = Dek::from_base64(exported)?;
    if !read_meta(b)?.check_dek(&dek) {
        return Err(crate::crypto::CryptoError::BadKey.into());
    }
    Ok(dek)
}

/// 改口令：只重新封装 DEK（新盐），数据与已导出的密钥不受影响。需先用旧口令验证。
pub fn change_passphrase(
    b: &dyn Backend,
    old: &str,
    new: &str,
    kdf: KdfParams,
) -> Result<(), SyncError> {
    let dek = read_meta(b)?.unlock(old)?;
    b.put(&manifest::meta_key(), &Meta::new(&dek, new, kdf)?.to_json())?;
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
            let m = Manifest::decode(dek, &dev, g, &raw)?;
            for e in m.files.values().filter(|e| !e.deleted) {
                referenced.insert(manifest::blob_key(&crate::crypto::blob_id(dek, &e.hash)));
            }
        }
    }
    let mut removed = Vec::new();
    for o in b.list(&manifest::blobs_prefix())? {
        let old = o.modified.is_some_and(|t| t + min_age_secs <= now);
        if old && !referenced.contains(&o.key) {
            b.delete(&o.key)?;
            removed.push(o.key);
        }
    }
    Ok(removed)
}
