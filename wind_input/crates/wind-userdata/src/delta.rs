//! 两级增量表（设计 §3.3）：冻结层 `Arc<[Entry]>` 按键有序 + 尾巴 ≤ [`TAIL_MAX`] 条。写入只
//! 克隆尾巴；尾巴满时并入冻结层并重建分组索引。按键线程只读 `Arc<Delta>`，不碰锁。
//!
//! 同键多操作（晋升 = Delete + Upsert）按 seq 顺序各自保留，不合并成一条；折回只摘除
//! `seq ≤ 水位` 的该目标条目，折回期间新到的操作留下。
//!
//! 简拼分组索引复用 `wind-store::abbrev_index` 的规则（[`group_of`]）：有边界 ⇒ 音节首字母串；
//! 无边界 ⇒ `\u{1}` + 码首字符（兜底组）。线性扫 delta 算简拼在 2000 条时每键约 1 ms，
//! 不可用——所以冻结层带分组索引。

use crate::record::{OpKind, Payload, Record, Target};
use std::collections::BTreeMap;
use std::sync::Arc;

const TAIL_MAX: usize = 128;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub seq: u64,
    /// 写入时刻（毫秒，取自日志记录）；叠加时折成秒写进 created_at / last_used。
    pub ts_ms: i64,
    pub target: Target,
    pub schema: String,
    pub code: String,
    pub text: String,
    pub op: DeltaOp,
    /// 音节边界位图（与 wdat 条目同义；0 = 无边界信息）。
    pub boundary: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DeltaOp {
    Upsert { weight: i32, kind: u8 },
    Delete,
    SetWeight(i32),
    Incr(u32),
    Touch(i64),
}

/// 简拼分组键，规则与 `wind-store::abbrev_index::group_of` 一致。
pub fn group_of(code: &str, boundary: u64) -> String {
    if boundary != 0 {
        return code
            .char_indices()
            .filter(|(i, _)| *i < 64 && (boundary >> i) & 1 == 1)
            .map(|(_, c)| c)
            .collect();
    }
    match code.chars().next() {
        Some(c) => format!("\u{1}{c}"),
        None => String::new(),
    }
}

type Key<'a> = (Target, &'a str, &'a str, &'a str, u64);

fn key(e: &Entry) -> Key<'_> {
    (e.target, &e.schema, &e.code, &e.text, e.seq)
}

type GroupKey = (Target, String, String);

#[derive(Clone, Default)]
pub struct Delta {
    frozen: Arc<[Entry]>,
    tail: Vec<Entry>,
    /// (target, schema, 分组) → 冻结层索引。
    groups: Arc<BTreeMap<GroupKey, Vec<usize>>>,
    /// (target, schema) → 未折回条数。按键路径每次写后判折回阈值要用，不能逐条扫。
    counts: Arc<BTreeMap<(Target, String), usize>>,
}

impl Delta {
    /// 返回加了一条的新表：只克隆尾巴；尾巴满 [`TAIL_MAX`] 条时并入冻结层。
    pub fn with(&self, e: Entry) -> Delta {
        let mut tail = self.tail.clone();
        let mut counts = (*self.counts).clone();
        *counts.entry((e.target, e.schema.clone())).or_insert(0) += 1;
        tail.push(e);
        if tail.len() <= TAIL_MAX {
            return Delta {
                frozen: self.frozen.clone(),
                tail,
                groups: self.groups.clone(),
                counts: Arc::new(counts),
            };
        }
        let all: Vec<Entry> = self.frozen.iter().cloned().chain(tail).collect();
        Self::freeze(all)
    }

    /// 一次加一批：尾巴装得下就只克隆尾巴，否则整体并入冻结层**一次**——逐条 `with` 会每满
    /// 128 条重排一遍全表，大批导入成平方级。
    pub fn with_all(&self, es: Vec<Entry>) -> Delta {
        if self.tail.len() + es.len() <= TAIL_MAX {
            let mut d = self.clone();
            let mut counts = (*d.counts).clone();
            for e in &es {
                *counts.entry((e.target, e.schema.clone())).or_insert(0) += 1;
            }
            d.tail.extend(es);
            d.counts = Arc::new(counts);
            return d;
        }
        let all: Vec<Entry> = self
            .frozen
            .iter()
            .cloned()
            .chain(self.tail.iter().cloned())
            .chain(es)
            .collect();
        Self::freeze(all)
    }

    fn freeze(mut all: Vec<Entry>) -> Delta {
        all.sort_by(|a, b| key(a).cmp(&key(b)));
        let mut groups: BTreeMap<GroupKey, Vec<usize>> = BTreeMap::new();
        let mut counts: BTreeMap<(Target, String), usize> = BTreeMap::new();
        for (i, e) in all.iter().enumerate() {
            *counts.entry((e.target, e.schema.clone())).or_insert(0) += 1;
            groups
                .entry((e.target, e.schema.clone(), group_of(&e.code, e.boundary)))
                .or_default()
                .push(i);
        }
        Delta {
            frozen: all.into(),
            tail: Vec::new(),
            groups: Arc::new(groups),
            counts: Arc::new(counts),
        }
    }

    fn all(&self) -> impl Iterator<Item = &Entry> {
        self.frozen.iter().chain(self.tail.iter())
    }

    /// 某键的全部操作，按 seq 升序。冻结层按 (target, schema, code, text, seq) 有序，二分定位。
    pub fn ops_for(&self, target: Target, schema: &str, code: &str, text: &str) -> Vec<&Entry> {
        let lo = self
            .frozen
            .partition_point(|e| key(e) < (target, schema, code, text, 0));
        let mut v: Vec<&Entry> = self.frozen[lo..]
            .iter()
            .take_while(|e| {
                e.target == target && e.schema == schema && e.code == code && e.text == text
            })
            .collect();
        v.extend(self.tail.iter().filter(|e| {
            e.target == target && e.schema == schema && e.code == code && e.text == text
        }));
        v.sort_by_key(|e| e.seq);
        v
    }

    /// 某 (目标, 方案) 未折回的操作条数（折回阈值判断用）。O(log 目标数)。
    pub fn pending(&self, target: Target, schema: &str) -> usize {
        self.counts
            .get(&(target, schema.to_string()))
            .copied()
            .unwrap_or(0)
    }

    /// 有未折回操作的 (目标, 方案)。
    pub fn targets(&self) -> std::collections::BTreeSet<(Target, String)> {
        self.counts.keys().cloned().collect()
    }

    /// 最小的未折回 seq；空表 None。日志压实只能裁到它之前。
    pub fn min_seq(&self) -> Option<u64> {
        self.all().map(|e| e.seq).min()
    }

    pub fn exact(&self, target: Target, schema: &str, code: &str) -> Vec<&Entry> {
        let lo = self
            .frozen
            .partition_point(|e| key(e) < (target, schema, code, "", 0));
        let mut v: Vec<&Entry> = self.frozen[lo..]
            .iter()
            .take_while(|e| e.target == target && e.schema == schema && e.code == code)
            .collect();
        v.extend(
            self.tail
                .iter()
                .filter(|e| e.target == target && e.schema == schema && e.code == code),
        );
        v.sort_by_key(|e| e.seq);
        v
    }

    pub fn prefix(&self, target: Target, schema: &str, prefix: &str) -> Vec<&Entry> {
        let lo = self
            .frozen
            .partition_point(|e| key(e) < (target, schema, prefix, "", 0));
        let mut v: Vec<&Entry> = self.frozen[lo..]
            .iter()
            .take_while(|e| e.target == target && e.schema == schema && e.code.starts_with(prefix))
            .collect();
        v.extend(
            self.tail
                .iter()
                .filter(|e| e.target == target && e.schema == schema && e.code.starts_with(prefix)),
        );
        v.sort_by_key(|e| e.seq);
        v
    }

    pub fn by_group(&self, target: Target, schema: &str, group: &str) -> Vec<&Entry> {
        let mut v: Vec<&Entry> = self
            .groups
            .get(&(target, schema.to_string(), group.to_string()))
            .map(|ix| ix.iter().map(|&i| &self.frozen[i]).collect())
            .unwrap_or_default();
        v.extend(self.tail.iter().filter(|e| {
            e.target == target && e.schema == schema && group_of(&e.code, e.boundary) == group
        }));
        v
    }

    /// 摘除某目标下 `seq ≤ upto_seq` 的条目（折回后调用）；别的目标不动。
    pub fn drop_folded(&self, target: Target, upto_seq: u64) -> Delta {
        let all: Vec<Entry> = self
            .all()
            .filter(|e| !(e.target == target && e.seq <= upto_seq))
            .cloned()
            .collect();
        Self::freeze(all)
    }

    /// 同 [`Self::drop_folded`]，再按 schema 过滤：折回是按 (target, schema) 一个一个做的。
    pub fn drop_folded_for_schema(&self, target: Target, schema: &str, upto_seq: u64) -> Delta {
        let all: Vec<Entry> = self
            .all()
            .filter(|e| !(e.target == target && e.schema == schema && e.seq <= upto_seq))
            .cloned()
            .collect();
        Self::freeze(all)
    }

    pub fn len(&self) -> usize {
        self.frozen.len() + self.tail.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 日志记录 → 增量表条目。没有候选语义的记录（BatchMarker 等）返回 None。
pub fn entry_from_record(r: &Record) -> Option<Entry> {
    let op = match (r.kind, &r.payload) {
        (OpKind::Upsert | OpKind::Promote, Payload::Weight(w)) => DeltaOp::Upsert {
            weight: *w,
            kind: 0,
        },
        (OpKind::Delete | OpKind::Expire, _) => DeltaOp::Delete,
        (OpKind::SetWeight, Payload::Weight(w)) => DeltaOp::SetWeight(*w),
        (OpKind::Incr, Payload::Count(c)) => DeltaOp::Incr(*c),
        (OpKind::Touch, Payload::Ts(t)) => DeltaOp::Touch(*t),
        _ => return None,
    };
    Some(Entry {
        seq: r.seq,
        ts_ms: r.ts_ms,
        target: r.target,
        schema: r.schema.clone(),
        code: r.code.clone(),
        text: r.text.clone(),
        op,
        boundary: r.boundary,
    })
}

#[cfg(test)]
#[path = "delta_tests.rs"]
mod tests;
