//! 码元字符集 `input_chars` / `leading_chars` 的端到端验证
//! （设计见 docs/design/codetable-input-chars.md）。
//!
//! 五笔 86 的码元是 a-y，故用 `input_chars = "a-x"` 把 `y` 排除出去——`y` 在默认配置下
//! 是**完全正常**的码元（`ay` 有候选），对照因此鲜明：同一个键、同一套操作，仅因字符集
//! 不同而走向两条路。
//!
//! ## ⚠️ 反向对照不可省
//!
//! 每条「非码元字符被拒」的用例都配一条「默认字符集下同一操作照常进缓冲」的对照。
//! 只测正向的话，哪怕 `input_chars` 整个没接线、或 `y` 因别的原因本就打不出，用例
//! 一样会绿——对照用例把「是本特性起了作用」与「碰巧看起来对」区分开。
//!
//! 词典缺失时自动跳过 —— ⚠️ `build_dev/data` 不存在时**整族静默跳过而计数照绿**，
//! 判据是耗时（正常 1s 量级 vs 跳过 0.0x s）。见 project_build_dev_data_missing。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn dict_ready(d: &std::path::Path) -> bool {
    d.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists()
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
        seq_char: 0,
    }
}

fn press(coord: &Coordinator, code: &str) {
    for c in code.chars() {
        coord.handle_key_event(&key_event((c.to_ascii_uppercase() as u32) & 0xFF));
    }
}

/// 按下字母或数字键。
///
/// ⚠️ **只对字母与数字成立**：它们的 VK 恰好等于大写 ASCII（`'A'`=0x41=VK_A、
/// `'1'`=0x31=VK_1）。符号键不是——`'/'` 是 0x2F 而 VK_OEM_2 是 0xBF，按字符传会
/// 敲到一个根本不存在的键上，测试却照样「通过」（因为没人接管那个键码）。符号一律用
/// [`press_vk`]。
fn press_one(coord: &Coordinator, c: char) -> KeyAction {
    debug_assert!(
        c.is_ascii_alphanumeric(),
        "符号键的 VK 与字符不同，请用 press_vk"
    );
    coord.handle_key_event(&key_event((c.to_ascii_uppercase() as u32) & 0xFF))
}

/// 按下指定虚拟键。符号键专用（`/` = VK_OEM_2 = 0xBF，`;` = VK_OEM_1 = 0xBA）。
fn press_vk(coord: &Coordinator, vk: u32) -> KeyAction {
    coord.handle_key_event(&key_event(vk))
}

/// `/` 键的虚拟键码（VK_OEM_2）。
const VK_SLASH: u32 = 0xBF;
/// `;` 键的虚拟键码（VK_OEM_1）。
const VK_SEMICOLON: u32 = 0xBA;

/// 上屏动作的文本；非上屏动作返回 `None`。
fn committed(action: &KeyAction) -> Option<&str> {
    match action {
        KeyAction::InsertText { text, .. } => Some(text.as_str()),
        _ => None,
    }
}

/// 仍在组合（键进了缓冲）时的组合区文本；非组合动作返回 `None`。
fn composing(action: &KeyAction) -> Option<&str> {
    match action {
        KeyAction::UpdateComposition { text, .. } => Some(text.as_str()),
        _ => None,
    }
}

/// `input_chars` / `leading_chars` 皆空 = 内置默认 `a-z`（历史行为）。
fn wubi_config(input_chars: &str, leading_chars: &str) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.codetable.input_chars = input_chars.into();
    cfg.schema.codetable.leading_chars = leading_chars.into();
    cfg
}

// ────────────────────────── 非码元字母 ──────────────────────────

/// 主用例：`a-x` 下组码中按 `y` → 终结组合，顶屏当前高亮候选后接 `y`。
#[test]
fn non_code_letter_terminates_composition() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi_config("a-x", ""), Some(&d));

    press(&coord, "a");
    let first = coord
        .debug_all_candidate_texts()
        .first()
        .cloned()
        .expect("打 a 应有候选");

    let act = press_one(&coord, 'y');
    assert_eq!(
        committed(&act),
        Some(format!("{first}y").as_str()),
        "非码元字母应顶屏高亮候选再输出自身（不丢已打的码），实际: {act:?}"
    );
}

/// ★ **反向对照**：默认字符集下同一操作，`y` 照常进缓冲、不上屏。
///
/// 这一条证明主用例的行为来自 `input_chars = "a-x"`，而不是 `y` 本来就打不出、
/// 或整条判定压根没接线。
#[test]
fn default_charset_buffers_the_same_letter() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi_config("", ""), Some(&d));

    press(&coord, "a");
    let act = press_one(&coord, 'y');
    assert_eq!(
        committed(&act),
        None,
        "默认码元集 a-z 下 y 是合法码元，应继续组码而非上屏，实际: {act:?}"
    );
}

/// ★ 空缓冲下的非码元字母**也必须出字，不能透传**。
///
/// C++ 在中文模式下对字母键是无条件吃的（`chinese_letter` 分支），返回 PassThrough
/// 就构成「吃了再吐」——不补发 WM_KEYDOWN 的宿主直接丢字符。铁律见
/// project_fullwidth_eat_flip：C++ 吃键集 ⊆ Rust 出字集。
#[test]
fn non_code_letter_on_empty_buffer_still_outputs() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi_config("a-x", ""), Some(&d));

    let act = press_one(&coord, 'y');
    assert_eq!(
        committed(&act),
        Some("y"),
        "空缓冲的非码元字母须由本侧出字（透传会被宿主丢键），实际: {act:?}"
    );
}

// ────────────────────── 数字作码元（打 Win10）──────────────────────

/// 主用例：`a-z0-9` + 首码 `a-z` 下，组码中按 `1` 进缓冲，而不是选第 1 个候选。
#[test]
fn digit_enters_buffer_when_composing() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi_config("a-z0-9", "a-z"), Some(&d));

    press(&coord, "a");
    let act = press_one(&coord, '1');
    assert_eq!(
        committed(&act),
        None,
        "数字配成码元后，组码中的 1 不应再选词上屏，实际: {act:?}"
    );
    assert!(
        composing(&act).is_some_and(|t| t.contains("a1")),
        "数字应进缓冲、组合区显示 a1，实际: {act:?}"
    );
}

/// ★ **反向对照**：默认字符集下，同一操作的 `1` 照常选走第 1 个候选。
///
/// 没有这一条，主用例无法区分「数字进了缓冲」与「数字选词整个没生效」。
#[test]
fn default_charset_lets_digit_select_candidate() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi_config("", ""), Some(&d));

    press(&coord, "a");
    let first = coord
        .debug_all_candidate_texts()
        .first()
        .cloned()
        .expect("打 a 应有候选");
    let act = press_one(&coord, '1');
    assert_eq!(
        committed(&act),
        Some(first.as_str()),
        "默认字符集下数字键须照常选词（否则主用例是假绿），实际: {act:?}"
    );
}

/// ★ 首码约束：数字是码元但**不是首码**，空缓冲下按 `1` 不得进缓冲。
///
/// 这是「数字可作码元但不作第一码」的核心保证——否则用户既选不了第 1 个候选，
/// 也拿不回原生数字输入。
#[test]
fn digit_is_not_allowed_as_leading_char() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi_config("a-z0-9", "a-z"), Some(&d));

    let act = press_one(&coord, '1');
    assert_eq!(
        composing(&act),
        None,
        "空缓冲的数字不得起头进缓冲，实际: {act:?}"
    );
    assert!(
        matches!(act, KeyAction::PassThrough),
        "空缓冲无候选时数字应透传给宿主（保留原生数字输入），实际: {act:?}"
    );
}

/// 对照上一条：`leading_chars` 留空 = 首码集等于全集，此时数字**可以**起头。
/// 证明上一条的拦截来自 `leading_chars` 而非「数字根本进不了缓冲」。
#[test]
fn digit_may_lead_when_leading_chars_unset() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi_config("a-z0-9", ""), Some(&d));

    let act = press_one(&coord, '1');
    assert!(
        composing(&act).is_some_and(|t| t.contains('1')),
        "首码集未设时数字应可起头，实际: {act:?}"
    );
}

// ────────────────────── 符号作码元（含 / 的词条）──────────────────────

/// 组码中的 `/` 配成码元后进缓冲，而不是落标点流水线。
#[test]
fn symbol_enters_buffer_when_composing() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi_config("a-z/", "a-z"), Some(&d));

    press(&coord, "a");
    let act = press_vk(&coord, VK_SLASH);
    assert!(
        composing(&act).is_some_and(|t| t.contains("a/")),
        "配成码元的 / 应进缓冲，实际: {act:?}"
    );
}

/// `;` 是出厂次选键，但**组码中码元优先**——闸门排在 `decideBufferedTrigger` 那条
/// 次选/三选链之前。这条锁住「符号被别的功能占用时，组码中仍归码表」。
#[test]
fn semicolon_beats_select_key_when_composing() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi_config("a-z;", "a-z"), Some(&d));

    press(&coord, "a");
    let act = press_vk(&coord, VK_SEMICOLON);
    assert!(
        composing(&act).is_some_and(|t| t.contains("a;")),
        "组码中的 ; 应作码元进缓冲，而非选第 2 个候选，实际: {act:?}"
    );
}

/// ★ **反向对照**：默认字符集下同一个 `/` 走标点流水线上屏，不进缓冲。
#[test]
fn default_charset_sends_symbol_to_punctuation() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi_config("", ""), Some(&d));

    press(&coord, "a");
    let act = press_vk(&coord, VK_SLASH);
    assert!(
        composing(&act).is_none_or(|t| !t.contains("a/")),
        "默认字符集下 / 不该进缓冲（否则主用例是假绿），实际: {act:?}"
    );
}

// ────────── 首码集仲裁：码元 vs 模式引导键（现场：`;` 与快捷输入）──────────

/// 复现用户现场：`;` 既是 `quick_mix` 的引导键，又被方案写进 `input_chars`。
fn wubi_config_with_semicolon_mix(input_chars: &str, leading_chars: &str) -> Config {
    let mut cfg = wubi_config(input_chars, leading_chars);
    cfg.schema.mix_modes = vec![wind_config::config::MixModeConfig {
        id: "quick_mix".into(),
        name: "快捷".into(),
        trigger_keys: vec!["semicolon".into()],
        members: vec!["quick_input.calc".into()],
        ..Default::default()
    }];
    cfg
}

/// ★ 首码集**不含** `;` ⇒ 空缓冲归模式，快捷输入照常进得去。
///
/// 这是「两者共存」的配法：`;` 只在组码中作码元。
#[test]
fn mode_trigger_wins_when_symbol_is_not_leading() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi_config_with_semicolon_mix("a-z;", "a-z"), Some(&d));

    press_vk(&coord, VK_SEMICOLON);
    assert_eq!(
        coord.debug_active_mode(),
        Some("mix"),
        "; 不在首码集时，空缓冲应照常进快捷输入"
    );
}

/// ★ 首码集**含** `;` ⇒ 空缓冲归码表，引导键让位。
#[test]
fn code_char_wins_when_symbol_is_leading() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    // leading_chars 留空 = 首码集等于全集，`;` 可作首码——正是用户当前的配法。
    let coord = Coordinator::new_headless(wubi_config_with_semicolon_mix("a-z;", ""), Some(&d));

    let act = press_vk(&coord, VK_SEMICOLON);
    assert_eq!(
        coord.debug_active_mode(),
        None,
        "; 在首码集时不应再进快捷输入，实际: {act:?}"
    );
    assert!(
        composing(&act).is_some_and(|t| t.contains(';')),
        "; 应作首码进缓冲，实际: {act:?}"
    );
}

/// 组码中不受首码集影响：`;` 一律作码元（模式激活只在空缓冲，够不着）。
#[test]
fn composing_always_takes_symbol_regardless_of_leading() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi_config_with_semicolon_mix("a-z;", "a-z"), Some(&d));

    press(&coord, "a");
    let act = press_vk(&coord, VK_SEMICOLON);
    assert_eq!(coord.debug_active_mode(), None, "组码中不该进模式");
    assert!(
        composing(&act).is_some_and(|t| t.contains("a;")),
        "组码中的 ; 应作码元，实际: {act:?}"
    );
}

/// 冲突体检要报出 mix 引导键——用户此前配 `;` 时一句告警都没有，只能靠试。
#[test]
fn mix_trigger_conflict_is_reported_when_leading() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi_config_with_semicolon_mix("a-z;", ""), Some(&d));
    let conflicts = coord.code_char_conflicts();

    let (_, owners) = conflicts
        .iter()
        .find(|(c, _)| *c == ';')
        .unwrap_or_else(|| panic!("; 应报冲突，实际: {conflicts:?}"));
    assert!(
        owners.contains(&"快捷输入/混输引导键"),
        "应报出 mix 引导键被夺走，实际: {owners:?}"
    );
}

/// ★ **反向对照**：`;` 不作首码时，mix 引导键**不该**被报为冲突。
///
/// 此时模式只在空缓冲用、码元只在组码中用，井水不犯河水。报出来只会变成噪音，
/// 把真冲突淹掉——这正是告警最容易退化的方式。
#[test]
fn mix_trigger_not_reported_when_symbol_is_not_leading() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi_config_with_semicolon_mix("a-z;", "a-z"), Some(&d));
    let conflicts = coord.code_char_conflicts();

    let owners = conflicts
        .iter()
        .find(|(c, _)| *c == ';')
        .map(|(_, o)| o.clone())
        .unwrap_or_default();
    assert!(
        !owners.contains(&"快捷输入/混输引导键"),
        "; 不作首码时不该报 mix 冲突（模式仍可用），实际: {owners:?}"
    );
    // 次选键属于「组码中类」占用，仍应如实报出。
    assert!(
        owners.contains(&"次选键"),
        "组码中类占用（次选键）仍应报出，实际: {owners:?}"
    );
}

// ────────────────────── 配置冲突体检（只告警，不改行为）──────────────────────

/// 默认字符集不含非字母字符 ⇒ 恒无冲突。
#[test]
fn default_charset_reports_no_conflict() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi_config("", ""), Some(&d));
    assert!(
        coord.code_char_conflicts().is_empty(),
        "默认码元集不该报冲突，实际: {:?}",
        coord.code_char_conflicts()
    );
}

/// `-` / `=` 是出厂翻页键（`page_keys` 含 `minus_equal`），配成码元即冲突。
#[test]
fn conflict_with_page_keys_is_reported() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi_config("a-z-=", "a-z"), Some(&d));
    let conflicts = coord.code_char_conflicts();

    for want in ['-', '='] {
        let hit = conflicts.iter().find(|(c, _)| *c == want);
        let (_, owners) = hit.unwrap_or_else(|| panic!("{want:?} 应报冲突，实际: {conflicts:?}"));
        assert!(
            owners.contains(&"翻页/高亮键"),
            "{want:?} 的冲突方应含翻页键，实际: {owners:?}"
        );
    }
}

/// `;` 是出厂次选键（`select_key_groups` 含 `semicolon_quote`）。
#[test]
fn conflict_with_select_key_is_reported() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi_config("a-z;", "a-z"), Some(&d));
    let conflicts = coord.code_char_conflicts();

    let (_, owners) = conflicts
        .iter()
        .find(|(c, _)| *c == ';')
        .unwrap_or_else(|| panic!("; 应报冲突，实际: {conflicts:?}"));
    assert!(
        owners.contains(&"次选键"),
        "; 的冲突方应含次选键，实际: {owners:?}"
    );
}

/// 数字作码元 = 放弃组码期间的数字选词，须如实告警。
#[test]
fn digit_reports_number_select_conflict() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi_config("a-z0-9", "a-z"), Some(&d));
    let conflicts = coord.code_char_conflicts();

    let (_, owners) = conflicts
        .iter()
        .find(|(c, _)| *c == '1')
        .unwrap_or_else(|| panic!("数字应报冲突，实际: {conflicts:?}"));
    assert!(
        owners.contains(&"数字选词键"),
        "数字的冲突方应含数字选词键，实际: {owners:?}"
    );
}

/// 不冲突的符号不该被误报——否则告警会退化成背景噪音，真冲突反而被淹没。
#[test]
fn non_conflicting_symbol_is_not_reported() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    // `/` 出厂不作翻页/次选/以词定字/引导键。
    let coord = Coordinator::new_headless(wubi_config("a-z/", "a-z"), Some(&d));
    assert!(
        !coord.code_char_conflicts().iter().any(|(c, _)| *c == '/'),
        "/ 出厂无功能占用，不该报冲突，实际: {:?}",
        coord.code_char_conflicts()
    );
}

/// 子集内的字母照常进缓冲——证明 `a-x` 只排除了 `y`，没把整套字母判死。
#[test]
fn code_letter_still_buffers_under_subset() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi_config("a-x", ""), Some(&d));

    let act = press_one(&coord, 'a');
    assert_eq!(
        committed(&act),
        None,
        "a 在 a-x 内，应继续组码，实际: {act:?}"
    );
    assert!(
        !coord.debug_all_candidate_texts().is_empty(),
        "码元字母进缓冲后应有候选"
    );
}
