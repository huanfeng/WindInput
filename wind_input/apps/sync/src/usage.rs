//! 远端空间占用（设计 §12）：统计、缓存 `<state_dir>/usage.json`、每天一次的节奏。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use wind_sync::{Backend, Dek};
use wind_sync_ipc::{DeviceUsage, ProviderUsage, Usage};

use crate::errors::{backend_error, sync_error};
use crate::log::{log, now_unix};

pub const FILE_NAME: &str = "usage.json";
/// 自动统计的间隔：距上次成功满这么久才随同步轮再算。
pub const INTERVAL_SECS: u64 = 86400;
/// 失败后到下次自动重试的等待。
pub const RETRY_SECS: u64 = 3600;

/// 落盘的缓存：只对统计时的同步库有效。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cached {
    pub library: String,
    pub usage: Usage,
}

fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE_NAME)
}

/// 缺失或损坏都当没统计过。
pub fn load(state_dir: &Path) -> Option<Cached> {
    let b = std::fs::read(path(state_dir)).ok()?;
    serde_json::from_slice(&b).ok()
}

pub fn save(state_dir: &Path, c: &Cached) -> Result<(), String> {
    let b = serde_json::to_vec(c).map_err(|e| format!("序列化空间占用失败: {e}"))?;
    wind_sync_ipc::write_file_atomic(&path(state_dir), &b)
}

/// 本轮是否该统计：没统计过、换了库，或距上次成功满 [`INTERVAL_SECS`]；失败后等到 `retry_at`。
pub fn due(cached: Option<&Cached>, library: &str, now: u64, retry_at: u64) -> bool {
    if now < retry_at {
        return false;
    }
    match cached {
        Some(c) if c.library == library => {
            let at = c.usage.computed_at.max(0) as u64;
            // 统计时间在未来（时钟回拨过）：当作到期，免得一直不再统计
            at > now || now >= at.saturating_add(INTERVAL_SECS)
        }
        _ => true,
    }
}

/// 统计一次，`cancelled` 为真时中途放弃。服务端配额取不到只记日志，不算失败。
pub fn measure(b: &dyn Backend, dek: &Dek, cancelled: &dyn Fn() -> bool) -> Result<Usage, String> {
    let r = wind_sync::usage::compute_with(b, dek, cancelled).map_err(|e| sync_error(&e))?;
    let provider = match b.usage() {
        Ok(p) => p.map(|p| ProviderUsage {
            used: p.used,
            available: p.available,
            quota: p.quota,
        }),
        Err(e) => {
            log!("读取存储配额失败: {}", backend_error(&e));
            None
        }
    };
    Ok(Usage {
        computed_at: now_unix() as i64,
        total: r.total,
        blobs: r.blobs,
        blobs_unreferenced: r.blobs_unreferenced,
        devices: r
            .devices
            .into_iter()
            .map(|d| DeviceUsage {
                id: d.id,
                learned: d.learned,
                stats: d.stats,
            })
            .collect(),
        other: r.other,
        provider,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computed_at_in_the_future_is_due() {
        let c = Cached {
            library: "lib".into(),
            usage: Usage {
                computed_at: 5_000,
                ..Default::default()
            },
        };
        assert!(!due(Some(&c), "lib", 5_001, 0));
        assert!(due(Some(&c), "lib", 4_000, 0), "时钟回拨后要能再统计");
    }

    #[test]
    fn cache_roundtrip_and_corrupt_file() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(load(d.path()), None);
        let c = Cached {
            library: "lib".into(),
            usage: Usage {
                computed_at: 7,
                total: 9,
                ..Default::default()
            },
        };
        save(d.path(), &c).unwrap();
        assert_eq!(load(d.path()), Some(c));
        std::fs::write(path(d.path()), "{oops").unwrap();
        assert_eq!(load(d.path()), None);
    }
}
