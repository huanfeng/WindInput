//! 同步状态 `<state_dir>/state.json`：原子写。损坏时留档并重建，不阻塞同步。

use std::path::{Path, PathBuf};

use wind_sync::SyncState;

use crate::log::log;

pub const FILE_NAME: &str = "state.json";

pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE_NAME)
}

/// 缺失 → 新建（随机设备号）；损坏 → 改名留档后新建。
pub fn load(state_dir: &Path) -> SyncState {
    let p = path(state_dir);
    match std::fs::read(&p) {
        Ok(b) => match serde_json::from_slice::<SyncState>(&b) {
            Ok(s) if wind_sync::manifest::valid_device_id(&s.device) => return s,
            _ => {
                log!("同步状态文件损坏，已改名留档并重建");
                let _ = std::fs::rename(&p, state_dir.join("state.json.bad"));
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log!("读取同步状态失败: {e}"),
    }
    SyncState::new(wind_sync::ids::random_id())
}

pub fn save(state_dir: &Path, s: &SyncState) -> Result<(), String> {
    std::fs::create_dir_all(state_dir).map_err(|e| format!("创建同步状态目录失败: {e}"))?;
    let b = serde_json::to_vec(s).map_err(|e| format!("序列化同步状态失败: {e}"))?;
    let p = path(state_dir);
    let tmp = crate::fsutil::tmp_sibling(&p, ".tmp");
    let r = std::fs::File::create(&tmp)
        .and_then(|mut f| {
            use std::io::Write;
            f.write_all(&b)?;
            f.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, &p));
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r.map_err(|e| format!("写同步状态失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_corrupt_recovery() {
        let d = tempfile::tempdir().unwrap();
        let s = load(d.path());
        assert!(!s.device.is_empty());
        save(d.path(), &s).unwrap();
        assert_eq!(load(d.path()), s, "设备号持久");
        std::fs::write(path(d.path()), "{oops").unwrap();
        let fresh = load(d.path());
        assert_ne!(fresh.device, s.device);
        assert!(d.path().join("state.json.bad").exists());
    }
}
