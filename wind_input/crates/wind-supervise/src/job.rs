// SPDX-License-Identifier: MPL-2.0
//! Windows Job 对象（`KILL_ON_JOB_CLOSE`）：创建者进程退出（含被强杀）时，系统结束 Job 内
//! 全部进程。非 Windows 为空实现（那边靠 [`crate::die_with_parent`]）。

use std::io;
use std::process::Child;

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows::core::PCWSTR;

    pub struct Job(HANDLE);

    // SAFETY: Job 句柄只用于 AssignProcessToJobObject，内核对象句柄可跨线程使用。
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn kill_on_close() -> io::Result<Job> {
            // SAFETY: 标准 Win32 调用；info 在调用期间有效。
            unsafe {
                let h = CreateJobObjectW(None, PCWSTR::null()).map_err(io::Error::other)?;
                let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if let Err(e) = SetInformationJobObject(
                    h,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) {
                    let _ = CloseHandle(h);
                    return Err(io::Error::other(e));
                }
                Ok(Job(h))
            }
        }

        pub fn assign(&self, child: &Child) -> io::Result<()> {
            // SAFETY: child 句柄在调用期间有效。
            unsafe { AssignProcessToJobObject(self.0, HANDLE(child.as_raw_handle())) }
                .map_err(io::Error::other)
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::io;
    use std::process::Child;

    pub struct Job;

    impl Job {
        pub fn kill_on_close() -> io::Result<Job> {
            Ok(Job)
        }

        pub fn assign(&self, _child: &Child) -> io::Result<()> {
            Ok(())
        }
    }
}

/// 句柄随本值 drop（或进程退出）关闭，Job 内进程随之被结束。
pub struct Job(imp::Job);

impl Job {
    pub fn kill_on_close() -> io::Result<Job> {
        imp::Job::kill_on_close().map(Job)
    }

    /// 把已拉起的子进程放进 Job。要在它派生孙进程之前调用。
    pub fn assign(&self, child: &Child) -> io::Result<()> {
        self.0.assign(child)
    }
}
