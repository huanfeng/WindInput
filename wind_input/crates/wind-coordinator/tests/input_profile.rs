//! `InputProfile`：平台差异由构造时传入的档位决定，而不是编译目标。
//!
//! 这些测试在主机上直接构造 `InputProfile::mobile()`，覆盖过去只有 Android 构建才走得到的分支。
//! 词典缺失时对应测试静默跳过（判据是耗时 0.00s），数据在 `build_dev/data`。

use std::path::PathBuf;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::{Coordinator, InputProfile};
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_keys::keymap::{VK_0, VK_A};

const VK_3: u32 = VK_0 + 3;
const VK_B: u32 = VK_A + 1;
const VK_M: u32 = VK_A + 12;
const VK_P: u32 = VK_A + 15;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has(file: &str) -> bool {
    data_dir().join(file).exists()
}

fn headless(cfg: Config, profile: InputProfile) -> std::sync::Arc<Coordinator> {
    Coordinator::new_headless_with_profile(cfg, Some(&data_dir()), None, profile).0
}

fn key(vk: u32) -> KeyEventData {
    KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

fn press(coord: &Coordinator, vk: u32) {
    coord.handle_key_event(&key(vk));
}

fn english_config() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["english".into(), "wubi86".into()];
    cfg.schema.active = "english".into();
    cfg.input.default.chinese_mode = true;
    cfg
}

fn has_english() -> bool {
    has("schemas/english.schema.toml") && data_dir().join("schemas/english").is_dir()
}

#[test]
fn profile_is_what_was_constructed() {
    let d = Coordinator::new_headless(Config::default(), None);
    assert_eq!(d.profile(), InputProfile::desktop());
    let m = Coordinator::new_headless_with_profile(
        Config::default(),
        None,
        None,
        InputProfile::mobile(),
    )
    .0;
    assert_eq!(m.profile(), InputProfile::mobile());
}

#[test]
fn mobile_digit_enters_english_buffer() {
    if !has_english() {
        return;
    }
    let c = headless(english_config(), InputProfile::mobile());
    press(&c, VK_M);
    press(&c, VK_P);
    press(&c, VK_3);
    assert!(
        c.debug_preedit().contains("mp3"),
        "移动端英文组码中数字进缓冲，实际 {:?}",
        c.debug_preedit()
    );
}

#[test]
fn desktop_digit_keeps_selecting() {
    if !has_english() {
        return;
    }
    let c = headless(english_config(), InputProfile::desktop());
    press(&c, VK_M);
    press(&c, VK_P);
    press(&c, VK_3);
    assert!(
        !c.debug_preedit().contains("mp3"),
        "桌面数字照旧选词，不进缓冲，实际 {:?}",
        c.debug_preedit()
    );
}

#[test]
fn separator_claim_follows_profile() {
    if !has("schemas/shuangpin.schema.toml") {
        return;
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["shuangpin".into(), "pinyin".into()];
    cfg.schema.active = "shuangpin".into(); // separator 保持默认 auto
    cfg.input.default.chinese_mode = true;
    let desktop = headless(cfg.clone(), InputProfile::desktop());
    assert_eq!(
        desktop.manual_separator_char(),
        None,
        "桌面 auto 不动选词键"
    );
    let mobile = headless(cfg, InputProfile::mobile());
    assert_eq!(
        mobile.manual_separator_char(),
        Some('\''),
        "移动端 auto 夺取 `'`"
    );
}

#[test]
fn single_letter_limit_follows_profile() {
    if !has("schemas/pinyin.schema.toml") {
        return;
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into(), "wubi86".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    let desktop = headless(cfg.clone(), InputProfile::desktop());
    press(&desktop, VK_B);
    let desktop_n = desktop.debug_candidate_count();
    let mobile = headless(cfg, InputProfile::mobile());
    press(&mobile, VK_B);
    let mobile_n = mobile.debug_candidate_count();
    assert!(
        mobile_n <= 60,
        "移动端拼音单字母首批不超过上限，实际 {mobile_n}"
    );
    assert!(
        desktop_n > 60,
        "桌面不收紧，实际 {desktop_n}（应大于移动端上限）"
    );
}

#[test]
fn assoc_config_follows_overlay() {
    let mut cfg = Config::default();
    cfg.input.association.kind = "off".into();
    cfg.mobile.association.kind = "smart".into();
    let base = Coordinator::new_headless(cfg.clone(), None);
    assert_eq!(base.debug_assoc_kind(), "off", "桌面取基线段");
    let mobile = Coordinator::new_headless_with_profile(cfg, None, None, InputProfile::mobile()).0;
    assert_eq!(
        mobile.debug_assoc_kind(),
        "smart",
        "移动端取 [mobile.association]"
    );
}

#[test]
fn engine_grammar_follows_overlay() {
    let mut cfg = Config::default();
    cfg.schema.pinyin.grammar.weight = 0.0;
    cfg.mobile.grammar.weight = 0.7;
    let base = Coordinator::new_headless(cfg.clone(), None);
    assert_eq!(base.debug_pinyin_grammar_weight(), 0.0, "桌面看基线");
    let mobile = Coordinator::new_headless_with_profile(cfg, None, None, InputProfile::mobile()).0;
    assert_eq!(
        mobile.debug_pinyin_grammar_weight(),
        0.7,
        "[mobile.grammar] 只在 Mobile 下生效"
    );
}
