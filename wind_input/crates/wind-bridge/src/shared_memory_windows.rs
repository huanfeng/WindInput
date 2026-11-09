//! Windows 命名共享内存写端（HostRender SHM）
//!
//! 与 Go `internal/bridge/shared_memory.go` 对齐：
//! CreateFileMappingW(page-file backed) + AppContainer SDDL，
//! 由 wind_tsf.dll 侧用 OpenFileMappingW 以只读权限打开。

#![cfg(windows)]

use std::{io, mem};

use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    PAGE_READWRITE, UnmapViewOfFile,
};

use crate::shared_render_frame::{
    FrameParams, FrameTooLarge, encode_frame_into, encode_hidden_into,
};

#[cfg(test)]
use wind_ipc::protocol::SharedRenderHeader;

/// 将 UTF-8 字符串转为以 NUL 结尾的 UTF-16 向量
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Windows 命名共享内存（page-file backed）写端
///
/// - 创建时附加与 bridge pipe 相同的 SDDL，允许 AppContainer/UWP 进程 OpenFileMappingW。
/// - `write_frame` / `write_hidden` 内部维护单调递增 sequence，序号先提交后写帧。
pub struct WindowsSharedMemory {
    handle: HANDLE,
    ptr: *mut u8,
    size: usize,
    name: String,
    sequence: u32,
}

// SAFETY: 访问由调用方单线程序列化（HostRenderSink 持有 &mut）
unsafe impl Send for WindowsSharedMemory {}

impl WindowsSharedMemory {
    /// 创建命名共享内存（page-file backed），大小为 `size` 字节。
    ///
    /// 名称格式应为 `"Local\\WindXxx"` 或 `"Global\\WindXxx"`。
    pub fn create(name: &str, size: usize) -> io::Result<Self> {
        // 构建与 bridge pipe 相同的 SDDL 安全属性，允许 AppContainer 打开
        let sd = crate::security::create_pipe_security_attributes();
        let sa = sd.as_ref().map(|s| SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: s.as_ptr() as *mut _,
            bInheritHandle: false.into(),
        });

        let name_w = to_wide(name);
        let size_u64 = size as u64;
        let hi = (size_u64 >> 32) as u32;
        let lo = (size_u64 & 0xFFFF_FFFF) as u32;

        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                sa.as_ref().map(|s| s as *const _),
                PAGE_READWRITE,
                hi,
                lo,
                windows::core::PCWSTR(name_w.as_ptr()),
            )
        }
        .map_err(|_| io::Error::last_os_error())?;

        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, size) };
        if view.Value.is_null() {
            let e = io::Error::last_os_error();
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Err(e);
        }

        Ok(Self {
            handle,
            ptr: view.Value as *mut u8,
            size,
            name: name.to_owned(),
            sequence: 0,
        })
    }

    /// 返回创建时的名称
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 返回映射大小（字节）
    pub fn size(&self) -> usize {
        self.size
    }

    /// 整段映射的可写视图，供**自带布局与并发协议**的使用方（如语言栏图标 SHM）。
    ///
    /// [`Self::write_frame`] 那套是 host-render 帧格式专用；图标 SHM 走的是
    /// 双缓冲 + seqlock，布局完全不同，故这里只出借原始字节、不施加任何格式。
    /// 调用方负责保证写入不越界、并在切换活动 slot 前完成数据写入。
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: ptr/size 来自成功的 MapViewOfFile，生命周期由 &mut self 约束
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.size) }
    }

    /// 编码并写入一帧可见帧。
    ///
    /// 内部自增 sequence，以新序号覆盖 `p.sequence`，成功返回新 seq。
    /// 失败（FrameTooLarge）时不递增 sequence。
    pub fn write_frame(&mut self, p: &FrameParams) -> Result<u32, FrameTooLarge> {
        let new_seq = self.sequence + 1;
        let dst = unsafe { std::slice::from_raw_parts_mut(self.ptr, self.size) };
        // 仅在 encode 成功后才提交序号（与 brief 语义对齐：失败不递增）
        encode_frame_into(
            dst,
            &FrameParams {
                sequence: new_seq,
                x: p.x,
                y: p.y,
                width: p.width,
                height: p.height,
                bgra: p.bgra,
                rects: p.rects,
                rendered_hover_index: p.rendered_hover_index,
                target_instance_id: p.target_instance_id,
                software_shadow: p.software_shadow,
                pass: p.pass,
            },
        )?;
        self.sequence = new_seq;
        Ok(new_seq)
    }

    /// 写入"隐藏"帧（flags=0，不可见），sequence 递增，返回新 seq。
    pub fn write_hidden(&mut self, target_instance_id: u32) -> u32 {
        self.sequence += 1;
        let dst = unsafe { std::slice::from_raw_parts_mut(self.ptr, self.size) };
        encode_hidden_into(dst, self.sequence, target_instance_id);
        self.sequence
    }

    /// 测试辅助：从映射内存读回 (header, pixels)。
    #[cfg(test)]
    pub fn read_back(&self) -> (SharedRenderHeader, Vec<u8>) {
        let hdr: SharedRenderHeader =
            unsafe { std::ptr::read_unaligned(self.ptr as *const SharedRenderHeader) };
        let n =
            ({ hdr.data_size } as usize).min(self.size.saturating_sub(SharedRenderHeader::SIZE));
        let slice = unsafe { std::slice::from_raw_parts(self.ptr, self.size) };
        let pixels = slice[SharedRenderHeader::SIZE..SharedRenderHeader::SIZE + n].to_vec();
        (hdr, pixels)
    }
}

impl Drop for WindowsSharedMemory {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.ptr as *mut _,
            });
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use wind_ipc::protocol::{HostRenderHitRect, SharedRenderHeader};

    /// 以只读权限二次打开 SHM（模拟 TSF DLL 端），返回 (header, pixels)
    fn open_readonly_and_read(name: &str) -> (SharedRenderHeader, Vec<u8>) {
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
        .expect("OpenFileMappingW failed");

        // size=0：映射整个对象
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_READ, 0, 0, 0) };
        assert!(!view.Value.is_null(), "MapViewOfFile(readonly) failed");

        let base = view.Value as *const u8;
        let hdr: SharedRenderHeader =
            unsafe { std::ptr::read_unaligned(base as *const SharedRenderHeader) };
        let data_size = { hdr.data_size } as usize;
        let pixels =
            unsafe { std::slice::from_raw_parts(base.add(SharedRenderHeader::SIZE), data_size) }
                .to_vec();

        unsafe {
            let _ = UnmapViewOfFile(view);
            let _ = CloseHandle(handle);
        }
        (hdr, pixels)
    }

    /// 以 OpenEventW + WaitForSingleObject(timeout=0) 验证 Event 已置信号
    fn open_and_wait0(name: &str) -> bool {
        use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::{
            OpenEventW, SYNCHRONIZATION_ACCESS_RIGHTS, WaitForSingleObject,
        };

        let name_w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SYNCHRONIZE = 0x00100000，允许 WaitForSingleObject
        let handle = unsafe {
            OpenEventW(
                SYNCHRONIZATION_ACCESS_RIGHTS(0x0010_0000u32),
                false,
                windows::core::PCWSTR(name_w.as_ptr()),
            )
        }
        .expect("OpenEventW failed");

        let result = unsafe { WaitForSingleObject(handle, 0) };
        unsafe {
            let _ = CloseHandle(handle);
        }
        result == WAIT_OBJECT_0
    }

    /// 从 WindowsSharedMemory 直接读回 header+pixels（复用 read_back）
    fn shm_read_header(shm: &WindowsSharedMemory) -> (SharedRenderHeader, Vec<u8>) {
        shm.read_back()
    }

    #[test]
    fn create_write_and_open_readback() {
        let name = format!("Local\\WindTestShm{}", std::process::id());
        let mut shm = WindowsSharedMemory::create(&name, 1 << 20).unwrap();
        let bgra = vec![0x7Fu8; 4 * 4 * 4];
        let rects = [HostRenderHitRect {
            index: 1,
            x: 0,
            y: 0,
            w: 4,
            h: 4,
        }];
        let seq = shm
            .write_frame(&FrameParams {
                sequence: 0,
                x: 3,
                y: 4,
                width: 4,
                height: 4,
                bgra: &bgra,
                rects: &rects,
                rendered_hover_index: 1,
                target_instance_id: 42,
                software_shadow: false,
                pass: None,
            })
            .unwrap();
        assert_eq!(seq, 1);
        // 用原生 OpenFileMappingW 以读权限二次打开（模拟 DLL 端），校验字节可见
        let (hdr, pixels) = open_readonly_and_read(&name);
        assert_eq!({ hdr.sequence }, 1);
        assert_eq!({ hdr.target_instance_id }, 42);
        assert_eq!(pixels[..bgra.len()], bgra[..]);
    }

    #[test]
    fn named_event_create_and_signal() {
        use crate::named_event::NamedEvent;
        let name = format!("Local\\WindTestEvt{}", std::process::id());
        let evt = NamedEvent::create(&name).unwrap();
        evt.signal();
        // 以 OpenEventW + WaitForSingleObject(0) 验证已置信号（auto-reset：等待成功即被消费）
        assert!(open_and_wait0(&name));
    }

    #[test]
    fn write_hidden_bumps_sequence_and_clears_visible() {
        let name = format!("Local\\WindTestShmHide{}", std::process::id());
        let mut shm = WindowsSharedMemory::create(&name, 4096).unwrap();
        let s1 = shm
            .write_frame(&FrameParams {
                sequence: 0,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                bgra: &[0u8; 4],
                rects: &[],
                rendered_hover_index: -1,
                target_instance_id: 1,
                software_shadow: false,
                pass: None,
            })
            .unwrap();
        let s2 = shm.write_hidden(0);
        assert_eq!(s2, s1 + 1);
        let (hdr, _) = shm_read_header(&shm);
        assert_eq!(
            { hdr.flags } & wind_ipc::protocol::SharedRenderHeader::FLAG_VISIBLE,
            0
        );
        assert_eq!({ hdr.target_instance_id }, 0);
    }
}
