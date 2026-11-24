//! P1 迁移（设计 §7.2）：redb 的 temp_words / draft_words / freq / assoc_history 四张表一次性
//! 迁入运行时引擎。按 (目标, 方案) 用 `fold_with` 把旧表行与引擎已有条目**合并**（见
//! [`merge_legacy`]）：重跑幂等，且上一轮迁移失败后照常运行写进引擎的数据不会被旧表盖掉。
//! 全部完成后写 `state/migrated-p1`。旧表只读保留到 P4，不删、不再写。

use crate::Store;
use crate::store::{ASSOC_HIST, DRAFT_WORDS, FREQ, TEMP_WORDS};
use std::collections::BTreeMap;
use std::path::Path;
use wind_userdata::record::Target;
use wind_userdata::snapshot::SnapshotEntry;

pub const MIGRATED_P1: &str = "migrated-p1";

type Rows = BTreeMap<String, Vec<SnapshotEntry>>;

fn entry(
    code: &str,
    text: &str,
    weight: i32,
    count: u32,
    created_at: i64,
    last_used: i64,
    boundary: u64,
) -> SnapshotEntry {
    SnapshotEntry {
        code: code.to_string(),
        text: text.to_string(),
        weight,
        boundary,
        count,
        created_at,
        last_used,
        flags: 0,
        order: 0,
    }
}

/// 旧表行与引擎里已有的条目合并。两边都有的键：以引擎为准（它更新），但 count 取较大、
/// created_at 取较早、last_used 取较晚——上一轮迁移失败时，没迁成的目标在引擎里只有那一轮
/// 新累计的小计数，不能盖掉旧表的历史计数。只在一边的照收。重跑幂等。
fn merge_legacy(legacy: Vec<SnapshotEntry>, existing: Vec<SnapshotEntry>) -> Vec<SnapshotEntry> {
    let mut map: BTreeMap<(String, String), SnapshotEntry> = legacy
        .into_iter()
        .map(|e| ((e.code.clone(), e.text.clone()), e))
        .collect();
    for e in existing {
        let key = (e.code.clone(), e.text.clone());
        let merged = match map.remove(&key) {
            Some(old) => SnapshotEntry {
                count: e.count.max(old.count),
                created_at: if old.created_at > 0 {
                    old.created_at.min(e.created_at)
                } else {
                    e.created_at
                },
                last_used: e.last_used.max(old.last_used),
                ..e
            },
            None => e,
        };
        map.insert(key, merged);
    }
    map.into_values().collect()
}

/// 旧表一行 `(code, text, value)` 的解码器。
type LegacyDecode<'a> = dyn Fn(&str, &str, &[u8]) -> Option<SnapshotEntry> + 'a;

impl Store {
    /// 读旧表 `table` 全部行，按方案分组；`decode(code, text, value) -> Option<SnapshotEntry>`。
    fn legacy_rows(
        &self,
        table: redb::TableDefinition<&str, &[u8]>,
        decode: &LegacyDecode,
    ) -> anyhow::Result<Rows> {
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(table)?;
            let mut out: Rows = BTreeMap::new();
            for item in t.range::<&str>(..)? {
                let (k, v) = item?;
                let Some((schema, code, text)) = crate::user_words::split_key(k.value()) else {
                    continue;
                };
                if code.is_empty() || text.is_empty() {
                    continue;
                }
                if let Some(e) = decode(code, text, v.value()) {
                    out.entry(schema.to_string()).or_default().push(e);
                }
            }
            Ok(out)
        })
    }

    pub(crate) fn migrate_p1_if_needed(&self, state_dir: &Path) -> anyhow::Result<bool> {
        let marker = state_dir.join(MIGRATED_P1);
        if marker.is_file() {
            return Ok(false);
        }
        let jobs: [(Target, Rows); 4] = [
            (
                Target::Temp,
                self.legacy_rows(TEMP_WORDS, &|c, t, v| {
                    crate::user_words::dec_val(v).map(|(w, n, ca, b)| entry(c, t, w, n, ca, ca, b))
                })?,
            ),
            (
                Target::Draft,
                self.legacy_rows(DRAFT_WORDS, &|c, t, v| {
                    crate::draft_words::dec_draft(v).map(|ca| entry(c, t, 0, 0, ca, ca, 0))
                })?,
            ),
            (
                Target::Freq,
                self.legacy_rows(FREQ, &|c, t, v| {
                    crate::freq::dec_freq(v)
                        .map(|r| entry(c, t, 0, r.count, r.last_used, r.last_used, 0))
                })?,
            ),
            (
                Target::Assoc,
                self.legacy_rows(ASSOC_HIST, &|c, t, v| {
                    crate::freq::dec_freq(v)
                        .map(|r| entry(c, t, 0, r.count, r.last_used, r.last_used, 0))
                })?,
            ),
        ];
        let ud = self.runtime().data().clone();
        let mut total = 0usize;
        for (target, by_schema) in jobs {
            for (schema, rows) in by_schema {
                total += rows.len();
                let rows = std::cell::RefCell::new(Some(rows));
                ud.fold_with(target, &schema, &mut |existing| {
                    merge_legacy(rows.borrow_mut().take().unwrap_or_default(), existing)
                })?;
            }
        }
        wind_userdata::state::atomic_write(&marker, b"1\n")?;
        tracing::info!("P1 迁移完成：{total} 条运行时数据从 redb 迁入引擎");
        Ok(true)
    }
}

impl Store {
    /// 旧表里某方案的行数 `(临时词, 词频, 草稿)`，供 `examples/p1_migrate_check.rs` 与迁移结果比对。
    #[doc(hidden)]
    pub fn legacy_counts(&self, schema: &str) -> anyhow::Result<(usize, usize, usize)> {
        let n = |table, dec: &LegacyDecode| {
            self.legacy_rows(table, dec)
                .map(|r| r.get(schema).map_or(0, |v| v.len()))
        };
        Ok((
            n(TEMP_WORDS, &|c, t, v| {
                crate::user_words::dec_val(v).map(|(w, k, ca, b)| entry(c, t, w, k, ca, ca, b))
            })?,
            n(FREQ, &|c, t, v| {
                crate::freq::dec_freq(v)
                    .map(|r| entry(c, t, 0, r.count, r.last_used, r.last_used, 0))
            })?,
            n(DRAFT_WORDS, &|c, t, v| {
                crate::draft_words::dec_draft(v).map(|ca| entry(c, t, 0, 0, ca, ca, 0))
            })?,
        ))
    }
}

/// 往旧表直接写行（模拟老版本写的库）。测试与跨 crate 测试用，生产代码不调。
#[doc(hidden)]
impl Store {
    #[allow(clippy::too_many_arguments)] // 参数与旧表字段一一对应
    pub fn legacy_put_temp(
        &self,
        schema: &str,
        code: &str,
        text: &str,
        w: i32,
        count: u32,
        ca: i64,
        b: u64,
    ) -> anyhow::Result<()> {
        self.legacy_put(
            TEMP_WORDS,
            schema,
            code,
            text,
            &crate::user_words::enc_val(w, count, ca, b),
        )
    }
    pub fn legacy_put_draft(
        &self,
        schema: &str,
        code: &str,
        text: &str,
        ca: i64,
    ) -> anyhow::Result<()> {
        self.legacy_put(DRAFT_WORDS, schema, code, text, &ca.to_le_bytes())
    }
    pub fn legacy_put_freq(
        &self,
        schema: &str,
        code: &str,
        text: &str,
        count: u32,
        last_used: i64,
    ) -> anyhow::Result<()> {
        self.legacy_put(
            FREQ,
            schema,
            code,
            text,
            &crate::freq::enc_freq(count, last_used),
        )
    }
    pub fn legacy_put_assoc(
        &self,
        schema: &str,
        ctx: &str,
        word: &str,
        count: u32,
        last_used: i64,
    ) -> anyhow::Result<()> {
        self.legacy_put(
            ASSOC_HIST,
            schema,
            ctx,
            word,
            &crate::freq::enc_freq(count, last_used),
        )
    }
    fn legacy_put(
        &self,
        table: redb::TableDefinition<&str, &[u8]>,
        schema: &str,
        code: &str,
        text: &str,
        val: &[u8],
    ) -> anyhow::Result<()> {
        let key = crate::user_words::enc_key(schema, code, text);
        self.with_db(|db| {
            let txn = db.begin_write()?;
            txn.open_table(table)?.insert(key.as_str(), val)?;
            txn.commit()?;
            Ok(())
        })
    }
}

#[cfg(test)]
#[path = "migrate_p1_tests.rs"]
mod tests;
