//! 监听用户目录：只把同步范围内的路径事件送进主循环（路径相对用户目录，`/` 分隔）。
//! 范围开关随设置变化，经原子位图共享给监听回调。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::Sender;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use wind_sync::scope::{ScopeFlags, in_scope};

use crate::local::rel_of;
use crate::log::log;
use crate::runner::Input;

#[derive(Clone, Default)]
pub struct SharedScope(Arc<AtomicU8>);

impl SharedScope {
    pub fn new(f: ScopeFlags) -> Self {
        let s = SharedScope::default();
        s.set(f);
        s
    }
    pub fn set(&self, f: ScopeFlags) {
        let bits = u8::from(f.config)
            | u8::from(f.schemas) << 1
            | u8::from(f.themes) << 2
            | u8::from(f.dicts) << 3;
        self.0.store(bits, Ordering::Relaxed);
    }
    pub fn get(&self) -> ScopeFlags {
        let b = self.0.load(Ordering::Relaxed);
        ScopeFlags {
            config: b & 1 != 0,
            schemas: b & 2 != 0,
            themes: b & 4 != 0,
            dicts: b & 8 != 0,
        }
    }
}

pub struct Watch {
    _w: RecommendedWatcher,
}

pub fn start(root: &Path, scope: SharedScope, tx: Sender<Input>) -> Result<Watch, String> {
    let root_buf: PathBuf = root.to_path_buf();
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        if matches!(ev.kind, EventKind::Access(_)) {
            return;
        }
        let flags = scope.get();
        for p in &ev.paths {
            if let Some(rel) = rel_of(&root_buf, p)
                && in_scope(&rel, &flags)
            {
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
