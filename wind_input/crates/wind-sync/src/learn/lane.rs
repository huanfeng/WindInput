//! 学习数据与统计的每设备独立远端通道（设计 §4.6）。
//!
//! 布局：`learn/<dev>/index.<n>`（目录，JSON 加密）与 `learn/<dev>/obj/<id>`（deflate 后加密的对象）。
//! 每台设备只写自己的目录；对象 id = blob_id(明文哈希)，目录以 `<dev>/index.<n>` 作 AAD 绑定位置。

use crate::backend::Backend;
use crate::crypto::{self, FilesKey, ObjKind, blob_id, content_hash};
use crate::error::SyncError;
use crate::learn::format::{deflate, inflate};
use crate::manifest::{
    learn_index_key, learn_obj_key, learn_prefix, learn_root, parse_learn_index_key, safe_key,
    valid_device_id,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// 目录保留的代数：新编号 − 2 及更早的删除。
const KEEP_INDEXES: u64 = 2;
/// 回收对象的最小年龄（秒）。
pub const GC_MIN_AGE_SECS: u64 = 7 * 86400;

/// 一代目录：对象名（如 `pinyin/freq.base`）→ 明文内容哈希。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Index {
    #[serde(rename = "gen")]
    pub gen_: u64,
    pub objects: BTreeMap<String, String>,
}

/// 他机的一次更新。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerUpdate {
    pub dev: String,
    /// 目录解析成功时为新目录的世代；失败时为本地已知世代（不推进）。
    pub gen_: u64,
    /// 目录解析成功时为新目录；失败时为空目录（世代取已知值）。
    pub index: Index,
    pub index_ok: bool,
    /// 变化的对象明文；`None` = 在新目录里被删除。失败的对象不出现，记入 warnings。
    pub objects: BTreeMap<String, Option<Vec<u8>>>,
    pub warnings: Vec<String>,
}

impl PeerUpdate {
    /// 目录与全部变化对象都取到了。仅 complete 时才推进 known，否则下轮会重试。
    pub fn complete(&self) -> bool {
        self.index_ok && self.warnings.is_empty()
    }
}

fn valid_object_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('\\')
        && !name.chars().any(char::is_control)
        && name
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

fn index_aad_id(dev: &str, n: u64) -> String {
    format!("{dev}/index.{n}")
}

fn seal_index(key: &FilesKey, dev: &str, idx: &Index) -> Vec<u8> {
    let json = serde_json::to_vec(idx).expect("Index 序列化不会失败");
    crypto::seal(key, ObjKind::Learn, &index_aad_id(dev, idx.gen_), &json)
}

fn open_index(key: &FilesKey, dev: &str, n: u64, raw: &[u8]) -> Result<Index, String> {
    let json = crypto::open(key, ObjKind::Learn, &index_aad_id(dev, n), raw)
        .map_err(|e| format!("目录解密失败: {e}"))?;
    let idx: Index = serde_json::from_slice(&json).map_err(|e| format!("目录解析失败: {e}"))?;
    if idx.gen_ != n {
        return Err(format!("目录世代 {} 与编号 {n} 不符", idx.gen_));
    }
    Ok(idx)
}

fn list_indexes(b: &dyn Backend, prefix: &str) -> Result<Vec<(String, u64, String)>, SyncError> {
    Ok(b.list(prefix)?
        .into_iter()
        .filter(|o| safe_key(&o.key))
        .filter_map(|o| parse_learn_index_key(&o.key).map(|(d, n)| (d, n, o.key)))
        .collect())
}

/// 发布本机的对象集合：只上传哈希相对 `prev` 变化的对象，全部成功后写新目录，
/// 再删除新编号 − 2 及更早的旧目录。
pub fn publish(
    b: &dyn Backend,
    key: &FilesKey,
    dev: &str,
    prev: Option<&Index>,
    objects: &BTreeMap<String, Vec<u8>>,
) -> Result<Index, SyncError> {
    if !valid_device_id(dev) {
        return Err(SyncError::Local(format!("设备号不合法: {dev}")));
    }
    if let Some(bad) = objects.keys().find(|n| !valid_object_name(n)) {
        return Err(SyncError::Local(format!("对象名不合法: {bad}")));
    }
    let own = list_indexes(b, &learn_prefix(dev))?;
    let remote_max = own
        .iter()
        .filter(|(d, ..)| d == dev)
        .map(|(_, n, _)| *n)
        .max()
        .unwrap_or(0);
    let new_gen = prev.map_or(0, |p| p.gen_).max(remote_max) + 1;

    let mut index = Index {
        gen_: new_gen,
        objects: BTreeMap::new(),
    };
    let mut uploaded = BTreeSet::new();
    for (name, plain) in objects {
        let hash = content_hash(plain);
        let unchanged = prev.is_some_and(|p| p.objects.get(name) == Some(&hash));
        if !unchanged {
            let id = blob_id(key, &hash);
            if uploaded.insert(id.clone()) {
                let sealed = crypto::seal(key, ObjKind::Learn, &id, &deflate(plain));
                b.put(&learn_obj_key(dev, &id), &sealed)?;
            }
        }
        index.objects.insert(name.clone(), hash);
    }
    b.put(
        &learn_index_key(dev, new_gen),
        &seal_index(key, dev, &index),
    )?;
    for (_, n, k) in own {
        if n + KEEP_INDEXES <= new_gen {
            b.delete(&k)?;
        }
    }
    Ok(index)
}

/// 取各他机的最新目录与变化的对象。`known` = 设备 → 本地已应用的那份目录。
pub fn fetch_peers(
    b: &dyn Backend,
    key: &FilesKey,
    me: &str,
    known: &BTreeMap<String, Index>,
) -> Result<Vec<PeerUpdate>, SyncError> {
    let mut latest: BTreeMap<String, u64> = BTreeMap::new();
    for (d, n, _) in list_indexes(b, &learn_root())? {
        if d != me {
            let e = latest.entry(d).or_insert(n);
            *e = (*e).max(n);
        }
    }
    let mut out = Vec::new();
    for (dev, n) in latest {
        let prev = known.get(&dev);
        let prev_gen = prev.map_or(0, |p| p.gen_);
        if n <= prev_gen {
            continue;
        }
        let mut upd = PeerUpdate {
            dev: dev.clone(),
            gen_: prev_gen,
            index: Index {
                gen_: prev_gen,
                objects: BTreeMap::new(),
            },
            index_ok: false,
            objects: BTreeMap::new(),
            warnings: Vec::new(),
        };
        let idx = match b.get(&learn_index_key(&dev, n))? {
            None => Err("目录已不存在".to_string()),
            Some(raw) => open_index(key, &dev, n, &raw),
        };
        let idx = match idx {
            Ok(i) => i,
            Err(e) => {
                upd.warnings
                    .push(format!("设备 {dev} 的目录 {n} 不可用: {e}"));
                out.push(upd);
                continue;
            }
        };
        upd.gen_ = idx.gen_;
        upd.index_ok = true;
        if let Some(p) = prev {
            for name in p.objects.keys().filter(|k| !idx.objects.contains_key(*k)) {
                if valid_object_name(name) {
                    upd.objects.insert(name.clone(), None);
                }
            }
        }
        for (name, hash) in &idx.objects {
            if !valid_object_name(name) {
                upd.warnings
                    .push(format!("设备 {dev} 的对象名不合法，已跳过: {name}"));
                continue;
            }
            if prev.is_some_and(|p| p.objects.get(name) == Some(hash)) {
                continue;
            }
            let id = blob_id(key, hash);
            // 后端错误向上传（可重试）；对象缺失、损坏只记警告。
            let Some(raw) = b.get(&learn_obj_key(&dev, &id))? else {
                upd.warnings
                    .push(format!("设备 {dev} 的对象 {name} 不存在"));
                continue;
            };
            match decode_object(key, &id, hash, &raw) {
                Ok(plain) => {
                    upd.objects.insert(name.clone(), Some(plain));
                }
                Err(e) => upd
                    .warnings
                    .push(format!("设备 {dev} 的对象 {name} 不可用: {e}")),
            }
        }
        upd.index = idx;
        out.push(upd);
    }
    Ok(out)
}

fn decode_object(key: &FilesKey, id: &str, hash: &str, raw: &[u8]) -> Result<Vec<u8>, String> {
    let packed = crypto::open(key, ObjKind::Learn, id, raw).map_err(|e| e.to_string())?;
    let plain = inflate(&packed).map_err(|e| e.to_string())?;
    if content_hash(&plain) != hash {
        return Err("内容哈希不符".into());
    }
    Ok(plain)
}

/// 回收本机目录下不被 `keep` 引用、且修改时间早于 `now - 7 天` 的对象。
/// 后端给不出修改时间的一律保留。返回删除个数。
pub fn gc_own(
    b: &dyn Backend,
    key: &FilesKey,
    dev: &str,
    keep: &[&Index],
    now: u64,
) -> Result<usize, SyncError> {
    if !valid_device_id(dev) {
        return Err(SyncError::Local(format!("设备号不合法: {dev}")));
    }
    let referenced: BTreeSet<String> = keep
        .iter()
        .flat_map(|i| i.objects.values())
        .map(|h| learn_obj_key(dev, &blob_id(key, h)))
        .collect();
    let mut n = 0;
    let obj_prefix = learn_obj_key(dev, "");
    for o in b.list(&obj_prefix)? {
        if !o.key.starts_with(&obj_prefix) {
            continue;
        }
        let old = o.modified.is_some_and(|t| t + GC_MIN_AGE_SECS <= now);
        if old && safe_key(&o.key) && !referenced.contains(&o.key) {
            b.delete(&o.key)?;
            n += 1;
        }
    }
    Ok(n)
}

/// 删除某设备在学习通道下的全部对象（设备被移除时）。返回删除个数。
pub fn purge_device(b: &dyn Backend, dev: &str) -> Result<usize, SyncError> {
    if !valid_device_id(dev) {
        return Err(SyncError::Local(format!("设备号不合法: {dev}")));
    }
    let mut n = 0;
    let prefix = learn_prefix(dev);
    for o in b.list(&prefix)? {
        if o.key.starts_with(&prefix) && safe_key(&o.key) {
            b.delete(&o.key)?;
            n += 1;
        }
    }
    Ok(n)
}
