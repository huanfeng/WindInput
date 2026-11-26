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
    /// 其中任一设备最新两代清单都不再引用的（等待回收）。
    pub blobs_unreferenced: u64,
    /// `learn/<dev>/`，按设备号排序。
    pub devices: Vec<DeviceUsage>,
    /// 其余：`meta.json`、设备清单与设备信息等。
    pub other: u64,
}

pub fn compute(b: &dyn Backend, dek: &Dek) -> Result<RemoteUsage, SyncError> {
    let root = format!("{ROOT}/");
    let objs: Vec<ObjMeta> = b
        .list(&root)?
        .into_iter()
        .filter(|o| manifest::listed_within(&o.key, &root))
        .collect();
    let referenced = remote::referenced_blobs(b, dek, objs.iter().map(|o| o.key.as_str()))?;
    let (blobs_prefix, learn_root) = (manifest::blobs_prefix(), manifest::learn_root());
    let mut u = RemoteUsage::default();
    let mut learn: BTreeMap<&str, Vec<&ObjMeta>> = BTreeMap::new();
    for o in &objs {
        u.total += o.size;
        if o.key.starts_with(&blobs_prefix) {
            u.blobs += o.size;
            if !referenced.contains(&o.key) {
                u.blobs_unreferenced += o.size;
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
