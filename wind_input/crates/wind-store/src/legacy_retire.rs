//! 旧库链收尾（设计 §7.4）：`format_version` 写到 5（旧库各表迁完）之后删掉本机状态目录里的
//! `migrated-*` 标记（此后判据只有 `meta.toml`），`userdata.redb` 改名 `userdata.redb.legacy`。

use crate::Store;
use std::path::Path;

/// `userdata.redb` 改名 `userdata.redb.legacy`（已有 ⇒ `.legacy.2`、`.3`…，从不覆盖）。失败只
/// WARN（下次启动再试）。调用方保证此时 redb 已不再打开。
pub fn retire_legacy_db(user_dir: &Path) {
    let src = user_dir.join("userdata.redb");
    if !src.is_file() {
        return;
    }
    let mut dst = user_dir.join("userdata.redb.legacy");
    let mut n = 2;
    while dst.exists() {
        dst = user_dir.join(format!("userdata.redb.legacy.{n}"));
        n += 1;
    }
    match std::fs::rename(&src, &dst) {
        Ok(()) => tracing::info!("旧库已改名为 {}", dst.display()),
        Err(e) => tracing::warn!("旧库改名失败，下次启动再试：{e}"),
    }
}

impl Store {
    /// `format_version` 写到 5 之后调：删掉 `migrated-*` 标记。本进程内旧库链仍记为已完成。
    pub fn forget_legacy_markers(&self) {
        self.legacy_done
            .store(true, std::sync::atomic::Ordering::Release);
        for m in crate::migrate_p3::ALL_MIGRATION_MARKERS {
            let _ = std::fs::remove_file(self.state_dir().join(m));
        }
    }

    /// 旧库链（P1–P4a）已迁完：用户目录记过（`legacy_done`）或五个标记都在。
    pub(crate) fn legacy_chain_done(&self) -> bool {
        self.legacy_done.load(std::sync::atomic::Ordering::Acquire)
            || crate::migrate_p3::ALL_MIGRATION_MARKERS
                .iter()
                .all(|m| self.state_dir().join(m).is_file())
    }
}

#[cfg(test)]
#[path = "legacy_retire_tests.rs"]
mod tests;
