//! macOS 式方向键（`design/macos-style-arrow-keys.md`）：`session_actions` 的
//! `caret_left` / `caret_right` 两动词端到端。
//!
//! - `caret_left`：编码区光标左移（← 从末尾进入编辑编码），开头吞键，无会话放行；
//! - `caret_right`：光标右移，**到末尾 = 上屏高亮候选**（macOS「→ 进入选词」的边沿语义）；
//! - overlay（临拼等）有自己的缓冲光标，本动词让位；
//! - 出厂不绑这两个动词，←→ 仍是移动高亮（配方由用户自选）。
//!
//! 词典缺失时整族静默跳过——核对 `--nocapture` 里有没有「跳过：」。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_keys::keymap::{VK_BACKTICK, VK_DOWN, VK_LEFT, VK_RIGHT};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn data_ready() -> bool {
    let d = data_dir();
    let ok = d.join("schemas/pinyin/rime_frost.dict.yaml").exists();
    if !ok {
        eprintln!("跳过：词库不存在");
    }
    ok
}

fn key_event(key_code: u32) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

fn press_vk(coord: &Coordinator, vk: u32) -> KeyAction {
    coord.handle_key_event(&key_event(vk))
}

fn press(coord: &Coordinator, code: &str) -> KeyAction {
    let mut last = KeyAction::Consumed;
    for c in code.chars() {
        last = coord.handle_key_event(&key_event((c.to_ascii_uppercase() as u32) & 0xFF));
    }
    last
}

fn committed(action: &KeyAction) -> Option<&str> {
    match action {
        KeyAction::InsertText { text, .. } => Some(text.as_str()),
        _ => None,
    }
}

/// B 档配方（design §可选策略）：←→ 从 highlight_keys 摘掉，绑成光标移动。
/// ↑↓ 保持出厂移高亮（本文件不涉）。
fn macos_config() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    cfg.keys.highlight_keys = vec!["tab".into()];
    cfg.keys
        .session_actions
        .insert("left".into(), "caret_left".into());
    cfg.keys
        .session_actions
        .insert("right".into(), "caret_right".into());
    cfg
}

fn coord(cfg: Config) -> std::sync::Arc<Coordinator> {
    Coordinator::new_headless(cfg, Some(&data_dir()))
}

/// ← 从末尾进入编辑：光标进编码区，随后打的字符**插在光标处**，候选按整串重算。
#[test]
fn left_enters_code_and_mid_insert_requeries() {
    if !data_ready() {
        return;
    }
    let c = coord(macos_config());
    press(&c, "hi");
    assert_eq!(c.debug_input_buffer(), "hi");
    let caret_end = c.debug_ui_caret_bytes();

    let act = press_vk(&c, VK_LEFT);
    assert!(
        matches!(act, KeyAction::UpdateComposition { .. }),
        "实际: {act:?}"
    );
    assert_eq!(c.debug_input_buffer(), "hi", "移动不改缓冲");
    let caret_left = c.debug_ui_caret_bytes();
    assert_eq!(caret_end - caret_left, 1, "左移一个字符");

    // 光标处插入：`hi` → `hai`，候选按整串重查（「海」进首页）。
    press(&c, "a");
    assert_eq!(c.debug_input_buffer(), "hai");
    assert!(
        c.debug_page_texts().iter().any(|t| t == "海"),
        "hai 的候选应含「海」，实际: {:?}",
        c.debug_page_texts()
    );
}

/// `caret_right` 到末尾 = 上屏高亮候选（macOS「→ 进入选词」）。
#[test]
fn right_at_end_commits_highlighted() {
    if !data_ready() {
        return;
    }
    let c = coord(macos_config());
    press(&c, "nihao");
    let first = c.debug_page_texts()[0].clone();
    let act = press_vk(&c, VK_RIGHT);
    assert_eq!(
        committed(&act),
        Some(first.as_str()),
        "末尾的 → 应回上屏高亮候选，实际: {act:?}"
    );
    assert!(c.debug_input_buffer().is_empty(), "上屏后组合应结束");
}

/// 光标在编码中间时 `caret_right` 只是右移，不到末尾不上屏。
#[test]
fn right_inside_code_only_moves() {
    if !data_ready() {
        return;
    }
    let c = coord(macos_config());
    press(&c, "nihao");
    press_vk(&c, VK_LEFT);
    press_vk(&c, VK_LEFT);
    let before = c.debug_ui_caret_bytes();
    let act = press_vk(&c, VK_RIGHT);
    assert!(
        matches!(act, KeyAction::UpdateComposition { .. }),
        "中间的 → 只右移，实际: {act:?}"
    );
    assert_eq!(c.debug_ui_caret_bytes() - before, 1);
    assert_eq!(c.debug_input_buffer(), "nihao", "缓冲不动");
    assert!(!c.debug_page_texts().is_empty(), "组合仍在");
}

/// 已在开头：`caret_left` 吞键、光标不动——组合进行中把 ← 交给宿主会让宿主光标
/// 与组合区错位（macOS 同款也是吞）。
#[test]
fn left_at_start_swallowed() {
    if !data_ready() {
        return;
    }
    let c = coord(macos_config());
    press(&c, "hi");
    press_vk(&c, VK_LEFT);
    press_vk(&c, VK_LEFT);
    let act = press_vk(&c, VK_LEFT);
    assert!(matches!(act, KeyAction::Consumed), "实际: {act:?}");
    assert_eq!(c.debug_input_buffer(), "hi", "缓冲不动");
}

/// 无会话放行：空闲 ← / → 是宿主的方向键（导航类「无事可做即放行」的同一守卫）。
#[test]
fn no_session_passthrough() {
    if !data_ready() {
        return;
    }
    let c = coord(macos_config());
    assert!(matches!(press_vk(&c, VK_LEFT), KeyAction::PassThrough));
    assert!(matches!(press_vk(&c, VK_RIGHT), KeyAction::PassThrough));
}

/// overlay 让位：临拼有自己的缓冲光标，`left = caret_left` 绑定**不抢**它的 ←。
/// 临拼 `nihao` 按 ← 后打 `a` 应插在光标处（既有行为），而不是落到主路。
#[test]
fn overlay_temp_pinyin_yields() {
    if !data_ready() {
        return;
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.keys
        .session_actions
        .insert("left".into(), "caret_left".into());
    let c = coord(cfg);

    press_vk(&c, VK_BACKTICK);
    assert!(c.debug_in_temp_pinyin(), "前提：反引号进临拼");
    press(&c, "nihao");
    let before = c.debug_page_texts();
    assert!(!before.is_empty(), "前提：临拼 nihao 有候选");

    press_vk(&c, VK_LEFT);
    press(&c, "a");
    assert!(
        c.debug_in_temp_pinyin(),
        "← 仍在临拼内编辑，不得落主路 / 退模式"
    );
    let after = c.debug_page_texts();
    assert_ne!(
        before, after,
        "插入在临拼光标处（niha|ao），候选应变；若与插入前相同，说明 ← 没走到临拼自己的光标臂"
    );
}

/// 出厂回归：不绑 `caret_*` 时 ↑↓ 仍是移动高亮（`arrows` 组出厂只收 up/down，
/// left/right 本就没有导航绑定——故「←→ 不被本特性改动」由「无会话放行 + 未绑定不消费」
/// 两条既有守卫保证，不需要专门用例）。高亮动了 → 空格上屏的不再是原首选；
/// 行为级断言：集成测试够不着高亮下标，用「↓ 之后空格选中谁」观察。
#[test]
fn default_down_still_moves_highlight() {
    if !data_ready() {
        return;
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    let c = coord(cfg);
    press(&c, "gong");
    assert!(c.debug_page_texts().len() >= 2, "前提：gong 多候选");
    let first_before = c.debug_page_texts()[0].clone();
    let act = press_vk(&c, VK_DOWN);
    assert!(
        matches!(
            act,
            KeyAction::Consumed | KeyAction::UpdateComposition { .. }
        ),
        "出厂 ↓ = 移高亮（或其重绘），实际: {act:?}"
    );
    let act = press_vk(&c, wind_keys::keymap::VK_SPACE);
    let picked = committed(&act);
    assert_ne!(
        picked,
        Some(first_before.as_str()),
        "↓ 应把高亮移走；空格仍选中首选说明出厂行为被本特性改动（picked={picked:?}）"
    );
}
