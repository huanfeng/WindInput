//! 扩展程序事件的产出（设计见 `docs/design/extension-hook-channel.md`）。
//!
//! 协调器只负责「这里发生了什么」：构造 [`wind_hook::msg::Msg`] 交给 [`Tap`]。没有扩展启用时
//! `Tap` 的授权为空，每个出口只多一次原子读，事件对象不构造。序列化、写管道、分发都在
//! 服务进程的扩展子系统里做，不在按键线程上。
//!
//! 出口：
//! - 按键与按键路径上的上屏：`handle_key_event_policed` 的唯一出口（[`Coordinator::hook_after_key`]）。
//! - 不经按键路径的上屏：命令、鼠标点选、配对符号（[`Coordinator::hook_commit`]）。切中英
//!   带出的编码走按键出口（返回的 `InsertText`）或 `push_commit_text`，不另发，否则会重复。
//! - 模式：`build_status`，按「中英 / 全半角 / 方案」去重（[`Coordinator::hook_mode`]）。
//!
//! 密码框（InputScope 带 `IS_PASSWORD`）里只发按键分类与模式，具体键与上屏文本一律不发；
//! 这与「密码框强制英文」开关无关——那个开关管输入行为，这里管隐私。

use std::sync::Arc;

use wind_bridge::handler::{KeyAction, KeyEventData};
use wind_hook::msg::{
    CommitEvent, KeyEvent, LifecycleEvent, ModeEvent, Msg, PresenceEvent, lifecycle,
};
use wind_hook::tap::{Tap, now_ms};
use wind_hook::tier::Tier;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_WIN};

use crate::coordinator::Coordinator;

/// `[extensions]` 配置变化的订阅者（服务进程的扩展子系统）。
pub(crate) type ExtensionsConfigHook = Box<dyn Fn(&wind_config::ExtensionsConfig) + Send + Sync>;

/// 组合会话与模式去重的状态。
#[derive(Default)]
pub(crate) struct HookSession {
    /// 当前组合会话 id；不在组合中为 0。
    sid: u64,
    next_sid: u64,
    last_mode: Option<(bool, bool, String)>,
    /// 上次发出的 (有终端在线, 有输入焦点)；终端数本身的增减不重发。
    last_presence: Option<(bool, bool)>,
}

impl HookSession {
    /// 按本次按键的结果推进会话。返回本次事件该带的 sid：仍在组合中是当前会话，
    /// 本键结束了组合（上屏 / 清除）是刚结束的那个会话，否则 0。
    fn advance(&mut self, action: &KeyAction) -> u64 {
        let ended = self.sid;
        match action {
            KeyAction::UpdateComposition { .. } | KeyAction::HoldComposition { .. } => {
                if self.sid == 0 {
                    self.next_sid += 1;
                    self.sid = self.next_sid;
                }
                self.sid
            }
            // 上屏后紧接着开了新组合（顶码余码、顶屏后持有标点）：旧会话结束，新会话开始。
            KeyAction::CommitAndHoldComposition { .. }
            | KeyAction::CommitThenDeferComposition { .. }
            | KeyAction::InsertText {
                has_new_composition: true,
                ..
            } => {
                self.next_sid += 1;
                self.sid = self.next_sid;
                ended
            }
            KeyAction::InsertText { .. }
            | KeyAction::InsertTextWithCursor { .. }
            | KeyAction::ReplaceBackward { .. }
            | KeyAction::CommitReplacingHeld { .. }
            | KeyAction::ClearComposition
            | KeyAction::ClearCompositionThenPassThrough => {
                self.sid = 0;
                ended
            }
            _ => self.sid,
        }
    }
}

/// 一次按键结果里真正落到光标前的文字。
pub(crate) fn committed_text(action: &KeyAction) -> &str {
    match action {
        KeyAction::InsertText { text, .. }
        | KeyAction::InsertTextWithCursor { text, .. }
        | KeyAction::ReplaceBackward { text, .. }
        | KeyAction::CommitReplacingHeld { text, .. } => text.as_str(),
        KeyAction::CommitAndHoldComposition { commit_text, .. }
        | KeyAction::CommitThenDeferComposition { commit_text, .. } => commit_text.as_str(),
        _ => "",
    }
}

fn mod_names(mods: u32) -> Vec<String> {
    [
        (MOD_SHIFT, "shift"),
        (MOD_CTRL, "ctrl"),
        (MOD_ALT, "alt"),
        (MOD_WIN, "win"),
    ]
    .into_iter()
    .filter(|(bit, _)| mods & bit != 0)
    .map(|(_, n)| n.to_string())
    .collect()
}

impl Coordinator {
    /// 扩展事件出口。服务进程的扩展子系统据此接收事件、设置授权。
    pub fn hook_tap(&self) -> Arc<Tap> {
        self.hook.clone()
    }

    /// 订阅 `[extensions]` 配置：注册时以当前值回调一次，之后每次配置热重载都回调。只能注册一次。
    pub fn set_extensions_config_hook(&self, f: ExtensionsConfigHook) {
        f(&self.rt().config.extensions);
        let _ = self.extensions_config_hook.set(f);
    }

    pub(crate) fn notify_extensions_config(&self) {
        if let Some(f) = self.extensions_config_hook.get() {
            f(&self.rt().config.extensions);
        }
        self.hook_lifecycle(lifecycle::CONFIG_RELOADED);
    }

    /// 生命周期事件。不含输入内容，没有扩展在跑时不构造。
    pub(crate) fn hook_lifecycle(&self, event: &str) {
        if self.hook.grants().is_empty() {
            return;
        }
        self.hook.emit(Msg::Lifecycle(LifecycleEvent {
            ts: now_ms(),
            event: event.into(),
        }));
    }

    /// 终端在线与焦点状态，变化时发一条。由服务进程的扩展监护线程周期调用（约 200ms）：
    /// 连接增减、焦点与激活变化散在多处，轮询比在每处埋点更不易漏，延迟对扩展足够。
    /// 没有扩展在跑时只读一次原子量。
    pub fn hook_presence(&self) {
        if self.hook.grants().is_empty() {
            return;
        }
        let terminals = self.push_server.client_count();
        let focused = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.ime_active && s.has_edit_context
        };
        let cur = (terminals > 0, focused);
        {
            let mut s = self.hook_session.lock().unwrap_or_else(|e| e.into_inner());
            if s.last_presence == Some(cur) {
                return;
            }
            s.last_presence = Some(cur);
        }
        self.hook.emit(Msg::Presence(PresenceEvent {
            ts: now_ms(),
            terminals: terminals as u32,
            focused,
        }));
    }

    /// 焦点上下文：(是否密码框, 应用进程名)。只取 `last_input_diag` 一把锁，且它是叶子锁
    /// （持有期间不再拿别的锁）：上屏出口有的调用方正持着 `state`，在这里取它不会成环。
    fn hook_focus(&self) -> (bool, String) {
        let d = self
            .last_input_diag
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        (
            crate::input_diag::is_password_scope(d.mask),
            d.process_name.clone(),
        )
    }

    /// 按键路径的出口：按键事件 + 本键带出的上屏。
    pub(crate) fn hook_after_key(&self, data: &KeyEventData, action: &KeyAction) {
        let grants = self.hook.grants();
        if grants.is_empty() {
            return;
        }
        let sid = self
            .hook_session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .advance(action);
        let (password, app) = self.hook_focus();
        let ts = now_ms();
        if data.event_type == EVENT_KEY_DOWN && grants.has_key() {
            let detail = grants.has(Tier::KeyDetail) && !password;
            self.hook.emit(Msg::Key(KeyEvent {
                ts,
                sid,
                app: app.clone(),
                key: detail
                    .then(|| wind_keys::names::vk_key_name(data.key_code))
                    .flatten()
                    .map(String::from),
                mods: if detail {
                    mod_names(data.modifiers)
                } else {
                    Vec::new()
                },
                class: wind_keys::names::vk_key_class(data.key_code).to_string(),
                handled: !matches!(action, KeyAction::PassThrough | KeyAction::NotHandled),
                observed: false,
            }));
        }
        let text = committed_text(action);
        if !text.is_empty() && grants.has(Tier::Commit) && !password {
            let source = if text.is_ascii() {
                "punct"
            } else {
                "candidate"
            };
            self.hook.emit(Msg::Commit(CommitEvent {
                ts,
                sid,
                app,
                text: text.to_string(),
                source: source.into(),
                rect: None,
            }));
        }
    }

    /// 不经按键路径的上屏（命令、鼠标点选、配对符号）。
    pub(crate) fn hook_commit(&self, text: &str, source: &str) {
        if text.is_empty() || !self.hook.wants(Tier::Commit) {
            return;
        }
        let (password, app) = self.hook_focus();
        if password {
            return;
        }
        let sid = {
            let mut s = self.hook_session.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut s.sid)
        };
        self.hook.emit(Msg::Commit(CommitEvent {
            ts: now_ms(),
            sid,
            app,
            text: text.to_string(),
            source: source.into(),
            rect: None,
        }));
    }

    /// 扩展授权变化或宿主（重新）就绪后调用：
    /// - 清掉模式去重，按当前状态补发一次模式事件（新连上的扩展要知道当前模式）；
    /// - Windows：把「有没有扩展要按键」推给全部 DLL，决定它们是否旁路上报放行键。
    pub fn hook_changed(&self) {
        self.hook_session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_mode = None;
        self.hook_session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_presence = None;
        let _ = self.build_status();
        #[cfg(windows)]
        self.push_extension_key_observe_config(0);
    }

    /// DLL 旁路上报的按键（Windows 上放行给宿主、核心本看不见的键，如英文模式的字母）。
    /// 只产出扩展事件，不进任何按键处理。DLL 已在密码框里不报，这里仍按同一规则再判一次。
    pub(crate) fn hook_observed_key(&self, key_code: u32, modifiers: u32) {
        let grants = self.hook.grants();
        if !grants.has_key() {
            return;
        }
        let (password, app) = self.hook_focus();
        let detail = grants.has(Tier::KeyDetail) && !password;
        let sid = self
            .hook_session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sid;
        self.hook.emit(Msg::Key(KeyEvent {
            ts: now_ms(),
            sid,
            app,
            key: detail
                .then(|| wind_keys::names::vk_key_name(key_code))
                .flatten()
                .map(String::from),
            mods: if detail {
                mod_names(modifiers)
            } else {
                Vec::new()
            },
            class: wind_keys::names::vk_key_class(key_code).to_string(),
            handled: false,
            observed: true,
        }));
    }

    /// 模式（中英 / 全半角 / 方案）变化时发一条；状态快照到处都在取，这里去重。
    pub(crate) fn hook_mode(&self, chinese: bool, full_width: bool) {
        if !self.hook.wants(Tier::Mode) {
            return;
        }
        let schema = self.active_schema_id();
        let cur = (chinese, full_width, schema);
        {
            let mut s = self.hook_session.lock().unwrap_or_else(|e| e.into_inner());
            if s.last_mode.as_ref() == Some(&cur) {
                return;
            }
            s.last_mode = Some(cur.clone());
        }
        self.hook.emit(Msg::Mode(ModeEvent {
            ts: now_ms(),
            chinese: cur.0,
            full_width: cur.1,
            schema: cur.2,
        }));
    }
}

#[cfg(test)]
#[path = "hook_events_tests.rs"]
mod tests;
