//! 只读数据文件映射的统一入口。
//!
//! 词库、反查索引、注释等缓存文件一律经 [`map_file`] 映射。按 [`CachePolicy`]（配置
//! `schema.dict_cache_policy`，服务启动时由 `set_policy` 设定）处理映射出来的页：
//!
//! - [`CachePolicy::Lock`]：仍是文件映射，映射后把页锁在物理内存里，系统在内存紧张 / 进程
//!   闲置时不能再把它们换出或裁出工作集。不多占一份内存。
//! - [`CachePolicy::Preload`]：映射后整份拷进进程私有内存再返回，返回类型同为 [`Mapped`]，
//!   调用点无需区分。读盘只发生在启动，之后系统仍可回收这些页。
//! - [`CachePolicy::Off`]：不做处理，页按需从磁盘换入、可被系统回收。库层默认值（移动端
//!   等不调 `set_policy` 的形态保持原样）。
//!
//! 锁定与预加载都占物理内存，所以有**预算**（[`set_budget`]，服务启动时按物理内存定，见
//! [`default_budget`]）：按整个文件计，已处理量加上这个文件超过预算就不处理它，退化为普通映射。
//! 计数是**实时**的——映射被释放（词库重建、方案卸载后重载）时 [`Mapped`] 的 `Drop` 把它占的
//! 预算还回去，所以长期运行不会因为反复重映射把预算耗尽。

use memmap2::{Mmap, MmapMut};
use std::fs::File;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

/// 词库缓存策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePolicy {
    /// 锁定：不让系统回收缓存。
    Lock,
    /// 预加载：启动时把缓存载入内存，允许系统回收。
    Preload,
    /// 关闭：不做处理。
    Off,
}

impl CachePolicy {
    /// 配置值 → 策略。认不得的值（含空串）取出厂值 `Lock`——注册表会在写入时拦掉非法值，
    /// 这里只兜手改文件的情形。
    pub fn from_config(s: &str) -> Self {
        match s {
            "preload" => Self::Preload,
            "off" => Self::Off,
            _ => Self::Lock,
        }
    }

    /// 策略 → 配置值（诊断输出用）。
    pub fn as_config(self) -> &'static str {
        match self {
            Self::Lock => "lock",
            Self::Preload => "preload",
            Self::Off => "off",
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Lock => 1,
            Self::Preload => 2,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Lock,
            2 => Self::Preload,
            _ => Self::Off,
        }
    }
}

static POLICY: AtomicU8 = AtomicU8::new(0);
/// 预算字节数；库层默认不设限（移动端等不调 `set_budget` 的形态）。
static BUDGET: AtomicU64 = AtomicU64::new(u64::MAX);
/// 各项实时字节数：当前仍活着的映射里，被锁定 / 被预加载 / 锁页失败 / 超预算未处理的部分。
static LOCKED: AtomicU64 = AtomicU64::new(0);
static PRELOADED: AtomicU64 = AtomicU64::new(0);
static LOCK_FAILED: AtomicU64 = AtomicU64::new(0);
static OVER_BUDGET: AtomicU64 = AtomicU64::new(0);
static WARNED: AtomicBool = AtomicBool::new(false);

/// 设定策略。须在加载任何词库之前调用，之后映射的文件才生效（已映射的不回头处理）。
pub fn set_policy(p: CachePolicy) {
    POLICY.store(p.to_u8(), Ordering::Relaxed);
}

/// 当前生效的策略。
pub fn policy() -> CachePolicy {
    CachePolicy::from_u8(POLICY.load(Ordering::Relaxed))
}

/// 设定预算（字节）。同 [`set_policy`]，须在加载任何词库之前调用。
pub fn set_budget(bytes: u64) {
    BUDGET.store(bytes, Ordering::Relaxed);
}

/// 当前预算；未设限时为 `u64::MAX`。
pub fn budget_bytes() -> u64 {
    BUDGET.load(Ordering::Relaxed)
}

/// 预算的出厂算法：`min(512 MB, 物理内存的 1/8)`；取不到物理内存时只用 512 MB。
/// 程序能判定的不进配置——这是安全网，不是用户要调的旋钮。
pub fn default_budget() -> u64 {
    const CAP: u64 = 512 << 20;
    match imp::physical_memory() {
        Some(total) => CAP.min(total / 8),
        None => CAP,
    }
}

/// 锁定策略下当前仍锁在物理内存里的字节数。
pub fn locked_bytes() -> u64 {
    LOCKED.load(Ordering::Relaxed)
}

/// 锁定策略下锁页失败的字节数；非 0 说明系统配额不够，这部分仍可能被回收。
pub fn lock_failed_bytes() -> u64 {
    LOCK_FAILED.load(Ordering::Relaxed)
}

/// 预加载策略下当前拷在内存里的字节数。
pub fn preloaded_bytes() -> u64 {
    PRELOADED.load(Ordering::Relaxed)
}

/// 因超出预算而没有锁定 / 预加载的字节数（这些文件按普通映射使用）。
pub fn over_budget_bytes() -> u64 {
    OVER_BUDGET.load(Ordering::Relaxed)
}

/// 映射结果：行为等同 [`Mmap`]（`Deref<Target = [u8]>`），多带一份「我占了多少预算」的记账，
/// 释放时归还。全仓持有词库映射的地方都用这个类型，不要直接存 `Mmap`。
pub struct Mapped {
    inner: Mmap,
    locked: u64,
    preloaded: u64,
    lock_failed: u64,
    over_budget: u64,
}

impl Mapped {
    fn plain(inner: Mmap) -> Self {
        Self {
            inner,
            locked: 0,
            preloaded: 0,
            lock_failed: 0,
            over_budget: 0,
        }
    }
}

impl std::ops::Deref for Mapped {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.inner
    }
}

impl AsRef<[u8]> for Mapped {
    fn as_ref(&self) -> &[u8] {
        &self.inner
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        // 映射随 `inner` 一起释放，系统会顺带解除锁页；这里只还计数。
        LOCKED.fetch_sub(self.locked, Ordering::Relaxed);
        PRELOADED.fetch_sub(self.preloaded, Ordering::Relaxed);
        LOCK_FAILED.fetch_sub(self.lock_failed, Ordering::Relaxed);
        OVER_BUDGET.fetch_sub(self.over_budget, Ordering::Relaxed);
    }
}

/// 在预算内为 `len` 字节占位（锁定 / 预加载各用自己的计数）；占不到返回 `None`。
/// 成功时返回占位后的该项总量。
fn reserve(counter: &AtomicU64, budget: u64, len: u64) -> Option<u64> {
    let mut cur = counter.load(Ordering::Relaxed);
    loop {
        let next = cur.checked_add(len)?;
        if next > budget {
            return None;
        }
        match counter.compare_exchange_weak(cur, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Some(next),
            Err(actual) => cur = actual,
        }
    }
}

/// 记超预算并返回该字节数；预算耗尽后后面每个文件都会超：只在第一次升到 warn，其余 debug。
fn note_over_budget(len: u64, what: &str) -> u64 {
    OVER_BUDGET.fetch_add(len, Ordering::Relaxed);
    let msg = format!(
        "词库缓存{what}：{} KB 超出预算（{} MB），该文件按普通映射使用",
        len / 1024,
        budget_bytes() / (1024 * 1024)
    );
    if WARNED.swap(true, Ordering::Relaxed) {
        tracing::debug!("{msg}");
    } else {
        tracing::warn!("{msg}（后续只记 debug）");
    }
    len
}

/// 只读映射整个文件，并按当前 [`CachePolicy`] 与预算处理。
pub fn map_file(file: &File) -> std::io::Result<Mapped> {
    map_file_with(file, policy())
}

/// [`map_file`] 的本体，策略由参数给——测试不必去改全局策略（那会影响并行跑的其它测试）。
fn map_file_with(file: &File, policy: CachePolicy) -> std::io::Result<Mapped> {
    let mapped = unsafe { Mmap::map(file)? };
    let len = mapped.len() as u64;
    if len == 0 {
        return Ok(Mapped::plain(mapped));
    }
    let budget = budget_bytes();
    match policy {
        CachePolicy::Off => Ok(Mapped::plain(mapped)),
        CachePolicy::Preload => match reserve(&PRELOADED, budget, len) {
            Some(total) => match to_private(&mapped) {
                Ok(inner) => {
                    tracing::debug!(
                        "词库缓存预加载：载入 {} KB，现 {} MB",
                        len / 1024,
                        total / (1024 * 1024)
                    );
                    let mut m = Mapped::plain(inner);
                    m.preloaded = len;
                    Ok(m)
                }
                Err(e) => {
                    // 拷不进私有内存（多半是内存紧张）就退回普通映射，与锁页失败同样处理：
                    // 缓存策略只影响快慢，不能让词库因此加载不了。
                    PRELOADED.fetch_sub(len, Ordering::Relaxed);
                    tracing::warn!("词库缓存预加载失败，退回普通映射（{} KB）：{e}", len / 1024);
                    Ok(Mapped::plain(mapped))
                }
            },
            None => {
                let mut m = Mapped::plain(mapped);
                m.over_budget = note_over_budget(len, "预加载");
                Ok(m)
            }
        },
        CachePolicy::Lock => match reserve(&LOCKED, budget, len) {
            Some(total) => {
                let started = std::time::Instant::now();
                let mut m = Mapped::plain(mapped);
                match imp::lock(m.as_ptr(), m.len(), total as usize) {
                    Ok(()) => {
                        tracing::debug!(
                            "词库缓存锁定：锁定 {} KB（{} ms），现 {} MB",
                            len / 1024,
                            started.elapsed().as_millis(),
                            total / (1024 * 1024)
                        );
                        m.locked = len;
                    }
                    Err(e) => {
                        LOCKED.fetch_sub(len, Ordering::Relaxed);
                        LOCK_FAILED.fetch_add(len, Ordering::Relaxed);
                        // 配额不够时后面每个文件都会失败：只在第一次升到 warn，其余 debug。
                        if WARNED.swap(true, Ordering::Relaxed) {
                            tracing::debug!("词库缓存锁定：锁定 {} KB 失败：{e}", len / 1024);
                        } else {
                            tracing::warn!(
                                "词库缓存锁定：锁定 {} KB 失败，该文件仍可能被系统回收（后续失败只记 debug）：{e}",
                                len / 1024
                            );
                        }
                        m.lock_failed = len;
                    }
                }
                Ok(m)
            }
            None => {
                let mut m = Mapped::plain(mapped);
                m.over_budget = note_over_budget(len, "锁定");
                Ok(m)
            }
        },
    }
}

#[cfg(windows)]
mod imp {
    use windows::Win32::System::Memory::{
        GetProcessWorkingSetSizeEx, SETPROCESSWORKINGSETSIZEEX_FLAGS, SetProcessWorkingSetSizeEx,
        VirtualLock,
    };
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows::Win32::System::Threading::GetCurrentProcess;

    /// 物理内存总量（字节）。
    pub(super) fn physical_memory() -> Option<u64> {
        let mut st = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        unsafe { GlobalMemoryStatusEx(&mut st).ok()? };
        Some(st.ullTotalPhys)
    }

    /// 超出当前锁定量的余量：`VirtualLock` 的配额是「工作集下限 − 少量开销」。
    const HEADROOM: usize = 32 << 20;

    /// 先把工作集下限抬到够锁（`locked_total` = 含本文件在内的当前锁定总量），再锁。
    /// 下限只是配额，不预占物理内存；上限只增不减。
    pub(super) fn lock(ptr: *const u8, len: usize, locked_total: usize) -> std::io::Result<()> {
        let need = locked_total + HEADROOM;
        unsafe {
            let proc = GetCurrentProcess();
            let (mut min, mut max) = (0usize, 0usize);
            let mut flags = 0u32;
            GetProcessWorkingSetSizeEx(proc, &mut min, &mut max, &mut flags)
                .ok()
                .map_err(|e| std::io::Error::other(format!("GetProcessWorkingSetSizeEx: {e}")))?;
            if min < need {
                // 上限留出同量空间，否则下限抬不过当前上限。不带硬限制标志，不影响系统裁剪其余页。
                let new_max = max.max(need + HEADROOM);
                SetProcessWorkingSetSizeEx(
                    proc,
                    need,
                    new_max,
                    SETPROCESSWORKINGSETSIZEEX_FLAGS(0),
                )
                .map_err(|e| std::io::Error::other(format!("SetProcessWorkingSetSizeEx: {e}")))?;
            }
            VirtualLock(ptr as *const _, len)
                .map_err(|e| std::io::Error::other(format!("VirtualLock: {e}")))
        }
    }
}

#[cfg(unix)]
mod imp {
    /// 物理内存总量（字节）。
    pub(super) fn physical_memory() -> Option<u64> {
        let (pages, size) = unsafe {
            (
                libc::sysconf(libc::_SC_PHYS_PAGES),
                libc::sysconf(libc::_SC_PAGESIZE),
            )
        };
        (pages > 0 && size > 0).then(|| pages as u64 * size as u64)
    }

    /// `mlock`：Linux 受 `RLIMIT_MEMLOCK`（常见默认仅数 MB，需调高或 `CAP_IPC_LOCK`）、
    /// macOS 受系统可锁页上限约束；失败由调用方记日志并退化。
    pub(super) fn lock(ptr: *const u8, len: usize, _locked_total: usize) -> std::io::Result<()> {
        if unsafe { libc::mlock(ptr as *const _, len) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

#[cfg(not(any(windows, unix)))]
mod imp {
    pub(super) fn physical_memory() -> Option<u64> {
        None
    }

    pub(super) fn lock(_ptr: *const u8, _len: usize, _locked_total: usize) -> std::io::Result<()> {
        Err(std::io::Error::other("本平台不支持锁页"))
    }
}

/// 拷进匿名内存（`map_anon` 不接受长度 0，调用方已排除）。
fn to_private(src: &[u8]) -> std::io::Result<Mmap> {
    let mut anon = MmapMut::map_anon(src.len())?;
    anon.copy_from_slice(src);
    anon.make_read_only()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 预算与全局计数是进程级的：碰它们的测试串行，且只经 `map_file_with` 传策略，
    /// 不改全局策略（其它测试默认 `Off`，不会动这些计数）。
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn temp_file(tag: &str, bytes: &[u8]) -> (std::path::PathBuf, File) {
        let path = std::env::temp_dir().join(format!("wind_map_{tag}_{}", std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        let f = File::open(&path).unwrap();
        (path, f)
    }

    #[test]
    fn reserve_stops_at_the_budget_and_never_overshoots() {
        let c = AtomicU64::new(0);
        assert_eq!(reserve(&c, 100, 60), Some(60));
        assert_eq!(reserve(&c, 100, 40), Some(100), "刚好到顶可以");
        assert_eq!(reserve(&c, 100, 1), None);
        assert_eq!(c.load(Ordering::Relaxed), 100, "占不到时不改计数");
        assert_eq!(reserve(&c, 100, u64::MAX), None, "溢出按占不到处理");
    }

    #[test]
    fn locked_map_keeps_content_and_returns_its_budget_on_drop() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (path, f) = temp_file("lock", b"wind pin bytes");
        let (l0, f0) = (locked_bytes(), lock_failed_bytes());
        let m = map_file_with(&f, CachePolicy::Lock).unwrap();
        assert_eq!(&m[..], b"wind pin bytes", "锁页失败只能退化，内容必须原样");
        // 配额够就记锁定，不够就记失败；两者之和恰为文件长度。
        assert_eq!(locked_bytes() - l0 + lock_failed_bytes() - f0, 14);
        drop(m);
        assert_eq!(
            (locked_bytes(), lock_failed_bytes()),
            (l0, f0),
            "释放后计数归还"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn map_over_budget_is_left_plain_and_counted() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (path, f) = temp_file("over", b"wind pin bytes");
        let (l0, o0) = (locked_bytes(), over_budget_bytes());
        set_budget(10);
        let m = map_file_with(&f, CachePolicy::Lock).unwrap();
        set_budget(u64::MAX);
        assert_eq!(&m[..], b"wind pin bytes");
        assert_eq!(locked_bytes(), l0, "超预算不锁");
        assert_eq!(over_budget_bytes() - o0, 14);
        drop(m);
        assert_eq!(over_budget_bytes(), o0, "释放后归还");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn preload_copies_into_memory_counts_and_returns_on_drop() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (path, f) = temp_file("pre", b"wind pin bytes");
        let p0 = preloaded_bytes();
        let m = map_file_with(&f, CachePolicy::Preload).unwrap();
        assert_eq!(&m[..], b"wind pin bytes");
        assert_eq!(preloaded_bytes() - p0, 14);
        drop(m);
        assert_eq!(preloaded_bytes(), p0);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn default_budget_is_capped_at_512_mb() {
        let b = default_budget();
        assert!(b > 0 && b <= 512 << 20, "{b}");
    }

    #[test]
    fn config_values_map_to_policies_and_unknown_falls_back_to_lock() {
        for (s, p) in [
            ("lock", CachePolicy::Lock),
            ("preload", CachePolicy::Preload),
            ("off", CachePolicy::Off),
            ("", CachePolicy::Lock),
            ("bogus", CachePolicy::Lock),
        ] {
            assert_eq!(CachePolicy::from_config(s), p, "{s:?}");
        }
        for p in [CachePolicy::Lock, CachePolicy::Preload, CachePolicy::Off] {
            assert_eq!(CachePolicy::from_config(p.as_config()), p);
            assert_eq!(CachePolicy::from_u8(p.to_u8()), p);
        }
    }
}
