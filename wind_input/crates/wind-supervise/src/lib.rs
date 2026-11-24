// SPDX-License-Identifier: MPL-2.0
//! wind-supervise：子进程监护的共享逻辑。核心监护扩展宿主、宿主监护扩展、数据同步守护
//! 都是同一套事：拉起、收尸、退避重启、熔断、宽限停止、随父进程生死、stderr 日志轮转。
//!
//! 只管进程，不管协议：帧读写、消息队列、写管道卡死的判定都留给调用方
//! （判定为卡死后调 [`Supervisor::kill`]，下一拍照常收尸）。
//!
//! - [`backoff`]：退避与熔断（纯逻辑，时间由调用方传入）。
//! - [`Supervisor`]：期望态 + 子进程 + 退避，由调用方按节拍调 [`Supervisor::tick`]。
//! - [`Job`]：Windows 的 `KILL_ON_JOB_CLOSE` Job 对象；其他平台是空实现。
//! - [`die_with_parent`] / [`die_with_parent_on`]：Linux 下随父进程退出。
//! - [`open_log`]：追加打开日志，超过上限先轮换。

pub mod backoff;
mod job;
mod supervisor;

pub use job::Job;
pub use supervisor::{Event, Supervisor, stop_child, stop_child_background};

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::process::Command;

/// 追加打开日志。文件超过 `max` 字节先改名为 `<stem>.old.<ext>`（覆盖旧的）；目录不存在则创建。
pub fn open_log(path: &Path, max: u64) -> io::Result<File> {
    if let Some(dir) = path.parent() {
        if std::fs::metadata(path).is_ok_and(|m| m.len() > max) {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("log");
            let old = match path.extension().and_then(|e| e.to_str()) {
                Some(ext) => format!("{stem}.old.{ext}"),
                None => format!("{stem}.old"),
            };
            let _ = std::fs::rename(path, dir.join(old));
        }
        std::fs::create_dir_all(dir)?;
    }
    OpenOptions::new().create(true).append(true).open(path)
}

/// 子进程侧：父进程（线程）死亡时收到 SIGTERM。非 Linux 为空操作。
pub fn die_with_parent() {
    #[cfg(target_os = "linux")]
    // SAFETY: prctl(PR_SET_PDEATHSIG) 只设置本进程属性，无内存安全影响。
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
    }
}

/// 父进程侧：让 `cmd` 拉起的子进程在 fork 后、exec 前设好 [`die_with_parent`]。非 Linux 为空操作。
///
/// 注意 PDEATHSIG 跟的是**拉起它的那个线程**，调用方要在长命线程里拉起。
pub fn die_with_parent_on(cmd: &mut Command) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: pre_exec 闭包里只调异步信号安全的 prctl。
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = cmd;
}
