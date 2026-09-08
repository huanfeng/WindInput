//! 延迟处理器：启动时返回安全默认值，就绪后切换到真实处理器
//!
//! 与 Go 版本 `wind_input/internal/bridge/deferred_handler.go` 对齐。

use crate::handler::*;
use std::sync::{Arc, RwLock};
use tracing::info;

/// 延迟处理器：在服务初始化完成前返回安全默认值
pub struct DeferredHandler {
    inner: RwLock<Option<Arc<dyn MessageHandler>>>,
}

impl DeferredHandler {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(None),
        })
    }

    /// 设置真实处理器（初始化完成后调用）
    pub fn set_ready(&self, handler: Arc<dyn MessageHandler>) {
        info!("DeferredHandler: switching to real handler");
        *self.inner.write().unwrap() = Some(handler);
    }

    /// 检查是否已就绪
    pub fn is_ready(&self) -> bool {
        self.inner.read().unwrap().is_some()
    }

    /// 获取内部真实处理器（如果已就绪）
    fn with_handler<F, R>(&self, default: R, f: F) -> R
    where
        F: FnOnce(&dyn MessageHandler) -> R,
    {
        let guard = self.inner.read().unwrap();
        match guard.as_ref() {
            Some(handler) => f(handler.as_ref()),
            None => default,
        }
    }
}

impl MessageHandler for DeferredHandler {
    fn handle_key_event(&self, data: &KeyEventData) -> KeyAction {
        self.with_handler(KeyAction::PassThrough, |h| h.handle_key_event(data))
    }

    /// bridge 真正入口（server.rs 调 policed）。必须转发到内层处理器的 policed，
    /// 否则内层 Coordinator 重写的 policed（含统计埋点 + preedit 占位后处理）被跳过——
    /// 只走 trait 默认实现（仅调内层 handle_key_event），导致上屏统计在生产中恒为 0。
    fn handle_key_event_policed(&self, data: &KeyEventData) -> KeyAction {
        self.with_handler(KeyAction::PassThrough, |h| h.handle_key_event_policed(data))
    }

    fn preedit_uses_placeholder(&self) -> bool {
        self.with_handler(false, |h| h.preedit_uses_placeholder())
    }

    fn handle_client_connected(&self, pid: u32) {
        // 服务重启是本回调的主场景：`bridge.start()` 早于 `set_ready`（引擎/词典加载
        // 期间），DLL 若恰好在这段窗口重连，per-app 规则预热会被静默吞掉，退化回旧行为
        // （手动切一次焦点才生效）——不算错误，但值得留痕，否则「为什么这次没生效」
        // 排查时无从下手。
        if !self.is_ready() {
            tracing::debug!(
                "DeferredHandler: 未就绪期间收到连接 pid={pid}，per-app 规则预热被跳过（真实 FOCUS_GAINED 兜底）"
            );
        }
        self.with_handler((), |h| h.handle_client_connected(pid))
    }

    fn handle_focus_gained(&self, data: &FocusData) -> Option<StatusUpdateData> {
        self.with_handler(None, |h| h.handle_focus_gained(data))
    }

    fn handle_focus_lost(&self, client_token: u64, reason: wind_ipc::protocol::FocusLostReason) {
        self.with_handler((), |h| h.handle_focus_lost(client_token, reason))
    }

    fn handle_show_context_menu(&self, x: i32, y: i32) {
        self.with_handler((), |h| h.handle_show_context_menu(x, y))
    }

    fn query_menu_encoded(&self, simplified: bool) -> Vec<u8> {
        self.with_handler(Vec::new(), |h| h.query_menu_encoded(simplified))
    }

    fn handle_menu_action_id(&self, id: i32) {
        self.with_handler((), |h| h.handle_menu_action_id(id))
    }

    fn handle_candidate_select(&self, page_local_index: i32) {
        self.with_handler((), |h| h.handle_candidate_select(page_local_index))
    }

    fn handle_candidate_scroll(&self, delta: i32) {
        self.with_handler((), |h| h.handle_candidate_scroll(delta))
    }

    fn handle_candidate_hover(&self, page_local_index: i32) {
        self.with_handler((), |h| h.handle_candidate_hover(page_local_index))
    }

    fn handle_candidate_context_menu(&self, page_local_index: i32, action: &str) {
        self.with_handler((), |h| {
            h.handle_candidate_context_menu(page_local_index, action)
        })
    }

    fn handle_front_context(&self, app: &str, title: &str, sel: &str) {
        self.with_handler((), |h| h.handle_front_context(app, title, sel))
    }

    fn handle_ime_activated(&self, client_token: u64) -> Option<StatusUpdateData> {
        self.with_handler(None, |h| h.handle_ime_activated(client_token))
    }

    fn handle_ime_deactivated(&self, client_token: u64) {
        self.with_handler((), |h| h.handle_ime_deactivated(client_token))
    }

    fn handle_mode_notify(&self, flags: u32) {
        self.with_handler((), |h| h.handle_mode_notify(flags))
    }

    fn handle_toggle_mode(&self) -> (Option<StatusUpdateData>, String) {
        self.with_handler((None, String::new()), |h| h.handle_toggle_mode())
    }

    fn handle_system_mode_switch(
        &self,
        chinese_mode: bool,
        source: wind_ipc::protocol::ModeSwitchSource,
        ctrl_held: bool,
    ) -> (Option<StatusUpdateData>, String) {
        self.with_handler((None, String::new()), |h| {
            h.handle_system_mode_switch(chinese_mode, source, ctrl_held)
        })
    }

    fn handle_menu_command(&self, command: &str) -> Option<StatusUpdateData> {
        self.with_handler(None, |h| h.handle_menu_command(command))
    }

    fn handle_composition_terminated(&self) {
        self.with_handler((), |h| h.handle_composition_terminated())
    }

    fn handle_caret_update(&self, data: &CaretData) {
        self.with_handler((), |h| h.handle_caret_update(data))
    }

    // ⚠ 必须显式转发，不能吃 trait 默认实现：默认实现调的是 `self.handle_caret_update`，
    // 而这里的 `self` 是本包装器 → 又转发到内层的 handle_caret_update，内层的
    // handle_focus_gained_caret 永远不会被调用，副作用（消费首显等待→立即显示候选）
    // 原封不动地回来。整条链每一步都合法，编译器不会报错，只能靠真机日志发现。
    // 本 trait 今后新增带默认实现的方法时，这里都要跟着补一条转发。
    fn handle_focus_gained_caret(&self, data: &CaretData) {
        self.with_handler((), |h| h.handle_focus_gained_caret(data))
    }

    fn handle_caret_probe(&self, data: &CaretData) {
        self.with_handler((), |h| h.handle_caret_probe(data))
    }

    fn handle_caret_pending(&self) {
        self.with_handler((), |h| h.handle_caret_pending())
    }

    fn handle_selection_changed(&self, prev_char: u16) {
        self.with_handler((), |h| h.handle_selection_changed(prev_char))
    }

    fn handle_commit_request(&self, data: &CommitRequestData) -> Option<CommitResultData> {
        self.with_handler(None, |h| h.handle_commit_request(data))
    }

    fn handle_host_render_failed(&self, reason: u32) {
        self.with_handler((), |h| h.handle_host_render_failed(reason))
    }

    fn get_current_mode(&self, client_token: u64, window_class: &str) -> (bool, bool) {
        // 未就绪时回中文模式（安全默认）；就绪后委派真实处理器读权威模式。
        self.with_handler((true, false), |h| {
            h.get_current_mode(client_token, window_class)
        })
    }

    /// TSF 侧英文模式统计上报（CMD_INPUT_STATS）。必须转发——trait 默认实现是空的，
    /// 不转发则 DLL 明明发了、Coordinator 也实现了，英文统计在生产中仍恒为 0
    /// （与上方 `handle_key_event_policed` 完全同类的坑）。
    fn handle_english_stats(&self, chars: u32, digits: u32, puncts: u32, spaces: u32) {
        self.with_handler((), |h| {
            h.handle_english_stats(chars, digits, puncts, spaces)
        })
    }

    /// compartment 禁用态上报。同上：默认实现为空，不转发则输入诊断永远拿不到数据。
    fn handle_input_state_report(&self, pid: u32, disabled: bool, reason: u8, mask: u64) {
        self.with_handler((), |h| {
            h.handle_input_state_report(pid, disabled, reason, mask)
        })
    }

    /// 诊断快照上报。同上：不转发则 HUD 的窗口/上下文分区永远是空的，且毫无报错。
    fn handle_diag_snapshot(&self, snap: &DiagSnapshotPayload) {
        self.with_handler((), |h| h.handle_diag_snapshot(snap))
    }

    /// UIElement 三件套。同上：不转发则宿主自绘的游戏永远拿不到候选、我们的窗也照弹。
    fn handle_uielement_state(&self, pid: u32, host_draws: bool) {
        self.with_handler((), |h| h.handle_uielement_state(pid, host_draws))
    }

    fn note_key_source_pid(&self, pid: u32) {
        self.with_handler((), |h| h.note_key_source_pid(pid))
    }

    fn uielement_page(&self) -> UiElementPage {
        self.with_handler(UiElementPage::default(), |h| h.uielement_page())
    }

    fn handle_uielement_action(&self, action: u32, arg: u32) {
        self.with_handler((), |h| h.handle_uielement_action(action, arg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 记录被转发到的方法，供转发完整性断言。
    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<&'static str>>,
    }

    impl Recorder {
        fn got(&self, name: &'static str) -> bool {
            self.calls.lock().unwrap().contains(&name)
        }
    }

    impl MessageHandler for Recorder {
        // ── 被测方法：记录是否收到转发 ──
        fn handle_english_stats(&self, _c: u32, _d: u32, _p: u32, _s: u32) {
            self.calls.lock().unwrap().push("english_stats");
        }
        fn handle_input_state_report(&self, _pid: u32, _dis: bool, _r: u8, _m: u64) {
            self.calls.lock().unwrap().push("input_state_report");
        }
        fn handle_diag_snapshot(&self, _s: &DiagSnapshotPayload) {
            self.calls.lock().unwrap().push("diag_snapshot");
        }
        fn handle_client_connected(&self, _pid: u32) {
            self.calls.lock().unwrap().push("client_connected");
        }
        fn handle_uielement_state(&self, _pid: u32, _host_draws: bool) {
            self.calls.lock().unwrap().push("uielement_state");
        }
        fn note_key_source_pid(&self, _pid: u32) {
            self.calls.lock().unwrap().push("key_source_pid");
        }
        fn uielement_page(&self) -> UiElementPage {
            self.calls.lock().unwrap().push("uielement_page");
            UiElementPage {
                items: vec!["x".into()],
                ..Default::default()
            }
        }
        fn handle_uielement_action(&self, _a: u32, _arg: u32) {
            self.calls.lock().unwrap().push("uielement_action");
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
        deferred.handle_uielement_state(1, true);
        deferred.handle_uielement_action(1, 0);
        assert!(
            deferred.uielement_page().items.is_empty(),
            "未就绪时应回空快照"
        );
        assert!(rec.calls.lock().unwrap().is_empty(), "未就绪时不应触达内层");

        deferred.set_ready(rec.clone());
        deferred.handle_english_stats(5, 0, 0, 0);
        deferred.handle_input_state_report(42, true, 1, 0xFF);
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
            rec.got("diag_snapshot"),
            "诊断快照未转发 → HUD 的窗口/上下文分区恒为空"
        );
        assert!(
            rec.got("client_connected"),
            "连接建立未转发 → 服务重启时已聚焦宿主的 per-app 规则预热整段失效"
        );
        deferred.handle_uielement_state(7, true);
        deferred.handle_uielement_action(2, 0);
        deferred.note_key_source_pid(7);
        assert_eq!(deferred.uielement_page().items, vec!["x".to_string()]);
        assert!(
            rec.got("uielement_state") && rec.got("uielement_action") && rec.got("uielement_page"),
            "UIElement 三件套未转发 → 自绘候选的游戏拿不到候选、我们的候选窗也照弹"
        );
        assert!(
            rec.got("key_source_pid"),
            "按键来源 pid 未转发 → 没有 focus_gained 的游戏宿主永远对不上「谁接管了候选」"
        );
    }
}
