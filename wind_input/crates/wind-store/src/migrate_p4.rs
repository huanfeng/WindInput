//! P4a 迁移（设计 §7.2）：redb 剩下的运行期数据迁成用户数据目录 `data/` 下的文件——快捷格式 /
//! 常用字 / 字符集用户层 / 补全历史 / 每日统计 / 统计元数据 / `phrase_sys_hash`（P5 起全在 `data/`）。每一类目标文件已经
//! 存在且非空 ⇒ 这一类跳过（文件是真源，别拿旧表盖掉它）。全部做完写 `migrated-p4`；没有标记就
//! 整体重做（跳过规则让重做幂等）。旧表只读保留到 P4b。

use crate::Store;
use crate::common_chars::CommonCharRecord;
use crate::quick_format::QuickFormatRecord;
use crate::stats::{DailyStats, StatsMeta};
use crate::store::{CHARSET_USER, COMMON_CHARS, META, QUICK_FORMAT, STATS_DAILY};
use std::path::Path;

pub const MIGRATED_P4: &str = "migrated-p4";

fn nonempty_file(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() > 0)
}

impl Store {
    pub(crate) fn migrate_p4_if_needed(&self, state_dir: &Path) -> anyhow::Result<bool> {
        let marker = state_dir.join(MIGRATED_P4);
        if marker.is_file() {
            return Ok(false);
        }
        use redb::ReadableTable;
        // 快捷格式
        if !nonempty_file(&self.quick_format_path()) {
            let rows: Vec<(String, QuickFormatRecord)> = self.with_db(|db| {
                let txn = db.begin_read()?;
                let t = txn.open_table(QUICK_FORMAT)?;
                let mut out = Vec::new();
                for item in t.iter()? {
                    let (k, v) = item?;
                    if let Ok(r) = serde_json::from_slice::<QuickFormatRecord>(v.value()) {
                        out.push((k.value().to_string(), r));
                    }
                }
                Ok(out)
            })?;
            if !rows.is_empty() {
                self.import_quick_format_rows(rows, true)?;
            }
        }
        // 常用字
        if !nonempty_file(&self.common_chars_path()) {
            let items: Vec<(String, Option<bool>)> = self.with_db(|db| {
                let txn = db.begin_read()?;
                let t = txn.open_table(COMMON_CHARS)?;
                let mut out = Vec::new();
                for item in t.iter()? {
                    let (k, v) = item?;
                    if let Ok(r) = serde_json::from_slice::<CommonCharRecord>(v.value())
                        && !k.value().is_empty()
                    {
                        out.push((k.value().to_string(), Some(r.common)));
                    }
                }
                Ok(out)
            })?;
            if !items.is_empty() {
                self.apply_common_char_overrides(&items)?;
            }
        }
        // 字符集用户层（逐个 key：文件已在的跳过）
        let docs: Vec<(String, String)> = self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(CHARSET_USER)?;
            let mut out = Vec::new();
            for item in t.iter()? {
                let (k, v) = item?;
                if !k.value().is_empty() {
                    out.push((
                        k.value().to_string(),
                        String::from_utf8_lossy(v.value()).into_owned(),
                    ));
                }
            }
            Ok(out)
        })?;
        for (key, text) in docs {
            match self.charset_path(&key) {
                Ok(p) if !nonempty_file(&p) => self.set_charset_doc(&key, &text)?,
                Ok(_) => {}
                Err(_) => tracing::warn!("P4 迁移跳过文件名不合法的字符类"),
            }
        }
        // 补全历史
        if !nonempty_file(&self.completion_path()) {
            let rows = self.legacy_completion_rows()?;
            if !rows.is_empty() {
                let jsonl: String = rows
                    .into_iter()
                    .map(|(k, t, r)| {
                        serde_json::json!({
                            "kind": k.as_str(), "text": t,
                            "count": r.count, "last_used": r.last_used,
                        })
                        .to_string()
                            + "\n"
                    })
                    .collect();
                self.import_completions_jsonl(&jsonl)?;
            }
        }
        // 每日统计与元数据：按日合并、已有的日以文件为准（不是整类跳过）——按月逐文件写，中途失败
        // 后重试要能补齐余下的月份
        let (days, meta) = self.legacy_stats()?;
        if !days.is_empty() {
            let jsonl: String = days
                .into_iter()
                .map(|(d, st)| serde_json::json!({ "date": d, "stat": st }).to_string() + "\n")
                .collect();
            self.import_stats_jsonl(&jsonl, false)?;
        }
        if let Some(m) = meta
            && !nonempty_file(&self.stats_meta_path())
        {
            self.put_stats_meta(&m)?;
        }
        // 系统短语哈希
        if self.phrase_sys_hash()?.is_none()
            && let Some(h) = self.legacy_phrase_sys_hash()?
        {
            self.set_phrase_sys_hash(&h)?;
        }
        wind_userdata::state::atomic_write(&marker, b"1\n")?;
        tracing::info!("P4a 迁移完成：覆盖表与小文件已从 redb 迁出");
        Ok(true)
    }

    /// P4a 迁移是否已完成（标记 `migrated-p4` 在）。
    pub fn migrated_p4(&self) -> bool {
        self.state_dir().join(MIGRATED_P4).is_file()
    }

    // ───── 测试 / 探针：往旧表写（模拟升级前的库） ─────

    #[doc(hidden)]
    pub fn legacy_put_quick_format(
        &self,
        kind: &str,
        rec: &QuickFormatRecord,
    ) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(rec)?;
        self.legacy_put_raw(QUICK_FORMAT, kind, &bytes)
    }

    #[doc(hidden)]
    pub fn legacy_put_common_char(&self, ch: &str, common: bool) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(&CommonCharRecord { common })?;
        self.legacy_put_raw(COMMON_CHARS, ch, &bytes)
    }

    #[doc(hidden)]
    pub fn legacy_put_charset(&self, key: &str, text: &str) -> anyhow::Result<()> {
        self.legacy_put_raw(CHARSET_USER, key, text.as_bytes())
    }

    #[doc(hidden)]
    pub fn legacy_put_daily_stat(&self, date: &str, stat: &DailyStats) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(stat)?;
        self.legacy_put_raw(STATS_DAILY, date, &bytes)
    }

    #[doc(hidden)]
    pub fn legacy_put_stats_meta(&self, meta: &StatsMeta) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(meta)?;
        self.legacy_put_raw(META, "stats_meta", &bytes)
    }

    #[doc(hidden)]
    pub fn legacy_put_phrase_sys_hash(&self, h: &str) -> anyhow::Result<()> {
        self.meta_set("phrase_sys_hash", h)
    }

    fn legacy_put_raw(
        &self,
        table: redb::TableDefinition<&str, &[u8]>,
        key: &str,
        val: &[u8],
    ) -> anyhow::Result<()> {
        self.with_db(|db| {
            let txn = db.begin_write()?;
            txn.open_table(table)?.insert(key, val)?;
            txn.commit()?;
            Ok(())
        })
    }
}

#[cfg(test)]
#[path = "migrate_p4_tests.rs"]
mod tests;
