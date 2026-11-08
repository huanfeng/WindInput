//! 快捷输入专用选词上屏键（`keys.quick_input_select`，出厂 Ctrl+数字）与
//! 「数字透镜下字母选词」开关（`schema.mix_modes[*].numeric_letter_select`）。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use wind_bridge::handler::{CaretData, KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_config::config::FreeInputMode;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_ALT, MOD_CTRL, MOD_SHIFT, caret_source};
use wind_ui_types::UiCommand;

const VK_SEMICOLON: u32 = 0xBA;
const VK_NEXT: u32 = 0x22;
const VK_OEM_PLUS: u32 = 0xBB;
const VK_OEM_PERIOD: u32 = 0xBE;
const VK_NUMPAD0: u32 = 0x60;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    ["wubi86", "pinyin", "english"]
        .iter()
        .all(|s| d.join(format!("schemas/{s}.schema.toml")).exists())
}

macro_rules! skip_without_data {
    () => {
        if !has_data() {
            eprintln!("跳过：缺 build_dev 词库");
            return;
        }
    };
}

fn key(k: u32, modifiers: u32) -> KeyEventData {
    KeyEventData {
        key_code: k,
        scan_code: 0,
        modifiers,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

fn config() -> Config {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.ui.candidate.per_page = 10;
    c
}

/// 逐键敲入：小写字母 / 数字原样，大写字母带 Shift，`.` `+` 按美式键位。
fn type_str(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        let (vk, m) = match ch {
            'a'..='z' | '0'..='9' => ((ch.to_ascii_uppercase() as u32) & 0xFF, 0),
            'A'..='Z' => (ch as u32, MOD_SHIFT),
            '+' => (VK_OEM_PLUS, MOD_SHIFT),
            '.' => (VK_OEM_PERIOD, 0),
            _ => panic!("type_str 不认识 {ch:?}"),
        };
        coord.handle_key_event(&key(vk, m));
    }
}

fn enter_quick(coord: &Coordinator) {
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    assert_eq!(
        coord.debug_active_mode(),
        Some("mix"),
        "前提：`;` 进快捷输入"
    );
}

/// 独立 redb（置顶落 shadow 规则要有 store）。文件名带 pid：并行跑测试时固定名会互删。
fn fresh_store(tag: &str) -> (std::sync::Arc<wind_store::Store>, PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "wind_quick_select_{tag}_{}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let store = std::sync::Arc::new(wind_store::Store::open(&path).unwrap());
    (store, path)
}

fn ctrl_digit(coord: &Coordinator, d: u8) -> KeyAction {
    coord.handle_key_event(&key(u32::from(b'0' + d), MOD_CTRL))
}

fn committed(act: &KeyAction) -> Option<String> {
    match act {
        KeyAction::InsertText { text, .. } => Some(text.clone()),
        _ => None,
    }
}

/// Ctrl+N 上屏当前页第 N 个；Ctrl+0 是第 10 个。
#[test]
fn ctrl_digit_commits_page_candidate() {
    skip_without_data!();
    let coord = Coordinator::new_headless(config(), Some(&data_dir()));
    for (d, idx) in [(2u8, 1usize), (0, 9)] {
        enter_quick(&coord);
        type_str(&coord, "shi");
        let page = coord.debug_page_texts();
        assert!(page.len() >= 10, "前提：`;shi` 一页满 10 条，实得 {page:?}");
        let act = ctrl_digit(&coord, d);
        assert_eq!(
            committed(&act).as_deref(),
            Some(page[idx].as_str()),
            "Ctrl+{d} 应上屏第 {} 个，实得 {act:?}",
            idx + 1
        );
        assert_eq!(coord.debug_active_mode(), None, "整段上屏后退出快捷输入");
    }
}

/// 翻页后 Ctrl+1 选的是新页的第 1 个。
#[test]
fn ctrl_digit_follows_page() {
    skip_without_data!();
    let coord = Coordinator::new_headless(config(), Some(&data_dir()));
    enter_quick(&coord);
    type_str(&coord, "shi");
    let first_page = coord.debug_page_texts();
    coord.handle_key_event(&key(VK_NEXT, 0));
    let second_page = coord.debug_page_texts();
    assert_ne!(first_page, second_page, "前提：PageDown 翻到第二页");
    let act = ctrl_digit(&coord, 1);
    assert_eq!(committed(&act).as_deref(), Some(second_page[0].as_str()));
}

/// 数字透镜里数字键在录算式，Ctrl+数字照样选得到。
#[test]
fn ctrl_digit_selects_in_numeric_lens() {
    skip_without_data!();
    let coord = Coordinator::new_headless(config(), Some(&data_dir()));
    enter_quick(&coord);
    type_str(&coord, "1+2");
    let page = coord.debug_page_texts();
    assert!(!page.is_empty(), "前提：`;1+2` 有算式候选");
    let act = ctrl_digit(&coord, 1);
    assert_eq!(committed(&act).as_deref(), Some(page[0].as_str()));
}

/// ★ 缺陷回归：没有候选时按 Ctrl+数字 / Ctrl+Shift+数字 不得放弃整段输入。
#[test]
fn ctrl_digit_without_candidates_keeps_input() {
    skip_without_data!();
    let coord = Coordinator::new_headless(config(), Some(&data_dir()));
    // 空缓冲与零候选的缓冲各一例（`.` 是算式字符却换不来候选）。
    for typed in ["", "."] {
        enter_quick(&coord);
        type_str(&coord, typed);
        assert!(
            coord.debug_page_texts().is_empty(),
            "前提：`;{typed}` 没有候选，实得 {:?}",
            coord.debug_page_texts()
        );
        let before = coord.debug_preedit();
        for mods in [MOD_CTRL, MOD_CTRL | MOD_SHIFT] {
            let act = coord.handle_key_event(&key(u32::from(b'1'), mods));
            assert!(
                matches!(act, KeyAction::Consumed),
                "`;{typed}` mods=0x{mods:X} 应吃键，实得 {act:?}"
            );
            assert_eq!(coord.debug_active_mode(), Some("mix"), "仍在快捷输入");
            assert_eq!(coord.debug_preedit(), before, "组合原样保留");
        }
        coord.handle_key_event(&key(0x1B, 0)); // Esc
    }
}

/// ★ 缺陷回归：有候选时 Ctrl+Shift+数字（删除模板）也不得放弃整段输入。
#[test]
fn ctrl_shift_digit_with_candidates_keeps_input() {
    skip_without_data!();
    let coord = Coordinator::new_headless(config(), Some(&data_dir()));
    enter_quick(&coord);
    type_str(&coord, "shi");
    let page = coord.debug_page_texts();
    let act = coord.handle_key_event(&key(u32::from(b'2'), MOD_CTRL | MOD_SHIFT));
    assert!(matches!(act, KeyAction::Consumed), "实得 {act:?}");
    assert_eq!(coord.debug_active_mode(), Some("mix"));
    assert_eq!(coord.debug_page_texts(), page, "候选原样保留");
}

/// 主输入路不受影响：Ctrl+数字 仍是置顶。
#[test]
fn main_path_ctrl_digit_still_pins() {
    skip_without_data!();
    let mut cfg = config();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    let (store, path) = fresh_store("main_pin");
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
    type_str(&coord, "shi");
    let before = coord.debug_page_texts();
    assert!(before.len() >= 3, "前提：shi 至少 3 个候选");
    let act = ctrl_digit(&coord, 3);
    assert!(
        matches!(act, KeyAction::Consumed),
        "置顶应吃键，实得 {act:?}"
    );
    assert_eq!(coord.debug_active_mode(), None);
    assert_eq!(coord.debug_page_texts()[0], before[2], "第 3 个应被置顶");
    drop(coord);
    let _ = std::fs::remove_file(&path);
}

/// 选词键只认快捷输入：临时拼音里 Ctrl+数字 与「选词键不绑」时逐项相同（返回值、模式、
/// 编码栏、候选），不上屏候选。
#[test]
fn other_overlays_keep_pin_semantics() {
    skip_without_data!();
    let run = |select: &str| {
        let mut cfg = config();
        cfg.input.temp_pinyin.enabled = true;
        cfg.schema.codetable.z_key_action = "temp_pinyin".into();
        cfg.keys.quick_input_select = select.into();
        let (store, path) = fresh_store(&format!("temp_pin_{}", select.replace('+', "_")));
        let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
        type_str(&coord, "zshi");
        assert_eq!(
            coord.debug_active_mode(),
            Some("temp_pinyin"),
            "前提：z 进临拼"
        );
        assert!(
            coord.debug_page_texts().len() >= 3,
            "前提：临拼 shi 至少 3 个候选"
        );
        let act = ctrl_digit(&coord, 3);
        let out = (
            format!("{act:?}"),
            coord.debug_active_mode(),
            coord.debug_preedit(),
            coord.debug_page_texts(),
        );
        drop(coord);
        let _ = std::fs::remove_file(&path);
        (act, out)
    };
    let (act, with_key) = run("ctrl+number");
    let (_, without_key) = run("none");
    assert!(
        committed(&act).is_none(),
        "临拼里 Ctrl+3 不应上屏，实得 {act:?}"
    );
    assert_eq!(with_key, without_key, "选词键不得改变临拼里 Ctrl+3 的结局");
}

/// ★ 缺陷回归：临拼没有词库落点，置顶 / 删除模板按下去吃键、整段输入原样保留
/// （此前落到 overlay 的 Ctrl/Alt 守卫，回 ClearComposition 清空整段）。
#[test]
fn temp_pinyin_pin_template_keeps_input() {
    skip_without_data!();
    let mut cfg = config();
    cfg.input.temp_pinyin.enabled = true;
    cfg.schema.codetable.z_key_action = "temp_pinyin".into();
    let (store, path) = fresh_store("temp_pin_keep");
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
    type_str(&coord, "zshi");
    assert_eq!(
        coord.debug_active_mode(),
        Some("temp_pinyin"),
        "前提：z 进临拼"
    );
    let preedit = coord.debug_preedit();
    let page = coord.debug_page_texts();
    assert!(page.len() >= 3, "前提：临拼 shi 至少 3 个候选");
    for mods in [MOD_CTRL, MOD_CTRL | MOD_SHIFT] {
        let act = coord.handle_key_event(&key(u32::from(b'3'), mods));
        assert!(
            matches!(act, KeyAction::Consumed),
            "mods=0x{mods:X} 应吃键，实得 {act:?}"
        );
        assert_eq!(coord.debug_active_mode(), Some("temp_pinyin"), "仍在临拼");
        assert_eq!(coord.debug_preedit(), preedit, "编码栏原样保留");
        assert_eq!(coord.debug_page_texts(), page, "候选原样保留");
    }
    // 宿主快捷键（非置顶/删除模板）仍走兜底守卫：放弃整段。
    let act = coord.handle_key_event(&key(u32::from(b'A'), MOD_CTRL));
    assert!(
        matches!(act, KeyAction::ClearCompositionThenPassThrough),
        "Ctrl+A 仍应清组合，实得 {act:?}"
    );
    drop(coord);
    let _ = std::fs::remove_file(&path);
}

/// 空缓冲的 overlay（直达热键进的临拼，不写引导符）同样吃键：结局恒为 Consumed，
/// 不因「没有待输入」改回透传。
#[test]
fn empty_overlay_pin_template_is_consumed() {
    skip_without_data!();
    let mut cfg = config();
    cfg.input.temp_pinyin.enabled = true;
    cfg.input.temp_pinyin.hotkey = "ctrl+shift+p".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    coord.handle_key_event(&key(u32::from(b'P'), MOD_CTRL | MOD_SHIFT));
    assert_eq!(
        coord.debug_active_mode(),
        Some("temp_pinyin"),
        "前提：直达热键进临拼"
    );
    assert!(
        coord.debug_page_texts().is_empty(),
        "前提：缓冲为空、无候选"
    );
    for mods in [MOD_CTRL, MOD_CTRL | MOD_SHIFT] {
        let act = coord.handle_key_event(&key(u32::from(b'3'), mods));
        assert!(
            matches!(act, KeyAction::Consumed),
            "mods=0x{mods:X} 应吃键，实得 {act:?}"
        );
        assert_eq!(coord.debug_active_mode(), Some("temp_pinyin"), "仍在临拼");
    }
}

/// 小键盘 Ctrl+数字（`numpad_behavior = direct`，键码 0x60..0x69）在快捷输入里同样选词。
#[test]
fn ctrl_numpad_digit_commits_in_quick_input() {
    skip_without_data!();
    let coord = Coordinator::new_headless(config(), Some(&data_dir()));
    enter_quick(&coord);
    type_str(&coord, "shi");
    let page = coord.debug_page_texts();
    let act = coord.handle_key_event(&key(VK_NUMPAD0 + 2, MOD_CTRL));
    assert_eq!(
        committed(&act).as_deref(),
        Some(page[1].as_str()),
        "小键盘 Ctrl+2 应上屏第 2 个，实得 {act:?}"
    );
    // 数字透镜（数字键在录算式）里同样认。
    enter_quick(&coord);
    type_str(&coord, "1+2");
    let page = coord.debug_page_texts();
    let act = coord.handle_key_event(&key(VK_NUMPAD0 + 1, MOD_CTRL));
    assert_eq!(committed(&act).as_deref(), Some(page[0].as_str()));
}

/// 自定义选词键：`ctrl+alt+number` 生效；`none` 时 Ctrl+数字 不再选词。
#[test]
fn custom_select_key_config() {
    skip_without_data!();
    let mut cfg = config();
    cfg.keys.quick_input_select = "ctrl+alt+number".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    enter_quick(&coord);
    type_str(&coord, "shi");
    let page = coord.debug_page_texts();
    let act = coord.handle_key_event(&key(u32::from(b'2'), MOD_CTRL | MOD_ALT));
    assert_eq!(committed(&act).as_deref(), Some(page[1].as_str()));

    let mut cfg = config();
    cfg.keys.quick_input_select = "none".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    enter_quick(&coord);
    type_str(&coord, "shi");
    let act = ctrl_digit(&coord, 2);
    assert!(
        committed(&act).is_none(),
        "不绑时 Ctrl+2 不上屏，实得 {act:?}"
    );
}

/// 关掉「数字透镜下字母选词」：`;2fa`、`;1.5x` 字母直接进缓冲（自由输入开关两档都是）。
#[test]
fn letters_are_literal_when_letter_select_off() {
    skip_without_data!();
    for free in [FreeInputMode::Auto, FreeInputMode::Off] {
        let mut cfg = config();
        cfg.schema.mix_modes[0].numeric_letter_select = false;
        cfg.schema.mix_modes[0].free_input = free;
        let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
        for s in ["2fa", "1.5x"] {
            enter_quick(&coord);
            type_str(&coord, s);
            assert_eq!(
                coord.debug_active_mode(),
                Some("mix"),
                "{free:?} {s}: 仍在快捷输入"
            );
            assert_eq!(
                coord.debug_preedit(),
                format!(";{s}"),
                "{free:?} {s}: 字母应字面入缓冲"
            );
            coord.handle_key_event(&key(0x1B, 0)); // Esc
        }
    }
}

/// 反向对照：出厂（开）时数字透镜的字母仍是选词键。
#[test]
fn letters_select_when_letter_select_on() {
    skip_without_data!();
    let coord = Coordinator::new_headless(config(), Some(&data_dir()));
    enter_quick(&coord);
    type_str(&coord, "1+2");
    let page = coord.debug_page_texts();
    assert!(!page.is_empty(), "前提：`;1+2` 有算式候选");
    let act = coord.handle_key_event(&key(u32::from(b'A'), 0));
    assert_eq!(committed(&act).as_deref(), Some(page[0].as_str()));
}

/// 候选窗序号标签：开时数字透镜标 a/b/c，关时回到数字。
#[test]
fn numeric_lens_labels_follow_switch() {
    skip_without_data!();
    for (on, want) in [(true, "a"), (false, "1")] {
        let mut cfg = config();
        cfg.schema.mix_modes[0].numeric_letter_select = on;
        let (coord, rx) = Coordinator::new_headless_with_ui(cfg, Some(&data_dir()));
        coord.handle_key_event(&key(VK_SEMICOLON, 0));
        coord.handle_caret_update(&CaretData {
            x: 100,
            y: 200,
            height: 20,
            composition_start_x: 100,
            composition_start_y: 200,
            source: caret_source::TSF_SELECTION,
            composition_rect: None,
        });
        type_str(&coord, "1+2");
        let labels: Vec<String> = rx
            .try_iter()
            .filter_map(|cmd| match cmd {
                UiCommand::UpdateCandidates { candidates, .. } => Some(candidates),
                _ => None,
            })
            .last()
            .expect("应下发候选")
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert_eq!(
            labels.first().map(String::as_str),
            Some(want),
            "numeric_letter_select={on} 时首个标签"
        );
    }
}

/// 分派顺序（设置端冲突检测按它判）：`keys.key_actions` 的组合键先于选词键匹配，
/// 落在同一组合上时快捷输入里的选词被它抢走。
#[test]
fn key_action_combo_shadows_select_key() {
    skip_without_data!();
    let mut cfg = config();
    cfg.input.temp_pinyin.enabled = true;
    cfg.keys.quick_input_select = "ctrl+alt+number".into();
    cfg.keys
        .key_actions
        .insert("ctrl+alt+1".into(), "temp_pinyin".into());
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    enter_quick(&coord);
    type_str(&coord, "shi");
    let page = coord.debug_page_texts();
    // 同模板的其它数字仍归选词。
    let act = coord.handle_key_event(&key(u32::from(b'2'), MOD_CTRL | MOD_ALT));
    assert_eq!(committed(&act).as_deref(), Some(page[1].as_str()));

    enter_quick(&coord);
    type_str(&coord, "shi");
    coord.handle_key_event(&key(u32::from(b'1'), MOD_CTRL | MOD_ALT));
    assert_eq!(
        coord.debug_active_mode(),
        Some("temp_pinyin"),
        "Ctrl+Alt+1 应归 key_actions 那条绑定"
    );
}
