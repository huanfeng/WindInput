use super::*;

/// ASSOC_SESSION 标志（0x20）只加在不带新组合的 InsertText 上，其它一律不加。
#[test]
fn assoc_session_flag_only_on_plain_commit() {
    let flags = |b: &[u8]| {
        u32::from_le_bytes(
            b[wind_ipc::protocol::IpcHeader::SIZE..][..4]
                .try_into()
                .unwrap(),
        )
    };
    let plain = KeyAction::InsertText {
        text: "好".into(),
        new_composition: None,
        mode_changed: false,
        chinese_mode: true,
        has_new_composition: false,
    };
    let with_comp = KeyAction::InsertText {
        text: "好".into(),
        new_composition: Some(" ".into()),
        mode_changed: false,
        chinese_mode: true,
        has_new_composition: true,
    };
    assert_eq!(flags(&encode_key_action(&plain, true)) & 0x20, 0x20);
    assert_eq!(flags(&encode_key_action(&plain, false)) & 0x20, 0);
    assert_eq!(
        flags(&encode_key_action(&with_comp, true)) & 0x20,
        0,
        "带新组合的上屏不置位"
    );
    assert_ne!(
        encode_key_action(&KeyAction::Consumed, true),
        Vec::<u8>::new()
    );
}

use std::sync::atomic::{AtomicU32, Ordering};
use wind_ipc::codec::decode_uielement_page;
use wind_ipc::protocol::{
    CMD_ACK, CMD_CANDIDATE_SELECT, CMD_HOST_RENDER_FAILED, CMD_KEY_OBSERVED, CMD_UIELEMENT_ACTION,
    CMD_UIELEMENT_PAGE, CMD_UIELEMENT_QUERY, CMD_UIELEMENT_STATE, IpcHeader, KeyObservedPayload,
    MOD_SHIFT, UIELEMENT_ACTION_SET_SELECTION, UIELEMENT_FLAG_UI_LESS_THREAD,
    UiElementActionPayload, UiElementStatePayload,
};
// host-render 与按键投递是 Windows 专属通路，相应用例也只在 Windows 编译。
#[cfg(windows)]
use wind_ipc::protocol::{
    CMD_CANDIDATE_SCROLL, CMD_HOST_RENDER_REQUEST, CMD_HOST_RENDER_SETUP, CMD_KEY_EVENT,
};

/// 面板指针事件 (kind, event, button, x, y)。
type PanelPointer = (u32, u32, u32, i32, i32);

/// 最小测试 handler：记录 host_render_failed 的 reason 与鼠标 select/scroll 值，
/// 其余方法返回安全默认值。
#[derive(Default)]
struct RecordingHandler {
    last_failed_reason: AtomicU32,
    last_select: std::sync::atomic::AtomicI32,
    last_scroll: std::sync::atomic::AtomicI32,
    last_uielement: std::sync::Mutex<Vec<String>>,
    last_panel_pointer: std::sync::Mutex<Vec<PanelPointer>>,
    /// 视为后台客户端的 pid（0 = 无）。
    background_pid: AtomicU32,
    activation_calls: std::sync::Mutex<Vec<String>>,
}

impl MessageHandler for RecordingHandler {
    fn handle_key_event(&self, _d: &KeyEventData) -> KeyAction {
        KeyAction::PassThrough
    }
    fn handle_focus_gained(&self, _data: &FocusData) -> Option<StatusUpdateData> {
        None
    }
    fn handle_focus_lost(&self, _client_token: u64, _reason: FocusLostReason) {}
    fn handle_ime_activated(&self, client_token: u64) -> Option<StatusUpdateData> {
        self.activation_calls
            .lock()
            .unwrap()
            .push(format!("fg:{client_token:#x}"));
        None
    }
    fn is_background_client(&self, pid: u32) -> bool {
        pid != 0 && pid == self.background_pid.load(Ordering::SeqCst)
    }
    fn handle_background_ime_activated(&self, client_token: u64) {
        self.activation_calls
            .lock()
            .unwrap()
            .push(format!("bg:{client_token:#x}"));
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
    fn handle_host_render_failed(&self, reason: u32) {
        self.last_failed_reason.store(reason, Ordering::SeqCst);
    }
    fn handle_candidate_select(&self, page_local_index: i32) {
        self.last_select.store(page_local_index, Ordering::SeqCst);
    }
    fn handle_candidate_scroll(&self, delta: i32) {
        self.last_scroll.store(delta, Ordering::SeqCst);
    }
    fn handle_uielement_state(&self, pid: u32, host_draws: bool, host_reads: bool) {
        self.last_uielement
            .lock()
            .unwrap()
            .push(format!("state:{pid}:{host_draws}:{host_reads}"));
    }
    fn on_panel_pointer(&self, kind: u32, event: u32, button: u32, x: i32, y: i32) {
        self.last_panel_pointer
            .lock()
            .unwrap()
            .push((kind, event, button, x, y));
    }
    fn uielement_page(&self) -> UiElementPage {
        UiElementPage {
            items: vec!["你好".into(), "拟好".into()],
            selected: 1,
            page_size: 9,
            current_page: 0,
        }
    }
    fn handle_uielement_action(&self, action: u32, arg: u32) {
        self.last_uielement
            .lock()
            .unwrap()
            .push(format!("action:{action}:{arg}"));
    }
    fn handle_key_observed(&self, key_code: u32, modifiers: u32) {
        self.last_uielement
            .lock()
            .unwrap()
            .push(format!("observed:{key_code}:{modifiers}"));
    }
}

/// 旁路按键是分发表里的已知命令：交给处理器、不回响应（漏登记会每键刷一条 Unknown command）。
#[test]
fn key_observed_dispatches_without_response() {
    let handler = Arc::new(RecordingHandler::default());
    let dyn_handler: Arc<dyn MessageHandler> = handler.clone();
    let ctx = ClientCtx { conn_id: 0, pid: 0 };
    let p = KeyObservedPayload {
        key_code: 0x08,
        modifiers: MOD_SHIFT,
    };
    assert!(dispatch_for_test(&dyn_handler, CMD_KEY_OBSERVED, true, &p.to_bytes(), ctx).is_none());
    assert_eq!(
        *handler.last_uielement.lock().unwrap(),
        vec![format!("observed:8:{MOD_SHIFT}")]
    );
}

/// UIElement 三件套的分发：状态/操作异步不回响应（防管道污染），查询同步回快照帧。
#[test]
fn uielement_commands_dispatch() {
    let handler = Arc::new(RecordingHandler::default());
    let dyn_handler: Arc<dyn MessageHandler> = handler.clone();
    let ctx = ClientCtx { conn_id: 0, pid: 0 };

    let st = UiElementStatePayload {
        pid: 99,
        flags: UIELEMENT_FLAG_UI_LESS_THREAD,
    };
    assert!(
        dispatch_for_test(&dyn_handler, CMD_UIELEMENT_STATE, true, &st.to_bytes(), ctx).is_none()
    );
    let act = UiElementActionPayload {
        action: UIELEMENT_ACTION_SET_SELECTION,
        arg: 3,
    };
    assert!(
        dispatch_for_test(
            &dyn_handler,
            CMD_UIELEMENT_ACTION,
            true,
            &act.to_bytes(),
            ctx
        )
        .is_none()
    );
    // 截断载荷：静默丢弃、不 panic、不触达 handler。
    assert!(dispatch_for_test(&dyn_handler, CMD_UIELEMENT_STATE, true, &[1, 2], ctx).is_none());
    assert_eq!(
        *handler.last_uielement.lock().unwrap(),
        vec!["state:99:true:false".to_string(), "action:1:3".to_string()]
    );

    // 只置读取位：声明位必须仍为 false——合并成一位就没法只关掉推断那一半。
    let reads_only = UiElementStatePayload {
        pid: 77,
        flags: wind_ipc::protocol::UIELEMENT_FLAG_HOST_READS,
    };
    assert!(
        dispatch_for_test(
            &dyn_handler,
            CMD_UIELEMENT_STATE,
            true,
            &reads_only.to_bytes(),
            ctx
        )
        .is_none()
    );
    assert_eq!(
        handler.last_uielement.lock().unwrap().last().unwrap(),
        "state:77:false:true",
        "HOST_READS 不得被当成 host_draws——那样 compat 覆盖就失去了作用对象"
    );

    let frame = dispatch_for_test(&dyn_handler, CMD_UIELEMENT_QUERY, false, &[], ctx)
        .expect("同步查询必须回响应");
    assert_eq!(u16::from_le_bytes([frame[2], frame[3]]), CMD_UIELEMENT_PAGE);
    let page = decode_uielement_page(&frame[8..]).unwrap();
    assert_eq!(page.items, vec!["你好".to_string(), "拟好".to_string()]);
    assert_eq!(
        (page.selected, page.page_size, page.current_page),
        (1, 9, 0)
    );
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

/// 工具栏 / 软键盘的指针事件按载荷字段原样路由；同步回 ack，异步不回。
#[test]
fn panel_pointer_dispatches_to_handler() {
    use wind_ipc::codec::encode_panel_pointer;
    use wind_ipc::protocol::CMD_PANEL_POINTER;
    let handler = Arc::new(RecordingHandler::default());
    let dyn_handler: Arc<dyn MessageHandler> = handler.clone();
    let ctx = ClientCtx { conn_id: 0, pid: 0 };
    let payload = encode_panel_pointer(11, 3, 1, -5, 40);

    let resp = dispatch_for_test(&dyn_handler, CMD_PANEL_POINTER, false, &payload, ctx)
        .expect("同步请求必须回 ack");
    assert_eq!(u16::from_le_bytes([resp[2], resp[3]]), CMD_ACK);
    assert!(dispatch_for_test(&dyn_handler, CMD_PANEL_POINTER, true, &payload, ctx).is_none());
    // 载荷过短：不路由，但同步仍回 ack。
    assert!(
        dispatch_for_test(&dyn_handler, CMD_PANEL_POINTER, false, &payload[..19], ctx).is_some()
    );
    assert_eq!(
        *handler.last_panel_pointer.lock().unwrap(),
        vec![(11, 3, 1, -5, 40); 2]
    );
}

/// host 模式翻页按钮点击：DLL SendAsync 发负 index（-1 上页 / -2 下页），
/// 必须按 i32 路由到 handler（u32 解码会丢弃翻页），且异步不回响应
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
/// （否则 active 停留在别的进程，二次输入回退本地渲染）。
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
    mgr.setup(7, pid, 0).expect("setup 应成功");
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

/// 请求载荷里的能力位记到该实例上：带穿透层能力的 DLL 才拿到拆层帧，空载荷（旧 DLL）不拆。
#[cfg(windows)]
#[test]
fn host_render_request_records_capabilities() {
    let pid = std::process::id();
    let mgr = HostRenderManager::new(&format!("_srv_caps_{pid}"), vec!["*".to_string()]);
    let handler: Arc<dyn MessageHandler> = Arc::new(RecordingHandler::default());
    let caps = wind_ipc::protocol::HOST_RENDER_CAP_PASS_LAYER.to_le_bytes();
    for (conn_id, payload) in [(1u32, &[][..]), (2, &caps[..])] {
        let ctx = ClientCtx { conn_id, pid };
        dispatch_command(
            &handler,
            CMD_HOST_RENDER_REQUEST,
            false,
            payload,
            ctx,
            Some(&mgr),
        )
        .expect("同步命令应有响应");
    }
    mgr.note_focus(1, pid);
    assert!(!mgr.active_target().unwrap().pass_layer, "空载荷 = 旧 DLL");
    mgr.note_focus(2, pid);
    assert!(mgr.active_target().unwrap().pass_layer);
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

/// 后台客户端的 IME_ACTIVATED 走 background 分支，不进常规激活（不抢活动客户端）。
#[test]
fn ime_activated_from_background_client_does_not_take_over() {
    let handler = Arc::new(RecordingHandler::default());
    handler.background_pid.store(27168, Ordering::SeqCst);
    let dyn_handler: Arc<dyn MessageHandler> = handler.clone();
    let token: u64 = (27168u64 << 32) | 1;
    let payload = token.to_le_bytes();
    let send = |pid: u32| {
        let ctx = ClientCtx { conn_id: 9, pid };
        #[cfg(windows)]
        {
            dispatch_command(
                &dyn_handler,
                wind_ipc::protocol::CMD_IME_ACTIVATED,
                true,
                &payload,
                ctx,
                None,
            )
        }
        #[cfg(not(windows))]
        {
            dispatch_command(
                &dyn_handler,
                wind_ipc::protocol::CMD_IME_ACTIVATED,
                true,
                &payload,
                ctx,
            )
        }
    };
    assert!(send(27168).is_none());
    assert!(send(8092).is_none());
    assert_eq!(
        *handler.activation_calls.lock().unwrap(),
        vec![format!("bg:{token:#x}"), format!("fg:{token:#x}")],
        "后台 pid 只回推，前台 pid 走常规激活"
    );
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
