//! 快捷输入（`quick_mix` 的 english 成员）里英文上屏对齐临时英文（A2-3b 批 2）。
//!
//! 用户拍板：快捷输入里的英文**一律读临英那份开关**（`input.temp_english.*`），不另立第三份。
//! 本文件钉两件事：
//!
//! - **补空格**（`input.temp_english.commit_space`）：选中「算英文」的候选（英文词库来源 / 所打
//!   原文）上屏后补一个空格；空格、数字键、鼠标点选同一出口。中文候选、回车上屏原文不补。
//! - **全角**：全角态下英文候选、回车原文转全角，补的是全角空格——与临英同口径。
//!
//! 英文方案那份 `schema.english.commit_space` 在各用例里刻意置反：两份取值相同时，读错了
//! 开关也照样全绿。
//!
//! ⚠️ 词典缺失时整族静默跳过（判据是耗时而非通过条数），worktree 需自备 `build_dev`。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;

const VK_SEMICOLON: u32 = 0xBA;
const VK_SPACE: u32 = 0x20;
const VK_RETURN: u32 = 0x0D;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_english_schema() -> bool {
    let d = data_dir();
    d.join("schemas/english.schema.toml").exists() && d.join("schemas/english").is_dir()
}

fn key(key_code: u32) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

/// 主方案取五笔：快捷输入的英文成员与活跃方案无关。
fn quick_config(commit_space: bool) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "english".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.temp_english.commit_space = commit_space;
    cfg.schema.english.commit_space = !commit_space;
    cfg
}

fn coord_with(cfg: Config, tag: &str) -> Arc<Coordinator> {
    let path = std::env::temp_dir().join(format!("wind_quick_en_commit_{tag}.redb"));
    let _ = std::fs::remove_file(&path);
    Coordinator::new_headless_with_store(
        cfg,
        Some(&data_dir()),
        Arc::new(Store::open(&path).unwrap()),
    )
}

/// `;` 进快捷输入，再逐字母打 `word`。
fn enter_quick(coord: &Coordinator, word: &str) {
    coord.handle_key_event(&key(VK_SEMICOLON));
    for ch in word.chars() {
        coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
}

fn commit_text(action: &KeyAction) -> String {
    match action {
        KeyAction::InsertText { text, .. } => text.clone(),
        other => panic!("期望上屏动作，实际: {other:?}"),
    }
}

/// 页内第一条等于 `want` 的候选下标（前提断言）。
fn index_of(coord: &Coordinator, want: &str) -> usize {
    let page = coord.debug_page_texts();
    page.iter()
        .position(|t| t == want)
        .unwrap_or_else(|| panic!("前提：候选页里应有 `{want}`，实际: {page:?}"))
}

/// 数字键选中页内下标 `i` 的候选。
fn press_digit_for(coord: &Coordinator, i: usize) -> KeyAction {
    coord.handle_key_event(&key(0x31 + i as u32))
}

/// 空格上屏高亮候选。取 `very`：`v` 开头拼音成员给不出候选，首选必是英文词库那条
/// （`hello` 之类会被拼音 `he` 占满前几页——「成员顺序即优先级」，不是本文件要测的）。
#[test]
fn space_commit_appends_space() {
    if !has_english_schema() {
        return;
    }
    let c = coord_with(quick_config(true), "space_on");
    enter_quick(&c, "very");
    let page = c.debug_page_texts();
    assert_eq!(
        page.first().map(String::as_str),
        Some("very"),
        "前提：`;very` 首选应是 very，实际: {page:?}"
    );
    let act = c.handle_key_event(&key(VK_SPACE));
    assert_eq!(commit_text(&act), "very ");
}

/// 数字键选词库英文候选同样补空格。
#[test]
fn digit_select_english_appends_space() {
    if !has_english_schema() {
        return;
    }
    let c = coord_with(quick_config(true), "digit_on");
    enter_quick(&c, "vim");
    let i = index_of(&c, "Vimium");
    let act = press_digit_for(&c, i);
    assert_eq!(commit_text(&act), "Vimium ");
}

/// 鼠标点选（`select_candidate_at` → `mix_select_at`）同一出口，同样补空格。
#[test]
fn mouse_select_appends_space() {
    if !has_english_schema() {
        return;
    }
    let c = coord_with(quick_config(true), "mouse_on");
    enter_quick(&c, "vim");
    let i = index_of(&c, "Vim");
    let act = c.debug_mouse_select(i).expect("鼠标点选应带回上屏动作");
    assert_eq!(commit_text(&act), "Vim ");
}

/// 反向对照：临英那份关着（英文方案那份开着）时不补。
#[test]
fn no_space_when_temp_english_switch_off() {
    if !has_english_schema() {
        return;
    }
    let c = coord_with(quick_config(false), "space_off");
    enter_quick(&c, "very");
    let act = c.handle_key_event(&key(VK_SPACE));
    assert_eq!(commit_text(&act), "very");
}

/// ★ 中文候选不补：开关只作用于英文内容，快捷输入里的拼音字词照旧。
#[test]
fn chinese_candidate_never_appends_space() {
    if !has_english_schema() {
        return;
    }
    let c = coord_with(quick_config(true), "zh");
    enter_quick(&c, "nihao");
    let page = c.debug_page_texts();
    let first = page.first().cloned().expect("前提：`;nihao` 应有候选");
    assert!(
        !first.is_ascii(),
        "前提：`;nihao` 首选应是中文，实际: {page:?}"
    );
    let act = c.handle_key_event(&key(VK_SPACE));
    assert_eq!(commit_text(&act), first, "中文候选上屏不该补空格");
}

/// 回车上屏原文：终结性动作，不补空格（与临英回车同口径）。
#[test]
fn enter_commits_raw_without_space() {
    if !has_english_schema() {
        return;
    }
    let c = coord_with(quick_config(true), "enter");
    enter_quick(&c, "very");
    let act = c.handle_key_event(&key(VK_RETURN));
    assert_eq!(commit_text(&act), "very");
}

/// 全角态：英文候选转全角，补的是全角空格（U+3000），与临英 `commit_temp_english_text` 一致。
#[test]
fn full_width_converts_english_and_space() {
    if !has_english_schema() {
        return;
    }
    let c = coord_with(quick_config(true), "fw_space");
    c.handle_menu_command("toggle_width");
    enter_quick(&c, "very");
    let act = c.handle_key_event(&key(VK_SPACE));
    assert_eq!(commit_text(&act), "ｖｅｒｙ\u{3000}");
}

/// 全角态回车上屏原文同样转全角（对齐临英回车）。
#[test]
fn full_width_enter_converts_raw() {
    if !has_english_schema() {
        return;
    }
    let c = coord_with(quick_config(false), "fw_enter");
    c.handle_menu_command("toggle_width");
    enter_quick(&c, "very");
    let act = c.handle_key_event(&key(VK_RETURN));
    assert_eq!(commit_text(&act), "ｖｅｒｙ");
}

/// 空格兜底（无候选时上屏缓冲原文）：与临英空格兜底同口径，补空格。
/// 取 `vqzx`：拼音成员不收 `v` 起头，英文词库也无此前缀，候选为空。
#[test]
fn space_fallback_appends_space() {
    if !has_english_schema() {
        return;
    }
    // 原文候选与大小写变形关掉：它们开着时 `vqzx` 本身就是一条候选（批 3 的英文段段首），
    // 走不到兜底分支。
    let mut cfg = quick_config(true);
    cfg.input.temp_english.raw_candidate = wind_config::config::RawCandidateMode::Off;
    cfg.input.temp_english.case_variants = false;
    let c = coord_with(cfg, "fallback");
    enter_quick(&c, "vqzx");
    assert!(
        c.debug_page_texts().is_empty(),
        "前提：`;vqzx` 应无候选，实际: {:?}",
        c.debug_page_texts()
    );
    let act = c.handle_key_event(&key(VK_SPACE));
    assert_eq!(commit_text(&act), "vqzx ");
}

/// Free 透镜（缓冲含越界字符，唯一候选＝所打原文）：原文即「所打原码」，与临英的原文候选
/// 同口径——全角态转全角、按临英开关补空格。
#[test]
fn free_lens_raw_follows_temp_english() {
    if !has_english_schema() {
        return;
    }
    let c = coord_with(quick_config(true), "free");
    c.handle_menu_command("toggle_width");
    enter_quick(&c, "rock");
    c.handle_key_event(&key(0xDE)); // '
    for ch in "n".chars() {
        c.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
    assert_eq!(
        c.debug_page_texts(),
        vec!["rock'n".to_string()],
        "前提：`;rock'n` 应落 Free 透镜、唯一候选为原文"
    );
    let act = c.handle_key_event(&key(VK_SPACE));
    assert_eq!(commit_text(&act), "ｒｏｃｋ＇ｎ\u{3000}");
}

/// ★ 数字透镜不算英文：`;12` 的结果里有一条 `12`，文本恰好等于所打原码，不得被当成
/// 英文原文补空格（数字透镜下数字键是输入，故用鼠标点选）。
#[test]
fn numeric_lens_never_appends_space() {
    if !has_english_schema() {
        return;
    }
    let c = coord_with(quick_config(true), "numeric");
    c.handle_key_event(&key(VK_SEMICOLON));
    c.handle_key_event(&key(0x31));
    c.handle_key_event(&key(0x32));
    let i = index_of(&c, "12");
    let act = c.debug_mouse_select(i).expect("鼠标点选应带回上屏动作");
    assert_eq!(commit_text(&act), "12", "数字透镜结果不该补空格");
}

/// ★ 数字透镜的回车原文不转全角：算式原文与英文无关，且与空格兜底同一判据
/// （`mix_raw_counts_as_english`）。曾出现全角态 `;1+` 空格得 `1+`、回车得 `１＋` 的分叉。
#[test]
fn full_width_enter_keeps_numeric_raw() {
    if !has_english_schema() {
        return;
    }
    let c = coord_with(quick_config(false), "fw_enter_num");
    c.handle_menu_command("toggle_width");
    c.handle_key_event(&key(VK_SEMICOLON));
    c.handle_key_event(&key(0x31));
    c.handle_key_event(&key(0x32));
    let act = c.handle_key_event(&key(VK_RETURN));
    assert_eq!(commit_text(&act), "12", "数字透镜回车应上屏半角原文");
}

/// ⑥ 标点顶屏：英文候选随临英转全角，但**不补空格**（补了会得到 `ｖｅｒｙ　，`）。
/// 取 `free_input = off`：出厂 `auto` 下标点会作字面输入进缓冲，走不到顶屏臂。
#[test]
fn full_width_punct_commit_converts_without_space() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = quick_config(true);
    cfg.schema.mix_modes[0].free_input = wind_config::config::FreeInputMode::Off;
    let c = coord_with(cfg, "fw_punct");
    c.handle_menu_command("toggle_width");
    enter_quick(&c, "very");
    let act = c.handle_key_event(&key(0xBC)); // ,
    let text = commit_text(&act);
    assert!(
        text.starts_with("ｖｅｒｙ") && text.chars().count() == 5,
        "应上屏全角 very + 一个标点、中间不补空格，实际: {text:?}"
    );
}

/// 切中英文（`commit_on_switch` 出厂开）上屏缓冲原文：全角态随临英转全角。
#[test]
fn full_width_mode_switch_converts_raw() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = quick_config(false);
    cfg.keys.commit_on_switch = true;
    cfg.keys
        .key_actions
        .insert("ctrl+alt+shift+j".into(), "toggle_mode".into());
    let c = coord_with(cfg, "fw_switch");
    c.handle_menu_command("toggle_width");
    enter_quick(&c, "very");
    let mut ev = key(0x4A);
    ev.modifiers =
        wind_ipc::protocol::MOD_CTRL | wind_ipc::protocol::MOD_ALT | wind_ipc::protocol::MOD_SHIFT;
    let act = c.handle_key_event(&ev);
    assert_eq!(commit_text(&act), "ｖｅｒｙ");
}

/// 回车上屏的原文**包括拼音码**：含英文成员的实例里，缓冲原文一律按英文对待
/// （`;nihao` 全角态回车得 `ｎｉｈａｏ`）——回车时无从分辨这串是拼音还是英文。
#[test]
fn full_width_enter_converts_pinyin_raw_too() {
    if !has_english_schema() {
        return;
    }
    let c = coord_with(quick_config(false), "fw_enter_py");
    c.handle_menu_command("toggle_width");
    enter_quick(&c, "nihao");
    let act = c.handle_key_event(&key(VK_RETURN));
    assert_eq!(commit_text(&act), "ｎｉｈａｏ");
}

/// 数字加单位（`;12.5GB`）：大写字母把数字透镜带进 Free，唯一候选是所打原文，
/// 按原文对待——全角态转全角、按临英开关补空格。
#[test]
fn number_with_unit_counts_as_raw() {
    if !has_english_schema() {
        return;
    }
    let c = coord_with(quick_config(true), "unit");
    c.handle_menu_command("toggle_width");
    c.handle_key_event(&key(VK_SEMICOLON));
    for vk in [0x31, 0x32, 0xBE, 0x35] {
        c.handle_key_event(&key(vk)); // 1 2 . 5
    }
    for vk in [0x47, 0x42] {
        let mut ev = key(vk); // Shift+G Shift+B
        ev.modifiers = wind_ipc::protocol::MOD_SHIFT;
        c.handle_key_event(&ev);
    }
    assert_eq!(
        c.debug_page_texts(),
        vec!["12.5GB".to_string()],
        "前提：`;12.5GB` 应落 Free 透镜、唯一候选为原文"
    );
    let act = c.handle_key_event(&key(VK_SPACE));
    assert_eq!(commit_text(&act), "１２．５ＧＢ\u{3000}");
}
