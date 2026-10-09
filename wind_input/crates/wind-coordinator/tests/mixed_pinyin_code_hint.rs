//! 混输方案下拼音候选的编码提示（`schema.mix.pinyin_code_hint`）端到端。
//!
//! 此前混输读的是拼音方案那份 `schema.pinyin.code_hint_source`，拼音出厂改 `off` 后混输
//! 跟着没了编码提示。这里走完整链路：混输方案打拼音 → 候选注释里有没有主码表编码。
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

fn cfg(schema_pinyin_src: &str, mix_hint: bool) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86_pinyin".into(), "wubi86".into(), "pinyin".into()];
    cfg.schema.active = "wubi86_pinyin".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.alt_commit = "comment".into();
    cfg.ui.candidate.comment_template_vertical = "${code_rev}".into();
    cfg.ui.candidate.comment_template_horizontal = "${code_rev}".into();
    cfg.schema.pinyin.code_hint_source = schema_pinyin_src.into();
    cfg.schema.mix.pinyin_code_hint = mix_hint;
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

/// 出厂开：拼音那份是 off，混输照样有编码。
#[test]
fn mixed_shows_codetable_code_even_when_pinyin_source_off() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data 的混输方案");
        return;
    }
    assert_eq!(first_comment(cfg("off", true)).as_deref(), Some("wqvb"));
}

/// 关掉混输那份：拼音那份开着也不显示。
#[test]
fn mixed_switch_off_hides_code_even_when_pinyin_source_on() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data 的混输方案");
        return;
    }
    assert_eq!(first_comment(cfg("auto", false)), None);
}
