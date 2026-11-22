//! 临时词（对照 `wind-store/src/temp_words.rs`）。

use super::{Runtime, now_secs};
use crate::engine::Op;
use crate::record::{OpKind, Payload, Target};
use crate::snapshot::SnapshotEntry;

pub const TEMP_WORD_MAX_WEIGHT: i32 = 10000;

#[derive(Clone, Debug, PartialEq)]
pub struct TempWord {
    pub code: String,
    pub text: String,
    pub weight: i32,
    pub count: u32,
    /// unix 秒
    pub created_at: i64,
    pub boundary: u64,
}

impl From<SnapshotEntry> for TempWord {
    fn from(e: SnapshotEntry) -> TempWord {
        TempWord {
            code: e.code,
            text: e.text,
            weight: e.weight,
            count: e.count,
            created_at: e.created_at,
            boundary: e.boundary,
        }
    }
}

const T: Target = Target::Temp;

impl Runtime {
    /// 学习一次的操作序列：新词 Upsert + Incr(1)；已有词 Incr(1)，旧边界为 0 时先 Upsert 补边界。
    pub(crate) fn temp_learn_ops(
        &self,
        schema: &str,
        code: &str,
        text: &str,
        add_weight: i32,
        boundary: u64,
    ) -> (Vec<Op>, u32, bool) {
        match self.ud.get(T, schema, code, text) {
            Some(e) => {
                let b = if e.boundary != 0 {
                    e.boundary
                } else {
                    boundary
                };
                let mut ops = Vec::new();
                if e.boundary == 0 && boundary != 0 {
                    ops.push(Op::new(
                        T,
                        schema,
                        OpKind::Upsert,
                        code,
                        text,
                        Payload::Weight(e.weight),
                        b,
                    ));
                }
                ops.push(Op::new(
                    T,
                    schema,
                    OpKind::Incr,
                    code,
                    text,
                    Payload::Count(1),
                    b,
                ));
                (ops, e.count.saturating_add(1), false)
            }
            None => (
                vec![
                    Op::new(
                        T,
                        schema,
                        OpKind::Upsert,
                        code,
                        text,
                        Payload::Weight(add_weight.min(TEMP_WORD_MAX_WEIGHT)),
                        boundary,
                    ),
                    Op::new(
                        T,
                        schema,
                        OpKind::Incr,
                        code,
                        text,
                        Payload::Count(1),
                        boundary,
                    ),
                ],
                1,
                true,
            ),
        }
    }

    /// 新词 weight = min(add_weight, MAX)、count 1；已存在只 count+1、权重不变。返回新 count。
    pub fn learn_temp(
        &self,
        schema: &str,
        code: &str,
        text: &str,
        add_weight: i32,
        boundary: u64,
    ) -> anyhow::Result<u32> {
        Ok(self
            .learn_temp_detailed(schema, code, text, add_weight, boundary)?
            .0)
    }

    /// 同 [`Self::learn_temp`]，另返回是否新词（门面据此推进结构代次或计数代次）。
    pub fn learn_temp_detailed(
        &self,
        schema: &str,
        code: &str,
        text: &str,
        add_weight: i32,
        boundary: u64,
    ) -> anyhow::Result<(u32, bool)> {
        let (ops, count, is_new) = self.temp_learn_ops(schema, code, text, add_weight, boundary);
        self.ud.apply_batch(&ops)?;
        if is_new {
            self.bump_words(schema);
        } else {
            self.bump_count();
        }
        Ok((count, is_new))
    }

    pub fn temp_word(&self, schema: &str, code: &str, text: &str) -> Option<TempWord> {
        self.ud.get(T, schema, code, text).map(TempWord::from)
    }

    pub fn increment_temp_if_exists(
        &self,
        schema: &str,
        code: &str,
        text: &str,
    ) -> anyhow::Result<(bool, u32)> {
        let Some(e) = self.ud.get(T, schema, code, text) else {
            return Ok((false, 0));
        };
        self.ud.apply(
            T,
            schema,
            OpKind::Incr,
            code,
            text,
            Payload::Count(1),
            e.boundary,
        )?;
        self.bump_count();
        Ok((true, e.count.saturating_add(1)))
    }

    pub fn temp_words(&self, schema: &str, code: &str) -> Vec<TempWord> {
        self.ud
            .exact(T, schema, code)
            .into_iter()
            .map(TempWord::from)
            .collect()
    }

    pub fn for_each_temp(&self, schema: &str, prefix: &str, f: &mut dyn FnMut(&TempWord) -> bool) {
        self.ud
            .scan(T, schema, prefix, &mut |e| f(&TempWord::from(e)));
    }

    pub fn count_temp(&self, schema: &str) -> usize {
        let mut n = 0;
        self.ud.scan(T, schema, "", &mut |_| {
            n += 1;
            true
        });
        n
    }

    pub fn remove_temp(&self, schema: &str, code: &str, text: &str) -> anyhow::Result<()> {
        if self.ud.get(T, schema, code, text).is_some() {
            self.ud
                .apply(T, schema, OpKind::Delete, code, text, Payload::None, 0)?;
            self.bump_words(schema);
        }
        Ok(())
    }

    /// 一批 [`Self::remove_temp`]：不存在的跳过，其余一次 `apply_batch`。
    pub fn remove_temps(&self, schema: &str, keys: &[(String, String)]) -> anyhow::Result<()> {
        let ops = super::delete_ops(&self.ud, T, schema, keys);
        if !ops.is_empty() {
            self.ud.apply_batch(&ops)?;
            self.bump_words(schema);
        }
        Ok(())
    }

    /// 一批转正：存在的临时词以 `weight` 合并进用户词（同 `add_user_word`：权重取 max、0 边界
    /// 补上），再删掉临时词——用户词的写在前、临时词的删在后，与逐条转正同序，**同一次**
    /// `apply_batch`。不存在的跳过。返回转正了的 `(code, text)`。
    pub fn promote_temps(
        &self,
        schema: &str,
        keys: &[(String, String)],
        weight: i32,
    ) -> anyhow::Result<Vec<(String, String)>> {
        let mut seen = std::collections::BTreeSet::new();
        let mut rows = Vec::new();
        for (c, t) in keys {
            if !seen.insert((c.as_str(), t.as_str())) {
                continue;
            }
            if let Some(tw) = self.temp_word(schema, c, t) {
                super::user::check_len(c, t)?;
                rows.push(super::UserWord {
                    code: c.clone(),
                    text: t.clone(),
                    weight,
                    boundary: tw.boundary,
                    order: 0,
                });
            }
        }
        let promoted: Vec<(String, String)> = rows
            .iter()
            .map(|r| (r.code.clone(), r.text.clone()))
            .collect();
        let mut ops: Vec<Op> = rows
            .iter()
            .filter_map(|r| self.user_upsert_op(schema, &r.code, &r.text, r.weight, r.boundary))
            .collect();
        ops.extend(super::delete_ops(&self.ud, T, schema, &promoted));
        if !ops.is_empty() {
            self.ud.apply_batch(&ops)?;
        }
        self.bump_words(schema);
        Ok(promoted)
    }

    pub fn clear_temp(&self, schema: &str) -> anyhow::Result<usize> {
        let n = self.ud.clear(T, schema)?;
        self.bump_words(schema);
        Ok(n)
    }

    /// 超出 `max_keep` 则按 (count, created_at, weight) 升序删前若干条（逐条 Delete：按键节奏下
    /// 一次只超出几条）。
    pub fn evict_temp(&self, schema: &str, max_keep: usize) -> anyhow::Result<usize> {
        // 上界门控：协调器每 64 次造词调一次，绝大多数时候没超限，不该为此扫全表
        if self.ud.upper_bound(T, schema) <= max_keep {
            return Ok(0);
        }
        let mut all: Vec<SnapshotEntry> = Vec::new();
        self.ud.scan(T, schema, "", &mut |e| {
            all.push(e);
            true
        });
        if all.len() <= max_keep {
            return Ok(0);
        }
        all.sort_by_key(|e| (e.count, e.created_at, e.weight));
        let ops: Vec<Op> = all
            .iter()
            .take(all.len() - max_keep)
            .map(|e| {
                Op::new(
                    T,
                    schema,
                    OpKind::Delete,
                    &e.code,
                    &e.text,
                    Payload::None,
                    0,
                )
            })
            .collect();
        self.ud.apply_batch(&ops)?;
        self.bump_words(schema);
        Ok(ops.len())
    }

    /// 导入（对照 `import_temp_word_rows`）：已存在取 max、补边界；不存在新建（created_at = 现在）。
    pub fn import_temp_rows(&self, schema: &str, rows: &[TempWord]) -> anyhow::Result<usize> {
        let now = now_secs();
        self.ud.fold_with(T, schema, &mut |v| {
            let mut map: std::collections::BTreeMap<(String, String), SnapshotEntry> = v
                .into_iter()
                .map(|e| ((e.code.clone(), e.text.clone()), e))
                .collect();
            for r in rows {
                let cap = r.weight.min(TEMP_WORD_MAX_WEIGHT);
                match map.get_mut(&(r.code.clone(), r.text.clone())) {
                    Some(e) => {
                        e.weight = e.weight.max(cap);
                        e.count = e.count.max(r.count);
                        if e.boundary == 0 {
                            e.boundary = r.boundary;
                        }
                    }
                    None => {
                        map.insert(
                            (r.code.clone(), r.text.clone()),
                            SnapshotEntry {
                                code: r.code.clone(),
                                text: r.text.clone(),
                                weight: cap,
                                boundary: r.boundary,
                                count: r.count,
                                created_at: now,
                                last_used: now,
                                flags: 0,
                                order: 0,
                            },
                        );
                    }
                }
            }
            map.into_values().collect()
        })?;
        self.bump_words(schema);
        Ok(rows.len())
    }
}
