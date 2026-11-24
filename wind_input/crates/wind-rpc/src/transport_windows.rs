//! Windows named pipe 传输：控制 + 事件通道，同步线程模型。
//!
//! 参考 wind-bridge/server.rs（CreateNamedPipe + PIPE_UNLIMITED_INSTANCES + 每连接一线程）
//! 与 push.rs（单向 writer 线程）。线路帧统一为 4 字节大端长度前缀 + JSON 载荷。
//!
//! 管道名：控制 `\\.\pipe\wind_input{suffix}_ctrl`，事件 `\\.\pipe\wind_input{suffix}_events`。
//! 本地授权靠 OS ACL（SDDL，见 security.rs），不再需要 token/Origin/CORS。

use std::ffi::CString;
use std::sync::Arc;

use windows::Win32::Foundation::*;
use windows::Win32::Security::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Pipes::*;
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

use wind_ipc::rpc::MAX_MESSAGE_SIZE;

use crate::dispatch::DispatchState;
use crate::events::EventSink;
use crate::security::create_pipe_security_attributes;
use crate::server::handle_frame;

/// 包装 HANDLE 使其可跨线程移交给 client/writer 线程。
struct PipeHandle(HANDLE);
unsafe impl Send for PipeHandle {}
unsafe impl Sync for PipeHandle {}

/// 创建一个 PIPE_ACCESS_DUPLEX 命名管道实例（带共享 SDDL 安全描述符）。
fn create_pipe_instance(pipe_name_c: &CString) -> Option<HANDLE> {
    let sd = create_pipe_security_attributes();
    let sa = sd.as_ref().map(|s| {
        use windows::Win32::Security::SECURITY_ATTRIBUTES;
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: s.as_ptr() as *mut _,
            bInheritHandle: false.into(),
        }
    });
    let handle = unsafe {
        CreateNamedPipeA(
            windows::core::PCSTR(pipe_name_c.as_ptr() as *const u8),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            65536,
            65536,
            0,
            sa.as_ref().map(|s| s as *const _),
        )
    };
    match handle {
        Ok(h) => Some(h),
        Err(e) => {
            tracing::error!("CreateNamedPipe failed: {}", e);
            std::thread::sleep(std::time::Duration::from_millis(100));
            None
        }
    }
}

/// 等待客户端连接（ERROR_PIPE_CONNECTED 视为已连接）。失败返回 false。
fn wait_connect(handle: HANDLE) -> bool {
    let connected = unsafe { ConnectNamedPipe(handle, None) };
    if connected.is_err() {
        let err = windows::core::Error::from_win32();
        if err.code() != ERROR_PIPE_CONNECTED.into() {
            tracing::warn!("ConnectNamedPipe failed: {}", err);
            unsafe {
                let _ = CloseHandle(handle);
            }
            return false;
        }
    }
    true
}

/// 从管道读满 buf.len() 字节（循环处理部分读 / ERROR_MORE_DATA）。EOF/错误返回 false。
fn read_exact(handle: HANDLE, buf: &mut [u8]) -> bool {
    let mut filled = 0usize;
    while filled < buf.len() {
        let mut got: u32 = 0;
        let ok = unsafe { ReadFile(handle, Some(&mut buf[filled..]), Some(&mut got), None) };
        if ok.is_err() {
            let last = unsafe { GetLastError() };
            if last != ERROR_MORE_DATA {
                return false;
            }
        }
        if got == 0 {
            return false;
        }
        filled += got as usize;
    }
    true
}

/// 写满整个 buf。失败返回 false。
fn write_all(handle: HANDLE, buf: &[u8]) -> bool {
    let mut written = 0usize;
    while written < buf.len() {
        let mut wrote: u32 = 0;
        let ok = unsafe { WriteFile(handle, Some(&buf[written..]), Some(&mut wrote), None) };
        if ok.is_err() || wrote == 0 {
            return false;
        }
        written += wrote as usize;
    }
    true
}

/// 读一帧：4 字节大端长度前缀 + JSON 载荷。
fn read_frame(handle: HANDLE) -> Option<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    if !read_exact(handle, &mut len_buf) {
        return None;
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > MAX_MESSAGE_SIZE {
        return None;
    }
    let mut payload = vec![0u8; len];
    if !read_exact(handle, &mut payload) {
        return None;
    }
    Some(payload)
}

// ── 控制通道（请求-响应） ─────────────────────────────

pub(crate) fn run_ctrl_server(endpoint: &str, state: Arc<DispatchState>) {
    let pipe_name_c = match CString::new(endpoint) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Invalid ctrl pipe name: {}", e);
            return;
        }
    };
    // 多 acceptor:同时保持多个监听实例,吸收设置应用加载时的并发 RPC 突发,
    // 避免"同刻仅 1 监听实例 → 并发连接撞 ERROR_PIPE_BUSY(231)"。
    const ACCEPTORS: usize = 4;
    let name = std::sync::Arc::new(pipe_name_c);
    for i in 1..ACCEPTORS {
        let name = name.clone();
        let state = state.clone();
        std::thread::Builder::new()
            .name(format!("rpc-ctrl-acc{i}"))
            .spawn(move || ctrl_accept_loop(&name, state))
            .ok();
    }
    ctrl_accept_loop(&name, state); // 本线程也跑一个 acceptor
}

/// 单个 acceptor 循环:建实例 → 等连接 → 起线程处理 → 立即建下一个实例继续监听。
fn ctrl_accept_loop(pipe_name_c: &CString, state: Arc<DispatchState>) {
    loop {
        let handle = match create_pipe_instance(pipe_name_c) {
            Some(h) => h,
            None => continue,
        };
        if !wait_connect(handle) {
            continue;
        }
        let state = state.clone();
        let pipe = PipeHandle(handle);
        std::thread::Builder::new()
            .name("rpc-ctrl-client".into())
            .spawn(move || handle_ctrl_client(pipe, state))
            .ok();
    }
}

/// 读进程令牌的变长信息（TokenUser / TokenIntegrityLevel 等），失败返回 None。
fn token_info(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> Option<Vec<u8>> {
    let mut len = 0u32;
    unsafe {
        let _ = GetTokenInformation(token, class, None, 0, &mut len);
    }
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    unsafe {
        GetTokenInformation(
            token,
            class,
            Some(buf.as_mut_ptr() as *mut std::ffi::c_void),
            len,
            &mut len,
        )
        .ok()?;
    }
    Some(buf)
}

/// 令牌的用户 SID 是否与本进程相同、不是 AppContainer、完整性级别不低于 Medium。
fn token_is_trusted(client: HANDLE, own: HANDLE) -> bool {
    // AppContainer：浏览器沙箱 / UWP，一律不可信。
    match token_info(client, TokenIsAppContainer) {
        Some(b) if b.len() >= 4 && u32::from_ne_bytes([b[0], b[1], b[2], b[3]]) == 0 => {}
        _ => return false,
    }
    // 完整性级别（SID 末位子权限 RID）≥ Medium(0x2000)。
    let Some(il) = token_info(client, TokenIntegrityLevel) else {
        return false;
    };
    let label = unsafe { &*(il.as_ptr() as *const TOKEN_MANDATORY_LABEL) };
    let sid = label.Label.Sid;
    let rid = unsafe {
        let n = *GetSidSubAuthorityCount(sid);
        if n == 0 {
            return false;
        }
        *GetSidSubAuthority(sid, (n - 1) as u32)
    };
    if rid < 0x2000 {
        // SECURITY_MANDATORY_MEDIUM_RID
        return false;
    }
    // 同一用户：多用户机器上他人进程同样能连 Everyone 可写的管道。
    let (Some(cu), Some(ou)) = (token_info(client, TokenUser), token_info(own, TokenUser)) else {
        return false;
    };
    let (cu, ou) = unsafe {
        (
            &*(cu.as_ptr() as *const TOKEN_USER),
            &*(ou.as_ptr() as *const TOKEN_USER),
        )
    };
    unsafe { EqualSid(cu.User.Sid, ou.User.Sid).is_ok() }
}

/// 管道对端进程是否可信（见 [`token_is_trusted`]）。任何一步取不到信息都按不可信处理。
fn peer_is_trusted(pipe: HANDLE) -> bool {
    unsafe {
        let mut pid = 0u32;
        if GetNamedPipeClientProcessId(pipe, &mut pid).is_err() || pid == 0 {
            return false;
        }
        let Ok(proc) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut client = HANDLE::default();
        let ok = OpenProcessToken(proc, TOKEN_QUERY, &mut client).is_ok();
        let _ = CloseHandle(proc);
        if !ok {
            return false;
        }
        let mut own = HANDLE::default();
        let trusted = OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut own).is_ok()
            && token_is_trusted(client, own);
        let _ = CloseHandle(client);
        if !own.is_invalid() {
            let _ = CloseHandle(own);
        }
        trusted
    }
}

fn handle_ctrl_client(pipe: PipeHandle, state: Arc<DispatchState>) {
    let handle = pipe.0;
    let trusted = peer_is_trusted(handle);
    while let Some(frame) = read_frame(handle) {
        let resp = handle_frame(&state, &frame, trusted);
        if resp.is_empty() {
            continue;
        }
        if !write_all(handle, &resp) {
            break;
        }
    }
    unsafe {
        let _ = DisconnectNamedPipe(handle);
        let _ = CloseHandle(handle);
    }
}

// ── 事件通道（单向广播） ─────────────────────────────

pub(crate) fn run_events_server(endpoint: &str, sink: EventSink) {
    let pipe_name_c = match CString::new(endpoint) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Invalid events pipe name: {}", e);
            return;
        }
    };
    loop {
        let handle = match create_pipe_instance(&pipe_name_c) {
            Some(h) => h,
            None => continue,
        };
        if !wait_connect(handle) {
            continue;
        }
        let rx = sink.subscribe();
        let pipe = PipeHandle(handle);
        std::thread::Builder::new()
            .name("rpc-events-writer".into())
            .spawn(move || events_writer_loop(pipe, rx))
            .ok();
    }
}

fn events_writer_loop(pipe: PipeHandle, rx: std::sync::mpsc::Receiver<Vec<u8>>) {
    let handle = pipe.0;
    while let Ok(data) = rx.recv() {
        // 空帧为 prune 探活（见 EventSink::prune），忽略不写线路。
        if data.is_empty() {
            continue;
        }
        if !write_all(handle, &data) {
            break;
        }
    }
    unsafe {
        let _ = DisconnectNamedPipe(handle);
        let _ = CloseHandle(handle);
    }
}
