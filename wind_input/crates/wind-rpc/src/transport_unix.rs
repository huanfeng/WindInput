//! unix socket 传输（macOS / Linux）：控制 + 事件通道，同步线程模型。
//!
//! 线路帧统一为 wind-ipc 的 4 字节大端长度前缀 + JSON 载荷（与 windows pipe 一致）。
//!
//! macOS 接线点：socket 路径由 `server::unix_endpoint` 计算（XDG_RUNTIME_DIR 回退 /tmp）；
//! 若 macOS 需放到 ~/Library/Application Support，在该函数调整即可，传输逻辑无需改动。

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;

use wind_ipc::rpc::MAX_MESSAGE_SIZE;

use crate::dispatch::DispatchState;
use crate::events::EventSink;
use crate::server::handle_frame;

/// 绑定 unix socket：先删除残留路径（上次未清理的 socket 文件），再 bind。
fn bind(endpoint: &str) -> std::io::Result<UnixListener> {
    let _ = std::fs::remove_file(endpoint);
    UnixListener::bind(endpoint)
}

/// 读一帧：4 字节大端长度前缀 + JSON 载荷。EOF/错误返回 None。
fn read_frame(stream: &mut UnixStream) -> Option<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).ok()?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > MAX_MESSAGE_SIZE {
        return None;
    }
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).ok()?;
    Some(payload)
}

// ── 控制通道（请求-响应） ─────────────────────────────

pub(crate) fn run_ctrl_server(endpoint: &str, state: Arc<DispatchState>) {
    let listener = match bind(endpoint) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("RPC ctrl bind {} failed: {}", endpoint, e);
            return;
        }
    };
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let state = state.clone();
                std::thread::Builder::new()
                    .name("rpc-ctrl-client".into())
                    .spawn(move || handle_ctrl_client(stream, state))
                    .ok();
            }
            Err(e) => {
                tracing::warn!("RPC ctrl accept failed: {}", e);
            }
        }
    }
}

/// 对端是否与本进程同一 uid（SO_PEERCRED / getpeereid）。取不到凭据按不可信处理。
/// socket 落在 /tmp 时他人也能 connect，不能只靠文件权限。
fn peer_is_same_user(stream: &UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    let fd = stream.as_raw_fd();
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut cred as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        rc == 0 && cred.uid == unsafe { libc::geteuid() }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let (mut uid, mut gid): (libc::uid_t, libc::gid_t) = (0, 0);
        let rc = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };
        rc == 0 && uid == unsafe { libc::geteuid() }
    }
}

fn handle_ctrl_client(mut stream: UnixStream, state: Arc<DispatchState>) {
    let trusted = peer_is_same_user(&stream);
    while let Some(frame) = read_frame(&mut stream) {
        let resp = handle_frame(&state, &frame, trusted);
        if resp.is_empty() {
            continue;
        }
        if stream.write_all(&resp).is_err() {
            break;
        }
    }
}

// ── 事件通道（单向广播） ─────────────────────────────

pub(crate) fn run_events_server(endpoint: &str, sink: EventSink) {
    let listener = match bind(endpoint) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("RPC events bind {} failed: {}", endpoint, e);
            return;
        }
    };
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let rx = sink.subscribe();
                std::thread::Builder::new()
                    .name("rpc-events-writer".into())
                    .spawn(move || events_writer_loop(stream, rx))
                    .ok();
            }
            Err(e) => {
                tracing::warn!("RPC events accept failed: {}", e);
            }
        }
    }
}

fn events_writer_loop(mut stream: UnixStream, rx: std::sync::mpsc::Receiver<Vec<u8>>) {
    while let Ok(data) = rx.recv() {
        // 空帧为 prune 探活（见 EventSink::prune），忽略不写线路。
        if data.is_empty() {
            continue;
        }
        if stream.write_all(&data).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_process_peer_is_trusted() {
        let (a, _b) = UnixStream::pair().unwrap();
        assert!(peer_is_same_user(&a));
    }
}
