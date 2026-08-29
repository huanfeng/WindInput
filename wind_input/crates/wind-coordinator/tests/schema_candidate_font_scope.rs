//! 方案级候选字体（`[candidate] font_family`）的**归属判据**。
//!
//! 设计见 `docs/design/mongolian-vertical-candidates.md`。
//!
//! # ★★ 这组测试真正要钉住的只有一件事：归属取「数据方案」而不是「活跃方案」
//!
//! 同住 `[candidate]` 段的 `layout` 取 `active_behavior`（它是用户可见的呈现态），
//! 而 `font_family` 取 `effective_data_schema`（「这些字用什么渲染」＝数据属性，
//! 与 `[phrases]`、`[punct] custom_mappings` 同源）。
//!
//! **看见同一个段就统一判据是错的**——`[punct]` 的 `mode` 与 `custom_mappings` 已经
//! 踩过一模一样的形状（见 `docs/design/schema-scoped-behavior.md`）。取错的表现是：
//! 临时英文期间候选归 `english` 桶、字体却还跟着五笔走，而两边看各自都「对」。
//!
//! ⇒ 只测「五笔方案下拿到五笔的字体」是不够的：那条在两种判据下都通过。
//! 必须有一条**进入临英**后再问一次的用例，两条一起才把归属钉住。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

/// 两个方案定义都要在——缺任一个，本组断言都会以「字体从没变过」的方式静默通过。
fn ready() -> bool {
    data_dir().join("schemas/wubi86.schema.toml").exists()
        && data_dir().join("schemas/english.schema.toml").exists()
}

fn key(vk: u32, modifiers: u32) -> KeyEventData {
    KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

/// 造一组只含 `[candidate] font_family` 的方案 override。
fn font_overrides(tag: &str, entries: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wind_candfont_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (schema_id, family) in entries {
        std::fs::write(
            dir.join(format!("{schema_id}.toml")),
            format!("[candidate]\nfont_family = \"{family}\"\n"),
        )
        .unwrap();
    }
    dir
}

fn cfg() -> Config {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.input.temp_english.enabled = true;
    c
}

fn coord_with(tag: &str, entries: &[(&str, &str)]) -> Arc<Coordinator> {
    let ov = font_overrides(tag, entries);
    Coordinator::new_headless_with_override(cfg(), Some(&data_dir()), Some(ov))
}

/// 不声明 `[candidate] font_family` 时返回空串 = 不覆盖。这是零回归基线。
#[test]
fn undeclared_font_is_empty() {
    if !ready() {
        eprintln!("跳过：缺少 wubi86 / english 方案");
        return;
    }
    let coord = coord_with("none", &[]);
    assert_eq!(coord.debug_candidate_font(), "");
}

/// 活跃方案声明了字体就拿到它。
#[test]
fn active_schema_font_is_picked_up() {
    if !ready() {
        eprintln!("跳过：缺少 wubi86 / english 方案");
        return;
    }
    let coord = coord_with("active", &[("wubi86", "Mongolian Baiti")]);
    assert_eq!(coord.debug_candidate_font(), "Mongolian Baiti");
}

/// ★★ 进入临时英文后，字体必须切到 `english` 方案声明的那个。
///
/// 这条是本组的核心：判据若写成 `active_behavior`，临英期间仍会返回五笔的字体，
/// 而 `active_schema_font_is_picked_up` 那条照样绿。
#[test]
fn temp_english_takes_the_english_schema_font() {
    if !ready() {
        eprintln!("跳过：缺少 wubi86 / english 方案");
        return;
    }
    let coord = coord_with(
        "tempen",
        &[("wubi86", "Mongolian Baiti"), ("english", "Consolas")],
    );
    assert_eq!(
        coord.debug_candidate_font(),
        "Mongolian Baiti",
        "前置条件：未进临英时应是五笔方案的字体"
    );
    // Shift+首字母进入临英（与 english_head_candidates.rs 同一入口）。
    coord.handle_key_event(&key('H' as u32, MOD_SHIFT));
    coord.handle_key_event(&key('I' as u32, 0));
    assert_eq!(
        coord.debug_candidate_font(),
        "Consolas",
        "临英期间字体应归 english 方案——归属判据取成活跃方案了"
    );
    // 退出后自动回落，不需要任何「恢复」动作（判据是每次重算出来的，不是保存/回放）。
    coord.handle_key_event(&key(0x1B, 0)); // Esc
    assert_eq!(coord.debug_candidate_font(), "Mongolian Baiti");
}
