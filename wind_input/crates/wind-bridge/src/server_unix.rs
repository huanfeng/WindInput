//! UDS 请求-响应服务器（macOS / Linux）
//!
//! 与 Go `internal/bridge/server_darwin.go` 对齐：每连接一线程，
//! 阻塞读 8 字节 header + payload，dispatch_command 复用平台无关逻辑。
use crate::handler::*;
use crate::server::{ClientCtx, dispatch_command};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, info, warn};
use wind_ipc::codec::decode_focus_gained;
use wind_ipc::protocol::*;

pub fn run_uds_server(socket_path: PathBuf, handler: Arc<dyn MessageHandler>) {
    if let Some(parent) = socket_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::remove_file(&socket_path); // 清理残留
    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            warn!("bind {:?} failed: {}", socket_path, e);
            return;
        }
    };
    info!("Bridge UDS listening on {:?}", socket_path);
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let handler = handler.clone();
                std::thread::Builder::new()
                    .name("bridge-client".into())
                    .spawn(move || handle_uds_client(s, handler))
                    .ok();
            }
            Err(e) => warn!("accept failed: {}", e),
        }
    }
}

fn handle_uds_client(mut stream: UnixStream, handler: Arc<dyn MessageHandler>) {
    set_nosigpipe(&stream);
    let mut header_buf = [0u8; IpcHeader::SIZE];
    loop {
        if stream.read_exact(&mut header_buf).is_err() {
            debug!("bridge client disconnected");
            break;
        }
        let header = IpcHeader::from_bytes(&header_buf);
        let payload_len = header.length as usize;
        let mut payload = vec![0u8; payload_len];
        if payload_len > 0 && stream.read_exact(&mut payload).is_err() {
            break;
        }
        // unix 无连接身份语义（host-render 仅 Windows），传 {0,0} 占位。
        let ctx = ClientCtx { conn_id: 0, pid: 0 };
        let response = dispatch_command(&handler, header.command, header.is_async(), &payload, ctx);
        if let Some(resp) = response
            && stream.write_all(&resp).is_err()
        {
            break;
        }
        // FOCUS_GAINED 重型段延后到响应写出之后（对齐 server.rs handle_client）
        if header.command == CMD_FOCUS_GAINED
            && let Ok(fg) = decode_focus_gained(&payload)
        {
            handler.handle_focus_gained(&FocusData {
                x: fg.caret.x,
                y: fg.caret.y,
                height: fg.caret.height,
                composition_start_x: fg.caret.composition_start_x,
                composition_start_y: fg.caret.composition_start_y,
                client_token: fg.client_token,
                input_scope_mask: fg.input_scope_mask,
                disabled: fg.disabled != 0,
                reason: fg.reason,
                // macOS `.app` 的 caret 段恒为 0（含 height），服务端 apply_focus_caret
                // 见 height==0 即返回；坐标另经 CmdCaretUpdate 上报。来源恒落 UNKNOWN
                // ——那边没有 TSF，不适用 CARET_SRC_* 的分级。
                caret_source: fg.caret_source,
                bundle_id: wind_ipc::codec::decode_focus_gained_bundle_id(&payload).to_string(),
                // macOS `.app` 目前不发窗口类段（那边没有「一个进程多种壳窗口」的等价
                // 问题）；解一次是为了两平台走同一条路径，将来 `.app` 补发即刻生效。
                window_class: wind_ipc::codec::decode_focus_gained_window_class(&payload)
                    .to_string(),
            });
        }
    }
}

/// macOS：设 SO_NOSIGPIPE 防对端断开触发 SIGPIPE 杀进程。
/// Linux 无此 socket 选项，靠 service 启动时 signal(SIGPIPE, SIG_IGN) 兜底（见 W9）。
#[cfg(target_os = "macos")]
pub(crate) fn set_nosigpipe(stream: &UnixStream) {
    use std::os::unix::io::AsRawFd;
    let one: libc::c_int = 1;
    unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_NOSIGPIPE,
            &one as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn set_nosigpipe(_stream: &UnixStream) {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::handler::{
        CaretData, CommitRequestData, CommitResultData, FocusData, KeyAction, KeyEventData,
        MessageHandler, StatusUpdateData,
    };
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
    use wind_ipc::protocol::{CMD_KEY_EVENT, CMD_PASS_THROUGH, IpcHeader};

    // 最小 handler：对 KEY_EVENT 回 PassThrough，便于断言一个合法响应帧
    struct EchoHandler;
    impl MessageHandler for EchoHandler {
        fn handle_key_event(&self, _d: &KeyEventData) -> KeyAction {
            KeyAction::PassThrough
        }
        fn handle_focus_gained(&self, _data: &FocusData) -> Option<StatusUpdateData> {
            None
        }
        fn handle_focus_lost(&self, _client_token: u64, _reason: FocusLostReason) {}
        fn handle_ime_activated(&self, _client_token: u64) -> Option<StatusUpdateData> {
            None
        }
        fn handle_ime_deactivated(&self, _client_token: u64) {}
        fn handle_mode_notify(&self, _flags: u32) {}
        fn handle_toggle_mode(&self) -> (Option<StatusUpdateData>, String) {
            (None, String::new())
        }
        fn handle_system_mode_switch(
            &self,
            _chinese_mode: bool,
            _source: wind_ipc::protocol::ModeSwitchSource,
            _ctrl_held: bool,
        ) -> (Option<StatusUpdateData>, String) {
            (None, String::new())
        }
        fn handle_menu_command(&self, _command: &str) -> Option<StatusUpdateData> {
            None
        }
        fn handle_composition_terminated(&self) {}
        fn handle_caret_update(&self, _data: &CaretData) {}
        fn handle_focus_gained_caret(&self, _data: &CaretData) {}
        fn handle_caret_probe(&self, _data: &CaretData) {}
        fn handle_caret_pending(&self) {}
        fn handle_selection_changed(&self, _prev_char: u16) {}
        fn handle_commit_request(&self, _data: &CommitRequestData) -> Option<CommitResultData> {
            None
        }
    }

    #[test]
    fn uds_key_event_roundtrip() {
        let dir = std::env::temp_dir().join(format!("wind_uds_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bridge.sock");
        let p2 = path.clone();
        let handler: Arc<dyn MessageHandler> = Arc::new(EchoHandler);
        std::thread::spawn(move || run_uds_server(p2, handler));
        // 等 socket 就绪
        let mut stream = None;
        for _ in 0..100 {
            if let Ok(s) = UnixStream::connect(&path) {
                stream = Some(s);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut stream = stream.expect("connect bridge.sock");

        // 发 KEY_EVENT 帧：header(8) + KeyPayload(18)
        let payload = [0u8; 18];
        let header = IpcHeader::new(CMD_KEY_EVENT, payload.len() as u32).to_bytes();
        stream.write_all(&header).unwrap();
        stream.write_all(&payload).unwrap();

        // 读响应 header
        let mut resp = [0u8; 8];
        stream.read_exact(&mut resp).unwrap();
        let rh = IpcHeader::from_bytes(&resp);
        let cmd = rh.command; // IpcHeader 是 packed struct，先复制字段再断言
        assert_eq!(cmd, CMD_PASS_THROUGH); // EchoHandler → PassThrough
    }
}
