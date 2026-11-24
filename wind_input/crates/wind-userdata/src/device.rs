//! 设备 id、机器指纹、单写者锁（设计 §3.7）。
//!
//! `device_id` 不是机器 id 也不是目录 id：本机 `state/device.toml` 记 `(device_id, machine_fp,
//! 上次配对的 dir_id)`。机器指纹不符 = 用户目录被复制到另一台机器；dir_id 不符 = 本机换了
//! 用户目录。两种情况都换新 id，旧日志改名 `.wjl.sealed` 封存（仍可作同步源），不与新日志
//! 混号。同机多账户各有本机目录，自然不同 id。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
struct DeviceFile {
    device_id: String,
    machine_fp: String,
    dir_id: String,
}

#[derive(Clone, Debug)]
pub struct Device {
    pub device_id: [u8; 16],
    pub machine_fp: String,
    pub dir_id: String,
}

pub enum DeviceOutcome {
    Same(Device),
    Renewed {
        new: Device,
        sealed_journal: Option<PathBuf>,
    },
}

pub fn hex(id: &[u8; 16]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<[u8; 16]> {
    if s.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// 机器指纹：主机名 + 用户名 + OS。只用来识别「目录被复制到别处」，取不到的项为空串。
pub fn machine_fingerprint() -> String {
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_default();
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default();
    format!("{host}|{user}|{}", std::env::consts::OS)
}

pub fn load_or_renew(
    state_dir: &Path,
    dir_id: &str,
    journal_dir: &Path,
) -> anyhow::Result<DeviceOutcome> {
    let path = state_dir.join("device.toml");
    let fp = machine_fingerprint();
    let old: Option<DeviceFile> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| toml::from_str(&s).ok());
    if let Some(o) = &old
        && o.machine_fp == fp
        && o.dir_id == dir_id
        && let Some(id) = unhex(&o.device_id)
    {
        return Ok(DeviceOutcome::Same(Device {
            device_id: id,
            machine_fp: fp,
            dir_id: dir_id.into(),
        }));
    }
    let sealed = old.as_ref().and_then(|o| {
        let j = journal_dir.join(format!("{}.wjl", o.device_id));
        if j.is_file() {
            let s = j.with_extension("wjl.sealed");
            std::fs::rename(&j, &s).ok().map(|_| s)
        } else {
            None
        }
    });
    let id = *uuid::Uuid::new_v4().as_bytes();
    let new = Device {
        device_id: id,
        machine_fp: fp.clone(),
        dir_id: dir_id.into(),
    };
    let text = toml::to_string(&DeviceFile {
        device_id: hex(&id),
        machine_fp: fp,
        dir_id: dir_id.into(),
    })?;
    crate::state::atomic_write(&path, text.as_bytes())?;
    Ok(DeviceOutcome::Renewed {
        new,
        sealed_journal: sealed,
    })
}

/// 只读实例用：读出现有 device_id，**不校验、不换号、不写任何文件**（换号会封存写者正在
/// 追加的日志）。没有或读不出 ⇒ `None`。
pub fn load_existing(state_dir: &Path) -> Option<[u8; 16]> {
    let text = std::fs::read_to_string(state_dir.join("device.toml")).ok()?;
    let f: DeviceFile = toml::from_str(&text).ok()?;
    unhex(&f.device_id)
}

/// `state/lock` 的独占锁；放手即释放。
pub struct Lock(#[allow(dead_code)] std::fs::File);

/// 拿不到 ⇒ `None`，调用方进入只读模式（查询正常、写入丢弃并记 WARN）。
pub fn try_lock(state_dir: &Path) -> anyhow::Result<Option<Lock>> {
    std::fs::create_dir_all(state_dir)?;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(state_dir.join("lock"))?;
    let r = f.try_lock();
    Ok(lock_outcome(r, f))
}

/// 拿到 ⇒ 锁；被别的进程占着 ⇒ `None`（只读）；锁 API 本身报错（网络家目录上 flock 不可用
/// 之类）⇒ 同样 `None` 并 WARN：退化为只读好过整个用户数据不可用。
fn lock_outcome(r: Result<(), std::fs::TryLockError>, f: std::fs::File) -> Option<Lock> {
    match r {
        Ok(()) => Some(Lock(f)),
        Err(std::fs::TryLockError::WouldBlock) => None,
        Err(std::fs::TryLockError::Error(e)) => {
            tracing::warn!("用户数据锁不可用（{e}），以只读模式运行：本次不保存学习与修改");
            None
        }
    }
}

#[cfg(test)]
#[path = "device_tests.rs"]
mod tests;
