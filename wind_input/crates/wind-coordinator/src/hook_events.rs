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
use wind_hook::msg::{CommitEvent, KeyEvent, ModeEvent, Msg};
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
mod tests {
    use super::*;

    fn update() -> KeyAction {
        KeyAction::UpdateComposition {
            text: "n".into(),
            caret_pos: 1,
        }
    }

    fn insert(text: &str) -> KeyAction {
        KeyAction::InsertText {
            text: text.into(),
            new_composition: None,
            mode_changed: false,
            chinese_mode: true,
            has_new_composition: false,
        }
    }

    #[test]
    fn session_spans_composition_and_ends_on_commit() {
        let mut s = HookSession::default();
        assert_eq!(s.advance(&KeyAction::PassThrough), 0);
        let sid = s.advance(&update());
        assert_ne!(sid, 0);
        assert_eq!(s.advance(&update()), sid, "同一次组合共用 sid");
        assert_eq!(s.advance(&insert("你")), sid, "上屏事件带刚结束的会话");
        assert_eq!(s.advance(&KeyAction::PassThrough), 0);
        assert_ne!(s.advance(&update()), sid, "下一次组合是新会话");
    }

    #[test]
    fn commit_with_follow_up_composition_starts_new_session() {
        let mut s = HookSession::default();
        let first = s.advance(&update());
        let act = KeyAction::CommitThenDeferComposition {
            commit_text: "我".into(),
            deferred_composition: "q".into(),
            timeout_ms: 0,
        };
        assert_eq!(s.advance(&act), first);
        let second = s.advance(&update());
        assert_ne!(second, 0);
        assert_ne!(second, first);
    }

    // ── 经协调器出口的行为 ──

    use std::sync::mpsc::Receiver;
    use wind_bridge::handler::MessageHandler;
    use wind_hook::tier::Grants;
    use wind_keys::keymap::{VK_A, VK_COMMA};

    fn coord(tag: &str) -> Arc<Coordinator> {
        let path = std::env::temp_dir().join(format!("wind_hook_{tag}.redb"));
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(wind_store::Store::open(&path).unwrap());
        Coordinator::new_headless_with_store(wind_config::Config::default(), None, store)
    }

    fn press(c: &Coordinator, vk: u32) -> KeyAction {
        c.handle_key_event_policed(&KeyEventData {
            key_code: vk,
            scan_code: 0,
            modifiers: 0,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
            seq_char: 0,
        })
    }

    fn connect(c: &Coordinator, tiers: &[&str]) -> Receiver<Msg> {
        c.hook_tap()
            .connect(Grants::parse(tiers.iter().copied()), 64)
    }

    fn drain(rx: &Receiver<Msg>) -> Vec<Msg> {
        rx.try_iter().collect()
    }

    #[test]
    fn key_and_commit_events_carry_detail_and_text() {
        let c = coord("detail");
        let rx = connect(&c, &["key.detail", "commit"]);
        press(&c, VK_COMMA);
        press(&c, VK_A);
        let got = drain(&rx);
        let keys: Vec<&KeyEvent> = got
            .iter()
            .filter_map(|m| if let Msg::Key(k) = m { Some(k) } else { None })
            .collect();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].key.as_deref(), Some("comma"));
        assert_eq!(keys[0].class, "punct");
        assert_eq!(keys[1].key.as_deref(), Some("a"));
        assert_eq!(keys[1].class, "letter");
        assert!(
            got.iter()
                .any(|m| matches!(m, Msg::Commit(e) if e.text == "，" && e.sid == 0)),
            "空闲时逗号直接上屏中文标点，不属于任何组合: {got:?}"
        );
        assert_ne!(keys[1].sid, 0, "字母开启组合会话");
    }

    #[test]
    fn key_class_grant_hides_key_name() {
        let c = coord("class");
        let rx = connect(&c, &["key.class"]);
        press(&c, VK_A);
        let got = drain(&rx);
        assert!(
            matches!(got.as_slice(), [Msg::Key(k)] if k.key.is_none() && k.class == "letter"),
            "{got:?}"
        );
    }

    #[test]
    fn password_field_hides_key_name_and_commit() {
        let c = coord("password");
        let rx = connect(&c, &["key.detail", "commit"]);
        c.apply_input_diag(1, false, 2, 1 << 31);
        press(&c, VK_A);
        press(&c, VK_COMMA);
        c.hook_commit("secret", "command");
        let got = drain(&rx);
        assert!(!got.is_empty());
        for m in &got {
            match m {
                Msg::Key(k) => assert!(k.key.is_none() && k.mods.is_empty(), "{k:?}"),
                other => panic!("密码框里不该有 {other:?}"),
            }
        }
    }

    #[test]
    fn non_key_origins_reach_extensions_through_push_exit() {
        use crate::session_exit::ActionOrigin;
        let c = coord("origins");
        let rx = connect(&c, &["commit"]);
        c.push_commit_text_from(ActionOrigin::SoftKeyboardClick, "软");
        c.push_commit_text_from(ActionOrigin::Command, "命");
        let got: Vec<(String, String)> = drain(&rx)
            .into_iter()
            .filter_map(|m| match m {
                Msg::Commit(e) => Some((e.text, e.source)),
                _ => None,
            })
            .collect();
        let want = [("软", "softkeyboard"), ("命", "command")]
            .map(|(t, s)| (t.to_string(), s.to_string()));
        assert_eq!(got, want);
    }

    #[test]
    fn idle_tap_produces_nothing() {
        let c = coord("idle");
        press(&c, VK_A);
        let rx = connect(&c, &["mode"]);
        assert!(drain(&rx).is_empty());
    }

    #[test]
    fn observed_key_is_marked_and_unhandled() {
        let c = coord("observed");
        let rx = connect(&c, &["key.detail"]);
        c.hook_observed_key(VK_A, wind_ipc::protocol::MOD_SHIFT);
        let got = drain(&rx);
        assert!(
            matches!(got.as_slice(), [Msg::Key(k)]
                if k.observed && !k.handled && k.key.as_deref() == Some("a") && k.mods == ["shift"]),
            "{got:?}"
        );
    }

    #[test]
    fn observed_key_in_password_field_hides_name() {
        let c = coord("observed_pw");
        let rx = connect(&c, &["key.detail"]);
        c.apply_input_diag(1, false, 2, 1 << 31);
        c.hook_observed_key(VK_A, 0);
        let got = drain(&rx);
        assert!(
            matches!(got.as_slice(), [Msg::Key(k)] if k.key.is_none() && k.observed),
            "{got:?}"
        );
    }

    #[test]
    fn observed_key_without_key_grant_is_dropped() {
        let c = coord("observed_none");
        let rx = connect(&c, &["commit"]);
        c.hook_observed_key(VK_A, 0);
        assert!(drain(&rx).is_empty());
    }

    #[test]
    fn mode_event_is_deduplicated() {
        let c = coord("mode");
        let rx = connect(&c, &["mode"]);
        c.build_status();
        c.build_status();
        let got = drain(&rx);
        assert!(matches!(got.as_slice(), [Msg::Mode(_)]), "{got:?}");
    }

    #[test]
    fn modifiers_are_named() {
        assert_eq!(mod_names(MOD_SHIFT | MOD_CTRL), ["shift", "ctrl"]);
        assert!(mod_names(0).is_empty());
    }
}
