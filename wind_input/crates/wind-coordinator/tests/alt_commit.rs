//! 上屏注释 / 拼音（`input.alt_commit`，C2-6 / 论坛 t138）端到端。
//!
//! Alt+数字 N 上屏第 N 个候选的拼音或注释，Alt+空格 取高亮那条。只在「中文 + 有候选」时
//! 生效，用户在 `keys.key_actions` 里绑过的 Alt 组合让位。
//!
//! 词典缺失时自动跳过（判据：耗时 0.0x 秒＝假绿）。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_ALT};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_schemas() -> bool {
    data_dir().join("schemas/pinyin.schema.toml").exists()
}

fn key(key_code: u32, modifiers: u32) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

fn cfg(mode: &str) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.alt_commit = mode.into();
    cfg
}

/// 打出 `nihao`，返回协调器（首选应为「你好」）。
fn typed(cfg: Config) -> std::sync::Arc<Coordinator> {
    let c = Coordinator::new_headless(cfg, Some(&data_dir()));
    for ch in "nihao".chars() {
        c.handle_key_event(&key(ch.to_ascii_uppercase() as u32, 0));
    }
    assert_eq!(
        c.debug_all_candidate_texts().first().map(String::as_str),
        Some("你好"),
        "前置条件：nihao 首选应为「你好」"
    );
    c
}

fn inserted(act: &KeyAction) -> Option<&str> {
    match act {
        KeyAction::InsertText { text, .. } => Some(text),
        _ => None,
    }
}

#[test]
fn alt_digit_commits_toned_pinyin_of_nth_candidate() {
    if !has_schemas() {
        return;
    }
    let c = typed(cfg("pinyin"));
    let act = c.handle_key_event(&key(0x31, MOD_ALT));
    assert_eq!(inserted(&act), Some("nǐ hǎo"), "实际: {act:?}");
    assert_eq!(c.debug_candidate_count(), 0, "上屏后会话应结束");
}

#[test]
fn alt_space_commits_highlighted_candidate() {
    if !has_schemas() {
        return;
    }
    let c = typed(cfg("pinyin_plain"));
    let act = c.handle_key_event(&key(0x20, MOD_ALT));
    assert_eq!(inserted(&act), Some("ni hao"), "实际: {act:?}");
}

#[test]
fn comment_mode_commits_rendered_comment_untruncated() {
    if !has_schemas() {
        return;
    }
    let mut cfg = cfg("comment");
    cfg.ui
        .comment
        .templates
        .push(wind_config::comment_lib::CommentTemplateDef {
            id: "py".into(),
            name: "py".into(),
            text: "[${pinyin}]".into(),
        });
    cfg.ui.comment.assign.global.default_vertical = Some("py".into());
    cfg.ui.candidate.comment_max_chars_vertical = 2;
    cfg.ui.candidate.comment_max_chars_horizontal = 2;
    let c = typed(cfg);
    let act = c.handle_key_event(&key(0x31, MOD_ALT));
    assert_eq!(inserted(&act), Some("[nǐ hǎo]"), "实际: {act:?}");
}

/// 没有注释可上屏时**什么都不上屏**、吞键、会话原样保留（不回落成候选字：用户按的就是
/// 「要注释」，给他候选字等于替他做了另一件事）。
#[test]
fn empty_annotation_keeps_session() {
    if !has_schemas() {
        return;
    }
    let mut cfg = cfg("comment");
    cfg.ui.comment.assign.global.default_vertical = Some("none".into());
    let c = typed(cfg);
    let before = c.debug_candidate_count();
    let act = c.handle_key_event(&key(0x31, MOD_ALT));
    assert!(matches!(act, KeyAction::Consumed), "实际: {act:?}");
    assert_eq!(c.debug_candidate_count(), before, "会话必须原样保留");
}

/// 出厂关：Alt+1 不被认领，落到既有的 Ctrl/Alt 兜底臂（清组合、键归宿主）。
#[test]
fn off_by_default_does_not_claim_alt_digit() {
    if !has_schemas() {
        return;
    }
    let c = typed(cfg("off"));
    let act = c.handle_key_event(&key(0x31, MOD_ALT));
    assert!(inserted(&act).is_none(), "关着时不该上屏任何东西: {act:?}");
}

/// 候选注释总开关关着（`ui.candidate.comment_enabled = false`）时，`comment` 档按 `off` 处理：
/// Alt+1 不被认领、落到 Ctrl/Alt 兜底臂（清组合、键归宿主），而不是吞键保留会话。
/// `pinyin` 档不看注释开关。
#[test]
fn comment_mode_with_comments_off_behaves_like_off() {
    if !has_schemas() {
        return;
    }
    let off = typed(cfg("off"));
    let off_act = off.handle_key_event(&key(0x31, MOD_ALT));

    let mut cfg_cm = cfg("comment");
    cfg_cm.ui.candidate.comment_enabled = false;
    let cm = typed(cfg_cm);
    let cm_act = cm.handle_key_event(&key(0x31, MOD_ALT));
    assert_eq!(
        format!("{cm_act:?}"),
        format!("{off_act:?}"),
        "注释关掉后 comment 档的 Alt+1 应与 off 同一结局"
    );
    assert_eq!(
        cm.debug_candidate_count(),
        off.debug_candidate_count(),
        "会话去留也应与 off 相同"
    );

    let mut cfg_py = cfg("pinyin");
    cfg_py.ui.candidate.comment_enabled = false;
    let py = typed(cfg_py);
    let act = py.handle_key_event(&key(0x31, MOD_ALT));
    assert_eq!(inserted(&act), Some("nǐ hǎo"), "pinyin 档不受影响: {act:?}");
}

/// 用户在 key_actions 里绑了 Alt+1 ⇒ 本功能让位。
#[test]
fn user_combo_binding_wins() {
    if !has_schemas() {
        return;
    }
    let mut cfg = cfg("pinyin");
    cfg.keys
        .key_actions
        .insert("alt+1".into(), "toggle_punct".into());
    let c = typed(cfg);
    let act = c.handle_key_event(&key(0x31, MOD_ALT));
    assert!(inserted(&act).is_none(), "用户绑定应优先: {act:?}");
    // 未被占用的 Alt+2 照常工作。
    let c2 = {
        let mut cfg = self::cfg("pinyin");
        cfg.keys
            .key_actions
            .insert("alt+1".into(), "toggle_punct".into());
        typed(cfg)
    };
    let act = c2.handle_key_event(&key(0x32, MOD_ALT));
    assert!(
        inserted(&act).is_some(),
        "Alt+2 未被占用，应照常上屏: {act:?}"
    );
}

/// 小键盘 Alt+数字是 Windows 的 Alt 码输入；`follow_main` 会把小键盘键改写成主键盘码，
/// 不能因此被误认成 Alt+数字。
#[test]
fn numpad_alt_digit_is_not_claimed() {
    if !has_schemas() {
        return;
    }
    let mut cfg = cfg("pinyin");
    cfg.input.numpad_behavior = "follow_main".into();
    let c = typed(cfg);
    let act = c.handle_key_event(&key(0x61, MOD_ALT)); // VK_NUMPAD1
    assert!(inserted(&act).is_none(), "小键盘 Alt+1 不该触发: {act:?}");
}

/// 无会话时一律不吃（Alt+空格 是窗口系统菜单，Alt+数字 是宿主加速键）。
#[test]
fn no_session_passes_through() {
    if !has_schemas() {
        return;
    }
    let c = Coordinator::new_headless(cfg("pinyin"), Some(&data_dir()));
    for vk in [0x20, 0x31] {
        assert!(
            !c.should_handle_key(&wind_host::KeyProbe {
                modifiers: wind_host::Modifiers(MOD_ALT),
                ..wind_host::KeyProbe::new(vk)
            }),
            "无会话时 Alt+0x{vk:02X} 必须归宿主"
        );
        let act = c.handle_key_event(&key(vk, MOD_ALT));
        assert!(matches!(act, KeyAction::PassThrough), "实际: {act:?}");
    }
}
