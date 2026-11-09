//! POSIX 共享内存 hostrender 写端（macOS / Linux）
//!
//! 与 Go `internal/bridge/host_render_darwin.go` 对齐：64 字节 SharedRenderHeader
//! + 紧跟 BGRA 像素，sequence 单调递增。
use std::io;
use std::os::unix::ffi::OsStrExt;
use wind_ipc::protocol::SharedRenderHeader;

use crate::shared_render_frame::{FrameParams, encode_frame_into, encode_hidden_into};

pub struct PosixSharedMemory {
    fd: libc::c_int,
    ptr: *mut libc::c_void,
    size: usize,
    name: std::ffi::CString,
    owner: bool, // true=create 者，Drop 时 shm_unlink
    sequence: u32,
}

unsafe impl Send for PosixSharedMemory {}

fn cname(name: &str) -> std::ffi::CString {
    std::ffi::CString::new(std::ffi::OsStr::new(name).as_bytes()).unwrap()
}

impl PosixSharedMemory {
    pub fn create(name: &str, size: usize) -> io::Result<Self> {
        Self::open_inner(name, size, true)
    }
    pub fn open_readonly(name: &str, size: usize) -> io::Result<Self> {
        Self::open_inner(name, size, false)
    }

    fn open_inner(name: &str, size: usize, create: bool) -> io::Result<Self> {
        let cn = cname(name);
        let (oflag, prot) = if create {
            // O_EXCL：确保拿到全新对象。macOS 上 POSIX shm 对象只能 ftruncate 一次——
            // 若上次进程非干净退出残留了同名对象，复用它再 ftruncate 会 EINVAL(os 22)。
            // 故 create 前先 shm_unlink 清残留，再以 O_EXCL 独占创建。
            (
                libc::O_CREAT | libc::O_RDWR | libc::O_EXCL,
                libc::PROT_READ | libc::PROT_WRITE,
            )
        } else {
            (libc::O_RDONLY, libc::PROT_READ)
        };
        if create {
            // 忽略错误：不存在时 ENOENT 正常。
            unsafe { libc::shm_unlink(cn.as_ptr()) };
        }
        let fd = unsafe { libc::shm_open(cn.as_ptr(), oflag, 0o600) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        if create && unsafe { libc::ftruncate(fd, size as libc::off_t) } != 0 {
            let e = io::Error::last_os_error();
            unsafe {
                libc::close(fd);
            }
            return Err(e);
        }
        let ptr = unsafe { libc::mmap(std::ptr::null_mut(), size, prot, libc::MAP_SHARED, fd, 0) };
        if ptr == libc::MAP_FAILED {
            let e = io::Error::last_os_error();
            unsafe {
                libc::close(fd);
            }
            return Err(e);
        }
        Ok(Self {
            fd,
            ptr,
            size,
            name: cn,
            owner: create,
            sequence: 0,
        })
    }

    pub fn write_frame(&mut self, x: i32, y: i32, width: u32, height: u32, bgra: &[u8]) -> u32 {
        self.sequence += 1;
        let dst = unsafe { std::slice::from_raw_parts_mut(self.ptr as *mut u8, self.size) };
        let _ = encode_frame_into(
            dst,
            &FrameParams {
                sequence: self.sequence,
                x,
                y,
                width,
                height,
                bgra,
                rects: &[],
                rendered_hover_index: -1,
                target_instance_id: 0,
                software_shadow: false,
                pass: None,
            },
        );
        self.sequence
    }

    /// 写一帧"隐藏"header：flags=0（不可见），sequence 递增，不写像素。
    /// 与 Go host_render_darwin.go 的 hide 路径对齐（候选窗隐藏时通知 .app 撤帧）。
    pub fn write_hidden(&mut self) -> u32 {
        self.sequence += 1;
        let dst = unsafe { std::slice::from_raw_parts_mut(self.ptr as *mut u8, self.size) };
        encode_hidden_into(dst, self.sequence, 0);
        self.sequence
    }

    pub fn read_header(&self) -> SharedRenderHeader {
        unsafe { std::ptr::read_unaligned(self.ptr as *const SharedRenderHeader) }
    }

    pub fn pixels(&self) -> &[u8] {
        let hdr = self.read_header();
        let n = (hdr.data_size as usize).min(self.size.saturating_sub(SharedRenderHeader::SIZE));
        unsafe {
            std::slice::from_raw_parts((self.ptr as *const u8).add(SharedRenderHeader::SIZE), n)
        }
    }
}

impl Drop for PosixSharedMemory {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr, self.size);
            libc::close(self.fd);
            if self.owner {
                libc::shm_unlink(self.name.as_ptr());
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use wind_ipc::protocol::SharedRenderHeader;

    #[test]
    fn write_then_read_back_header_and_pixels() {
        let name = format!("/wind_shm_test_{}", std::process::id());
        let w = 2u32;
        let h = 2u32;
        let stride = w * 4;
        let bgra: Vec<u8> = (0..(stride * h) as usize)
            .map(|i| (i % 256) as u8)
            .collect();
        let mut shm =
            PosixSharedMemory::create(&name, SharedRenderHeader::SIZE + bgra.len()).unwrap();
        let seq = shm.write_frame(10, 20, w, h, &bgra);
        assert_eq!(seq, 1);

        // 另开只读映射读回
        let ro =
            PosixSharedMemory::open_readonly(&name, SharedRenderHeader::SIZE + bgra.len()).unwrap();
        let hdr = ro.read_header();
        assert_eq!({ hdr.magic }, 0x57494E44);
        assert_eq!({ hdr.sequence }, 1);
        assert_eq!({ hdr.width }, w);
        assert_eq!({ hdr.height }, h);
        assert_eq!({ hdr.stride }, stride);
        assert_eq!({ hdr.x }, 10);
        assert_eq!(ro.pixels(), &bgra[..]);
    }

    #[test]
    fn create_succeeds_over_stale_object_and_at_4mb() {
        // 复现并回归 macOS EINVAL：手动留一个已 ftruncate 的同名残留对象，
        // 再 create 同名 4MB —— 修复后应先 shm_unlink 清残留、O_EXCL 重建而成功。
        let name = format!("/wind_shm_stale_{}", std::process::id());
        let cn = std::ffi::CString::new(name.clone()).unwrap();
        unsafe {
            libc::shm_unlink(cn.as_ptr());
            let fd = libc::shm_open(cn.as_ptr(), libc::O_CREAT | libc::O_RDWR, 0o600);
            assert!(fd >= 0, "预置残留对象失败");
            assert_eq!(libc::ftruncate(fd, 4096), 0);
            libc::close(fd); // 不 unlink，故意留残留
        }
        // 4MB（与 forwarder 的 MAX_SHARED_RENDER_SIZE 同量级），复用残留名
        let mut shm = PosixSharedMemory::create(&name, 4 * 1024 * 1024)
            .expect("create over stale object at 4MB should succeed");
        let seq = shm.write_frame(1, 2, 2, 2, &[7u8; 16]);
        assert_eq!(seq, 1);
        assert_eq!({ shm.read_header().magic }, 0x57494E44);
    }

    #[test]
    fn write_hidden_clears_visible_flag_and_bumps_seq() {
        let name = format!("/wind_shm_hide_{}", std::process::id());
        let mut shm = PosixSharedMemory::create(&name, SharedRenderHeader::SIZE + 64).unwrap();
        let s1 = shm.write_frame(1, 2, 2, 2, &[0u8; 16]);
        let s2 = shm.write_hidden();
        assert_eq!(s2, s1 + 1);
        let hdr = shm.read_header();
        assert_eq!({ hdr.flags } & SharedRenderHeader::FLAG_VISIBLE, 0);
        assert_eq!({ hdr.sequence }, s2);
    }
}
