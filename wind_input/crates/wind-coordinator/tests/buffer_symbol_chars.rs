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
//! 词典缺失时自动跳过 —— ⚠️ `build_dev/data` 不存在时**整族静默跳过而计数照绿**。
//!
//! ⚠️⚠️ **别用耗时当判据**。本族抄来的那句「正常 1s 量级 vs 跳过 0.0x s」实测已经不成立：
//! 冷启动 5.6s，此后每次 0.17–0.19s，正好落在它所说的「跳过」区间里——照它判会得出
//! 「一直在跳过」的反向结论。要确认真的跑了，就看 `--nocapture` 里有没有「跳过：」那行。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};

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
const VK_NEXT: u32 = 0x22;
const VK_DOWN: u32 = 0x28;
/// `'` 键（VK_OEM_7）。
const VK_QUOTE: u32 = 0xDE;

fn key_event(key_code: u32) -> KeyEventData {
    key_event_mods(key_code, 0)
}

fn key_event_mods(key_code: u32, modifiers: u32) -> KeyEventData {
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

fn press_vk(coord: &Coordinator, vk: u32) -> KeyAction {
    coord.handle_key_event(&key_event(vk))
}

/// 按下一串字母，返回最后一键的动作。
fn press(coord: &Coordinator, code: &str) -> KeyAction {
    let mut last = KeyAction::Consumed;
    for c in code.chars() {
        debug_assert!(c.is_ascii_alphanumeric(), "符号键请用 press_vk");
        last = coord.handle_key_event(&key_event((c.to_ascii_uppercase() as u32) & 0xFF));
    }
    last
}

/// Shift + 字母（用户真的按住 Shift 打大写，大写只活在影子串里）。
fn press_shift(coord: &Coordinator, c: char) -> KeyAction {
    debug_assert!(c.is_ascii_alphabetic());
    coord.handle_key_event(&key_event_mods(
        (c.to_ascii_uppercase() as u32) & 0xFF,
        MOD_SHIFT,
    ))
}

fn committed(action: &KeyAction) -> Option<&str> {
    match action {
        KeyAction::InsertText { text, .. } => Some(text.as_str()),
        _ => None,
    }
}

/// 顶码上屏的文本。⚠️ 顶码走的**不是** `InsertText`：码表顶码返回
/// `CommitThenDeferComposition`（先上屏已确认段，余码延迟成新组合），只认 `InsertText`
/// 会把顶码看成「什么都没发生」。
fn top_committed(action: &KeyAction) -> Option<&str> {
    match action {
        KeyAction::CommitThenDeferComposition { commit_text, .. } => Some(commit_text.as_str()),
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

    // 前置三：同一套配置下，**不带符号**的同样越界编码确实会顶码。少了这条正向对照，
    // 日后顶码开关或满码长的取值路径一变，本用例就安静地退化成「第 6 个字符什么都没发生」。
    {
        let probe = Coordinator::new_headless(
            {
                let mut c = pinyin_config("-");
                c.schema.available = vec!["wubi86".into()];
                c.schema.active = "wubi86".into();
                c.schema.codetable.top_code_commit = true;
                c
            },
            Some(&d),
        );
        press(&probe, "sunf");
        let act = press(&probe, "p"); // 第 5 码即越界
        assert!(
            top_committed(&act).is_some_and(|t| !t.is_empty()),
            "前置：这套配置下越界编码本该顶码上屏，否则本用例证明不了否决在起作用，实际: {act:?}"
        );
    }

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
    assert_eq!(
        composing(&act),
        Some("sung-"),
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

    // 断言「原功能真的执行了」，而不只是「没进缓冲」：后者有两条路径都满足
    //（正确让位 / 绑定静默失效后落兜底标点臂顶码上屏），区分不开就等于没测。
    assert_eq!(
        committed(&act),
        Some("孙"),
        "`-` 已是以词定字键，应让位并逐字上屏首字，实际: {act:?}"
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

// ───────────────────── 审查补漏：三条没有红线的实现分支 ─────────────────────

/// ★★ 候选清空时**页码必须归零**，否则 `page_range` 切出 `start > end` 当场 panic。
///
/// `page_range` 只把 `end` 夹到 `candidates.len()`，`start` 不夹（`handle_candidate.rs`）。
/// 早退里那行 `reset_candidate_view` 是唯一防线，而它此前无人守——11 条用例按 `-` 时
/// 全都还在第 1 页，走不到这个状态。
///
/// 可达性不靠猜：把翻页键换成 PageUp/PageDown，`-` 于是没有会话身份，翻到第 2 页再按它。
/// 末尾那句 `debug_page_texts()` 就是 panic 的守门人（生产侧同一刀在 `notify_ui_update`）。
#[test]
fn clears_page_index_when_candidates_are_dropped() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    let mut cfg = pinyin_config("-");
    cfg.keys.page_keys = vec!["pageupdown".into()];
    let coord = Coordinator::new_headless(cfg, Some(&d));

    press(&coord, "sun");
    press_vk(&coord, VK_NEXT);
    assert_eq!(coord.debug_page_info().0, 1, "前置：PageDown 应翻到第 2 页");

    let act = press_vk(&coord, VK_MINUS);
    assert_eq!(composing(&act), Some("sun-"), "实际: {act:?}");
    assert_eq!(
        coord.debug_page_info().0,
        0,
        "候选清空后页码必须归零，否则 page_range 切出 start>end"
    );
    let _ = coord.debug_page_texts(); // panic 守门人：留在第 2 页的话这里就炸
}

/// ★★ 方案把该符号配成**真码元**时，`buffer_has_literal_symbol` 必须放行——它照常参与
/// 顶码判定，与 `codetable_does_not_top_commit_a_symbol_bearing_buffer` 恰成镜像。
///
/// 守的是那个谓词里的 `active_is_code_char` 那条排除。删掉它全族照绿，而真实后果是：
/// 码表方案把 `-` 写进 `input_chars` 后，凡含 `-` 的编码一律不顶码、也查不到候选，
/// 而 `try_code_char_gate` 那边还照常把它当码元收进缓冲——两道闸门对同一个字符的
/// 判断当场分裂。
///
/// ⚠️ 判据用**顶码**而不是「候选还在」：五笔码表里本来就没有含 `-` 的编码，`su-` 无论
/// 如何都查不到候选，拿它当断言恒绿。顶码则是两条路真正分岔的地方。
#[test]
fn real_code_char_still_top_commits() {
    let d = data_dir();
    if !wubi_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut cfg = pinyin_config("-");
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.schema.codetable.top_code_commit = true;
    // `-` 写在末位是字面减号（不是区间），见 CodeCharSet 的语法说明。
    cfg.schema.codetable.input_chars = "a-z-".into();
    let coord = Coordinator::new_headless(cfg, Some(&d));

    press(&coord, "sunf");
    assert!(
        !coord.debug_page_texts().is_empty(),
        "前置：`sunf` 这 4 码要有字，否则顶码不会被触发"
    );

    let act = press_vk(&coord, VK_MINUS);
    assert!(
        top_committed(&act).is_some_and(|t| !t.is_empty()),
        "`-` 是真码元 ⇒ `sunf-` 是越界的合法编码，应照常顶码上屏，实际: {act:?}"
    );
}

/// ★★ **方向键**回卷换页之后，`-` 同样恢复翻页身份。
///
/// `turn_page` 的全部价值是「五个写点同步置 `paged`」，而此前只有 `page_next`（`=`）
/// 那一个写点有红线。把 `move_up`/`move_down` 的回卷换页改回裸赋值，全族照绿——
/// 而「用户正在用方向键翻页」恰恰是那条 UX 判据所指的场景。
#[test]
fn arrow_key_wrap_also_counts_as_paging() {
    let Some(coord) = coord_with("-") else { return };

    press(&coord, "sun");
    let (_, _, total) = coord.debug_page_info();
    assert!(total >= 2, "本用例要求 `sun` 至少两页候选，实际 {total} 页");

    // 一路按 ↓ 直到回卷进第 2 页。
    for _ in 0..40 {
        if coord.debug_page_info().0 > 0 {
            break;
        }
        press_vk(&coord, VK_DOWN);
    }
    assert_eq!(coord.debug_page_info().0, 1, "前置：↓ 回卷应进入第 2 页");

    let act = press_vk(&coord, VK_MINUS);
    assert!(
        matches!(act, KeyAction::Consumed),
        "方向键翻过页之后 `-` 仍是翻页键，不该变回字符，实际: {act:?}"
    );
    assert_eq!(coord.debug_page_info().0, 0, "`-` 应翻回第 1 页");
}

// ───────────────────── 审查补漏：无候选那一格刻意不碰 ─────────────────────

/// ★★ **无候选**时 `-` 不进缓冲——那一格并不空转。
///
/// 导航类动作被 `requires_candidates` 挡在 `apply_session_action` 门外，键继续往下走到
/// 标点臂，按 `input.punct_on_empty_behavior`（出厂 `clear`）甩掉废码。那是用户打错码时
/// 的退出口，夺走它等于让 `-` 成为唯一一个甩不掉废码的标点键。
#[test]
fn does_not_take_over_when_there_are_no_candidates() {
    let Some(coord) = coord_with("-") else { return };

    // 一串查不到任何候选的废码。⚠️ 别用 `zzzz`：出厂 `system.phrases.toml` 有 37 条 `zz*`
    // 标点短语，它反而候选满满，用例就测到「有候选」那一格去了。
    press(&coord, "vvvv");
    assert!(
        coord.debug_page_texts().is_empty(),
        "前置：`vvvv` 应当一条候选都没有，否则测的是另一格"
    );

    let act = press_vk(&coord, VK_MINUS);
    assert_ne!(
        composing(&act),
        Some("vvvv-"),
        "无候选那一格 `-` 不该进缓冲（它原本会甩掉废码），实际: {act:?}"
    );
}

/// ★★ 但缓冲里**已有**本闸门放进来的符号时照收不误（`e-mail-addr` 的第二个连字符）。
///
/// 这是上一条的例外，也是它不能写成「无候选一律让位」的原因：那一串早已不是本方案的
/// 编码、候选恒空，而标点臂会把用户打了一半的标识符整串丢掉。
#[test]
fn keeps_taking_over_once_the_buffer_already_holds_one() {
    let Some(coord) = coord_with("-") else { return };

    press(&coord, "sun");
    press_vk(&coord, VK_MINUS);
    press(&coord, "panel");
    assert!(
        coord.debug_page_texts().is_empty(),
        "前置：含符号的缓冲候选恒空，本用例要的正是这个状态"
    );

    let act = press_vk(&coord, VK_MINUS);
    assert_eq!(
        composing(&act),
        Some("sun-panel-"),
        "缓冲里已有符号时第二个 `-` 仍须进缓冲，实际: {act:?}"
    );
}

// ───────────────────── 审查补漏：组合区显示的两条 ─────────────────────

/// ★★ 分步上屏的**已转换前缀**不能在组合区消失。
///
/// 早退若不补 `sync_preedit_to_highlight`，`state.preedit` 会停在裸 `input_buffer` 上：
/// 打 `nihao` 选「你」之后按 `-`，组合区从「你hao」变成「hao-」,「你」凭空消失，
/// 而 `composition_caret` 仍按含前缀算，给出的 caret 比文本还长。
#[test]
fn keeps_the_committed_prefix_in_the_composition() {
    let Some(coord) = coord_with("-") else { return };

    press(&coord, "nihao");
    // 选一个只吃掉 `ni` 的单字候选，制造「已转换前缀 + 剩余码」的分步上屏态。
    let Some(idx) = coord.debug_page_texts().iter().position(|t| t == "你") else {
        eprintln!("跳过：本词库 `nihao` 首页没有单字「你」，换不出分步上屏态");
        return;
    };
    let act = press_vk(&coord, 0x31 + idx as u32); // 数字键选词
    let Some(prefix_shown) = composing(&act).map(str::to_string) else {
        eprintln!("跳过：选中「你」没有进入分步上屏态（整串被消费了）");
        return;
    };
    assert!(
        prefix_shown.starts_with('你'),
        "前置：选中后组合区应是「你」+ 剩余码，实际 {prefix_shown:?}"
    );

    let act = press_vk(&coord, VK_MINUS);
    let shown = composing(&act).unwrap_or("");
    assert!(
        shown.starts_with('你'),
        "已转换前缀「你」不该在按下 `-` 之后消失，实际: {shown:?}"
    );
    assert!(shown.ends_with('-'), "符号应已进缓冲，实际: {shown:?}");
}

/// ★★ Shift 打出的大写要投影到组合区，不能只活在影子串里。
///
/// 同样是早退里那个 `sync_preedit_to_highlight`（的 `project_case` 那半）。少了它，
/// 打 `X-Ray` 时组合区显示 `x-ray` 而回车上屏 `X-Ray`——正是本功能要根除的
/// 「看到的和拿到的不一致」，换个维度复发。
/// ⚠️ **不能从 Shift+首字母起头**：空缓冲下 Shift+字母是临时英文的进入条件
/// （`input.temp_english.shift_behavior` 出厂 `temp_english`），那条路整个不经本闸门，
/// 用例会变成在测临英。必须先让缓冲非空，之后的 Shift+字母才走普通模式的字母臂。
#[test]
fn projects_shift_typed_case_into_the_composition() {
    let Some(coord) = coord_with("-") else { return };

    press(&coord, "sun");
    press_vk(&coord, VK_MINUS);
    press_shift(&coord, 'r');
    let act = press(&coord, "ay");

    assert_eq!(
        composing(&act),
        Some("sun-Ray"),
        "组合区应显示用户实际打的大小写，实际: {act:?}"
    );
    let act = press_vk(&coord, VK_RETURN);
    assert_eq!(
        committed(&act),
        Some("sun-Ray"),
        "上屏文本与组合区必须一致，实际: {act:?}"
    );
}

// ───────────────────── 审查补漏：三条让位/边界 ─────────────────────

/// ★ 已转换前缀非空但**缓冲已空**时不接管：那一码是新一轮的开头，符号当首码没有意义。
///
/// 守的是闸门里 `input_buffer.is_empty()` 这个判据（而不是 `has_input_session`）。
#[test]
fn does_not_take_over_on_empty_buffer_with_committed_prefix() {
    let Some(coord) = coord_with("-") else { return };

    press(&coord, "nihao");
    let Some(idx) = coord.debug_page_texts().iter().position(|t| t == "你好") else {
        eprintln!("跳过：本词库 `nihao` 首页没有「你好」");
        return;
    };
    press_vk(&coord, 0x31 + idx as u32);

    // 整串被消费 ⇒ 缓冲空。此时的 `-` 该是普通标点，不是编码的一部分。
    let act = press_vk(&coord, VK_MINUS);
    assert!(
        composing(&act).is_none_or(|t| !t.ends_with('-')),
        "缓冲为空时 `-` 不该被收进编码，实际: {act:?}"
    );
}

/// ★ 显式 `keys.key_actions.minus = "none"` 是「这个键让位」，不是「这个键归我」。
///
/// 守的是 `symbol_buffer_key_free` 里 `| Some(BoundAction::None)` 那一臂。收紧成只认
/// `None` 的话，用户显式写了 `none` 反而会让本功能失效。
#[test]
fn explicit_none_binding_still_lets_the_symbol_through() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    let mut cfg = pinyin_config("-");
    cfg.keys.page_keys = vec!["pageupdown".into()];
    cfg.keys.key_actions.insert("minus".into(), "none".into());
    let coord = Coordinator::new_headless(cfg, Some(&d));

    press(&coord, "sun");
    let act = press_vk(&coord, VK_MINUS);
    assert_eq!(
        composing(&act),
        Some("sun-"),
        "显式 none 等同未配置，不该挡住本闸门，实际: {act:?}"
    );
}

/// ★ 音节分隔符不能被这道闸门抢走。
///
/// `manual_separator_key` 的 `auto` 档挑中 `'` 的条件，恰好是本闸门两问的补集 ——
/// 「它当上了分隔符」与「本闸门判它空闲」是同一个不等式，不显式问一句必撞：
/// 用户照文档的邀请把 `'` 加进白名单，`xi'an` 就彻底没有候选了。
#[test]
fn yields_to_the_syllable_separator() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    let mut cfg = pinyin_config("-'");
    // 把 `'` 从选词键里摘掉，让 `auto` 档真的挑中它作分隔符。
    cfg.keys.select_key_groups = vec![];
    let coord = Coordinator::new_headless(cfg, Some(&d));

    press(&coord, "xi");
    press_vk(&coord, VK_QUOTE);
    press(&coord, "an");

    assert!(
        !coord.debug_page_texts().is_empty(),
        "`'` 是隔音符、不是入缓冲符号，`xi'an` 必须照常有候选"
    );
}

/// ★ 数字选词不能被这道闸门抢走。
///
/// 数字选词是硬编码的 `VK_1..=VK_9` 臂，不走 `session_actions`，上面两问都查不到它，
/// 而它的消费点排在本闸门之后。用户照文档的邀请把数字加进白名单，1-9 选词就**无声**
/// 废掉了。真想让数字成为编码的一部分，那是 `input_chars`（真码元）的活。
#[test]
fn yields_to_digit_candidate_selection() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(pinyin_config("-1"), Some(&d));

    press(&coord, "sun");
    let first = coord
        .debug_page_texts()
        .first()
        .cloned()
        .expect("前置：`sun` 要有候选");

    let act = press_vk(&coord, 0x31); // 主键盘 `1`
    assert_eq!(
        committed(&act),
        Some(first.as_str()),
        "`1` 仍须选第 1 个候选，不该进缓冲，实际: {act:?}"
    );
}
