//! 方案级通用 UI 覆盖（`[candidate]` 下写 `ui.candidate.*` 同名键）的**生效与隔离**。
//!
//! 首个使用者是 `per_page`。设计见 `docs/design/schema-scoped-behavior.md` 的
//! 「通用折叠」一节。本组钉住四件事：
//! 1. 方案声明了就生效，没声明就是全局值（零回归基线）；
//! 2. 切方案后跟着换，切回来也跟着回（不是一次性写死）；
//! 3. 只覆盖主档：`per_page_extended > 0` 的 overlay 档不受影响，`= 0` 时跟随**生效的**主档；
//! 4. 写错的值整条丢弃，回落全局，而不是变成 0 / 默认 7。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_CTRL, MOD_SHIFT};

const VK_P: u32 = 0x50;
const CTRL_SHIFT: u32 = MOD_CTRL | MOD_SHIFT;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

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

/// 全局 `per_page = 7`、扩展档由调用方定；ctrl+shift+p 切到 english。
fn cfg(extended: usize) -> Config {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.ui.candidate.per_page = 7;
    c.ui.candidate.per_page_extended = extended;
    c.keys
        .key_actions
        .insert("ctrl+shift+p".into(), "switch_schema:english".into());
    c
}

fn coord(tag: &str, extended: usize, entries: &[(&str, &str)]) -> Arc<Coordinator> {
    let dir = std::env::temp_dir().join(format!("wind_uifold_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (id, body) in entries {
        std::fs::write(dir.join(format!("{id}.toml")), body).unwrap();
    }
    Coordinator::new_headless_with_override(cfg(extended), Some(&data_dir()), Some(dir))
}

#[test]
fn undeclared_follows_global() {
    if !ready() {
        return;
    }
    let c = coord("none", 0, &[]);
    assert_eq!(c.debug_per_page(false), 7);
}

#[test]
fn declared_per_page_overrides_global() {
    if !ready() {
        return;
    }
    let c = coord("decl", 0, &[("wubi86", "[candidate]\nper_page = 3\n")]);
    assert_eq!(c.debug_per_page(false), 3);
}

/// 切到没声明的方案回到全局，且声明了的方案切回来再次生效——不是一次性写死。
#[test]
fn follows_active_schema_across_switches() {
    if !ready() {
        return;
    }
    let c = coord("switch", 0, &[("wubi86", "[candidate]\nper_page = 3\n")]);
    assert_eq!(c.debug_per_page(false), 3);
    c.handle_key_event(&key(VK_P, CTRL_SHIFT));
    assert_eq!(c.debug_per_page(false), 7, "english 没声明，应回到全局值");
    // switch_schema:english 是单向直达；回来用 wubi86 的直达键。
    let mut_c = coord("switch2", 0, &[("english", "[candidate]\nper_page = 5\n")]);
    assert_eq!(mut_c.debug_per_page(false), 7);
    mut_c.handle_key_event(&key(VK_P, CTRL_SHIFT));
    assert_eq!(mut_c.debug_per_page(false), 5, "切到声明了的方案应生效");
}

/// 扩展档 > 0：overlay 模式仍用全局扩展档，不被方案的主档覆盖。
#[test]
fn extended_tier_is_not_overridden() {
    if !ready() {
        return;
    }
    let c = coord("ext", 9, &[("wubi86", "[candidate]\nper_page = 3\n")]);
    assert_eq!(c.debug_per_page(false), 3);
    assert_eq!(c.debug_per_page(true), 9);
}

/// 扩展档 = 0（「与主档相同」）：overlay 跟随**生效的**主档，而不是全局主档。
#[test]
fn zero_extended_follows_effective_main_tier() {
    if !ready() {
        return;
    }
    let c = coord("ext0", 0, &[("wubi86", "[candidate]\nper_page = 3\n")]);
    assert_eq!(c.debug_per_page(true), 3);
}

/// 写错的值（0 / 负数 / 类型不对）整条丢弃回落全局，并进降级清单。
#[test]
fn bad_value_falls_back_to_global() {
    if !ready() {
        return;
    }
    for (i, bad) in ["0", "-2", "\"many\""].iter().enumerate() {
        let c = coord(
            &format!("bad{i}"),
            0,
            &[("wubi86", &format!("[candidate]\nper_page = {bad}\n"))],
        );
        assert_eq!(c.debug_per_page(false), 7, "per_page = {bad} 不该生效");
    }
}

/// 竖排补位行的上限跟着生效的每页数走：全局 min_rows=7，方案每页 3 ⇒ 最多补 3 行。
#[test]
fn min_rows_clamp_uses_effective_per_page() {
    if !ready() {
        return;
    }
    let dir = std::env::temp_dir().join("wind_uifold_minrows");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("wubi86.toml"), "[candidate]\nper_page = 3\n").unwrap();
    let mut conf = cfg(0);
    conf.ui.candidate.min_rows = 7;
    let c = Coordinator::new_headless_with_override(conf, Some(&data_dir()), Some(dir));
    assert_eq!(c.debug_candidate_min_rows(), 3);
}
