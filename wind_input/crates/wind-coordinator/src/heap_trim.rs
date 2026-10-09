//! 把分配器手里的空闲内存还给操作系统。
//!
//! 进程默认分配器（Windows 进程堆 / glibc malloc / macOS malloc zone）释放后**不主动归还**：
//! 构建索引、全表扫用户词、redb 页缓存这类一次性大分配用完以后，私有内存停在历史峰值。
//! `image_cache.rs` 的实测是 23.6 MiB 释放后不落，一次 `malloc_trim(0)` 压回 5.8 MiB。
//!
//! 只在**空闲时**调：整理堆要走一遍空闲链表、把页 decommit，之后的分配又要重新 commit，
//! 放在按键路径上就是白白抖动。接线点是 redb 的空闲回收（长档连续 60 秒没人碰库；有全表扫描
//! 待回收时短档 3 秒），见 construct.rs。

/// 归还空闲堆内存。尽力而为：失败只打 debug，平台不支持时什么都不做。
pub(crate) fn release_free_heap() {
    imp::release();
}

#[cfg(windows)]
mod imp {
    use windows::Win32::System::Memory::{
        GetProcessHeap, HEAP_FLAGS, HeapCompact, HeapOptimizeResources, HeapSetInformation,
    };

    /// `HEAP_OPTIMIZE_RESOURCES_INFORMATION`（windows 0.58 未收录）。
    #[repr(C)]
    struct OptimizeInfo {
        version: u32,
        flags: u32,
    }

    pub(super) fn release() {
        // Rust 的 System 分配器在 Windows 上就是 HeapAlloc(GetProcessHeap())。
        // HeapOptimizeResources（Win 8.1+）：句柄传空 = 进程内所有启用 LFH 的堆，清掉 LFH
        // 缓存并尽量 decommit；HeapCompact 再合并相邻空闲块、decommit 大块。两者都只动空闲块。
        let info = OptimizeInfo {
            version: 1,
            flags: 0,
        };
        unsafe {
            if let Err(e) = HeapSetInformation(
                None,
                HeapOptimizeResources,
                Some(&info as *const OptimizeInfo as *const _),
                std::mem::size_of::<OptimizeInfo>(),
            ) {
                tracing::debug!("HeapOptimizeResources 失败：{e}");
            }
            if let Ok(heap) = GetProcessHeap() {
                HeapCompact(heap, HEAP_FLAGS(0));
            }
        }
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod imp {
    pub(super) fn release() {
        // glibc：把各 arena 顶端与中间的空闲页还给内核（madvise DONTNEED）。
        unsafe {
            libc::malloc_trim(0);
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    unsafe extern "C" {
        /// libmalloc：zone 传空 = 所有 zone，goal 0 = 尽可能多。
        fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
    }

    pub(super) fn release() {
        unsafe {
            malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
        }
    }
}

#[cfg(not(any(
    windows,
    target_os = "macos",
    all(target_os = "linux", target_env = "gnu")
)))]
mod imp {
    pub(super) fn release() {}
}

#[cfg(test)]
mod tests {
    /// 冒烟：各平台实现能调、不崩。效果（私有内存回落）只能在真进程里量，见 construct.rs。
    #[test]
    fn release_free_heap_is_callable() {
        let v: Vec<Vec<u8>> = (0..1000).map(|_| vec![0u8; 4096]).collect();
        drop(v);
        super::release_free_heap();
    }
}
