//! 用组的设置覆盖本机前的备份：`data/.backup/sync/<YYYYMMDD-HHMMSS>/`，只留最近几份。

use std::path::{Path, PathBuf};

use wind_sync::scope::{Kind, kind};

/// 保留的备份份数。
const KEEP: usize = 3;

/// unix 秒 → `YYYYMMDD-HHMMSS`（UTC）。
fn stamp(now: u64) -> String {
    let (days, secs) = (now / 86400, now % 86400);
    // 公历换算（Howard Hinnant civil_from_days）
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

fn join_rel(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, s| p.join(s))
}

/// 把 `paths` 中属于设置的文件复制进新的时间戳目录，删掉超出份数的旧目录；返回新目录。
pub fn backup_settings(userdata_dir: &Path, paths: &[String], now: u64) -> Result<PathBuf, String> {
    let base = join_rel(userdata_dir, "data/.backup/sync");
    let dir = base.join(stamp(now));
    for rel in paths.iter().filter(|p| kind(p) == Kind::Setting) {
        let dst = join_rel(&dir, rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建备份目录 {} 失败: {e}", parent.display()))?;
        }
        std::fs::copy(join_rel(userdata_dir, rel), &dst)
            .map_err(|e| format!("备份 {rel} 失败: {e}"))?;
    }
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("创建备份目录 {} 失败: {e}", dir.display()))?;
    let mut old: Vec<PathBuf> = std::fs::read_dir(&base)
        .map_err(|e| format!("读取备份目录失败: {e}"))?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    old.sort();
    let excess = old.len().saturating_sub(KEEP);
    for p in &old[..excess] {
        let _ = std::fs::remove_dir_all(p);
    }
    Ok(dir)
}

#[cfg(test)]
#[path = "backup_tests.rs"]
mod tests;
