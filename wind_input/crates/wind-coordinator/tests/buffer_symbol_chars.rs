//! 组码中符号入缓冲 `input.buffer_symbol_chars` 的端到端验证。
//!
//! 场景是打 `sun-panel` 这类带连字符的英文：`-` 既是英文里的高频字符，又是出厂翻页键
//! （`keys.page_keys` 含 `minus_equal`），撞车处此前两边都不通——首页按下空转吞键，
//! 不配翻页键则顶码上屏。
//!
//! ## ⚠️ 反向对照不可省
//!
//! 每条「`-` 进了缓冲」都配一条「同一操作在配置留空 / 翻过页之后不进缓冲」的对照。
//! 只测正向的话，哪怕闸门整个没接线（`-` 因别的原因没上屏），用例一样会绿。
//!
//! 词典缺失时自动跳过 —— ⚠️ `build_dev/data` 不存在时**整族静默跳过而计数照绿**，
//! 判据是耗时（正常 1s 量级 vs 跳过 0.0x s）。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn dict_ready(d: &std::path::Path) -> bool {
    d.join("schemas/pinyin/rime_frost.dict.yaml").exists()
}

fn wubi_ready(d: &std::path::Path) -> bool {
    d.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists()
}

/// `-` 键（VK_OEM_MINUS）。⚠️ 符号键的 VK 与字符不同（`'-'` 是 0x2D），按字符传会敲到
/// 一个没人接管的键上，而用例照样「通过」。
const VK_MINUS: u32 = 0xBD;
/// `=` 键（VK_OEM_PLUS），出厂的向后翻页键。
const VK_EQUAL: u32 = 0xBB;
const VK_RETURN: u32 = 0x0D;
const VK_BACK: u32 = 0x08;

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

/// 按下一串字母。
fn press(coord: &Coordinator, code: &str) {
    for c in code.chars() {
        debug_assert!(c.is_ascii_alphanumeric(), "符号键请用 press_vk");
        coord.handle_key_event(&key_event((c.to_ascii_uppercase() as u32) & 0xFF));
    }
}

fn committed(action: &KeyAction) -> Option<&str> {
    match action {
        KeyAction::InsertText { text, .. } => Some(text.as_str()),
        _ => None,
    }
}

fn composing(action: &KeyAction) -> Option<&str> {
    match action {
        KeyAction::UpdateComposition { text, .. } => Some(text.as_str()),
        _ => None,
    }
}

/// 出厂 `buffer_symbol_chars = "-"`；传 `""` 关闭本特性（对照组）。
fn pinyin_config(buffer_symbol_chars: &str) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.buffer_symbol_chars = buffer_symbol_chars.into();
    cfg
}

fn coord_with(buffer_symbol_chars: &str) -> Option<std::sync::Arc<Coordinator>> {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：拼音词库不存在");
        return None;
    }
    Some(Coordinator::new_headless(
        pinyin_config(buffer_symbol_chars),
        Some(&d),
    ))
}

// ───────────────────────── 出厂值：首页的 `-` 进缓冲 ─────────────────────────

/// 主用例：打 `sun` 后按 `-`，组合继续（不上屏、不吞键）。
#[test]
fn minus_enters_buffer_before_any_paging() {
    let Some(coord) = coord_with("-") else { return };

    press(&coord, "sun");
    let act = press_vk(&coord, VK_MINUS);

    assert_eq!(
        committed(&act),
        None,
        "首页的 `-` 不该顶码上屏（那是没配翻页键时的老行为），实际: {act:?}"
    );
    assert_eq!(
        composing(&act),
        Some("sun-"),
        "首页的 `-` 应进缓冲、组合继续；`Consumed` 说明还是被翻页键吞了，实际: {act:?}"
    );
    assert!(
        coord.debug_page_texts().is_empty(),
        "缓冲混进符号后这串已不是拼音编码，候选须清空——留着 `sun` 的候选会让空格上屏「孙」，\
         实际: {:?}",
        coord.debug_page_texts()
    );
}

/// ★ 组合区显示的必须是**原码**，不是拼音音节拆分。
///
/// 盯的是 `update_candidates` 里那道早退。少了它，引擎按合法前缀容错，组合区会拿 `sun`
/// 的拆分去渲染成 `sun'-panel`（多一个隔音符），与回车真正上屏的 `sun-panel` 对不上——
/// 用户看到的和拿到的是两个东西。
#[test]
fn composition_shows_raw_code_not_syllables() {
    let Some(coord) = coord_with("-") else { return };

    press(&coord, "sun");
    press_vk(&coord, VK_MINUS);
    press(&coord, "panel");
    let act = press_vk(&coord, VK_MINUS);

    assert_eq!(
        composing(&act),
        Some("sun-panel-"),
        "组合区应逐字显示原码，实际: {act:?}"
    );
}

/// ★ 退格能逐字退回，且退掉那个符号之后候选照常回来。
///
/// 「候选回来」这半句是本用例的重点：它证明候选清空是**跟着缓冲内容走**的判据，
/// 而不是某个一旦置上就摘不掉的状态位。
#[test]
fn backspace_restores_candidates_after_removing_the_symbol() {
    let Some(coord) = coord_with("-") else { return };

    press(&coord, "sun");
    press_vk(&coord, VK_MINUS);
    assert!(
        coord.debug_page_texts().is_empty(),
        "前置：符号进缓冲后候选空"
    );

    let act = press_vk(&coord, VK_BACK);
    assert_eq!(
        composing(&act),
        Some("sun"),
        "退格应退掉那个符号，实际: {act:?}"
    );
    assert!(
        !coord.debug_page_texts().is_empty(),
        "符号退掉后这串又是合法拼音，候选须回来"
    );
}

/// ★ 码表方案下**不顶码**：`sunf-p` 超了五笔满码长也不该把「校尉」顶上屏。
///
/// 这是 `accumulate_code_char` 那条顶码否决。少了它，用户正打的标识符会在第 5 个字符
/// 那一帧被拆成「顶上屏的中文 + 余码」两半。
///
/// # ⚠️ 两个前置条件，缺一条用例就恒绿
///
/// 1. **顶码开关要显式打开**：`schema.codetable.top_code_commit` 的 L1 默认是 `false`，
///    而 L2（`data/config.toml`）出厂是 `true`——测试构造走 `Config::default()` 即 L1，
///    不打开的话引擎第一行就 `return None`，顶码压根不发生。这条 L1/L2 不一致踩过一次：
///    去掉被测的那条否决，用例照样全绿。
/// 2. **前 4 码必须在码表里有字**（`sunf` = 校尉）：否则引擎因「顶不出东西」自己放弃，
///    同样测不到那条否决。`sun-pa` 就是这样一条假用例（前 4 码 `sun-`，码表无字）。
#[test]
fn codetable_does_not_top_commit_a_symbol_bearing_buffer() {
    let d = data_dir();
    if !wubi_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut cfg = pinyin_config("-");
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.schema.codetable.top_code_commit = true;
    let coord = Coordinator::new_headless(cfg, Some(&d));

    press(&coord, "sunf");
    assert!(
        !coord.debug_page_texts().is_empty(),
        "前置：`sunf` 这 4 码在五笔码表里要有字，否则顶码根本不会被触发"
    );

    press_vk(&coord, VK_MINUS);
    let act = coord.handle_key_event(&key_event(0x50)); // p，第 6 个字符，已超满码长
    assert_eq!(
        committed(&act),
        None,
        "码表满码长之外仍不该顶码（缓冲里那串已不是编码），实际: {act:?}"
    );

    let act = press_vk(&coord, VK_RETURN);
    assert_eq!(
        committed(&act),
        Some("sunf-p"),
        "回车应上屏整串原码，实际: {act:?}"
    );
}

/// ★ **反向对照一**：配置留空 ⇒ 同一操作退回历史行为（被翻页键吞掉，组合区不变）。
///
/// 这一条证明主用例的行为来自 `buffer_symbol_chars`，而不是 `-` 本来就会进缓冲。
#[test]
fn empty_charset_keeps_the_swallow() {
    let Some(coord) = coord_with("") else { return };

    press(&coord, "sun");
    let act = press_vk(&coord, VK_MINUS);

    assert!(
        matches!(act, KeyAction::Consumed),
        "关掉本特性后，首页的 `-` 仍应是空转吞键（历史行为），实际: {act:?}"
    );
}

/// ★ **反向对照二**：翻过页之后 `-` 恢复翻页身份，**不**进缓冲。
///
/// 这是 `State::paged` 那个状态位的存在理由，也是与「按 `current_page == 0` 判」的分野：
/// 翻回第 1 页后用户仍在翻页，此刻 `-` 必须还是翻页键。
#[test]
fn minus_pages_again_after_paging() {
    let Some(coord) = coord_with("-") else { return };

    press(&coord, "sun");
    let (_, _, total) = coord.debug_page_info();
    assert!(
        total >= 2,
        "本用例要求 `sun` 至少有两页候选，实际 {total} 页"
    );

    press_vk(&coord, VK_EQUAL);
    assert_eq!(coord.debug_page_info().0, 1, "`=` 应翻到第 2 页");

    let act = press_vk(&coord, VK_MINUS);
    assert_eq!(
        coord.debug_page_info().0,
        0,
        "`-` 应翻回第 1 页而不是进缓冲"
    );
    assert!(
        matches!(act, KeyAction::Consumed),
        "翻页是吞键 + 刷新候选窗，不产出组合区变更，实际: {act:?}"
    );

    // 回到第 1 页后**仍然翻过页**：再按 `-` 是空转吞键，不是字符。
    let act = press_vk(&coord, VK_MINUS);
    assert!(
        matches!(act, KeyAction::Consumed),
        "翻过页之后 `-` 保持翻页身份（此刻空转），不该变回字符，实际: {act:?}"
    );
}

/// ★ 翻页史随候选重装作废：翻过页后再敲一个字母，`-` 又是字符。
///
/// 盯的是 `paged` 的清零点（`reset_candidate_view`）。少了它，用户只要翻过一次页，
/// 这一整轮输入里的 `-` 就再也打不出来。
#[test]
fn paging_history_expires_with_the_candidate_list() {
    let Some(coord) = coord_with("-") else { return };

    press(&coord, "sun");
    press_vk(&coord, VK_EQUAL);
    assert_eq!(coord.debug_page_info().0, 1, "前置：已翻到第 2 页");

    press(&coord, "g"); // 新的一批候选
    assert_eq!(coord.debug_page_info().0, 0, "新候选应回到第 1 页");

    let act = press_vk(&coord, VK_MINUS);
    assert!(
        composing(&act).is_some(),
        "候选重装后翻页史应作废，`-` 重新是字符，实际: {act:?}"
    );
}

// ───────────────────────── 边界：空闲态与端到端 ─────────────────────────

/// ★ 空闲（缓冲为空）时 `-` 必须照旧出字符，不能被本闸门夺走。
///
/// 否则用户在任何程序里都打不出减号。
#[test]
fn idle_minus_still_outputs() {
    let Some(coord) = coord_with("-") else { return };

    let act = press_vk(&coord, VK_MINUS);
    assert_eq!(
        committed(&act),
        Some("-"),
        "空闲时 `-` 应照常出字符（本闸门只在组码中生效），实际: {act:?}"
    );
}

/// 端到端：`sun` + `-` + `panel` + 回车 ⇒ 上屏 `sun-panel`。
///
/// 这是整条需求的验收点。回车走 `input.enter_behavior = "commit"`（空码上屏原码）。
#[test]
fn types_a_hyphenated_identifier_end_to_end() {
    let Some(coord) = coord_with("-") else { return };

    press(&coord, "sun");
    press_vk(&coord, VK_MINUS);
    press(&coord, "panel");
    let act = press_vk(&coord, VK_RETURN);

    assert_eq!(
        committed(&act),
        Some("sun-panel"),
        "回车应上屏原码 `sun-panel`，实际: {act:?}"
    );
}

// ───────────────────────── 让位：该键另有活身份 ─────────────────────────

/// ★ `-` 改配成**以词定字键**后让位，不进缓冲。
///
/// 本闸门是「让位优先」的（与 `input_chars` 码元的「无条件夺取」相反），判据单点在
/// `symbol_buffer_key_free`。这里用以词定字而非翻页，是为了让挡回去的**不是**「未翻页
/// 的 PagePrev」那条例外——否则测不出别的身份也拦得住。
///
/// ⚠️ 用 `select_char_keys` 而不是 `select_key_groups`：后者的值域里没有 `minus_equal`
/// （两张组名表值域不同，见 `select_char_group_binds` 的注释），配了会静默失效，
/// 于是用例变成「`-` 还是翻页键」的重复验证而看不出来。
#[test]
fn yields_to_other_identities_on_the_same_key() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    let mut cfg = pinyin_config("-");
    // 把 `-`/`=` 从翻页键里摘掉，改配成以词定字键。
    cfg.keys.page_keys = vec!["pageupdown".into()];
    cfg.keys.select_char_keys = vec!["minus_equal".into()];
    let coord = Coordinator::new_headless(cfg, Some(&d));

    press(&coord, "sun");
    let act = press_vk(&coord, VK_MINUS);

    assert_eq!(
        composing(&act),
        None,
        "`-` 已是以词定字键，本闸门必须让位（逐字上屏而非进缓冲），实际: {act:?}"
    );
}

/// ★ `-` 被 `keys.key_actions` 绑成引导键时也让位。
///
/// 这一问查的是**另一张表**（`bound_action_for`），不是 `session_action_for` 的派生，
/// 所以它是 `symbol_buffer_key_free` 里真正独立的第二问。漏掉它的表现是用户把 `-`
/// 绑成临英触发键后那个绑定再也不生效，而设置页里它还好端端摆着。
#[test]
fn yields_to_a_key_actions_binding() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    let mut cfg = pinyin_config("-");
    // 先摘掉翻页身份，否则挡回去的是第一问，证明不了第二问也在。
    cfg.keys.page_keys = vec!["pageupdown".into()];
    cfg.keys
        .key_actions
        .insert("minus".into(), "temp_english".into());
    let coord = Coordinator::new_headless(cfg, Some(&d));

    press(&coord, "sun");
    let act = press_vk(&coord, VK_MINUS);

    assert_ne!(
        composing(&act),
        Some("sun-"),
        "`-` 已绑成临英引导键，本闸门必须让位，实际: {act:?}"
    );
    assert_eq!(
        coord.debug_active_mode(),
        Some("temp_english"),
        "让位后该键应照常执行它的引导动作"
    );
}
