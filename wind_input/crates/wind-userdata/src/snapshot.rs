//! 快照段（设计 §3.1）：wdat（带简拼区，含无边界兜底组）+ 定长 sidecar `.meta`。
//!
//! sidecar 头 32 字节：`"WUDM" | version u16 | reserved u16 | max_seq u64 | entry_count u32 |
//! gen u64 | reserved [u8;4]`（version = 2），其后每条 28 字节 `count u32 | created_at i64 |
//! last_used i64 | flags u8 | pad [u8;3] | order u32`，按 wdat 条目的**全局序号**对齐（DAT 键序 × 叶内序）。`max_seq` 是快照
//! 内嵌的折回水位：state 丢了也能只回放更大的 seq（§3.6）。
//!
//! **sidecar 是提交点**：wdat 按代号落成 `<name>-<gen>.wdat`（每次折回一个新路径，从不覆盖
//! 正被映射的文件，也就不需要 `reader_pool::replacing`），sidecar 头里记 gen 指向它，
//! sidecar 原子写完才算换段。崩溃在两步之间只留下一个无人引用的新 wdat，旧段原样可读；
//! 无人引用的代号文件在下一次换段后清掉（Windows 上仍被映射的删不掉，留给再下一次）。
//!
//! 零条目快照只有 sidecar（带水位、gen = 0），没有 wdat：清空之后 state 丢了，被清空的记录也
//! 不会因水位缺失而回放。
//!
//! 简拼区：每个条目按 [`crate::delta::group_of`] 的规则得一个键（有边界 ⇒ 首字母串；无边界
//! ⇒ `\u{1}` + 首字符），同码同键只留一个、权重取其中最高——这就是 `wind-store`
//! `USER_ABBREV` 兜底组在段里的等价物。同码异边界（xian 下的「先」与「西安」）各得各的键。

use anyhow::bail;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wind_dict::datformat::{WdatReader, WdatStreamWriter};

#[cfg(test)]
thread_local! {
    /// 故障注入：下一次 `build` 在 wdat 落盘后、sidecar 落盘前失败（模拟崩溃在两步之间）。
    pub(crate) static FAIL_BEFORE_META: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

const META_MAGIC: &[u8; 4] = b"WUDM";
const META_HEADER: usize = 32;
const META_ROW: usize = 28;
/// sidecar 格式版本：2 = 每行带 `order`（入库顺序）。读到别的版本 ⇒ `Err`（当零段并整份回放）。
const META_VERSION: u16 = 2;

#[derive(Clone, Debug, PartialEq)]
pub struct SnapshotEntry {
    pub code: String,
    pub text: String,
    pub weight: i32,
    pub boundary: u64,
    pub count: u32,
    pub created_at: i64,
    pub last_used: i64,
    pub flags: u8,
    /// 入库顺序：文本目标 = 行号（用户词层的 `natural_order`）；运行时目标 0。
    pub order: u32,
}

pub struct Snapshot {
    /// 零条目快照没有 wdat。
    reader: Option<Arc<WdatReader>>,
    meta: wind_dict::mapping::Mapped,
    pub max_seq: u64,
    pub entry_count: u32,
}

/// 从**按 (code, text) 排好序**的条目流建段：`<dir>/<name>.wdat` + `<dir>/<name>.meta`。
pub fn build(
    dir: &Path,
    name: &str,
    max_seq: u64,
    entries: impl Iterator<Item = SnapshotEntry>,
) -> anyhow::Result<Snapshot> {
    std::fs::create_dir_all(dir)?;
    let mut sw = WdatStreamWriter::new();
    let mut meta: Vec<u8> = Vec::new();
    // (简拼键, code, 该 code 最高权重)
    let mut abbrevs: Vec<(String, String, i32)> = Vec::new();
    let mut n = 0u32;

    fn flush(
        sw: &mut WdatStreamWriter,
        meta: &mut Vec<u8>,
        abbrevs: &mut Vec<(String, String, i32)>,
        code: &str,
        g: &mut Vec<SnapshotEntry>,
    ) {
        g.sort_by_key(|e| std::cmp::Reverse(e.weight));
        // 已按权重降序：同键第一次出现时的权重就是该键下最高的
        let mut keys: Vec<String> = Vec::new();
        for e in g.iter() {
            let key = crate::delta::group_of(code, e.boundary);
            if !key.is_empty() && !keys.contains(&key) {
                abbrevs.push((key.clone(), code.to_string(), e.weight));
                keys.push(key);
            }
        }
        sw.push_code(
            code,
            g.iter()
                .enumerate()
                .map(|(i, e)| (e.text.as_str(), e.weight, i as u32, e.boundary)),
        );
        for e in g.iter() {
            meta.extend_from_slice(&e.count.to_le_bytes());
            meta.extend_from_slice(&e.created_at.to_le_bytes());
            meta.extend_from_slice(&e.last_used.to_le_bytes());
            meta.push(e.flags);
            meta.extend_from_slice(&[0u8; 3]);
            meta.extend_from_slice(&e.order.to_le_bytes());
        }
        g.clear();
    }

    let mut cur: Option<String> = None;
    let mut group: Vec<SnapshotEntry> = Vec::new();
    for e in entries {
        if cur.as_deref() != Some(e.code.as_str()) {
            if let Some(c) = cur.take() {
                flush(&mut sw, &mut meta, &mut abbrevs, &c, &mut group);
            }
            cur = Some(e.code.clone());
        }
        group.push(e);
        n += 1;
    }
    if let Some(c) = cur.take() {
        flush(&mut sw, &mut meta, &mut abbrevs, &c, &mut group);
    }
    abbrevs.sort_by(|a, b| a.0.cmp(&b.0).then(b.2.cmp(&a.2)).then(a.1.cmp(&b.1)));
    let mut i = 0;
    while i < abbrevs.len() {
        let k = abbrevs[i].0.clone();
        let mut codes: Vec<(&str, i32)> = Vec::new();
        while i < abbrevs.len() && abbrevs[i].0 == k {
            codes.push((abbrevs[i].1.as_str(), abbrevs[i].2));
            i += 1;
        }
        sw.push_abbrev_sorted(&k, codes);
    }

    if n == 0 {
        // 零段也是快照：只有 sidecar（带水位），没有 wdat。
        crate::state::atomic_write(&meta_path(dir, name), &meta_header(max_seq, 0, 0))?;
        remove_stale_wdats(dir, name, None);
        return open(dir, name)?.ok_or_else(|| anyhow::anyhow!("刚建好的空段打不开"));
    }
    let mut generation = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    while wdat_path(dir, name, generation).exists() {
        generation += 1;
    }
    let wdat = wdat_path(dir, name, generation);
    sw.build().emit(&wdat)?;
    crate::state::fsync_file(&wdat)?;
    #[cfg(test)]
    if FAIL_BEFORE_META.with(|f| f.replace(false)) {
        bail!("故障注入：wdat 已写、sidecar 未写");
    }
    let mut head = meta_header(max_seq, n, generation);
    head.extend_from_slice(&meta);
    crate::state::atomic_write(&meta_path(dir, name), &head)?;
    remove_stale_wdats(dir, name, Some(generation));
    open(dir, name)?.ok_or_else(|| anyhow::anyhow!("刚建好的段打不开"))
}

fn meta_header(max_seq: u64, n: u32, generation: u64) -> Vec<u8> {
    let mut head = Vec::with_capacity(META_HEADER);
    head.extend_from_slice(META_MAGIC);
    head.extend_from_slice(&META_VERSION.to_le_bytes());
    head.extend_from_slice(&0u16.to_le_bytes());
    head.extend_from_slice(&max_seq.to_le_bytes());
    head.extend_from_slice(&n.to_le_bytes());
    head.extend_from_slice(&generation.to_le_bytes());
    head.extend_from_slice(&[0u8; 4]);
    head
}

fn meta_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.meta"))
}

fn wdat_path(dir: &Path, name: &str, generation: u64) -> PathBuf {
    dir.join(format!("{name}-{generation:016x}.wdat"))
}

/// 删掉 `<name>-<gen>.wdat` 里除 `keep` 以外的代号文件（尽力而为：Windows 上仍被映射的删不掉）。
fn remove_stale_wdats(dir: &Path, name: &str, keep: Option<u64>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let prefix = format!("{name}-");
    for e in rd.flatten() {
        let file = e.file_name();
        let Some(f) = file.to_str() else { continue };
        let Some(hex) = f
            .strip_prefix(&prefix)
            .and_then(|r| r.strip_suffix(".wdat"))
        else {
            continue;
        };
        let Ok(g) = u64::from_str_radix(hex, 16) else {
            continue;
        };
        if hex.len() == 16 && Some(g) != keep {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// sidecar 不存在 ⇒ `Ok(None)`（零段）；在但头坏、指向的 wdat 不在或条目数对不上 ⇒ `Err`
/// （调用方当零段，并对该目标整份回放日志）。
pub fn open(dir: &Path, name: &str) -> anyhow::Result<Option<Snapshot>> {
    let meta_path = meta_path(dir, name);
    if !meta_path.is_file() {
        return Ok(None);
    }
    let meta = wind_dict::mapping::map_file(&std::fs::File::open(&meta_path)?)?;
    if meta.len() < META_HEADER
        || &meta[..4] != META_MAGIC
        || u16::from_le_bytes([meta[4], meta[5]]) != META_VERSION
    {
        bail!("sidecar 头损坏: {}", meta_path.display());
    }
    let max_seq = u64::from_le_bytes(meta[8..16].try_into()?);
    let entry_count = u32::from_le_bytes(meta[16..20].try_into()?);
    if entry_count == 0 {
        return Ok(Some(Snapshot {
            reader: None,
            meta,
            max_seq,
            entry_count: 0,
        }));
    }
    let generation = u64::from_le_bytes(meta[20..28].try_into()?);
    let wdat = wdat_path(dir, name, generation);
    if !wdat.is_file() {
        bail!("sidecar 指向的段不在: {}", wdat.display());
    }
    let reader = wind_dict::reader_pool::open_wdat(&wdat)?;
    let mut total = 0u32;
    for (_, leaf) in reader.codes() {
        total += reader.leaf_entries(leaf).len() as u32;
    }
    if total != entry_count || meta.len() < META_HEADER + entry_count as usize * META_ROW {
        bail!("sidecar 与 wdat 对不上: {} 条 vs {} 条", entry_count, total);
    }
    Ok(Some(Snapshot {
        reader: Some(reader),
        meta,
        max_seq,
        entry_count,
    }))
}

impl Snapshot {
    /// 第 `entry_idx` 条的入库顺序；越界给零。
    pub fn order_of(&self, entry_idx: u32) -> u32 {
        let o = META_HEADER + entry_idx as usize * META_ROW + 24;
        self.meta
            .get(o..o + 4)
            .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// 第 `entry_idx` 条的 `(count, created_at, last_used, flags)`；越界给零。
    pub fn meta_of(&self, entry_idx: u32) -> (u32, i64, i64, u8) {
        let o = META_HEADER + entry_idx as usize * META_ROW;
        let Some(row) = self.meta.get(o..o + META_ROW) else {
            return (0, 0, 0, 0);
        };
        (
            u32::from_le_bytes(row[0..4].try_into().unwrap()),
            i64::from_le_bytes(row[4..12].try_into().unwrap()),
            i64::from_le_bytes(row[12..20].try_into().unwrap()),
            row[20],
        )
    }

    /// 全局序号 = 该码叶的首序号 + 叶内序。首序号直接从叶表算（不常驻「码 → 序号」表：
    /// 19 万条用户词那张表要十几 MB 堆）。
    fn idx_of(&self, code: &str, order: i32) -> u32 {
        let first = self
            .reader
            .as_deref()
            .and_then(|r| Some(r.leaf_first_entry(r.leaf_of(code)?)))
            .unwrap_or(0);
        first + order.max(0) as u32
    }

    fn entry(
        &self,
        code: &str,
        text: String,
        weight: i32,
        boundary: u64,
        idx: u32,
    ) -> SnapshotEntry {
        let (count, created_at, last_used, flags) = self.meta_of(idx);
        SnapshotEntry {
            code: code.to_string(),
            text,
            weight,
            boundary,
            count,
            created_at,
            last_used,
            flags,
            order: self.order_of(idx),
        }
    }

    /// 同码按权重降序（建段时的叶内序）。
    pub fn exact(&self, code: &str) -> Vec<SnapshotEntry> {
        let Some(r) = self.reader.as_deref() else {
            return Vec::new();
        };
        r.search(code)
            .into_iter()
            .map(|e| {
                let idx = self.idx_of(code, e.order);
                self.entry(code, e.text, e.weight, e.boundary, idx)
            })
            .collect()
    }

    /// `limit == 0` 由 [`Self::for_each_prefix`] 负责（`WdatReader::search_prefix(…, 0)` 返回空）。
    pub fn prefix(&self, prefix: &str, limit: usize) -> Vec<SnapshotEntry> {
        if limit == 0 {
            let mut v = Vec::new();
            self.for_each_prefix(prefix, &mut |e| {
                v.push(e);
                true
            });
            return v;
        }
        let Some(r) = self.reader.as_deref() else {
            return Vec::new();
        };
        r.search_prefix(prefix, limit)
            .into_iter()
            .map(|e| {
                let idx = self.idx_of(&e.code, e.order);
                self.entry(&e.code, e.text, e.weight, e.boundary, idx)
            })
            .collect()
    }

    /// 同 [`Self::prefix`]，只取 [`wind_dict::cached::prefix_capped_keep`] 放行的条目，
    /// 过滤在 wdat 里参与剪枝（权重全打平的导入词库靠它才不整棵子树遍历）。
    pub fn prefix_capped(
        &self,
        prefix: &str,
        limit: usize,
        max_syllables: u32,
        completed_len: usize,
    ) -> Vec<SnapshotEntry> {
        if limit == 0 {
            let mut v = self.prefix(prefix, 0);
            v.retain(|e| {
                wind_dict::cached::prefix_capped_keep(
                    e.boundary,
                    e.code.len(),
                    max_syllables,
                    completed_len,
                )
            });
            return v;
        }
        let Some(r) = self.reader.as_deref() else {
            return Vec::new();
        };
        r.search_prefix_syllable_capped(prefix, limit, max_syllables, completed_len)
            .into_iter()
            .map(|e| {
                let idx = self.idx_of(&e.code, e.order);
                self.entry(&e.code, e.text, e.weight, e.boundary, idx)
            })
            .collect()
    }

    /// 简拼：简拼区给出全拼码，再回主表装配条目；按权重降序截到 `limit`。
    pub fn abbrev(&self, abbrev: &str, limit: usize) -> Vec<SnapshotEntry> {
        let Some(r) = self.reader.as_deref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for a in r.search_abbrev(abbrev, 0) {
            out.extend(self.exact(&a.text));
        }
        out.sort_by(|x, y| y.weight.cmp(&x.weight).then(x.code.cmp(&y.code)));
        if limit > 0 {
            out.truncate(limit);
        }
        out
    }

    /// 按 (code, 叶内序) 顺序遍历 code 以 `prefix` 开头的条目；`f` 返回 false 即停。
    pub fn for_each_prefix(&self, prefix: &str, f: &mut dyn FnMut(SnapshotEntry) -> bool) {
        let Some(r) = self.reader.as_deref() else {
            return;
        };
        for (code, leaf) in r.codes() {
            if !code.starts_with(prefix) {
                if code.as_str() > prefix {
                    break;
                }
                continue;
            }
            let first = r.leaf_first_entry(leaf);
            for (i, (text, weight, _order, boundary)) in
                r.leaf_entries(leaf).into_iter().enumerate()
            {
                if !f(self.entry(&code, text.to_string(), weight, boundary, first + i as u32)) {
                    return;
                }
            }
        }
    }

    /// 全量遍历（DAT 键序 × 叶内序 = 全局序号顺序），供折回合并用。
    pub fn for_each(&self, f: &mut dyn FnMut(SnapshotEntry)) {
        let Some(r) = self.reader.as_deref() else {
            return;
        };
        let mut idx = 0u32;
        for (code, leaf) in r.codes() {
            for (text, weight, _order, boundary) in r.leaf_entries(leaf) {
                let (count, created_at, last_used, flags) = self.meta_of(idx);
                f(SnapshotEntry {
                    code: code.clone(),
                    text: text.to_string(),
                    weight,
                    boundary,
                    count,
                    created_at,
                    last_used,
                    flags,
                    order: self.order_of(idx),
                });
                idx += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn se(code: &str, text: &str, w: i32, b: u64, count: u32) -> SnapshotEntry {
        SnapshotEntry {
            code: code.into(),
            text: text.into(),
            weight: w,
            boundary: b,
            count,
            created_at: 100,
            last_used: 200,
            flags: 0,
            order: 0,
        }
    }

    #[test]
    fn build_open_query_and_meta_roundtrip() {
        let d = std::env::temp_dir().join(format!("wind_snap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mut es = vec![
            se("nihao", "你好", 10, 0b101, 3),
            se("ni", "你", 50, 0b1, 9),
            se("nihao", "拟好", 5, 0b101, 1),
            se("wubi", "五笔", 7, 0, 2),
        ];
        es.sort_by(|a, b| {
            (a.code.as_str(), a.text.as_str()).cmp(&(b.code.as_str(), b.text.as_str()))
        });
        let s = build(&d, "temp", 42, es.into_iter()).unwrap();
        assert_eq!(s.max_seq, 42);
        assert_eq!(s.entry_count, 4);
        drop(s);
        let s = open(&d, "temp").unwrap().expect("应能打开");
        assert_eq!(s.max_seq, 42);
        let hits = s.exact("nihao");
        assert_eq!(
            hits.iter().map(|h| h.text.as_str()).collect::<Vec<_>>(),
            vec!["你好", "拟好"],
            "同码按权重降序"
        );
        assert_eq!(
            (hits[0].count, hits[0].created_at, hits[0].last_used),
            (3, 100, 200)
        );
        assert_eq!(hits[1].count, 1, "sidecar 与条目一一对齐");
        // 简拼命中的是**候选**：nihao 下两条（你好、拟好）都算
        assert_eq!(s.abbrev("nh", 10).len(), 2, "多音节简拼");
        assert_eq!(s.abbrev("nh", 1).len(), 1, "limit 截断");
        assert_eq!(s.abbrev("\u{1}w", 10).len(), 1, "无边界兜底组");
        assert_eq!(s.prefix("ni", 10).len(), 3);
        let mut seen = Vec::new();
        s.for_each(&mut |e| seen.push((e.code.clone(), e.count)));
        assert_eq!(seen.len(), 4);
        assert!(seen.contains(&("wubi".to_string(), 2)));
        assert!(
            open(&d, "nope").unwrap().is_none(),
            "没有文件 = 零段，不是错误"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 每条的 sidecar 行按全局序号取：码多、叶长不一时，各查询入口取到的都得是自己那一行。
    #[test]
    fn meta_rows_line_up_across_codes_of_uneven_leaves() {
        let d = std::env::temp_dir().join(format!("wind_snap_rows-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mut es = Vec::new();
        for c in 0..40u32 {
            for t in 0..(c % 5 + 1) {
                // count 编码 (码, 词)，权重与词序反向，叶内要重排
                es.push(se(
                    &format!("k{c:03}"),
                    &format!("w{c}-{t}"),
                    t as i32,
                    0,
                    c * 10 + t,
                ));
            }
        }
        es.sort_by(|a, b| {
            (a.code.as_str(), a.text.as_str()).cmp(&(b.code.as_str(), b.text.as_str()))
        });
        build(&d, "user", 1, es.into_iter()).unwrap();
        let s = open(&d, "user").unwrap().unwrap();
        let want = |e: &SnapshotEntry| {
            let c: u32 = e.code[1..].parse().unwrap();
            let t: u32 = e.text.rsplit('-').next().unwrap().parse().unwrap();
            c * 10 + t
        };
        for c in [0u32, 7, 23, 39] {
            for e in s.exact(&format!("k{c:03}")) {
                assert_eq!(e.count, want(&e), "exact {}/{}", e.code, e.text);
            }
        }
        for e in s.prefix("k01", 100) {
            assert_eq!(e.count, want(&e), "prefix {}/{}", e.code, e.text);
        }
        let mut n = 0;
        s.for_each_prefix("k02", &mut |e| {
            assert_eq!(e.count, want(&e), "for_each_prefix {}/{}", e.code, e.text);
            n += 1;
            true
        });
        assert!(n > 10);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 同码异边界：xian 下「先」(x) 与「西安」(x'a) 两条，两个简拼都要召回。
    #[test]
    fn abbrev_keys_cover_every_boundary_under_one_code() {
        let d = std::env::temp_dir().join(format!("wind_snap_ab-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let es = vec![
            se("xian", "先", 100, 0b1, 0),
            se("xian", "西安", 50, 0b101, 0),
        ];
        let s = build(&d, "t", 1, es.into_iter()).unwrap();
        assert!(s.abbrev("x", 10).iter().any(|h| h.text == "先"));
        assert!(
            s.abbrev("xa", 10).iter().any(|h| h.text == "西安"),
            "低权重条目的边界也要建键"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 崩溃落在「新 wdat 已写、新 sidecar 未写」之间：旧段必须原样可读。
    #[test]
    fn crash_between_wdat_and_meta_keeps_previous_snapshot() {
        let d = std::env::temp_dir().join(format!("wind_snap_half-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let old = vec![
            se("a", "甲", 1, 0, 1),
            se("b", "乙", 1, 0, 1),
            se("c", "丙", 1, 0, 1),
        ];
        drop(build(&d, "t", 3, old.into_iter()).unwrap());
        FAIL_BEFORE_META.with(|f| f.set(true));
        assert!(build(&d, "t", 4, vec![se("d", "丁", 1, 0, 1)].into_iter()).is_err());
        let s = open(&d, "t").unwrap().expect("旧段应仍可打开");
        assert_eq!((s.max_seq, s.entry_count), (3, 3));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 旧段还映射着（查询读者仍持有）时重建同名段：Windows 上不能因 sidecar 被映射而写不进。
    #[test]
    fn rebuild_while_previous_snapshot_is_still_mapped() {
        let d = std::env::temp_dir().join(format!("wind_snap_live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let old = build(&d, "t", 1, vec![se("a", "甲", 1, 0, 1)].into_iter()).unwrap();
        let new = build(&d, "t", 2, vec![se("b", "乙", 1, 0, 1)].into_iter()).unwrap();
        let empty = build(&d, "t", 3, std::iter::empty()).unwrap();
        assert_eq!(old.exact("a").len(), 1, "旧段仍可读");
        assert_eq!(new.exact("b").len(), 1);
        assert_eq!((empty.max_seq, empty.entry_count), (3, 0));
        let s = open(&d, "t").unwrap().expect("最新段可打开");
        assert_eq!((s.max_seq, s.entry_count), (3, 0));
        drop((old, new, empty, s));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn mismatched_sidecar_is_an_error_not_a_silent_zero() {
        let d = std::env::temp_dir().join(format!("wind_snap_bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let s = build(&d, "t", 1, vec![se("a", "甲", 1, 0, 1)].into_iter()).unwrap();
        drop(s);
        std::fs::write(d.join("t.meta"), b"WUDM").unwrap();
        assert!(open(&d, "t").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 零条目也是一个快照：它带着水位，清空之后 state 丢了也不会让旧记录复活。
    #[test]
    fn empty_build_keeps_watermark_and_drops_old_segment() {
        let d = std::env::temp_dir().join(format!("wind_snap_empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        drop(build(&d, "t", 3, vec![se("a", "甲", 1, 0, 1)].into_iter()).unwrap());
        let s = build(&d, "t", 7, std::iter::empty()).unwrap();
        assert_eq!((s.max_seq, s.entry_count), (7, 0));
        assert!(s.exact("a").is_empty());
        let wdats = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "wdat"))
            .count();
        assert_eq!(wdats, 0, "旧代号文件已清");
        let s = open(&d, "t").unwrap().expect("零条目 sidecar 也能打开");
        assert_eq!(s.max_seq, 7);
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn order_survives_build_and_open() {
        let d = std::env::temp_dir().join(format!("wind_snap_order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mut a = se("ni", "你", 5, 0b1, 0);
        a.order = 7;
        let mut b = se("ni", "泥", 5, 0b1, 0);
        b.order = 3;
        drop(build(&d, "t", 1, vec![a, b].into_iter()).unwrap());
        let s = open(&d, "t").unwrap().unwrap();
        let mut got: Vec<(String, u32)> = s
            .exact("ni")
            .into_iter()
            .map(|e| (e.text, e.order))
            .collect();
        got.sort();
        assert_eq!(got, vec![("你".into(), 7), ("泥".into(), 3)]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
