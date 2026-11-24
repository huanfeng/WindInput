//! 语言栏图标 SHM 写端（Windows）
//!
//! 服务端把预渲染好的多档图标位图投送给 `wind_tsf.dll`，后者在 `GetIcon` 里直接取用。
//! 布局与并发协议见 `wind_ipc::protocol` 的 `IconShmHeader` / `IconVariant`，
//! 整体设计见 `docs/design/langbar-icon-shared-render.md`。
//!
//! ## 与 host-render SHM 的区别
//!
//! host-render 是服务端主动推帧；图标是被动回调，DLL 只在系统调 `GetIcon` 时读一次。
//! 故不需要 NamedEvent 与 DLL 侧后台线程：变化通知走既有 push 通道
//! （`push_state_update` → `OnUpdate(TF_LBI_ICON)`），本模块只保证任何时刻读到完整位图。
//!
//! ## 并发协议：双缓冲 + seqlock
//!
//! 写端：写非活动 slot → 释放屏障 → 切换 `active_slot` → 递增 `sequence`。
//! 读端：读 `sequence` 与 `active_slot` → 拷贝 → 重读 `sequence`，不等则重试。
//!
//! `sequence` 必须**最后**更新：读端以「两次 sequence 相同」推断期间没发生过切换，
//! 这个推断只在 sequence 是整个发布动作的最后一步时才成立。

#![cfg(windows)]

use std::io;
use std::sync::atomic::{Ordering, fence};

use windows::Win32::Foundation::{CloseHandle, HANDLE};

use wind_ipc::protocol::{
    ICON_SHM_SIZE, ICON_SLOT0_OFFSET, ICON_TABLE_OFFSET, ICON_VARIANT_COUNT, IconShmHeader,
    IconVariant, icon_shm_name, icon_slot_stride, icon_variant_table,
};

use crate::shared_memory_windows::WindowsSharedMemory;

/// 位图集合与协议约定不符。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IconPublishError {
    /// 变体数量不等于 [`ICON_VARIANT_COUNT`]
    VariantCount { expected: usize, got: usize },
    /// 某个变体的字节长度与变体表声明不符
    VariantLen {
        index: usize,
        expected: usize,
        got: usize,
    },
}

impl std::fmt::Display for IconPublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VariantCount { expected, got } => {
                write!(f, "图标变体数量应为 {expected}，实际 {got}")
            }
            Self::VariantLen {
                index,
                expected,
                got,
            } => write!(
                f,
                "第 {index} 个图标变体长度应为 {expected} 字节，实际 {got}"
            ),
        }
    }
}

impl std::error::Error for IconPublishError {}

/// 写者所有权令牌：持有期间本进程是图标 SHM 的唯一写者。
///
/// 不能用「SHM 对象是否已存在」当判据：DLL 一直持有映射，服务重启后 SHM 对象依然健在，
/// 新服务会被挡在门外、图标永久停在旧内容。改用 DLL 不碰的命名互斥体，服务进程一退出
/// （含崩溃）即销毁。
struct WriterLock(HANDLE);

impl WriterLock {
    /// 取得写者身份。返回 `None` 表示本机已有另一个活着的写者。
    fn acquire(shm_name: &str) -> Option<Self> {
        use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
        use windows::Win32::System::Threading::CreateMutexW;

        let name = format!("{shm_name}_writer");
        let name_w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // bInitialOwner=false：只借用「对象是否存在」这一位信息，不进入互斥等待语义，
        // 免得崩溃时留下 abandoned 状态要处理。
        let handle =
            unsafe { CreateMutexW(None, false, windows::core::PCWSTR(name_w.as_ptr())) }.ok()?;
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe {
                let _ = CloseHandle(handle);
            }
            return None;
        }
        Some(Self(handle))
    }
}

impl Drop for WriterLock {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

// SAFETY: 句柄只在 Drop 时被关闭，期间不做任何跨线程可变访问
unsafe impl Send for WriterLock {}

/// 图标 SHM 写端。整个服务进程持有一个。
pub struct IconShm {
    /// 写者身份，随本对象一同释放。字段顺序无关紧要，但它必须比 `shm` 活得久或同寿。
    _writer: WriterLock,
    shm: WindowsSharedMemory,
    sequence: u32,
    active_slot: u32,
    table: Vec<IconVariant>,
}

impl IconShm {
    /// 创建并初始化图标 SHM。
    ///
    /// `suffix` 取 `wind_config::variant::pipe_suffix()`（`""` / `"_dev"`）——
    /// 必须与 C++ 侧 `Globals.h` 的 `WIND_ICON_SHM_NAME` 逐字一致，
    /// 否则 DLL 永远打不开、静默退回本地绘制，且没有任何报错。
    pub fn create(suffix: &str) -> io::Result<Self> {
        let name = icon_shm_name(suffix);

        // 写者唯一性：`CreateFileMappingW` 遇同名会打开已有对象而不报错，第二个写者（如走到
        // `push_state_update` 的集成测试，`cfg(test)` 拦不住）会静默改写正在使用的图标。
        // 判据用互斥体，理由见 [`WriterLock`]。
        let Some(writer) = WriterLock::acquire(&name) else {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("图标共享内存 {name} 已有活跃写者"),
            ));
        };

        let shm = WindowsSharedMemory::create(&name, ICON_SHM_SIZE)?;
        let mut this = Self {
            _writer: writer,
            shm,
            sequence: 0,
            active_slot: 0,
            table: icon_variant_table(),
        };
        this.write_header_and_table();
        Ok(this)
    }

    /// SHM 名（日志与排查用）。
    pub fn name(&self) -> &str {
        self.shm.name()
    }

    /// 当前序号。测试与日志用。
    pub fn sequence(&self) -> u32 {
        self.sequence
    }

    /// 变体表内容固定不变，创建时写一次即可。
    ///
    /// header 里的 `sequence` 此刻为 0——读端据此可知「SHM 已建但尚未发布过内容」，
    /// 应退回本地绘制而不是显示一张全透明的空图标。
    fn write_header_and_table(&mut self) {
        let header = IconShmHeader::new();
        let bytes = header.to_bytes();
        let buf = self.shm.as_mut_slice();
        buf[..IconShmHeader::SIZE].copy_from_slice(&bytes);

        let mut off = ICON_TABLE_OFFSET;
        for v in &self.table {
            buf[off..off + IconVariant::SIZE].copy_from_slice(&v.to_bytes());
            off += IconVariant::SIZE;
        }
    }

    /// 发布一整套变体位图，返回新序号。
    ///
    /// `bitmaps` 的顺序必须与 [`icon_variant_table`] 一致（尺寸档外层、主题内层）。
    /// 长度不符时整批拒绝而不是写一半——写一半会让读端拿到新旧混合的图标集。
    pub fn publish(&mut self, bitmaps: &[Vec<u8>]) -> Result<u32, IconPublishError> {
        if bitmaps.len() != ICON_VARIANT_COUNT {
            return Err(IconPublishError::VariantCount {
                expected: ICON_VARIANT_COUNT,
                got: bitmaps.len(),
            });
        }
        for (i, (bmp, v)) in bitmaps.iter().zip(self.table.iter()).enumerate() {
            let expected = { v.byte_len } as usize;
            if bmp.len() != expected {
                return Err(IconPublishError::VariantLen {
                    index: i,
                    expected,
                    got: bmp.len(),
                });
            }
        }

        let target_slot = 1 - self.active_slot;
        let slot_base = ICON_SLOT0_OFFSET + (target_slot as usize) * icon_slot_stride();
        let table = self.table.clone();
        {
            let buf = self.shm.as_mut_slice();
            for (bmp, v) in bitmaps.iter().zip(table.iter()) {
                let start = slot_base + { v.offset } as usize;
                buf[start..start + bmp.len()].copy_from_slice(bmp);
            }
        }

        // 数据必须先对读端可见，之后才能宣布切换——顺序颠倒会让读端按新 slot
        // 去读尚未写完的字节。
        fence(Ordering::Release);

        let new_seq = self.sequence.wrapping_add(1);
        {
            let buf = self.shm.as_mut_slice();
            // active_slot @12
            buf[12..16].copy_from_slice(&target_slot.to_le_bytes());
            // sequence @8 —— 必须最后写，它是读端判定「快照完整」的唯一依据
            fence(Ordering::Release);
            buf[8..12].copy_from_slice(&new_seq.to_le_bytes());
        }

        self.active_slot = target_slot;
        self.sequence = new_seq;
        Ok(new_seq)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use wind_ipc::protocol::{ICON_SHM_MAGIC, icon_variant_bytes};

    fn full_set(fill: u8) -> Vec<Vec<u8>> {
        icon_variant_table()
            .iter()
            .map(|v| vec![fill; { v.byte_len } as usize])
            .collect()
    }

    /// 以只读方式二次打开（模拟 DLL 端），按 seqlock 协议读回一个变体。
    fn read_variant_readonly(name: &str, size_px: u16, theme: u8) -> Option<Vec<u8>> {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Memory::{
            FILE_MAP_READ, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile,
        };

        let name_w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let handle = unsafe {
            OpenFileMappingW(
                FILE_MAP_READ.0,
                false,
                windows::core::PCWSTR(name_w.as_ptr()),
            )
        }
        .expect("OpenFileMappingW");
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_READ, 0, 0, 0) };
        assert!(!view.Value.is_null());
        let base = view.Value as *const u8;
        let all = unsafe { std::slice::from_raw_parts(base, ICON_SHM_SIZE) };

        let rd_u32 =
            |off: usize| u32::from_le_bytes([all[off], all[off + 1], all[off + 2], all[off + 3]]);

        let magic = rd_u32(0);
        let seq_before = rd_u32(8);
        let slot = rd_u32(12);
        let mut out = None;
        if magic == ICON_SHM_MAGIC && seq_before != 0 {
            let slot_base = ICON_SLOT0_OFFSET + slot as usize * icon_slot_stride();
            let mut off = ICON_TABLE_OFFSET;
            for _ in 0..ICON_VARIANT_COUNT {
                let sz = u16::from_le_bytes([all[off], all[off + 1]]);
                let th = all[off + 2];
                let voff = rd_u32(off + 4) as usize;
                let vlen = rd_u32(off + 8) as usize;
                if sz == size_px && th == theme {
                    let s = slot_base + voff;
                    out = Some(all[s..s + vlen].to_vec());
                    break;
                }
                off += IconVariant::SIZE;
            }
            // seqlock 校验：拷贝期间没发生过发布
            if rd_u32(8) != seq_before {
                out = None;
            }
        }

        unsafe {
            let _ = UnmapViewOfFile(view);
            let _ = CloseHandle(handle);
        }
        out
    }

    /// 只读持有者（模拟 DLL）不得阻止写者重建。
    ///
    /// 钉住「服务重启后图标不失效」：DLL 持有映射使旧 SHM 对象健在，不能据此拒绝新写者。
    #[test]
    fn readonly_holder_does_not_block_writer_recreate() {
        use windows::Win32::System::Memory::{FILE_MAP_READ, OpenFileMappingW};

        let suffix = format!("_t6_{}", std::process::id());
        let name = icon_shm_name(&suffix);

        let first = IconShm::create(&suffix).expect("首次创建应成功");

        // 模拟 DLL：以只读打开并在服务退出后继续持有
        let name_w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let reader = unsafe {
            OpenFileMappingW(
                FILE_MAP_READ.0,
                false,
                windows::core::PCWSTR(name_w.as_ptr()),
            )
        }
        .expect("只读打开应成功");

        drop(first); // 服务退出，但 reader 仍持有 → SHM 对象不销毁

        match IconShm::create(&suffix) {
            Ok(_) => {}
            Err(e) => panic!("只读持有者阻塞了写者重建：{e}"),
        }

        unsafe {
            let _ = CloseHandle(reader);
        }
    }

    /// 同名 SHM 已有所有者时必须拒绝创建第二份。
    ///
    /// 这条守的是一个具体事故：集成测试会走到 `push_state_update`，若允许第二个持有者，
    /// 本机跑一次测试就会把正在使用的输入法图标写成测试状态（且没有任何报错）。
    #[test]
    fn create_rejects_second_owner_of_same_name() {
        let suffix = format!("_t5_{}", std::process::id());
        let first = IconShm::create(&suffix).expect("首次创建应成功");

        // 不用 unwrap_err：IconShm 未实现 Debug（它持有原始句柄，没有有意义的调试表示）
        let err = match IconShm::create(&suffix) {
            Ok(_) => panic!("同名 SHM 被创建了第二份"),
            Err(e) => e,
        };
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);

        // 所有者释放后必须能重建，否则服务重启一次图标就永久失效
        drop(first);
        assert!(
            IconShm::create(&suffix).is_ok(),
            "所有者已释放，却仍无法重新创建"
        );
    }

    #[test]
    fn create_writes_magic_and_table() {
        let suffix = format!("_t1_{}", std::process::id());
        let shm = IconShm::create(&suffix).expect("create");
        // 尚未 publish：sequence 为 0，读端应据此退回本地绘制
        assert_eq!(shm.sequence(), 0);
        assert!(shm.name().ends_with(&suffix));
    }

    /// 发布后能被只读端按 (尺寸, 主题) 精确取回。
    #[test]
    fn publish_then_read_back_by_variant() {
        let suffix = format!("_t2_{}", std::process::id());
        let mut shm = IconShm::create(&suffix).expect("create");
        let name = shm.name().to_owned();

        let mut set = full_set(0);
        // 给 24px/暗色 这一档填独特值，验证按变体定位而非按下标猜
        let table = icon_variant_table();
        let idx = table
            .iter()
            .position(|v| v.size_px == 24 && v.theme == wind_ipc::protocol::ICON_THEME_DARK)
            .unwrap();
        set[idx] = vec![0xAB; icon_variant_bytes(24)];

        let seq = shm.publish(&set).expect("publish");
        assert_eq!(seq, 1);

        let got = read_variant_readonly(&name, 24, wind_ipc::protocol::ICON_THEME_DARK)
            .expect("read back");
        assert_eq!(got.len(), icon_variant_bytes(24));
        assert!(got.iter().all(|&b| b == 0xAB), "取回的不是该变体的数据");
    }

    /// 连续发布在两个 slot 间交替——这是「读端永远能读到完整一套」的前提。
    #[test]
    fn publish_alternates_slots() {
        let suffix = format!("_t3_{}", std::process::id());
        let mut shm = IconShm::create(&suffix).expect("create");
        assert_eq!(shm.active_slot, 0);
        shm.publish(&full_set(1)).expect("publish 1");
        assert_eq!(shm.active_slot, 1);
        shm.publish(&full_set(2)).expect("publish 2");
        assert_eq!(shm.active_slot, 0);
        assert_eq!(shm.sequence(), 2);
    }

    /// 长度不符时整批拒绝，且不改变已发布内容——写一半会让读端拿到新旧混合的图标集。
    #[test]
    fn publish_rejects_bad_input_without_partial_write() {
        let suffix = format!("_t4_{}", std::process::id());
        let mut shm = IconShm::create(&suffix).expect("create");
        let name = shm.name().to_owned();
        shm.publish(&full_set(0x11)).expect("first publish");
        let seq_before = shm.sequence();

        // 数量不对
        let err = shm.publish(&full_set(0x22)[..3]).unwrap_err();
        assert!(matches!(err, IconPublishError::VariantCount { .. }));

        // 某一档长度不对
        let mut bad = full_set(0x33);
        bad[0] = vec![0u8; 7];
        let err = shm.publish(&bad).unwrap_err();
        assert!(matches!(err, IconPublishError::VariantLen { index: 0, .. }));

        assert_eq!(shm.sequence(), seq_before, "失败的发布不应递增序号");
        let got = read_variant_readonly(&name, 16, wind_ipc::protocol::ICON_THEME_LIGHT)
            .expect("read back");
        assert!(got.iter().all(|&b| b == 0x11), "失败的发布污染了已有内容");
    }
}
