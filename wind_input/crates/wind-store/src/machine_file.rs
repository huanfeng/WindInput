//! 机器写的小文件（补全历史、统计；设计 §2.5 第 3 条）：解析失败先从上一版（`.backup/` 里的，没有
//! 再看旧版留在旁边的 `.bak`）自愈，也不行就把坏文件另存为 `.corrupt-<时间>` 后从空重建。坏文件本身
//! 从不被覆盖。

use std::path::Path;
use wind_userdata::issues::{self, IssueKind};
use wind_userdata::text::fold;

/// 读机写文件：不存在 ⇒ `Ok(None)`；解析成功 ⇒ `Ok(Some(v))`；解析失败 ⇒ 坏文件另存留存，
/// 上一版能解析就把它恢复成正文并返回其内容，否则返回 `Ok(None)`（调用方从空开始）。两种情况都
/// 登记 `Rebuilt`。读不了（IO）⇒ `Err`。
pub(crate) fn load_machine_file<T>(
    path: &Path,
    parse: &dyn Fn(&[u8]) -> anyhow::Result<T>,
) -> anyhow::Result<Option<T>> {
    let Some((bytes, _)) = fold::read_text(path)? else {
        return Ok(None);
    };
    let err = match parse(&bytes) {
        Ok(v) => return Ok(Some(v)),
        Err(e) => e,
    };
    tracing::warn!("{} 无法解析（{err:#}），尝试从备份恢复", path.display());
    let mut restored = None;
    for bak in [fold::backup_path(path), fold::legacy_backup_path(path)] {
        if let Some((b, _)) = fold::read_text(&bak)?
            && let Ok(v) = parse(&b)
        {
            restored = Some((b, v));
            break;
        }
    }
    let kept = fold::preserve_corrupt(path)?;
    let kept_as = kept.as_deref().map(fold::kept_as).unwrap_or_default();
    issues::report(path, IssueKind::Rebuilt { kept_as });
    match restored {
        Some((b, v)) => {
            fold::commit_text(path, &b, None)?;
            tracing::warn!("{} 已从备份恢复", path.display());
            Ok(Some(v))
        }
        None => {
            tracing::warn!("{} 备份也不可用，已从空重建", path.display());
            Ok(None)
        }
    }
}

#[cfg(test)]
#[path = "machine_file_tests.rs"]
mod tests;
