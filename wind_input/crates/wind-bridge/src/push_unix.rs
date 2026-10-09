//! UDS 推送服务器（macOS / Linux）
//!
//! 与 Go `internal/bridge/server_push_darwin.go` 对齐：连接即发 SERVICE_READY，
//! 读 8 字节 token 注册客户端，writer 线程经 mpsc 把帧写回。

use crate::push::PushClient;
use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tracing::{debug, info, warn};
use wind_ipc::protocol::*;

/// 服务端自生成的 push 客户端 token（仅用于 clients 列表去重）。
/// darwin UDS push 协议不做 token 握手（见 handle_push_conn 说明），故由服务端发号。
static NEXT_PUSH_TOKEN: AtomicU64 = AtomicU64::new(1);

pub(crate) fn run_uds_push_server(socket_path: PathBuf, clients: Arc<Mutex<Vec<PushClient>>>) {
    if let Some(parent) = socket_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::remove_file(&socket_path);
    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            warn!("bind push {:?} failed: {}", socket_path, e);
            return;
        }
    };
    info!("Push UDS listening on {:?}", socket_path);
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let clients = clients.clone();
                std::thread::Builder::new()
                    .name("push-conn".into())
                    .spawn(move || handle_push_conn(s, clients))
                    .ok();
            }
            Err(e) => warn!("push accept failed: {}", e),
        }
    }
}

fn handle_push_conn(mut stream: UnixStream, clients: Arc<Mutex<Vec<PushClient>>>) {
    // 推送里有上屏文本、按键合成、候选内容：只推给本用户的进程。
    #[cfg(target_os = "linux")]
    if !crate::endpoint::peer_is_current_user(&stream) {
        warn!("push client 不是本用户的进程，拒绝");
        return;
    }
    crate::server_unix::set_nosigpipe(&stream);
    // 1. 发 SERVICE_READY
    let ready = IpcHeader::new(CMD_SERVICE_READY, 0).to_bytes();
    if stream.write_all(&ready).is_err() {
        return;
    }
    // 2. accept 即注册（darwin UDS push 协议：服务端只写、客户端只读，不做 token 握手）。
    //    对齐旧 Go server_darwin.go acceptPushLoop（accept 即登记 pushClients）。
    //    Swift PushClient 连上后只 readLoop、不回写任何字节；早期 Rust 误植了 Windows pipe
    //    (server_push.go) 的 8 字节 token 握手 → read_exact(token) 永久阻塞 → 客户端永不进
    //    clients 列表 → 候选帧 fanout 收不到 → 候选窗不显示。token 仅用于列表去重，服务端发号。
    //    断连检测：writer loop 在 push 写失败时移除该 client（与原逻辑一致）。
    let token = NEXT_PUSH_TOKEN.fetch_add(1, Ordering::Relaxed);
    info!("Push client registered (token=0x{:016X})", token);
    // 3. 注册
    let (tx, rx) = crate::push::push_channel();
    {
        let mut c = clients.lock().unwrap();
        // hooked: unix 侧不注册 connected_hook（host-render 是 Windows 专属），
        // 置 false 与 Windows 语义一致——若将来接入 hook，补跑逻辑自动覆盖。
        c.push(PushClient {
            token,
            tx,
            hooked: false,
            // 上面第 1 步已经发过（unix 侧连接即发，不走 `PushServer::mark_ready` 的补发）
            ready_sent: true,
        });
    }
    // 4. writer loop
    let mut writer = stream;
    while let Ok(data) = rx.recv() {
        if writer.write_all(&data).is_err() {
            break;
        }
    }
    let mut c = clients.lock().unwrap();
    c.retain(|x| x.token != token);
    debug!("Push client 0x{:016X} disconnected", token);
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::push::{PushConfig, PushServer};
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
    use wind_ipc::protocol::{CMD_SERVICE_READY, CMD_STATE_PUSH, IpcHeader};

    #[test]
    fn uds_push_fanout_delivers_to_client() {
        let dir = std::env::temp_dir().join(format!("wind_push_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bridge_push.sock");
        let server = Arc::new(PushServer::new(PushConfig {
            suffix: String::new(),
            write_timeout_ms: 1000,
        }));
        let clients = server.clients_for_test();
        let p2 = path.clone();
        std::thread::spawn(move || run_uds_push_server(p2, clients));

        let mut stream = None;
        for _ in 0..100 {
            if let Ok(s) = UnixStream::connect(&path) {
                stream = Some(s);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut stream = stream.expect("connect push");

        // 读 SERVICE_READY（服务端连接即发）
        let mut ready = [0u8; 8];
        stream.read_exact(&mut ready).unwrap();
        let cmd_ready = IpcHeader::from_bytes(&ready).command; // packed struct，先复制字段
        assert_eq!(cmd_ready, CMD_SERVICE_READY);

        // 关键: 客户端**不发任何 token**（对齐 Swift PushClient：只读不写）。
        // accept 即注册，故无需握手即应收到 fanout。早期 token 握手会令此处永久阻塞。
        std::thread::sleep(std::time::Duration::from_millis(50)); // 等服务端注册落库

        // 服务端 fanout 一帧
        let msg = IpcHeader::new(CMD_STATE_PUSH, 0).to_bytes().to_vec();
        server.push_to_active(&msg);

        let mut got = [0u8; 8];
        stream.read_exact(&mut got).unwrap();
        let cmd_got = IpcHeader::from_bytes(&got).command; // packed struct，先复制字段
        assert_eq!(cmd_got, CMD_STATE_PUSH);
    }
}
