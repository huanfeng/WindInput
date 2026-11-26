//! 监听用户目录：只把同步范围内的路径事件送进主循环（路径相对用户目录，`/` 分隔）。

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use wind_sync::scope::in_scope;

use crate::local::{rel_of, skipped};
use crate::log::log;
use crate::runner::Input;

pub struct Watch {
    _w: RecommendedWatcher,
}

/// 事件路径相对用户目录的路径；范围外或落在本机目录、安装目录内的返回 `None`。
pub fn watched_rel(root: &Path, skips: &[String], p: &Path) -> Option<String> {
    rel_of(root, p).filter(|rel| in_scope(rel) && !skipped(skips, rel))
}

pub fn start(root: &Path, skips: Vec<String>, tx: Sender<Input>) -> Result<Watch, String> {
    let root_buf: PathBuf = root.to_path_buf();
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        if matches!(ev.kind, EventKind::Access(_)) {
            return;
        }
        for p in &ev.paths {
            if let Some(rel) = watched_rel(&root_buf, &skips, p) {
                let _ = tx.send(Input::Fs(rel));
            }
        }
    })
    .map_err(|e| format!("创建文件监听失败: {e}"))?;
    w.watch(root, RecursiveMode::Recursive)
        .map_err(|e| format!("监听 {} 失败: {e}", root.display()))?;
    log!("已监听用户目录");
    Ok(Watch { _w: w })
}
