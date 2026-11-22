//! 运行时领域 API（设计 §6 的 temp / draft / freq / assoc 四张表在引擎上的替身）。
//!
//! 语义逐条照 `wind-store` 的同名函数（`temp_words.rs` / `draft_words.rs` / `freq.rs` /
//! `assoc_history.rs`），时间单位同为 unix 秒。按键路径上的单条写入走 `apply_batch`；清空、
//! 导入、草稿过期 / 淘汰这类批量操作走 `fold_with`（§3.8）。本模块不依赖 `wind-store`。

mod draft;
mod freq;
mod temp;
mod user;

pub use draft::draft_alive;
pub use freq::FreqRec;
pub use temp::{TEMP_WORD_MAX_WEIGHT, TempWord};
pub use user::{RowEffect, USER_CODE_MAX_CHARS, USER_TEXT_MAX_CHARS, UserImportCounts, UserWord};

use crate::engine::UserData;
use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub struct Runtime {
    ud: Arc<UserData>,
    words_gen: Mutex<HashMap<String, u64>>,
    count_gen: AtomicU64,
}

/// 一批「删除这些键」的操作：只收现存的键，同批重复的只删一次（逐条删除的结果与之相同）。
pub(crate) fn delete_ops(
    ud: &UserData,
    target: crate::record::Target,
    schema: &str,
    keys: &[(String, String)],
) -> Vec<crate::engine::Op> {
    let mut seen = BTreeSet::new();
    keys.iter()
        .filter(|(c, t)| seen.insert((c.as_str(), t.as_str())))
        .filter(|(c, t)| ud.get(target, schema, c, t).is_some())
        .map(|(c, t)| {
            crate::engine::Op::new(
                target,
                schema,
                crate::record::OpKind::Delete,
                c,
                t,
                crate::record::Payload::None,
                0,
            )
        })
        .collect()
}

pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Runtime {
    pub fn new(ud: Arc<UserData>) -> Runtime {
        Runtime {
            ud,
            words_gen: Mutex::new(HashMap::new()),
            count_gen: AtomicU64::new(0),
        }
    }

    pub fn data(&self) -> &Arc<UserData> {
        &self.ud
    }

    /// 临时词「结构」代次（新增 / 删除 / 清空 / 导入 / 淘汰到 / 草稿转正）。进程内计数，重启归零。
    pub fn words_gen(&self, schema: &str) -> u64 {
        self.words_gen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema)
            .copied()
            .unwrap_or(0)
    }

    /// 临时词 count+1 的代次（不分方案，同 `Store::bump_words_count_gen`）。
    pub fn count_gen(&self) -> u64 {
        self.count_gen.load(Ordering::Acquire)
    }

    pub(crate) fn bump_words(&self, schema: &str) {
        *self
            .words_gen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(schema.to_string())
            .or_insert(0) += 1;
    }

    pub(crate) fn bump_count(&self) {
        self.count_gen.fetch_add(1, Ordering::AcqRel);
    }

    /// 有任何运行时数据的数据方案（备份按它逐个导出）。
    pub fn schemas(&self) -> BTreeSet<String> {
        self.ud.keys().into_iter().map(|(_, s)| s).collect()
    }
}
