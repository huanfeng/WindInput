//! Named Pipe 请求-响应服务器
//!
//! 与 Go 版本 `wind_input/internal/bridge/server.go` 对齐。
//! 每个客户端连接在独立线程中处理（对应 Go 的 goroutine）。

use crate::handler::*;
#[cfg(windows)]
use crate::host_render_windows::HostRenderManager;
use std::sync::Arc;
use tracing::{debug, info, warn};
#[cfg(windows)]
use tracing::{error, trace};

/// 单条客户端连接的身份（每连接唯一）。
///
/// `conn_id` 由服务器从 1 起单调分配（0 保留为 host-render 广播 target）；
/// `pid` 为管道对端进程 ID（`GetNamedPipeClientProcessId`）。host-render 的
/// setup/note_focus/cleanup 均以此对定位实例。unix 路径不含身份语义，传 {0,0}。
#[derive(Debug, Clone, Copy)]
pub(crate) struct ClientCtx {
    pub conn_id: u32,
    pub pid: u32,
}

/// Bridge 服务器配置
pub struct BridgeConfig {
    /// 管道名称后缀（构建变体）
    pub suffix: String,
    /// 请求处理超时（毫秒）
    pub request_timeout_ms: u64,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            suffix: String::new(),
            request_timeout_ms: 1000,
        }
    }
}

/// Bridge 服务器
pub struct BridgeServer {
    config: BridgeConfig,
    handler: Arc<dyn MessageHandler>,
    /// host-render 管理器（Windows）：命名管道连接循环据此为每连接分配实例、
    /// 应答 HostRenderSetup、断线清理。未注入（None）时 HOST_RENDER_REQUEST 仅回 ACK。
    #[cfg(windows)]
    host_render: Option<Arc<HostRenderManager>>,
}

impl BridgeServer {
    pub fn new(config: BridgeConfig, handler: Arc<dyn MessageHandler>) -> Self {
        Self {
            config,
            handler,
            #[cfg(windows)]
            host_render: None,
        }
    }

    /// 注入 host-render 管理器（Windows）。与 Coordinator 共享同一 `Arc` 实例，
    /// 使 setup（连接循环）与 write_frame/hide（协调器）作用于同一状态。
    #[cfg(windows)]
    pub fn with_host_render(mut self, mgr: Arc<HostRenderManager>) -> Self {
        self.host_render = Some(mgr);
        self
    }

    /// 获取管道名称。`{变体后缀}{per-user SID 后缀}`——管道名字空间是机器级的，
    /// 靠 SID 后缀按用户隔离（详见 [`crate::pipe_scope`]）。C++ TSF 端算同一名字。
    pub fn pipe_name(&self) -> String {
        format!(
            r"\\.\pipe\wind_input{}{}",
            self.config.suffix,
            crate::pipe_scope::user_scope_suffix()
        )
    }

    /// 启动 Named Pipe 服务器（Windows）
    #[cfg(windows)]
    pub fn start(&self) -> anyhow::Result<()> {
        let pipe_name = self.pipe_name();
        info!("Bridge server starting on {:?}", pipe_name);

        let handler = self.handler.clone();
        let timeout_ms = self.config.request_timeout_ms;
        let host_render = self.host_render.clone();

        // 在独立线程中运行阻塞的 Named Pipe 循环
        std::thread::Builder::new()
            .name("bridge-server".into())
            .spawn(move || {
                run_pipe_server(&pipe_name, handler, timeout_ms, host_render);
            })?;

        Ok(())
    }

    /// 启动 UDS 请求服务器（macOS / Linux）
    #[cfg(unix)]
    pub fn start(&self) -> anyhow::Result<()> {
        let path = crate::endpoint::request_socket_path(&self.config.suffix);
        info!("Bridge UDS server starting on {:?}", path);
        let handler = self.handler.clone();
        std::thread::Builder::new()
            .name("bridge-server".into())
            .spawn(move || crate::server_unix::run_uds_server(path, handler))?;
        Ok(())
    }

    /// 其余平台占位实现
    #[cfg(not(any(windows, unix)))]
    pub fn start(&self) -> anyhow::Result<()> {
        warn!("Bridge server not supported on this platform");
        Ok(())
    }
}

/// 包装 HANDLE 使其可跨线程传递
#[cfg(windows)]
pub(crate) struct PipeHandle(pub(crate) windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
unsafe impl Send for PipeHandle {}
#[cfg(windows)]
unsafe impl Sync for PipeHandle {}

/// Windows Named Pipe 服务器主循环
#[cfg(windows)]
fn run_pipe_server(
    pipe_name: &str,
    handler: Arc<dyn MessageHandler>,
    timeout_ms: u64,
    host_render: Option<Arc<HostRenderManager>>,
) {
    use std::ffi::CString;
    use windows::Win32::Foundation::*;
    use windows::Win32::Storage::FileSystem::*;
    use windows::Win32::System::Pipes::*;

    // 连接身份计数器：从 1 起单调递增（0 保留为 host-render 广播 target）。
    let mut next_conn_id: u32 = 1;

    let pipe_name_c = match CString::new(pipe_name) {
        Ok(s) => s,
        Err(e) => {
            error!("Invalid pipe name: {}", e);
            return;
        }
    };

    // 解析 SDDL 安全描述符，允许 AppContainer/UWP 进程连接
    let sd = crate::security::create_pipe_security_attributes();

    // 构建 SECURITY_ATTRIBUTES（sd 保持存活直到函数结束）
    let sa = sd.as_ref().map(|s| {
        use windows::Win32::Security::SECURITY_ATTRIBUTES;
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: s.as_ptr() as *mut _,
            bInheritHandle: false.into(),
        }
    });

    loop {
        // 创建 Named Pipe 实例
        let pipe_handle = unsafe {
            CreateNamedPipeA(
                windows::core::PCSTR(pipe_name_c.as_ptr() as *const u8),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                65536, // out buffer
                65536, // in buffer
                0,     // default timeout
                sa.as_ref().map(|s| s as *const _),
            )
        };

        let pipe_handle = match pipe_handle {
            Ok(h) => h,
            Err(e) => {
                error!("CreateNamedPipe failed: {}", e);
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
        };

        // 等待客户端连接
        let connected = unsafe { ConnectNamedPipe(pipe_handle, None) };
        if connected.is_err() {
            // ERROR_PIPE_CONNECTED = 客户端已连接
            let err = windows::core::Error::from_win32();
            if err.code() != ERROR_PIPE_CONNECTED.into() {
                warn!("ConnectNamedPipe failed: {}", err);
                unsafe {
                    let _ = CloseHandle(pipe_handle);
                }
                continue;
            }
        }

        debug!("Client connected to bridge pipe");

        // 分配连接身份：conn_id 单调递增（回绕跳过 0），pid 取对端进程。
        let conn_id = next_conn_id;
        next_conn_id = next_conn_id.wrapping_add(1);
        if next_conn_id == 0 {
            next_conn_id = 1;
        }
        let mut pid: u32 = 0;
        if unsafe { GetNamedPipeClientProcessId(pipe_handle, &mut pid) }.is_err() {
            debug!("GetNamedPipeClientProcessId failed for conn_id={}", conn_id);
        }
        let ctx = ClientCtx { conn_id, pid };

        // 为每个连接启动独立线程
        let handler = handler.clone();
        let host_render = host_render.clone();
        let pipe = PipeHandle(pipe_handle);
        std::thread::Builder::new()
            .name("bridge-client".into())
            .spawn(move || {
                handle_client(pipe, handler, timeout_ms, ctx, host_render);
            })
            .ok();
    }
}

/// 处理单个客户端连接
#[cfg(windows)]
fn handle_client(
    pipe: PipeHandle,
    handler: Arc<dyn MessageHandler>,
    _timeout_ms: u64,
    ctx: ClientCtx,
    host_render: Option<Arc<HostRenderManager>>,
) {
    use wind_ipc::codec::*;
    use wind_ipc::protocol::*;
    use windows::Win32::Foundation::*;
    use windows::Win32::Storage::FileSystem::*;

    // 连接建立即通知：`ctx.pid` 是本次唯一能免费拿到、还没被任何消息处理耽搁的对端 pid。
    // 放在读循环之前——命名管道有内核缓冲（见上方 CreateNamedPipeA 的 64KB 收发缓冲区），
    // 对端此刻发消息不会因为我们还没开始 ReadFile 而丢失或阻塞；本调用只影响*本连接*
    // 读到*第一条*消息前那一小段时间（<1ms 级的 GetForegroundWindow / 按需 OpenProcess），
    // 每条连接仅此一次，不会摊到后续每次按键往返上。
    handler.handle_client_connected(ctx.pid);

    let pipe = pipe.0;
    let mut header_buf = [0u8; IpcHeader::SIZE];
    let mut payload_buf = vec![0u8; 65536];

    loop {
        // 读取 8 字节 header
        let mut bytes_read: u32 = 0;
        let read_ok = unsafe { ReadFile(pipe, Some(&mut header_buf), Some(&mut bytes_read), None) };

        if read_ok.is_err() {
            // 检查是否是 ERROR_MORE_DATA（消息模式下消息比缓冲区大时的正常情况）
            let last_err = unsafe { windows::Win32::Foundation::GetLastError() };
            if last_err == windows::Win32::Foundation::ERROR_MORE_DATA
                && bytes_read as usize == IpcHeader::SIZE
            {
                // 读到了完整的 header，继续处理（payload 会在后续读取）。
                // 这是消息模式下的预期行为，不记日志。
            } else {
                debug!(
                    "Client disconnected from bridge pipe (read failed, bytes_read={}, last_err={:?})",
                    bytes_read, last_err
                );
                break;
            }
        }

        if bytes_read as usize != IpcHeader::SIZE {
            debug!(
                "Client disconnected from bridge pipe (incomplete header: {} bytes)",
                bytes_read
            );
            break;
        }

        let header = match decode_header(&header_buf) {
            Ok(h) => h,
            Err(e) => {
                warn!("Invalid header: {}", e);
                break;
            }
        };

        let cmd = header.command;
        let len = header.length;
        trace!(
            "Received command: 0x{:04X}, payload: {} bytes, async: {}",
            cmd,
            len,
            header.is_async()
        );

        // 读取 payload（如果有）
        let payload_len = header.length as usize;
        let payload = if payload_len > 0 {
            if payload_len > payload_buf.len() {
                payload_buf.resize(payload_len, 0);
            }
            let mut bytes_read: u32 = 0;
            let read_ok = unsafe {
                ReadFile(
                    pipe,
                    Some(&mut payload_buf[..payload_len]),
                    Some(&mut bytes_read),
                    None,
                )
            };
            if read_ok.is_err() {
                let last_err = unsafe { windows::Win32::Foundation::GetLastError() };
                if last_err == windows::Win32::Foundation::ERROR_MORE_DATA {
                    // ERROR_MORE_DATA 表示消息比请求的字节数多，但已读到请求的字节数
                    trace!(
                        "ReadFile payload: ERROR_MORE_DATA but got {} bytes (requested {})",
                        bytes_read, payload_len
                    );
                } else {
                    warn!(
                        "Failed to read payload ({} bytes, read={}, err={:?})",
                        payload_len, bytes_read, last_err
                    );
                    break;
                }
            }
            if (bytes_read as usize) < payload_len {
                warn!(
                    "Incomplete payload: got {} of {} bytes",
                    bytes_read, payload_len
                );
                break;
            }
            &payload_buf[..payload_len]
        } else {
            &[]
        };

        // 分发命令到处理器
        let response = dispatch_command(
            &handler,
            header.command,
            header.is_async(),
            payload,
            ctx,
            host_render.as_ref(),
        );

        // 写入响应（异步命令返回 None，不写入）
        if let Some(resp) = response {
            trace!(
                "Sending response: {} bytes for cmd 0x{:04X}",
                resp.len(),
                cmd
            );
            let mut bytes_written: u32 = 0;
            let write_ok = unsafe { WriteFile(pipe, Some(&resp), Some(&mut bytes_written), None) };
            if write_ok.is_err() {
                warn!("Failed to write response for cmd 0x{:04X}", cmd);
                break;
            }
        }

        // FOCUS_GAINED 重型段延后到响应写出之后（对齐 Go runActivationHandlerAndPush）：
        // 同步段已回 ModePush 解除 DLL 阻塞，此处再 build_status + push 完整激活状态
        // （工具栏/热键/图标/active token），不占用 DLL 的同步等待窗口。
        if cmd == CMD_FOCUS_GAINED
            && let Ok(fg) = decode_focus_gained(payload)
        {
            let data = FocusData {
                x: fg.caret.x,
                y: fg.caret.y,
                height: fg.caret.height,
                composition_start_x: fg.caret.composition_start_x,
                composition_start_y: fg.caret.composition_start_y,
                client_token: fg.client_token,
                input_scope_mask: fg.input_scope_mask,
                disabled: fg.disabled != 0,
                reason: fg.reason,
                caret_source: fg.caret_source,
                // Windows DLL 不发 bundleID 段（那边由服务进程 OpenProcess 反查进程名），
                // 但为了让窗口类段能按同一条线性走法解析，它会发一个 bundleIdLen=0 占位。
                bundle_id: String::new(),
                window_class: wind_ipc::codec::decode_focus_gained_window_class(payload)
                    .to_string(),
            };
            handler.handle_focus_gained(&data);
        }

        if cmd == CMD_INPUT_STATE_REPORT
            && let Ok(r) = decode_input_state_report(payload)
        {
            handler.handle_input_state_report(r.pid, r.disabled != 0, r.reason, r.input_scope_mask);
        }

        // 诊断快照（异步、无响应）。DLL 仅在服务端推开采集后才发，故这里不做额外门控。
        if cmd == CMD_DIAG_SNAPSHOT
            && let Ok(snap) = decode_diag_snapshot(payload)
        {
            handler.handle_diag_snapshot(&snap);
        }
    }

    // 断线清理：连接彻底消失，移除其 host-render 实例状态并（若为可见 owner）隐藏其帧。
    // conn_id 单调不复用，故用当前 setup_seq 作 expected_seq（未 setup 时为 0 → 早退，无害）。
    if let Some(mgr) = host_render.as_ref() {
        let seq = mgr.setup_seq_of(ctx.conn_id);
        mgr.cleanup_client(ctx.conn_id, seq);
    }

    unsafe {
        let _ = windows::Win32::System::Pipes::DisconnectNamedPipe(pipe);
        let _ = CloseHandle(pipe);
    }
    debug!("Client disconnected from bridge pipe");
}

/// 分发命令到处理器，返回响应字节
///
/// 与 Go 版 `processRequest` 对齐：
/// - 同步命令返回 Some(response_bytes)
/// - 异步命令返回 None（不写响应）
/// - FOCUS_GAINED：同步命令，回 CMD_MODE_PUSH（权威 chinese/full）；重型 push 延后到
///   handle_client 写出响应之后（见该处）。IME_ACTIVATED 仍异步，状态由 handler 经 push pipe 回送。
pub(crate) fn dispatch_command(
    handler: &Arc<dyn MessageHandler>,
    command: u16,
    is_async: bool,
    payload: &[u8],
    ctx: ClientCtx,
    #[cfg(windows)] host_render: Option<&Arc<HostRenderManager>>,
) -> Option<Vec<u8>> {
    use wind_ipc::codec::*;
    use wind_ipc::protocol::*;

    // ctx 目前仅 Windows host-render 路径消费；非 Windows 读取两字段以免 dead_code 告警。
    #[cfg(not(windows))]
    let _ = (ctx.conn_id, ctx.pid);

    match command {
        // ── 按键事件（同步） ──
        CMD_KEY_EVENT => {
            // 键事件是「该实例正在输入」的最强焦点证据：SearchHost 等 locked/transient
            // DocMgr 宿主二次聚焦时 DLL 会跳过 focus_gained（OnSetFocus 见 TextService.cpp），
            // IME_ACTIVATED 也不重发——若不在此刷新 active，host-render 目标会停留在
            // 上一个前台进程，导致第二次在开始菜单输入时回退本地渲染（真机踩坑）。
            #[cfg(windows)]
            if let Some(mgr) = host_render {
                mgr.note_focus(ctx.conn_id, ctx.pid);
            }
            let key_payload = match decode_key_payload(payload) {
                Ok(p) => p,
                Err(e) => {
                    warn!("Invalid key payload: {}", e);
                    return Some(encode_pass_through());
                }
            };
            let data = KeyEventData::from(&key_payload);
            // policed 入口：非 app_inline 时把应用侧组合串替换为占位空格（避免与候选窗 preedit 重复）。
            let action = handler.handle_key_event_policed(&data);
            Some(encode_key_action(&action))
        }

        // ── 提交请求（同步，barrier 机制） ──
        CMD_COMMIT_REQUEST => match decode_commit_request(payload) {
            Ok(req) => {
                let data = CommitRequestData {
                    barrier_seq: req.barrier_seq,
                    trigger_key: req.trigger_key,
                    modifiers: req.modifiers,
                    input_buffer: req.input_buffer,
                };
                match handler.handle_commit_request(&data) {
                    Some(result) => Some(encode_commit_result(
                        result.barrier_seq,
                        &result.text,
                        if result.new_composition.is_empty() {
                            None
                        } else {
                            Some(&result.new_composition)
                        },
                        result.mode_changed,
                        result.chinese_mode,
                    )),
                    None => Some(encode_ack()),
                }
            }
            Err(e) => {
                warn!("Invalid commit request payload: {}", e);
                Some(encode_ack())
            }
        },

        // ── 焦点获取（同步命令，对齐 Go fix(focus) 0acf860b） ──
        // 两段式：本同步段只做纯内存轻量操作并**立即回 CMD_MODE_PUSH**（权威 chinese/full）：
        //   DLL 现为同步发送，在 OnSetFocus 内阻塞等本响应，首键前写好 _bChineseMode，
        //   根治"切到微信首键上屏英文"；并解除阻塞（旧实现返回 None → DLL 卡到超时，
        //   表现为切应用卡顿）。重型 handle_focus_gained（build_status + push 完整激活状态）
        //   延后到 handle_client 写出响应之后再跑，不在 DLL 阻塞路径上（见 handle_client）。
        CMD_FOCUS_GAINED => {
            // 记录最近焦点实例（host-render 写帧目标）。已 setup 才在 active_target 生效。
            #[cfg(windows)]
            if let Some(mgr) = host_render {
                mgr.note_focus(ctx.conn_id, ctx.pid);
            }
            if let Ok(fg) = decode_focus_gained(payload) {
                // 同步 caret（首键前必须就绪，纯字段写入，对齐 Go applyFocusGainedCaret）。
                // ⚠ 必须走 handle_focus_gained_caret 而**不是** handle_caret_update：
                // 后者带副作用（消费首显等待 → 立即显示候选），会让焦点事件那一刻的
                // 非权威坐标抢在 reflow 之前把候选窗显示出来，造成"先在旧位置再跳走"。
                handler.handle_focus_gained_caret(&CaretData {
                    x: fg.caret.x,
                    y: fg.caret.y,
                    height: fg.caret.height,
                    composition_start_x: fg.caret.composition_start_x,
                    composition_start_y: fg.caret.composition_start_y,
                    // 来源随焦点载荷尾部的第 39 字节到达（旧 DLL 落 UNKNOWN）。
                    // 「这一帧只落缓存、没有来源信息的消费者」已不再成立——`ui.status.show_on_focus`
                    // 让状态气泡直接锚在这组坐标上，来源是它唯一能据以判断可信度的东西。
                    source: fg.caret_source,
                    composition_rect: None,
                });
            }
            // 新 DLL 同步发送（is_async=false）：回传权威模式解除其阻塞并消除首键竞态。
            // 旧 DLL fire-and-forget（is_async=true）：不读响应，回了反而污染管道 → 返回 None。
            // 无论哪种，重型 handle_focus_gained 都在 handle_client 写出响应后统一触发。
            if is_async {
                None
            } else {
                let token = decode_focus_gained(payload)
                    .map(|fg| fg.client_token)
                    .unwrap_or(0);
                // 窗口类同样要给同步路径：按应用套用初始模式在这里也算一次，且**早于**
                // 重型段 handle_focus_gained。只给后者会让门控看起来生效、状态却已被改。
                let (chinese_mode, full_width) = handler.get_current_mode(
                    token,
                    wind_ipc::codec::decode_focus_gained_window_class(payload),
                );
                Some(encode_mode_push(chinese_mode, full_width))
            }
        }

        // ── 焦点丢失 ──
        // macOS IMKit `deactivateServer` 同步 send+readFrame 等 ack（sendEmpty）；不回则
        // readFrame 永久阻塞 → 切换/失焦卡死。Windows TSF fire-and-forget（is_async）不读，
        // 回了反而污染管道 → 仅 !is_async 回 ack（对齐 feat / 历史 fix 803f7fa）。
        CMD_FOCUS_LOST => {
            // 载荷自 v0.111.4 起携带 8 字节 clientToken，v0.111.5 起追加 1 字节 reason。
            // 旧 DLL 发 0 字节 → token=0（handler 保守放行）+ reason=Thread（旧语义），
            // 故新旧两侧可任意组合。归属校验见 MessageHandler::handle_focus_lost。
            handler.handle_focus_lost(
                decode_client_token(payload),
                decode_focus_lost_reason(payload),
            );
            if is_async { None } else { Some(encode_ack()) }
        }

        // ── IME 激活（异步） ──
        // Go 的两阶段模式：Phase1 更新 activeProcessID/activeToken + 回 Ack，
        // Phase2 调用 HandleIMEActivated 并推送 ActivationStatusPush。
        CMD_IME_ACTIVATED => {
            let token = decode_client_token(payload);
            // 记录最近焦点实例（host-render 写帧目标）。已 setup 才在 active_target 生效。
            #[cfg(windows)]
            if let Some(mgr) = host_render {
                mgr.note_focus(ctx.conn_id, ctx.pid);
            }
            // handler 内部完成 activation + push ActivationStatusPush
            handler.handle_ime_activated(token);
            None // 异步命令不返回响应
        }

        // ── IME 停用（异步） ──
        CMD_IME_DEACTIVATED => {
            handler.handle_ime_deactivated(decode_client_token(payload));
            None
        }

        // ── 模式通知（异步） ──
        CMD_MODE_NOTIFY => {
            let flags = if payload.len() >= 4 {
                u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]])
            } else {
                0
            };
            handler.handle_mode_notify(flags);
            None
        }

        // ── 模式切换（同步） ──
        // Go: 返回 CommitText（有待提交文本时）或 StatusUpdate（含完整状态）
        CMD_TOGGLE_MODE => {
            let (status, commit_text) = handler.handle_toggle_mode();
            if !commit_text.is_empty() {
                let chinese_mode = status.as_ref().is_some_and(|s| s.chinese_mode);
                Some(encode_commit_text(
                    &commit_text,
                    None,
                    true,
                    chinese_mode,
                    false,
                ))
            } else if let Some(status) = status {
                Some(encode_status_update_from_data(&status))
            } else {
                Some(encode_ack())
            }
        }

        // ── 系统模式切换（同步） ──
        // Go: 解析 flags 中的 StatusChineseMode 位（0x0001）
        CMD_SYSTEM_MODE_SWITCH => {
            let flags = if payload.len() >= 4 {
                u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]])
            } else {
                0
            };
            let chinese_mode = (flags & STATUS_CHINESE_MODE) != 0;
            let (status, commit_text) = handler.handle_system_mode_switch(chinese_mode);
            if !commit_text.is_empty() {
                Some(encode_commit_text(
                    &commit_text,
                    None,
                    true,
                    chinese_mode,
                    false,
                ))
            } else if let Some(status) = status {
                Some(encode_status_update_from_data(&status))
            } else {
                Some(encode_ack())
            }
        }

        // ── 菜单命令（同步） ──
        // Go: 返回 StatusUpdate（含完整状态）或 Ack
        CMD_MENU_COMMAND => {
            let command = std::str::from_utf8(payload).unwrap_or("");
            match handler.handle_menu_command(command) {
                Some(status) => Some(encode_status_update_from_data(&status)),
                None => Some(encode_ack()),
            }
        }

        // ── 组合终止（异步） ──
        CMD_COMPOSITION_TERMINATED => {
            handler.handle_composition_terminated();
            None
        }

        // ── Host Render 请求（同步，0x0501 == CMD_HOST_RENDER_SETUP） ──
        // Windows：向管理器注册本连接实例，命中白名单则回 HostRenderSetup（instanceId +
        // 三 kind 的 SHM/Event 名），否则（未命中白名单/未注入管理器）回 ACK（DLL 退回进程内渲染）。
        // 非 Windows：host-render 不可用，回 ACK。
        CMD_HOST_RENDER_REQUEST => {
            #[cfg(windows)]
            {
                if let Some(mgr) = host_render {
                    match mgr.setup(ctx.conn_id, ctx.pid) {
                        Ok((instance_id, entries)) => {
                            return Some(encode_host_render_setup(instance_id, &entries));
                        }
                        Err(e) => {
                            debug!(
                                "host_render setup 拒绝 conn_id={} pid={}: {}",
                                ctx.conn_id, ctx.pid, e
                            );
                            return Some(encode_ack());
                        }
                    }
                }
                Some(encode_ack())
            }
            #[cfg(not(windows))]
            {
                Some(encode_ack())
            }
        }

        // ── Host Render 失败上报（DLL 侧初始化/映射失败，异步通知） ──
        // 载荷首 4 字节为 reason（u32 LE）；交处理器记录（协调器打 WARN）。不回响应。
        CMD_HOST_RENDER_FAILED => {
            let reason = if payload.len() >= 4 {
                u32::from_le_bytes(payload[0..4].try_into().unwrap())
            } else {
                0
            };
            handler.handle_host_render_failed(reason);
            None
        }

        // ── 光标更新（异步） ──
        CMD_CARET_UPDATE => {
            if let Ok(caret) = wind_ipc::codec::decode_focus_gained(payload)
                .map(|fg| fg.caret)
                .or_else(|_| {
                    // CaretPayload 20 bytes
                    if payload.len() >= 20 {
                        Ok(
                            wind_ipc::protocol::CaretPayload::from_bytes(payload).unwrap_or(
                                wind_ipc::protocol::CaretPayload {
                                    x: 0,
                                    y: 0,
                                    height: 0,
                                    composition_start_x: 0,
                                    composition_start_y: 0,
                                },
                            ),
                        )
                    } else if payload.len() >= 12 {
                        // macOS .app 的 CmdCaretUpdate 默认只发 12 字节 (x,y,height i32 LE，
                        // 无 composition_start)。main 原解码器只认 20 字节 → 12 字节被拒、
                        // handle_caret_update 永不调用 → 候选窗 caret 恒 (0,0,0) 卡屏幕左上。
                        Ok(wind_ipc::protocol::CaretPayload {
                            x: i32::from_le_bytes(payload[0..4].try_into().unwrap()),
                            y: i32::from_le_bytes(payload[4..8].try_into().unwrap()),
                            height: i32::from_le_bytes(payload[8..12].try_into().unwrap()),
                            composition_start_x: 0,
                            composition_start_y: 0,
                        })
                    } else {
                        Err(wind_ipc::codec::CodecError::BufferTooShort {
                            need: 12,
                            got: payload.len(),
                        })
                    }
                })
            {
                // ⚠ 2026-09-05 探针阶段：组合矩形**只观察、不参与决策**，故不进 `CaretData`。
                // 目的是先用真机日志看清各宿主对**跨行** range 的 `GetTextExt` 究竟返回什么
                // （规范说返回包围矩形，实现可能只返回首行、可能 TS_E_NOLAYOUT）。看清之后
                // 再决定锚点公式，并连同消费代码一起把字段加进 `CaretData`。
                //
                // 判读要点：`h ≈ 单行行高` ⇒ 组合在一行内，`left` 就是组合起点；
                // `h ≈ n 倍行高` ⇒ 跨了 n 行，此时 `(left, bottom)` 才是最后一行的行首。
                let comp_rect = wind_ipc::protocol::CaretPayload::comp_rect_from_bytes(payload);
                if let Some((l, t, r, b)) = comp_rect {
                    // `CaretPayload` 是 packed 布局，格式化宏会对字段取引用 ⇒ 未对齐引用是 UB。
                    // 先按值拷进局部变量再格式化。
                    let (cx, cy, ch) = (caret.x, caret.y, caret.height);
                    let (csx, csy) = (caret.composition_start_x, caret.composition_start_y);
                    debug!(
                        "caret_update 组合矩形: ({l},{t},{r},{b}) w={} h={}（对照 caret=({cx},{cy}) h={ch} compStart=({csx},{csy})）",
                        r - l,
                        b - t
                    );
                }
                handler.handle_caret_update(&CaretData {
                    x: caret.x,
                    y: caret.y,
                    height: caret.height,
                    composition_start_x: caret.composition_start_x,
                    composition_start_y: caret.composition_start_y,
                    // v2 载荷（24 字节）才有；旧 DLL 20 字节、macOS 12 字节均落 UNKNOWN
                    source: wind_ipc::protocol::CaretPayload::source_from_bytes(payload),
                    // v3 载荷（40 字节）才有
                    composition_rect: comp_rect,
                });
            }
            // macOS IMKit sendCaretUpdateIfAvailable 同步 send+readFrame（注释「服务端一律返
            // ack」）；每键组字/模式切换都发，不回 ack → readFrame 永久阻塞 → 每键卡死、无法输入。
            // Windows TSF fire-and-forget（is_async）不读，回了污染管道 → 仅 !is_async 回 ack。
            if is_async { None } else { Some(encode_ack()) }
        }

        // ── 选区变化（异步） ──
        CMD_SELECTION_CHANGED => {
            let prev_char = if payload.len() >= 2 {
                u16::from_le_bytes([payload[0], payload[1]])
            } else {
                0
            };
            handler.handle_selection_changed(prev_char);
            None
        }

        // ── 光标待定（异步） ──
        CMD_CARET_PENDING => {
            handler.handle_caret_pending();
            None
        }

        // ── 首显试探采样（异步）──
        // 载荷同 CaretPayload；长度不足直接丢弃：半截坐标定位候选窗比不定位更糟。
        CMD_CARET_PROBE => {
            // 入口无条件记一条：本分支此前对「解码失败」是静默丢弃的，真机排查时
            // 「一条日志都没有」既可能是没收到、也可能是收到但解码失败，无从区分。
            debug!("CMD_CARET_PROBE 到达: payload={} 字节", payload.len());
            if let Some(p) = wind_ipc::protocol::CaretPayload::from_bytes(payload) {
                handler.handle_caret_probe(&CaretData {
                    x: p.x,
                    y: p.y,
                    height: p.height,
                    composition_start_x: p.composition_start_x,
                    composition_start_y: p.composition_start_y,
                    source: wind_ipc::protocol::CaretPayload::source_from_bytes(payload),
                    composition_rect: None,
                });
            } else {
                warn!(
                    "CMD_CARET_PROBE 载荷不足 {} 字节（收到 {}），丢弃",
                    wind_ipc::protocol::CaretPayload::SIZE,
                    payload.len()
                );
            }
            None
        }

        // ── 显示功能主菜单（任务栏输入法指示右键，同步）──
        CMD_SHOW_CONTEXT_MENU => {
            // 载荷若含 8 字节则为屏幕坐标 (i32 x, i32 y)，否则用哨兵让 UI 取光标位
            let (x, y) = if payload.len() >= 8 {
                (
                    i32::from_le_bytes(payload[0..4].try_into().unwrap()),
                    i32::from_le_bytes(payload[4..8].try_into().unwrap()),
                )
            } else {
                (i32::MIN, i32::MIN)
            };
            // macOS：`.app` 用此请求「查询菜单项」以构建原生 NSMenu，返回统一菜单树帧。
            // payload 为 1 字节 [1] → IMK 输入源菜单(精简树)；否则(空/坐标) → 完整树(候选右键)。
            // 其它平台：进程内弹出菜单窗口（Windows popup_menu），仅回 ack。
            #[cfg(target_os = "macos")]
            {
                let _ = (x, y);
                let simplified = payload.len() == 1 && payload[0] == 1;
                Some(handler.query_menu_encoded(simplified))
            }
            #[cfg(not(target_os = "macos"))]
            {
                handler.handle_show_context_menu(x, y);
                Some(encode_ack())
            }
        }

        // ── darwin .app 统一菜单项被选中（菜单 id i32 LE，上行）──
        CMD_MENU_ACTION => {
            if payload.len() >= 4 {
                let id = i32::from_le_bytes(payload[0..4].try_into().unwrap());
                handler.handle_menu_action_id(id);
            }
            Some(encode_ack())
        }

        // ── 上报前台上下文（appLen+app + titleLen+title + selLen+sel，上行）──
        // 供命令直通车 app()/title()/sel() 取值；聚焦时快照。目前仅 darwin `.app` 发。
        CMD_FRONT_CONTEXT => {
            let mut off = 0usize;
            let mut take = || -> Option<String> {
                if payload.len() < off + 4 {
                    return None;
                }
                let n = u32::from_le_bytes(payload[off..off + 4].try_into().unwrap()) as usize;
                off += 4;
                if payload.len() < off + n {
                    return None;
                }
                let s = String::from_utf8_lossy(&payload[off..off + n]).into_owned();
                off += n;
                Some(s)
            };
            if let (Some(app), Some(title), Some(sel)) = (take(), take(), take()) {
                handler.handle_front_context(&app, &title, &sel);
            }
            Some(encode_ack())
        }

        // ── darwin .app 候选右键上下文菜单动作（index i32 + actionLen u32 + action UTF-8，上行）──
        CMD_CANDIDATE_CONTEXT_MENU => {
            if payload.len() >= 8 {
                let index = i32::from_le_bytes(payload[0..4].try_into().unwrap());
                let action_len = u32::from_le_bytes(payload[4..8].try_into().unwrap()) as usize;
                if payload.len() >= 8 + action_len {
                    let action = std::str::from_utf8(&payload[8..8 + action_len]).unwrap_or("");
                    handler.handle_candidate_context_menu(index, action);
                }
            }
            Some(encode_ack())
        }

        // ── 鼠标点选候选（页内下标 i32 LE，上行；darwin .app 同步 / Windows host DLL SendAsync）──
        // 0x020D 双用途：下行 CMD_MODE_PUSH（仅编码），上行 CMD_CANDIDATE_SELECT（仅 dispatch）。
        // 负值为翻页按钮（-1 上页 / -2 下页），对齐 Go handleHostCandidateSelect 的 int32 解码——
        // 按 u32 解码会把翻页点击变成巨大下标被 coordinator 丢弃（真机踩坑：翻页点击无效）。
        // Windows DLL fire-and-forget（is_async）不读响应，回 ack 会污染管道 → 仅 !is_async 回 ack。
        CMD_CANDIDATE_SELECT => {
            if payload.len() >= 4 {
                let idx = i32::from_le_bytes(payload[0..4].try_into().unwrap());
                handler.handle_candidate_select(idx);
            }
            if is_async { None } else { Some(encode_ack()) }
        }

        // ── 鼠标 hover 候选（页内下标 i32 LE，-1=无，上行）──
        // Windows host DLL 载荷另带 anchorX/belowY/aboveY（tooltip 锚点），当前仅取 index。
        CMD_CANDIDATE_HOVER => {
            if payload.len() >= 4 {
                let idx = i32::from_le_bytes(payload[0..4].try_into().unwrap());
                handler.handle_candidate_hover(idx);
            }
            if is_async { None } else { Some(encode_ack()) }
        }

        // ── host 候选框鼠标滚轮（delta i32，WHEEL_DELTA 倍数，Windows DLL SendAsync）──
        CMD_CANDIDATE_SCROLL => {
            if payload.len() >= 4 {
                let delta = i32::from_le_bytes(payload[0..4].try_into().unwrap());
                handler.handle_candidate_scroll(delta);
            }
            if is_async { None } else { Some(encode_ack()) }
        }

        // ── 扩展信封（低频消息的统一入口，见 protocol.rs 的 CMD_EXT）──
        // 解不出 / 未知 kind 一律安静忽略：这正是新旧版本互相兼容的根本，绝不能升级成错误。
        CMD_EXT => {
            match decode_ext(payload) {
                Some((kind, body)) => handler.handle_ext(kind, body),
                None => warn!("CMD_EXT 载荷无法解析（长度 {}），忽略", payload.len()),
            }
            if is_async { None } else { Some(encode_ack()) }
        }

        // ── 批处理事件 ──
        CMD_BATCH_EVENTS => {
            #[cfg(windows)]
            {
                handle_batch_events(handler, payload, ctx, host_render)
            }
            #[cfg(not(windows))]
            {
                handle_batch_events(handler, payload, ctx)
            }
        }

        // ── 输入统计（异步，TSF 侧英文模式上报）──
        // InputStatsPayload: englishChars(4) + englishDigits(4) + englishPuncts(4)
        //                  + englishSpaces(4) + elapsedMs(4) = 20 字节
        CMD_INPUT_STATS => {
            if payload.len() >= 20 {
                let chars = u32::from_le_bytes(payload[0..4].try_into().unwrap_or([0; 4]));
                let digits = u32::from_le_bytes(payload[4..8].try_into().unwrap_or([0; 4]));
                let puncts = u32::from_le_bytes(payload[8..12].try_into().unwrap_or([0; 4]));
                let spaces = u32::from_le_bytes(payload[12..16].try_into().unwrap_or([0; 4]));
                // payload[16..20] = elapsedMs，暂不传入（活跃时间由 StatCollector 自估）
                handler.handle_english_stats(chars, digits, puncts, spaces);
            }
            None
        }

        _ => {
            warn!("Unknown command: 0x{:04X}", command);
            if is_async {
                None
            } else {
                Some(encode_pass_through())
            }
        }
    }
}

/// 处理批处理事件
fn handle_batch_events(
    handler: &Arc<dyn MessageHandler>,
    payload: &[u8],
    ctx: ClientCtx,
    #[cfg(windows)] host_render: Option<&Arc<HostRenderManager>>,
) -> Option<Vec<u8>> {
    use wind_ipc::codec::*;
    use wind_ipc::protocol::*;

    if payload.len() < 4 {
        return Some(encode_ack());
    }

    let event_count = u16::from_le_bytes([payload[0], payload[1]]) as usize;
    let mut offset = 4; // skip BatchHeader (eventCount:u16 + reserved:u16)
    let mut responses = Vec::with_capacity(event_count);

    for _ in 0..event_count {
        if offset + IpcHeader::SIZE > payload.len() {
            break;
        }
        let sub_header = match decode_header(&payload[offset..]) {
            Ok(h) => h,
            Err(_) => break,
        };
        offset += IpcHeader::SIZE;

        let sub_payload_len = sub_header.length as usize;
        if offset + sub_payload_len > payload.len() {
            break;
        }
        let sub_payload = &payload[offset..offset + sub_payload_len];
        offset += sub_payload_len;

        // 分发子命令（cfg-split 转发 host_render）；异步命令仍分发（如 caret update）
        // 以产生副作用，仅同步命令收集响应（对齐 Go）。
        let sub_resp = {
            #[cfg(windows)]
            {
                dispatch_command(
                    handler,
                    sub_header.command,
                    sub_header.is_async(),
                    sub_payload,
                    ctx,
                    host_render,
                )
            }
            #[cfg(not(windows))]
            {
                dispatch_command(
                    handler,
                    sub_header.command,
                    sub_header.is_async(),
                    sub_payload,
                    ctx,
                )
            }
        };
        if !sub_header.is_async()
            && let Some(resp) = sub_resp
        {
            responses.push(resp);
        }
    }

    Some(encode_batch_response(&responses))
}

/// 从裸 8 字节载荷解出 clientToken；载荷不足 8 字节返回 0。
///
/// 用于 CMD_IME_ACTIVATED / CMD_FOCUS_LOST / CMD_IME_DEACTIVATED 这类「整个载荷就是一个
/// token」的命令。返回 0 有两种成因，调用方一律按「未知客户端」保守处理：旧 DLL 不带
/// token，或载荷被截断。
fn decode_client_token(payload: &[u8]) -> u64 {
    if payload.len() >= 8 {
        u64::from_le_bytes(payload[..8].try_into().unwrap())
    } else {
        0
    }
}

/// 从 CMD_FOCUS_LOST 载荷解出 reason（第 9 字节）。
///
/// 载荷不足 9 字节（旧 DLL 只发 token 甚至空载荷）时返回 `Thread`——那是旧行为的隐含
/// 语义，也是后果最完整的一种，误判方向安全。
fn decode_focus_lost_reason(payload: &[u8]) -> wind_ipc::protocol::FocusLostReason {
    wind_ipc::protocol::FocusLostReason::from_u8(payload.get(8).copied().unwrap_or(0))
}

/// 将 KeyAction 编码为响应字节
///
/// 与 Go 版 handleKeyEvent 的 switch 分支对齐
fn encode_key_action(action: &KeyAction) -> Vec<u8> {
    use wind_ipc::codec::*;

    match action {
        KeyAction::InsertText {
            text,
            new_composition,
            mode_changed,
            chinese_mode,
            has_new_composition,
        } => encode_commit_text(
            text,
            new_composition.as_deref(),
            *mode_changed,
            *chinese_mode,
            *has_new_composition,
        ),
        KeyAction::UpdateComposition { text, caret_pos } => {
            encode_update_composition(text, *caret_pos)
        }
        KeyAction::ClearComposition => encode_clear_composition(),
        KeyAction::ClearCompositionThenPassThrough => encode_clear_then_pass_through(),
        KeyAction::PassThrough | KeyAction::NotHandled => encode_pass_through(),
        KeyAction::StatusUpdate(status) => encode_status_update_from_data(status),
        KeyAction::Consumed => encode_consumed(),
        KeyAction::InsertTextWithCursor {
            text,
            cursor_offset,
        } => encode_commit_text_with_cursor(text, *cursor_offset),
        KeyAction::MoveCursorRight { count } => encode_move_cursor(*count),
        KeyAction::DeletePair => encode_delete_pair(),
        KeyAction::ReplaceBackward { count, text } => encode_replace_backward(*count, text),
        KeyAction::HoldComposition { text, timeout_ms } => {
            encode_hold_composition(*timeout_ms, text)
        }
        KeyAction::CommitReplacingHeld { text, chinese_mode } => {
            encode_commit_text_replacing_held(text, *chinese_mode)
        }
        KeyAction::CommitAndHoldComposition {
            commit_text,
            hold_text,
            timeout_ms,
        } => encode_commit_and_hold(*timeout_ms, commit_text, hold_text),
        KeyAction::CommitThenDeferComposition {
            commit_text,
            deferred_composition,
            timeout_ms,
        } => encode_commit_then_defer(*timeout_ms, commit_text, deferred_composition),
    }
}

/// 从 StatusUpdateData 编码 StatusUpdate 响应
fn encode_status_update_from_data(status: &StatusUpdateData) -> Vec<u8> {
    use wind_ipc::codec::*;
    encode_status_update(
        status.chinese_mode,
        status.full_width,
        status.chinese_punct,
        status.toolbar_visible,
        status.caps_lock,
        false, // host_render_avail: 此响应路径无法访问 HostRenderManager；C++ 在 activation push 已获得真值
        status.soft_keyboard,
        status.soft_keyboard_keys,
        &status.key_down_hotkeys,
        &status.key_up_hotkeys,
        &status.icon_label,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use wind_ipc::protocol::{CMD_ACK, CMD_CANDIDATE_SELECT, CMD_HOST_RENDER_FAILED, IpcHeader};
    // host-render 与按键投递是 Windows 专属通路，相应用例也只在 Windows 编译。
    #[cfg(windows)]
    use wind_ipc::protocol::{
        CMD_CANDIDATE_SCROLL, CMD_HOST_RENDER_REQUEST, CMD_HOST_RENDER_SETUP, CMD_KEY_EVENT,
    };

    /// 最小测试 handler：记录 host_render_failed 的 reason 与鼠标 select/scroll 值，
    /// 其余方法返回安全默认值。
    #[derive(Default)]
    struct RecordingHandler {
        last_failed_reason: AtomicU32,
        last_select: std::sync::atomic::AtomicI32,
        last_scroll: std::sync::atomic::AtomicI32,
    }

    impl MessageHandler for RecordingHandler {
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
        fn handle_host_render_failed(&self, reason: u32) {
            self.last_failed_reason.store(reason, Ordering::SeqCst);
        }
        fn handle_candidate_select(&self, page_local_index: i32) {
            self.last_select.store(page_local_index, Ordering::SeqCst);
        }
        fn handle_candidate_scroll(&self, delta: i32) {
            self.last_scroll.store(delta, Ordering::SeqCst);
        }
    }

    /// dispatch_command 的跨平台调用包装（Windows 多一个 host_render 参数）。
    fn dispatch_for_test(
        handler: &Arc<dyn MessageHandler>,
        cmd: u16,
        is_async: bool,
        payload: &[u8],
        ctx: ClientCtx,
    ) -> Option<Vec<u8>> {
        #[cfg(windows)]
        {
            dispatch_command(handler, cmd, is_async, payload, ctx, None)
        }
        #[cfg(not(windows))]
        {
            dispatch_command(handler, cmd, is_async, payload, ctx)
        }
    }

    /// host 模式翻页按钮点击：DLL SendAsync 发负 index（-1 上页 / -2 下页），
    /// 必须按 i32 路由到 handler（u32 解码会丢弃翻页，真机踩坑），且异步不回响应
    /// （回 ack 会污染管道，DLL 不读响应）。
    #[test]
    fn candidate_select_negative_pager_routed_async_silent() {
        let handler = Arc::new(RecordingHandler::default());
        let dyn_handler: Arc<dyn MessageHandler> = handler.clone();
        let ctx = ClientCtx { conn_id: 0, pid: 0 };

        let resp = dispatch_for_test(
            &dyn_handler,
            CMD_CANDIDATE_SELECT,
            true,
            &(-1i32).to_le_bytes(),
            ctx,
        );
        assert!(resp.is_none(), "异步 select 不得回响应（防管道污染）");
        assert_eq!(
            handler.last_select.load(Ordering::SeqCst),
            -1,
            "上页按钮应路由 -1"
        );

        let resp = dispatch_for_test(
            &dyn_handler,
            CMD_CANDIDATE_SELECT,
            true,
            &(-2i32).to_le_bytes(),
            ctx,
        );
        assert!(resp.is_none());
        assert_eq!(
            handler.last_select.load(Ordering::SeqCst),
            -2,
            "下页按钮应路由 -2"
        );
    }

    /// darwin 同步点选（非负下标）行为不变：路由 + 回 ACK。
    #[test]
    fn candidate_select_sync_nonnegative_returns_ack() {
        let handler = Arc::new(RecordingHandler::default());
        let dyn_handler: Arc<dyn MessageHandler> = handler.clone();
        let ctx = ClientCtx { conn_id: 0, pid: 0 };

        let resp = dispatch_for_test(
            &dyn_handler,
            CMD_CANDIDATE_SELECT,
            false,
            &3i32.to_le_bytes(),
            ctx,
        )
        .expect("同步 select 应回响应");
        let hdr_arr: [u8; IpcHeader::SIZE] = resp[..IpcHeader::SIZE].try_into().unwrap();
        let cmd = IpcHeader::from_bytes(&hdr_arr).command;
        assert_eq!(cmd, CMD_ACK);
        assert_eq!(handler.last_select.load(Ordering::SeqCst), 3);
    }

    /// host 候选框滚轮（DLL SendAsync）应路由 delta 且不回响应。
    /// Windows-only：0x0211 在非 Windows 是 FRONT_CONTEXT 臂（平台双语义）。
    #[cfg(windows)]
    #[test]
    fn candidate_scroll_routed_async_silent() {
        let handler = Arc::new(RecordingHandler::default());
        let dyn_handler: Arc<dyn MessageHandler> = handler.clone();
        let ctx = ClientCtx { conn_id: 0, pid: 0 };

        let resp = dispatch_for_test(
            &dyn_handler,
            CMD_CANDIDATE_SCROLL,
            true,
            &(-120i32).to_le_bytes(),
            ctx,
        );
        assert!(resp.is_none(), "异步 scroll 不得回响应");
        assert_eq!(handler.last_scroll.load(Ordering::SeqCst), -120);
    }

    /// 键事件必须刷新 host-render 活跃实例：SearchHost 等 transient DocMgr 宿主二次聚焦时
    /// focus_gained 被 DLL 跳过、IME_ACTIVATED 不重发，键事件是唯一可靠的焦点信号
    /// （否则 active 停留在别的进程 → 二次输入回退本地渲染，真机踩坑）。
    #[cfg(windows)]
    #[test]
    fn key_event_notes_focus_for_host_render() {
        let pid = std::process::id();
        let suffix = format!("_srv_keyfocus_{}", pid);
        let mgr = HostRenderManager::new(&suffix, vec!["*".to_string()]);
        let handler: Arc<dyn MessageHandler> = Arc::new(RecordingHandler::default());
        let ctx = ClientCtx { conn_id: 7, pid };

        // 空 payload 解码失败也应先记焦点（note_focus 在解码之前）
        let _ = dispatch_command(&handler, CMD_KEY_EVENT, false, &[], ctx, Some(&mgr));

        // setup 后 active_target 应指向键事件来源的 conn 7
        mgr.setup(7, pid).expect("setup 应成功");
        let target = mgr.active_target().expect("键事件应已刷新 active");
        assert_eq!(target.instance_id, 7, "active 实例应为键事件来源连接");
    }

    /// 命中白名单（`*`）的连接请求应回 HostRenderSetup（含 instanceId + 三 kind 条目）。
    #[cfg(windows)]
    #[test]
    fn host_render_request_returns_setup_payload() {
        let pid = std::process::id();
        let suffix = format!("_srv_setup_{}", pid);
        let mgr = HostRenderManager::new(&suffix, vec!["*".to_string()]);
        let handler: Arc<dyn MessageHandler> = Arc::new(RecordingHandler::default());
        let ctx = ClientCtx { conn_id: 1, pid };

        let resp = dispatch_command(
            &handler,
            CMD_HOST_RENDER_REQUEST,
            false,
            &[],
            ctx,
            Some(&mgr),
        )
        .expect("同步命令应有响应");

        let hdr_arr: [u8; IpcHeader::SIZE] = resp[..IpcHeader::SIZE].try_into().unwrap();
        let header = IpcHeader::from_bytes(&hdr_arr);
        let cmd = header.command;
        assert_eq!(cmd, CMD_HOST_RENDER_SETUP, "应回 HostRenderSetup 帧");
        let p = &resp[IpcHeader::SIZE..];
        assert_eq!(
            u32::from_le_bytes(p[0..4].try_into().unwrap()),
            1,
            "instanceId 应等于 conn_id"
        );
        assert_eq!(
            u32::from_le_bytes(p[4..8].try_into().unwrap()),
            3,
            "应含三 kind 条目"
        );
    }

    /// 未命中白名单的连接请求应回 ACK（DLL 退回进程内渲染）。
    #[cfg(windows)]
    #[test]
    fn host_render_request_not_whitelisted_returns_ack() {
        let pid = std::process::id();
        let suffix = format!("_srv_reject_{}", pid);
        // 白名单仅含 notepad.exe，当前测试进程不匹配。
        let mgr = HostRenderManager::new(&suffix, vec!["notepad.exe".to_string()]);
        let handler: Arc<dyn MessageHandler> = Arc::new(RecordingHandler::default());
        let ctx = ClientCtx { conn_id: 1, pid };

        let resp = dispatch_command(
            &handler,
            CMD_HOST_RENDER_REQUEST,
            false,
            &[],
            ctx,
            Some(&mgr),
        )
        .expect("同步命令应有响应");

        let hdr_arr: [u8; IpcHeader::SIZE] = resp[..IpcHeader::SIZE].try_into().unwrap();
        let header = IpcHeader::from_bytes(&hdr_arr);
        let cmd = header.command;
        assert_eq!(cmd, CMD_ACK, "未命中白名单应回 ACK");
    }

    /// HOST_RENDER_FAILED 应把 reason 路由到 handler（跨平台，不涉及管理器）。
    #[test]
    fn host_render_failed_routes_reason_to_handler() {
        let handler = Arc::new(RecordingHandler::default());
        let dyn_handler: Arc<dyn MessageHandler> = handler.clone();
        let ctx = ClientCtx { conn_id: 0, pid: 0 };
        let payload = 42u32.to_le_bytes();

        let resp = {
            #[cfg(windows)]
            {
                dispatch_command(
                    &dyn_handler,
                    CMD_HOST_RENDER_FAILED,
                    true,
                    &payload,
                    ctx,
                    None,
                )
            }
            #[cfg(not(windows))]
            {
                dispatch_command(&dyn_handler, CMD_HOST_RENDER_FAILED, true, &payload, ctx)
            }
        };
        assert!(resp.is_none(), "失败通知为异步命令，不应有响应");
        assert_eq!(
            handler.last_failed_reason.load(Ordering::SeqCst),
            42,
            "reason 应路由到 handler"
        );
    }
}
