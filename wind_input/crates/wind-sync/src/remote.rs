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

/// 已持有 DEK 的设备改口令（不需旧口令）：校验 DEK 与远端一致后，用新口令重新封装。
pub fn rewrap_passphrase(
    b: &dyn Backend,
    dek: &Dek,
    new: &str,
    kdf: KdfParams,
) -> Result<(), SyncError> {
    if !read_meta(b)?.check_dek(dek) {
        return Err(crate::crypto::CryptoError::BadKey.into());
    }
    b.put(&manifest::meta_key(), &Meta::new(dek, new, kdf)?.to_json())?;
    Ok(())
}

/// 重置远端：删光 `windinput-sync/v1/` 下所有对象，再用 `dek` 与新口令写 meta。
/// 之后各设备以自己的本地内容为准重新上传。调用方负责取得用户的明确确认。
pub fn reset_remote(
    b: &dyn Backend,
    dek: &Dek,
    pass: &str,
    kdf: KdfParams,
) -> Result<(), SyncError> {
    let meta = Meta::new(dek, pass, kdf)?.to_json();
    for o in b.list(&format!("{}/", manifest::ROOT))? {
        // meta 最后写：中途失败时远端仍是「已清空但未初始化」，重试即可
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::MemBackend;

    #[test]
    fn rewrap_keeps_dek_and_rejects_foreign_dek() {
        let b = MemBackend::new();
        let dek = init_remote(&b, "old", KdfParams::FAST_TEST).unwrap();
        rewrap_passphrase(&b, &dek, "new", KdfParams::FAST_TEST).unwrap();
        assert!(join_with_passphrase(&b, "old").is_err());
        assert_eq!(join_with_passphrase(&b, "new").unwrap(), dek);
        assert!(rewrap_passphrase(&b, &Dek::generate(), "x", KdfParams::FAST_TEST).is_err());
    }

    #[test]
    fn reset_clears_everything_then_writes_meta() {
        let b = MemBackend::new();
        let dek = init_remote(&b, "old", KdfParams::FAST_TEST).unwrap();
        b.raw_put("windinput-sync/v1/blobs/x", vec![1]);
        b.raw_put("other/keep", vec![1]);
        reset_remote(&b, &dek, "fresh", KdfParams::FAST_TEST).unwrap();
        assert_eq!(b.keys("windinput-sync/"), vec![manifest::meta_key()]);
        assert_eq!(b.keys("other/").len(), 1);
        assert_eq!(join_with_passphrase(&b, "fresh").unwrap(), dek);
    }
}
