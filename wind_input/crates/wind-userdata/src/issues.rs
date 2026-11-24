//! 数据文件问题登记（设计 §2.5）：坏行数、隔离、已重建 / 未迁移。进程级，以绝对路径为键；设置页经
//! `system.storageStatus` 读。只记路径与计数，不记文件内容。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IssueKind {
    /// 这么多行解析不了，已跳过（原样留在文件里）。
    BadLines(usize),
    /// 整份解析不了：不往里写，按最后一次成功的数据服务。
    Isolated,
    /// 坏文件已另存为 `kept_as` 并重建（机写文件从 `.bak` 恢复或从空开始；显式整体替换）。
    Rebuilt { kept_as: String },
    /// 内容不合规、整份或部分被忽略（字符分类文件）；`message` 是给用户看的一句原因。
    Invalid { message: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileIssue {
    pub path: PathBuf,
    pub kind: IssueKind,
}

static ISSUES: Mutex<BTreeMap<PathBuf, IssueKind>> = Mutex::new(BTreeMap::new());

fn lock() -> std::sync::MutexGuard<'static, BTreeMap<PathBuf, IssueKind>> {
    ISSUES.lock().unwrap_or_else(|e| e.into_inner())
}

/// 登记（同一路径覆盖旧值）。
pub fn report(path: &Path, kind: IssueKind) {
    lock().insert(path.to_path_buf(), kind);
}

pub fn clear(path: &Path) {
    lock().remove(path);
}

/// 只清掉该文件的「内容不合规」问题（校验通过时用；别的问题由各自的读写路径管）。
pub fn clear_invalid(path: &Path) {
    let mut g = lock();
    if matches!(g.get(path), Some(IssueKind::Invalid { .. })) {
        g.remove(path);
    }
}

/// `root` 之下（按路径分量判断）的全部问题，按路径排序。
pub fn list_under(root: &Path) -> Vec<FileIssue> {
    lock()
        .iter()
        .filter(|(p, _)| p.starts_with(root))
        .map(|(p, k)| FileIssue {
            path: p.clone(),
            kind: k.clone(),
        })
        .collect()
}

/// 一次成功解析的统一上报：有坏行记 WARN（只记行数）并登记；没有就清掉该文件的旧问题。
pub fn report_parse(path: &Path, bad_lines: usize) {
    if bad_lines > 0 {
        tracing::warn!(
            "{} 有 {bad_lines} 行无法解析，已跳过（原样保留在文件里）",
            path.display()
        );
        report(path, IssueKind::BadLines(bad_lines));
    } else {
        clear(path);
    }
}

#[cfg(test)]
#[path = "issues_tests.rs"]
mod tests;
