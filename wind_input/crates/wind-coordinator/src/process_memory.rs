//! 本进程的私有内存（诊断 RPC `system.memoryStats` 用）。
//!
//! 口径取「不含 mmap 文件页」的那个数，与靶机实测同口径（`docs/design/memory-footprint.md`）：
//!
//! | 平台 | 取值 | `kind` |
//! |---|---|---|
//! | Windows | `GetProcessMemoryInfo` 的 `PrivateUsage`（任务管理器「提交大小」） | `"private"` |
//! | Linux / Android | `/proc/self/status` 的 `RssAnon`（匿名驻留页；mmap 的词库是文件页，不计） | `"rss_anon"` |
//! | macOS 等 | 不提供（`None`） | — |
//!
//! macOS 不做：同口径的是 `task_info(TASK_VM_INFO).phys_footprint`，要为它引 mach 绑定，
//! 而诊断卡片在 macOS 上缺这一个数不影响其余各项（已加载方案、各结构自报大小）。
//!
//! 平台分层按根 AGENTS.md「cfg 兜底值在缺失平台可接受的探测函数」：同名函数三平台并列，
//! 不进 `HostServices`。

/// 进程私有内存读数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessMemory {
    pub bytes: u64,
    /// 口径：`"private"`（Windows）/ `"rss_anon"`（Linux、Android）。
    pub kind: &'static str,
}

/// 读本进程的私有内存；平台不支持或读取失败时 `None`。
pub fn private_memory() -> Option<ProcessMemory> {
    imp::read()
}

#[cfg(windows)]
mod imp {
    use super::ProcessMemory;
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;

    pub(super) fn read() -> Option<ProcessMemory> {
        let mut c = PROCESS_MEMORY_COUNTERS_EX {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        // SAFETY: 伪句柄无需关闭；`cb` 声明了 EX 版结构体的大小，API 据此填写 PrivateUsage。
        unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut c as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
                c.cb,
            )
        }
        .ok()?;
        Some(ProcessMemory {
            bytes: c.PrivateUsage as u64,
            kind: "private",
        })
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod imp {
    use super::ProcessMemory;

    pub(super) fn read() -> Option<ProcessMemory> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let kb = parse_rss_anon_kb(&status)?;
        Some(ProcessMemory {
            bytes: kb * 1024,
            kind: "rss_anon",
        })
    }

    /// `RssAnon:` 行（`RssAnon:<空白> 12345 kB`）→ 12345。
    pub(super) fn parse_rss_anon_kb(status: &str) -> Option<u64> {
        status
            .lines()
            .find_map(|l| l.strip_prefix("RssAnon:"))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn parses_rss_anon_line() {
            let s = "VmRSS:\t   9000 kB\nRssAnon:\t   1234 kB\nRssFile:\t  77 kB\n";
            assert_eq!(super::parse_rss_anon_kb(s), Some(1234));
            assert_eq!(super::parse_rss_anon_kb("VmRSS: 1 kB\n"), None);
        }

        #[test]
        fn reads_this_process() {
            let m = super::read().expect("Linux 上应读得到 RssAnon");
            assert!(m.bytes > 0);
            assert_eq!(m.kind, "rss_anon");
        }
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "android")))]
mod imp {
    use super::ProcessMemory;

    pub(super) fn read() -> Option<ProcessMemory> {
        None
    }
}
