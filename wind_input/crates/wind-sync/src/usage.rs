//! 远端空间占用（设计 §12）：列一次同步根，按前缀分类求和。

use std::collections::BTreeMap;

use crate::backend::{Backend, ObjMeta};
use crate::crypto::Dek;
use crate::error::SyncError;
use crate::learn::lane;
use crate::manifest::{self, ROOT};
use crate::remote;

/// 一台设备在学习通道下的占用。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceUsage {
    pub id: String,
    /// 学习数据（含目录与待回收的旧对象）。
    pub learned: u64,
    /// 打字统计。
    pub stats: u64,
}

/// 同步根之内的占用（字节）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteUsage {
    pub total: u64,
    /// `blobs/`：文件内容，各设备共用、按内容去重。
    pub blobs: u64,
    /// 其中任一设备最新两代清单都不再引用的；有清单解不开时无法确定，为 None。
    pub blobs_unreferenced: Option<u64>,
    /// `learn/<dev>/`，按设备号排序。
    pub devices: Vec<DeviceUsage>,
    /// 其余：`meta.json`、`devices/<dev>/` 下的清单与设备信息，以及同步根下不认识的对象。
    pub other: u64,
}

/// 统计中途收到取消时的错误文案。
pub const CANCELLED: &str = "统计已取消";

pub fn compute(b: &dyn Backend, dek: &Dek) -> Result<RemoteUsage, SyncError> {
    compute_with(b, dek, &|| false)
}

/// 同 [`compute`]，每次远端读取前检查 `cancelled`，为真即返回 [`CANCELLED`]。
/// 只有后端错误让整次失败；清单解不开只让 `blobs_unreferenced` 为 None。
pub fn compute_with(
    b: &dyn Backend,
    dek: &Dek,
    cancelled: &dyn Fn() -> bool,
) -> Result<RemoteUsage, SyncError> {
    let stop = || {
        if cancelled() {
            Err(SyncError::Local(CANCELLED.into()))
        } else {
            Ok(())
        }
    };
    let root = format!("{ROOT}/");
    let objs: Vec<ObjMeta> = b
        .list(&root)?
        .into_iter()
        .filter(|o| manifest::listed_within(&o.key, &root))
        .collect();
    stop()?;
    let referenced = match remote::referenced_blobs(b, dek, objs.iter().map(|o| o.key.as_str())) {
        Ok(r) => Some(r),
        Err(SyncError::Backend(e)) => return Err(e.into()),
        Err(e) => {
            eprintln!("[wind-sync] 有设备清单解不开，等待回收的大小无法确定: {e}");
            None
        }
    };
    let (blobs_prefix, learn_root) = (manifest::blobs_prefix(), manifest::learn_root());
    let mut u = RemoteUsage {
        blobs_unreferenced: referenced.as_ref().map(|_| 0),
        ..Default::default()
    };
    let mut learn: BTreeMap<&str, Vec<&ObjMeta>> = BTreeMap::new();
    for o in &objs {
        u.total += o.size;
        if o.key.starts_with(&blobs_prefix) {
            u.blobs += o.size;
            if let (Some(r), Some(n)) = (&referenced, u.blobs_unreferenced.as_mut())
                && !r.contains(&o.key)
            {
                *n += o.size;
            }
        } else if let Some(dev) = o
            .key
            .strip_prefix(&learn_root)
            .and_then(|r| r.split_once('/'))
            .map(|(d, _)| d)
            .filter(|d| manifest::valid_device_id(d))
        {
            learn.entry(dev).or_default().push(o);
        } else {
            u.other += o.size;
        }
    }
    let files = dek.files_key();
    for (dev, list) in learn {
        stop()?;
        let gens: Vec<u64> = list
            .iter()
            .filter_map(|o| manifest::parse_learn_index_key(&o.key).map(|(_, n)| n))
            .collect();
        let stats_keys = lane::stats_object_keys(b, &files, dev, &gens)?;
        let mut d = DeviceUsage {
            id: dev.to_string(),
            ..Default::default()
        };
        for o in list {
            if stats_keys.contains(&o.key) {
                d.stats += o.size;
            } else {
                d.learned += o.size;
            }
        }
        u.devices.push(d);
    }
    Ok(u)
}
