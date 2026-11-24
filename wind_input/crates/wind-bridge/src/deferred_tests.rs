use super::*;
use std::sync::Mutex;

/// 记录被转发到的方法，供转发完整性断言。
#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<String>>,
}

impl Recorder {
    /// 前缀匹配：带参数的记录（如 `uielement_state:7:true:false`）用调用名即可命中。
    fn got(&self, name: &str) -> bool {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c == name || c.starts_with(&format!("{name}:")))
    }
}

impl MessageHandler for Recorder {
    // ── 被测方法：记录是否收到转发 ──
    fn handle_english_stats(&self, _c: u32, _d: u32, _p: u32, _s: u32) {
        self.calls.lock().unwrap().push("english_stats".to_string());
    }
    fn handle_input_state_report(&self, _pid: u32, _dis: bool, _r: u8, _m: u64) {
        self.calls
            .lock()
            .unwrap()
            .push("input_state_report".to_string());
    }
    fn handle_diag_snapshot(&self, _s: &DiagSnapshotPayload) {
        self.calls.lock().unwrap().push("diag_snapshot".to_string());
    }
    fn handle_key_observed(&self, _k: u32, _m: u32) {
        self.calls.lock().unwrap().push("key_observed".to_string());
    }
    fn handle_client_connected(&self, _pid: u32) {
        self.calls
            .lock()
            .unwrap()
            .push("client_connected".to_string());
    }
    fn handle_ext(&self, kind: &str, _body: &[u8]) {
        self.calls.lock().unwrap().push(format!("ext:{kind}"));
    }
    fn handle_menu_pointer(&self, e: u32, b: u32, x: i32, y: i32) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("menu_pointer:{e}:{b}:{x}:{y}"));
    }
    fn handle_client_disconnected(&self) {
        self.calls
            .lock()
            .unwrap()
            .push("client_disconnected".to_string());
    }
    fn handle_uielement_state(&self, pid: u32, host_draws: bool, host_reads: bool) {
        // ⚠ 参数要进记录：只 push 一个固定字符串的话，DeferredHandler 把 host_reads
        // 丢掉也不会让任何测试变红（与 server.rs 的记录器同款做法）。
        self.calls
            .lock()
            .unwrap()
            .push(format!("uielement_state:{pid}:{host_draws}:{host_reads}"));
    }
    fn note_key_source_pid(&self, _pid: u32) {
        self.calls
            .lock()
            .unwrap()
            .push("key_source_pid".to_string());
    }
    fn uielement_page(&self) -> UiElementPage {
        self.calls
            .lock()
            .unwrap()
            .push("uielement_page".to_string());
        UiElementPage {
            items: vec!["x".into()],
            ..Default::default()
        }
    }
    fn handle_uielement_action(&self, _a: u32, _arg: u32) {
        self.calls
            .lock()
            .unwrap()
            .push("uielement_action".to_string());
    }

    // ── 以下仅为满足 trait 的必需项，本测试不关心 ──
    fn handle_key_event(&self, _d: &KeyEventData) -> KeyAction {
        KeyAction::PassThrough
    }
    fn handle_focus_gained(&self, _d: &FocusData) -> Option<StatusUpdateData> {
        None
    }
    fn handle_focus_lost(&self, _t: u64, _r: wind_ipc::protocol::FocusLostReason) {}
    fn handle_ime_activated(&self, _t: u64) -> Option<StatusUpdateData> {
        None
    }
    fn handle_ime_deactivated(&self, _t: u64) {}
    fn handle_mode_notify(&self, _f: u32) {}
    fn handle_toggle_mode(&self) -> (Option<StatusUpdateData>, String) {
        (None, String::new())
    }
    fn handle_system_mode_switch(
        &self,
        _c: bool,
        _s: wind_ipc::protocol::ModeSwitchSource,
        _ctrl: bool,
    ) -> (Option<StatusUpdateData>, String) {
        (None, String::new())
    }
    fn handle_menu_command(&self, _c: &str) -> Option<StatusUpdateData> {
        None
    }
    fn handle_composition_terminated(&self) {}
    fn handle_caret_update(&self, _d: &CaretData) {}
    fn handle_focus_gained_caret(&self, _d: &CaretData) {}
    fn handle_caret_pending(&self) {}
    fn handle_caret_probe(&self, _d: &CaretData) {}
    fn handle_selection_changed(&self, _p: u16) {}
    fn handle_commit_request(&self, _d: &CommitRequestData) -> Option<CommitResultData> {
        None
    }
}

/// 守护：**纯副作用**方法（返回 `()`、trait 默认实现为空）必须转发到内层处理器。
///
/// 这类方法漏转发是**静默**的——不报错、不 panic，只是功能悄悄失效：英文统计恒为 0、
/// 输入诊断收不到数据。而单测若直接对 `Coordinator` 调用，会绕过本代理，完全测不出来。
/// 历史上 `handle_key_event_policed` 就踩过（上屏统计恒为 0），
/// `handle_english_stats` / `handle_input_state_report` 是同一个坑的复发。
/// **新增此类方法时请一并在这里登记。**
#[test]
fn forwards_side_effect_only_methods() {
    let rec = Arc::new(Recorder::default());
    let deferred = DeferredHandler::new();

    // 未就绪：静默丢弃、不 panic（启动早期 DLL 可能已在上报）。
    deferred.handle_english_stats(1, 2, 3, 4);
    deferred.handle_input_state_report(1, true, 2, 3);
    deferred.handle_diag_snapshot(&DiagSnapshotPayload::default());
    deferred.handle_client_connected(1234);
    deferred.handle_uielement_state(1, true, false);
    deferred.handle_uielement_action(1, 0);
    assert!(
        deferred.uielement_page().items.is_empty(),
        "未就绪时应回空快照"
    );
    assert!(rec.calls.lock().unwrap().is_empty(), "未就绪时不应触达内层");

    deferred.set_ready(rec.clone());
    deferred.handle_english_stats(5, 0, 0, 0);
    deferred.handle_input_state_report(42, true, 1, 0xFF);
    deferred.handle_key_observed(0x41, 0);
    deferred.handle_diag_snapshot(&DiagSnapshotPayload {
        pid: 42,
        ..Default::default()
    });
    deferred.handle_client_connected(1234);

    assert!(
        rec.got("english_stats"),
        "英文统计未转发 → DLL 发了也白发，今日英文恒为 0"
    );
    assert!(rec.got("input_state_report"), "输入诊断上报未转发");
    assert!(
        rec.got("key_observed"),
        "旁路按键未转发 → 英文模式下扩展收不到按键"
    );
    assert!(
        rec.got("diag_snapshot"),
        "诊断快照未转发 → HUD 的窗口/上下文分区恒为空"
    );
    assert!(
        rec.got("client_connected"),
        "连接建立未转发 → 服务重启时已聚焦宿主的 per-app 规则预热整段失效"
    );
    deferred.handle_uielement_state(7, true, true);
    deferred.handle_uielement_action(2, 0);
    deferred.note_key_source_pid(7);
    assert_eq!(deferred.uielement_page().items, vec!["x".to_string()]);
    assert!(
        rec.calls
            .lock()
            .unwrap()
            .contains(&"uielement_state:7:true:true".to_string()),
        "两个判据位都必须原样转发——只转 host_draws 的话，读取推断在 core 侧永远不成立"
    );
    assert!(
        rec.got("uielement_state") && rec.got("uielement_action") && rec.got("uielement_page"),
        "UIElement 三件套未转发 → 自绘候选的游戏拿不到候选、我们的候选窗也照弹"
    );
    deferred.handle_ext("menu.open", b"{}");
    deferred.handle_menu_pointer(2, 3, -4, 5);
    deferred.handle_client_disconnected();
    assert!(
        rec.got("ext:menu.open"),
        "扩展信封未转发 → 外部宿主上行的 pos.* / shot.result / menu.* 全部石沉大海"
    );
    assert!(
        rec.calls
            .lock()
            .unwrap()
            .contains(&"menu_pointer:2:3:-4:5".to_string()),
        "菜单指针事件未原样转发 → Linux 自绘菜单点不动"
    );
    assert!(
        rec.got("client_disconnected"),
        "连接断开未转发 → addon 断线后 menu_open 复位不了"
    );
    assert!(
        rec.got("key_source_pid"),
        "按键来源 pid 未转发 → 没有 focus_gained 的游戏宿主永远对不上「谁接管了候选」"
    );
}
