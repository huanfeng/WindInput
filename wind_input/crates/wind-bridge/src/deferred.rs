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

    fn assoc_session_hint(&self) -> bool {
        self.with_handler(false, |h| h.assoc_session_hint())
    }

    fn composition_placeholder(&self) -> &'static str {
        self.with_handler(crate::handler::COMPOSITION_PLACEHOLDER, |h| {
            h.composition_placeholder()
        })
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

    fn handle_front_context(&self, app: &str, title: &str, sel: &str) {
        self.with_handler((), |h| h.handle_front_context(app, title, sel))
    }

    // 下面三个此前都落在 trait 默认的空实现上：扩展信封（`pos.*` / `shot.result` / Linux 的
    // `menu.*`）从外部宿主上来后被本代理悄悄吞掉，Coordinator 的实现从未被调用过。
    fn handle_ext(&self, kind: &str, body: &[u8]) {
        self.with_handler((), |h| h.handle_ext(kind, body))
    }

    fn handle_menu_pointer(&self, event: u32, button: u32, x: i32, y: i32) {
        self.with_handler((), |h| h.handle_menu_pointer(event, button, x, y))
    }

    fn on_panel_pointer(&self, kind: u32, event: u32, button: u32, x: i32, y: i32) {
        self.with_handler((), |h| h.on_panel_pointer(kind, event, button, x, y))
    }

    fn handle_client_disconnected(&self) {
        self.with_handler((), |h| h.handle_client_disconnected())
    }

    fn handle_ime_activated(&self, client_token: u64) -> Option<StatusUpdateData> {
        self.with_handler(None, |h| h.handle_ime_activated(client_token))
    }

    fn is_background_client(&self, pid: u32) -> bool {
        self.with_handler(false, |h| h.is_background_client(pid))
    }

    fn handle_background_ime_activated(&self, client_token: u64) {
        self.with_handler((), |h| h.handle_background_ime_activated(client_token))
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

    fn get_current_mode(
        &self,
        client_token: u64,
        window_class: &str,
        window_title: &str,
    ) -> (bool, bool, bool) {
        // 未就绪时回中文模式 + 中文标点（安全默认）；就绪后委派真实处理器读权威模式。
        self.with_handler((true, false, true), |h| {
            h.get_current_mode(client_token, window_class, window_title)
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

    /// 旁路按键上报。同上：不转发则英文模式下扩展永远收不到按键，且毫无报错。
    fn handle_key_observed(&self, key_code: u32, modifiers: u32) {
        self.with_handler((), |h| h.handle_key_observed(key_code, modifiers))
    }

    /// 诊断快照上报。同上：不转发则 HUD 的窗口/上下文分区永远是空的，且毫无报错。
    fn handle_diag_snapshot(&self, snap: &DiagSnapshotPayload) {
        self.with_handler((), |h| h.handle_diag_snapshot(snap))
    }

    /// UIElement 三件套。同上：不转发则宿主自绘的游戏永远拿不到候选、我们的窗也照弹。
    fn handle_uielement_state(&self, pid: u32, host_draws: bool, host_reads: bool) {
        self.with_handler((), |h| {
            h.handle_uielement_state(pid, host_draws, host_reads)
        })
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
#[path = "deferred_tests.rs"]
mod tests;
