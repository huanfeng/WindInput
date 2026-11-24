//! 折回水位 `state`（设计 §3.6）与真源级原子写（§3.5 次序）。
//!
//! `state` 只是加速：读不出 / 版本不符就当「水位未知」，由快照内嵌的 `max_seq` 兜底，
//! **绝不整份回放**——那会把用户手工删掉的行加回来、把事后补回的词删掉。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Default, Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct TargetState {
    pub folded_seq: u64,
    /// 文本目标：折回后文本的指纹（P2 用）；运行时目标 None。
    pub text_fp: Option<String>,
}

/// state 文件格式版本；读到别的版本当「水位未知」（§3.6），由快照内嵌水位兜底。
pub const STATE_VERSION: u32 = 1;

#[derive(Default, Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct State {
    #[serde(default)]
    pub version: u32,
    /// 还原 / 批量替换后递增；不同代的日志记录一律忽略（§3.8）。
    pub generation: u32,
    /// 序号续号来源：与日志尾取较大者 + 1；用户删掉 journal/ 后序号也不会倒退。
    pub next_seq: u64,
    /// 键 = `"{target}:{schema}"`。
    pub targets: BTreeMap<String, TargetState>,
}

/// 真源级原子写：tmp → fsync tmp → rename → fsync 目录。Windows 没有目录 fsync，rename 后
/// 对目标再 `sync_all` 一次（NTFS 元数据日志保证目录项）。
///
/// 现有缓存写盘口只做 tmp + rename 不 fsync，对可重建的缓存够用；这里写的是真源与水位，
/// 断电后留下零长文件就是整份用户词库清零，不能省。
pub fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("无父目录: {}", path.display()))?;
    std::fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp.{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        std::io::Write::write_all(&mut f, bytes)?;
        f.sync_all()?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    #[cfg(unix)]
    {
        std::fs::File::open(dir)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = fsync_file(path);
    }
    Ok(())
}

/// fsync 一个文件。要用可写句柄：Windows 的 `FlushFileBuffers` 对只读句柄报拒绝访问
/// （Linux 的 fsync 不挑句柄）。
pub fn fsync_file(path: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .sync_all()
}

impl State {
    /// 读不出 / 解析不出 ⇒ `None`（水位未知），调用方以快照内嵌水位为准。
    pub fn load(path: &Path) -> Option<State> {
        let s: State = toml::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        (s.version == STATE_VERSION).then_some(s)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let mut s = self.clone();
        s.version = STATE_VERSION;
        atomic_write(path, toml::to_string(&s)?.as_bytes())
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
