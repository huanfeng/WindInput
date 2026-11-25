//! 设备名册：`devices/<dev>/info` 对象（设计 docs/design/sync-messaging.md §3.2）。
//!
//! info 用 devinfo 子密钥加密，AAD 绑定对象类型与设备号。引擎与 blob 回收都不读它。

use crate::backend::Backend;
use crate::crypto::{self, CryptoError, DevInfoKey, ObjKind};
use crate::error::SyncError;
use crate::manifest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name: String,
    /// `windows` / `macos` / `linux` / `android`。
    pub platform: String,
    pub app_version: String,
    /// 能力位：`files`、`clip`、`xfer`、`lan`。
    pub caps: Vec<String>,
    /// 最近一次刷新的 Unix 秒（只在跨天时刷新，精度为天）。
    pub last_seen: u64,
    /// 所在设备组号；空表示尚未入组。
    #[serde(default)]
    pub group: String,
    /// 组名（组内成员各存一份，取 `group_name_at` 最大者）。
    #[serde(default)]
    pub group_name: String,
    /// 组名最近修改的 Unix 秒。
    #[serde(default)]
    pub group_name_at: u64,
}

/// 名册一项：设备号与它的信息（读不出时为错误）。
pub type DeviceEntry = (String, Result<DeviceInfo, SyncError>);

pub fn put_device_info(
    b: &dyn Backend,
    key: &DevInfoKey,
    dev: &str,
    info: &DeviceInfo,
) -> Result<(), SyncError> {
    if !manifest::valid_device_id(dev) {
        return Err(SyncError::Local(format!("非法设备号: {dev}")));
    }
    let json = serde_json::to_vec(info).expect("DeviceInfo 可序列化");
    b.put(
        &manifest::device_info_key(dev),
        &crypto::seal(key, ObjKind::DevInfo, dev, &json),
    )?;
    Ok(())
}

pub(crate) fn read_info(
    b: &dyn Backend,
    key: &DevInfoKey,
    dev: &str,
) -> Result<DeviceInfo, SyncError> {
    let raw = b
        .get(&manifest::device_info_key(dev))?
        .ok_or_else(|| SyncError::Local("该设备没有设备信息".into()))?;
    let json = crypto::open(key, ObjKind::DevInfo, dev, &raw)?;
    serde_json::from_slice(&json)
        .map_err(|e| SyncError::Crypto(CryptoError::Format(format!("设备信息: {e}"))))
}

/// 远端出现过的全部设备（有清单或有 info），按设备号排序；读不出 info 的给 `Err`。
/// 后端列举失败整体报错；单个 info 的后端读取失败也只记在该项上。
pub fn list_devices(b: &dyn Backend, key: &DevInfoKey) -> Result<Vec<DeviceEntry>, SyncError> {
    let prefix = manifest::devices_prefix();
    let devs: BTreeSet<String> = b
        .list(&prefix)?
        .iter()
        .filter_map(|o| {
            let (dev, _) = o.key.strip_prefix(&prefix)?.split_once('/')?;
            manifest::valid_device_id(dev).then(|| dev.to_string())
        })
        .collect();
    Ok(devs
        .into_iter()
        .map(|d| {
            let r = read_info(b, key, &d);
            (d, r)
        })
        .collect())
}

/// 该组的组名：成员中 `group_name_at` 最大者，并列取设备号小者；读不出的项跳过，无成员为 None。
pub fn group_name_of(roster: &[DeviceEntry], group: &str) -> Option<String> {
    roster
        .iter()
        .filter_map(|(dev, r)| Some((dev, r.as_ref().ok()?)))
        .filter(|(_, i)| i.group == group)
        .max_by(|(da, a), (db, b)| {
            a.group_name_at
                .cmp(&b.group_name_at)
                .then_with(|| db.cmp(da))
        })
        .map(|(_, i)| i.group_name.clone())
}

/// 远端 `devices/<id>/` 下是否已有对象（号已被占用）。
pub fn id_taken(b: &dyn Backend, id: &str) -> Result<bool, SyncError> {
    if !manifest::valid_device_id(id) {
        return Err(SyncError::Local(format!("非法设备号: {id}")));
    }
    Ok(!b.list(&manifest::device_dir_prefix(id))?.is_empty())
}

/// 删除该设备目录下全部对象（清单按代次升序，info 最后），返回删除个数。拒绝删本机由调用方保证。
pub fn remove_device(b: &dyn Backend, dev: &str) -> Result<usize, SyncError> {
    if !manifest::valid_device_id(dev) {
        return Err(SyncError::Local(format!("非法设备号: {dev}")));
    }
    // 先删清单（代次升序、最新最后），最后删 info：中途失败时剩下的仍能认出是同一个号
    let mut objs: Vec<_> = b
        .list(&manifest::device_dir_prefix(dev))?
        .into_iter()
        .filter(|o| manifest::safe_key(&o.key))
        .map(|o| {
            let gen_ = manifest::parse_manifest_key(&o.key).map(|(_, g)| g);
            (gen_.is_none(), gen_.unwrap_or(0), o.key)
        })
        .collect();
    objs.sort();
    for (_, _, key) in &objs {
        b.delete(key)?;
    }
    Ok(objs.len())
}

#[cfg(test)]
#[path = "devices_tests.rs"]
mod tests;
