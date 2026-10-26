//! 混输方案下拼音候选的编码提示端到端：出厂分配给混输类型「码表编码」（设计 §13.6，
//! 取代退役的 `schema.mix.pinyin_code_hint`）。走完整链路：混输方案打拼音 → 候选注释里
//! 有没有主码表编码。
//! 注释文本借「上屏注释」（`input.alt_commit = "comment"`）读出——它与候选窗共用同一裁决。
//!
//! 缺 `build_dev/data` 时自动跳过并打印原因（本组真跑也只要零点几秒，不能拿耗时判假绿）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_coordinator::web_host::WebDataHost;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_ALT};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_schemas() -> bool {
    data_dir()
        .join("schemas/wubi86_pinyin.schema.toml")
        .exists()
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

/// `mixed_tpl` 为 `Some` 时给混输类型的默认态改分这个模板。
fn cfg(mixed_tpl: Option<&str>) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86_pinyin".into(), "wubi86".into(), "pinyin".into()];
    cfg.schema.active = "wubi86_pinyin".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.alt_commit = "comment".into();
    if let Some(t) = mixed_tpl {
        cfg.ui.comment.assign.by_type.insert(
            "mixed".into(),
            wind_config::comment_lib::CommentCells {
                default_horizontal: Some(t.into()),
                ..Default::default()
            },
        );
    }
    cfg
}

/// 预热索引后打 `nihao`，Alt+1 取首选的注释；注释为空时 Alt+1 吞键、不上屏 ⇒ `None`。
fn first_comment(cfg: Config) -> Option<String> {
    let c: Arc<Coordinator> = Coordinator::new_headless(cfg, Some(&data_dir()));
    c.prewarm_indexes();
    assert_eq!(
        c.engine_mgr().codetable_reverse_hint("你好").as_deref(),
        Some("wqvb"),
        "前提：混输方案下主码表反查索引已就绪、查得到「你好」"
    );
    for ch in "nihao".chars() {
        c.handle_key_event(&key(ch.to_ascii_uppercase() as u32, 0));
    }
    assert_eq!(
        c.debug_all_candidate_texts().first().map(String::as_str),
        Some("你好"),
        "前提：混输 nihao 首选应为「你好」"
    );
    match c.handle_key_event(&key(0x31, MOD_ALT)) {
        KeyAction::InsertText { text, .. } => Some(text),
        _ => None,
    }
}

/// 出厂：混输类型分到「码表编码」，拼音候选显示主码表编码。
#[test]
fn mixed_shows_codetable_code_by_factory_assignment() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data 的混输方案");
        return;
    }
    assert_eq!(first_comment(cfg(None)).as_deref(), Some("wqvb"));
}

/// 混输类型改分「补全编码」：拼音候选不再显示码表编码。
#[test]
fn mixed_with_hint_only_template_hides_codetable_code() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data 的混输方案");
        return;
    }
    assert_ne!(
        first_comment(cfg(Some("code_hint"))).as_deref(),
        Some("wqvb")
    );
}
