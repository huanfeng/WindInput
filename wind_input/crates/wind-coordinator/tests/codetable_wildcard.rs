//! 码表通配输入（万能键）端到端验证（设计见 docs/design/codetable-wildcard.md §7）。
//!
//! 每条「开启通配」用例都配一条「关闭时同一操作」的对照——只测正向的话，
//! 通配整个没接线、或某键本就这么表现，用例一样会绿。
//!
//! 「开启」一律经 `Config` 走 `build_engine` 的折叠（不直接构造引擎）：`build_engine`
//! 若仍传 `wildcard: None`，`active_wildcard_key()` 恒 `None`，下面的进缓冲用例全红。
//!
//! ⚠️ `build_dev/data` 不存在时整族静默跳过而计数照绿，判据是耗时（正常 ≥1s）
//! 与输出里有没有「跳过」。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};
use wind_store::Store;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn dict_ready() -> bool {
    data_dir()
        .join("schemas/wubi86/wubi86_jidian.dict.yaml")
        .exists()
}

fn key_event(key_code: u32, modifiers: u32) -> KeyEventData {
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

/// 字母键（VK = 大写 ASCII）。符号键用 [`press_vk`]。
fn press(coord: &Coordinator, s: &str) -> KeyAction {
    let mut last = KeyAction::Consumed;
    for c in s.chars() {
        debug_assert!(
            c.is_ascii_alphabetic(),
            "符号键的 VK 与字符不同，请用 press_vk"
        );
        last = coord.handle_key_event(&key_event((c.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
    last
}

fn press_vk(coord: &Coordinator, vk: u32, shift: bool) -> KeyAction {
    coord.handle_key_event(&key_event(vk, if shift { MOD_SHIFT } else { 0 }))
}

const VK_SLASH: u32 = 0xBF; // `/`，Shift 即 `?`
const VK_SEMICOLON: u32 = 0xBA; // `;`（出厂次选键）
const VK_SPACE: u32 = 0x20;

/// 本键上屏的文本。顶字在出厂 `direct_commit` 档下返回的是 `CommitThenDeferComposition`，
/// 只认 `InsertText` 的话「不顶字」断言恒真。
fn committed(a: &KeyAction) -> Option<&str> {
    match a {
        KeyAction::InsertText { text, .. } => Some(text.as_str()),
        KeyAction::CommitThenDeferComposition { commit_text, .. } => Some(commit_text.as_str()),
        _ => None,
    }
}

fn wubi(wildcard: bool, key: &str) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.codetable.wildcard = wildcard;
    cfg.schema.codetable.wildcard_key = key.into();
    cfg
}

/// 真机出厂 `zz*` 标点短语的最小替身（同 input_flow.rs 的 `zz_system_phrases`）。
fn zz_phrases() -> Vec<wind_phrase::PhraseSeed> {
    let seed = |code: &str, text: &str| wind_phrase::PhraseSeed {
        code: code.into(),
        text: text.into(),
        weight: 0,
        position: 0,
        is_system: true,
        category: String::new(),
    };
    vec![seed("zzbd", "、"), seed("zzsz", "…")]
}

// ─────────────────────────── 进缓冲裁决（Task 10） ───────────────────────────

/// 非首位字母通配键进缓冲，即使它不在 `input_chars`（`a-y`）里。
/// 对照：关闭时同一操作按非码元字母处置（顶屏高亮候选再出 `z`）。
#[test]
fn wildcard_letter_enters_buffer_outside_input_chars() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut cfg = wubi(true, "z");
    cfg.schema.codetable.input_chars = "a-y".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press(&coord, "a");
    assert!(
        !coord.debug_all_candidate_texts().is_empty(),
        "前置：a 在五笔 86 下应有真实候选"
    );
    press(&coord, "z");
    assert_eq!(coord.debug_input_buffer(), "az");

    let mut off = wubi(false, "z");
    off.schema.codetable.input_chars = "a-y".into();
    let coord = Coordinator::new_headless(off, Some(&data_dir()));
    press(&coord, "a");
    let act = press(&coord, "z");
    assert!(
        committed(&act).is_some_and(|t| t.ends_with('z')),
        "对照：关闭时 z 是非码元，实际 {act:?}"
    );
}

/// 26 码元方案配符号通配键：组码中 `?` 进缓冲，不走标点流水线。
/// 对照：关闭时 `?` 不进缓冲。
#[test]
fn symbol_wildcard_enters_buffer_instead_of_punct() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi(true, "?"), Some(&data_dir()));
    press(&coord, "a");
    let act = press_vk(&coord, VK_SLASH, true);
    assert_eq!(coord.debug_input_buffer(), "a?");
    assert!(committed(&act).is_none(), "通配键不应顶屏：{act:?}");

    let coord = Coordinator::new_headless(wubi(false, "?"), Some(&data_dir()));
    press(&coord, "a");
    press_vk(&coord, VK_SLASH, true);
    assert_ne!(coord.debug_input_buffer(), "a?", "对照：关闭时 ? 不进缓冲");
}

/// 空缓冲下符号通配键**作通配**（论坛 t302；此前一律让位出标点）：首位 `?` 进缓冲、不出「？」。
/// 对照：关闭通配时同一键照常出全角「？」。
#[test]
fn lead_symbol_wildcard_enters_buffer() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let lead = |on: bool| {
        let coord = Coordinator::new_headless(wubi(on, "?"), Some(&data_dir()));
        let act = press_vk(&coord, VK_SLASH, true);
        (
            committed(&act).map(str::to_string),
            coord.debug_input_buffer(),
        )
    };
    assert_eq!(
        lead(false),
        (Some("？".to_string()), String::new()),
        "前置：关闭时出全角问号"
    );
    assert_eq!(
        lead(true),
        (None, "?".to_string()),
        "开启时首位 ? 作通配进缓冲"
    );
}

/// 非首位通配键与次选键冲突 ⇒ 让位（`;` 照常选第 2 个候选）且体检报冲突。
/// 同一键只报一条（`;` 的会话动作就是次选，不再另报成「会话键」）。
/// 对照：通配键 `z` 在出厂键位下无冲突。
#[test]
fn mid_composition_conflict_yields_and_is_reported() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi(true, ";"), Some(&data_dir()));
    assert_eq!(
        coord.wildcard_conflicts(),
        vec!["次选键"],
        "应只报一条次选键冲突"
    );
    press(&coord, "a");
    let second = coord
        .debug_all_candidate_texts()
        .get(1)
        .cloned()
        .expect("a 至少两个候选");
    let act = press_vk(&coord, VK_SEMICOLON, false);
    assert_eq!(committed(&act), Some(second.as_str()), "让位给次选键");

    let coord = Coordinator::new_headless(wubi(true, "z"), Some(&data_dir()));
    assert!(
        coord.wildcard_conflicts().is_empty(),
        "对照：z 在出厂键位下无冲突"
    );
}

/// 通配键是**显式配置**的方案码元时，体检报「方案码元」。
/// 对照：同一键不在显式码元集里（未配 `input_chars`）时不报。
#[test]
fn wildcard_that_is_a_configured_code_char_is_reported() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut cfg = wubi(true, "/");
    cfg.schema.codetable.input_chars = "a-z/".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    assert!(
        coord
            .wildcard_conflicts()
            .iter()
            .any(|o| o.starts_with("方案码元")),
        "{:?}",
        coord.wildcard_conflicts()
    );

    let coord = Coordinator::new_headless(wubi(true, "/"), Some(&data_dir()));
    assert!(
        !coord
            .wildcard_conflicts()
            .iter()
            .any(|o| o.starts_with("方案码元")),
        "对照：未显式配置 input_chars 时不报码元：{:?}",
        coord.wildcard_conflicts()
    );
}

/// 首位 z 已绑 `z_key_action` ⇒ 让位进模式，不作通配。
#[test]
fn leading_z_yields_to_z_key_action() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut cfg = wubi(true, "z");
    cfg.schema.available.push("pinyin".into());
    cfg.input.temp_pinyin.enabled = true;
    cfg.schema.codetable.z_key_action = "temp_pinyin".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press(&coord, "z");
    assert!(coord.debug_in_temp_pinyin(), "首位 z 应让位给 z_key_action");
}

/// 首位 z 是 `zz*` 短语活码 ⇒ 让位，且整轮按字面：`zzbd` 照出短语「、」。
/// 对照：关闭时同一操作候选相同。
#[test]
fn leading_z_phrase_code_stays_literal() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    for on in [true, false] {
        let coord = Coordinator::new_headless(wubi(on, "z"), Some(&data_dir()));
        coord.debug_install_phrases(zz_phrases());
        press(&coord, "zzbd");
        assert_eq!(coord.debug_input_buffer(), "zzbd", "wildcard={on}");
        assert!(
            coord.debug_all_candidate_texts().iter().any(|t| t == "、"),
            "wildcard={on}：zzbd 应出短语「、」，实际 {:?}",
            coord.debug_all_candidate_texts()
        );
    }
}

/// overlay（临时拼音）激活时通配不适用：`z` 在临拼里是拼音字母。
/// 通配键取的是**活跃方案**（五笔）的，overlay 用的是别的方案，协调器必须自己挡住。
#[test]
fn overlay_mode_ignores_wildcard() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut cfg = wubi(true, "z");
    cfg.schema.available.push("pinyin".into());
    cfg.input.temp_pinyin.enabled = true;
    cfg.schema.codetable.z_key_action = "temp_pinyin".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press(&coord, "z");
    assert!(coord.debug_in_temp_pinyin(), "前置：z 进临拼");
    press(&coord, "zhong");
    assert!(coord.debug_in_temp_pinyin(), "仍在临拼");
    assert!(
        coord.debug_all_candidate_texts().iter().any(|t| t == "中"),
        "临拼 zhong 应出「中」，实际 {:?}",
        coord.debug_all_candidate_texts()
    );
}

/// 满码后通配键按字面：`aaaa` + `z` 与关闭通配时逐键相同（顶字上屏「工」、余码续打），
/// 而不是变成一条无候选又不顶字的 5 码死串。对照：未满码时 `aaaz` 照常是通配组码。
#[test]
fn full_length_wildcard_key_is_literal() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let run = |on: bool| {
        let mut cfg = wubi(on, "z");
        cfg.schema.codetable.top_code_commit = true;
        let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
        press(&coord, "aaaa");
        let act = press(&coord, "z");
        (
            committed(&act).map(str::to_string),
            coord.debug_input_buffer(),
        )
    };
    let off = run(false);
    assert!(off.0.is_some(), "前置：关闭时 aaaa+z 顶字，实际 {off:?}");
    assert_eq!(run(true), off, "满码后 z 与关闭时逐键相同");

    let coord = Coordinator::new_headless(wubi(true, "z"), Some(&data_dir()));
    press(&coord, "aaaz");
    assert!(
        !coord.debug_candidate_triples().is_empty(),
        "对照：未满码的 aaaz 仍是通配"
    );
}

// ─────────────────────────── 五笔拼音混输（spec §10） ───────────────────────────

fn mixed_ready() -> bool {
    dict_ready()
        && data_dir()
            .join("schemas/wubi86_pinyin.schema.toml")
            .exists()
}

fn wubi_pinyin(wildcard: bool) -> Config {
    let mut cfg = wubi(wildcard, "z");
    cfg.schema.available = vec!["wubi86_pinyin".into(), "wubi86".into(), "pinyin".into()];
    cfg.schema.active = "wubi86_pinyin".into();
    cfg
}

/// `code` 是否为 `pattern`（`z` 作通配位）的五笔命中。五笔词库没有含 `z` 的码，
/// 故带 `z` 的码只可能是拼音 / 字面串——借此从 `(text, code, comment)` 里认出来源。
fn wubi_hit(pattern: &str, code: &str, equal: bool) -> bool {
    let len_ok = if equal {
        code.len() == pattern.len()
    } else {
        code.len() > pattern.len()
    };
    len_ok
        && !code.contains('z')
        && pattern
            .chars()
            .zip(code.chars())
            .all(|(p, c)| p == 'z' || p == c)
}

fn mixed_triples(on: bool, keys: &str) -> Vec<(String, String, String)> {
    let coord = Coordinator::new_headless(wubi_pinyin(on), Some(&data_dir()));
    press(&coord, keys);
    coord.debug_candidate_triples()
}

/// `gz`：通配出 `g` 开头的二码字，首位是等长结果，注释是完整编码。
/// 对照：关闭时没有任何 `g?` 五笔命中。
#[test]
fn mixed_gz_lists_two_code_chars() {
    if !mixed_ready() {
        eprintln!("跳过：五笔 / 混输方案数据不存在");
        return;
    }
    let on = mixed_triples(true, "gz");
    let (text, code, comment) = on.first().expect("gz 应有候选");
    assert!(
        wubi_hit("gz", code, true),
        "首选应是 g? 等长命中，实际 {text} {code}"
    );
    assert_eq!(comment, code, "注释是完整编码");
    let off = mixed_triples(false, "gz");
    assert!(
        !off.iter().any(|(_, c, _)| wubi_hit("gz", c, true)),
        "对照：关闭时无通配命中，实际 {off:?}"
    );
}

/// `hanz`：通配（`han?` 的五笔字）与拼音（「汉字」这类补全）并存，等长通配排在拼音之前。
#[test]
fn mixed_hanz_merges_wildcard_and_pinyin() {
    if !mixed_ready() {
        eprintln!("跳过：五笔 / 混输方案数据不存在");
        return;
    }
    let off = mixed_triples(false, "hanz");
    assert!(
        off.iter().any(|(t, _, _)| t == "汉字"),
        "前置：关闭时 hanz 应有拼音补全「汉字」，实际 {off:?}"
    );
    let on = mixed_triples(true, "hanz");
    let hanzi = on
        .iter()
        .position(|(t, _, _)| t == "汉字")
        .unwrap_or_else(|| panic!("开启后拼音「汉字」仍在，实际 {on:?}"));
    let last_equal = on
        .iter()
        .rposition(|(_, c, _)| wubi_hit("hanz", c, true))
        .unwrap_or_else(|| panic!("应有 han? 五笔命中，实际 {on:?}"));
    assert!(
        wubi_hit("hanz", &on[0].1, true),
        "首选是等长通配，实际 {:?}",
        on[0]
    );
    assert!(last_equal < hanzi, "等长通配全部排在拼音之前：{on:?}");
}

/// `hanzi` / `xianzai`：超码长整串字面，与关闭时逐条相同。
#[test]
fn mixed_overlength_is_identical_to_off() {
    if !mixed_ready() {
        eprintln!("跳过：五笔 / 混输方案数据不存在");
        return;
    }
    for (keys, word) in [("hanzi", "汉字"), ("xianzai", "现在")] {
        let on = mixed_triples(true, keys);
        assert!(
            on.iter().any(|(t, _, _)| t == word),
            "{keys} 应出「{word}」，实际 {on:?}"
        );
        assert_eq!(on, mixed_triples(false, keys), "{keys} 与关闭时相同");
    }
}

/// ★ Review Focus 5：`zhang` 首位字面，装不装 `zz*` 短语都与关闭时相同。
#[test]
fn mixed_lead_z_is_literal_with_and_without_zz_phrases() {
    if !mixed_ready() {
        eprintln!("跳过：五笔 / 混输方案数据不存在");
        return;
    }
    for phrases in [false, true] {
        let run = |on: bool| {
            let coord = Coordinator::new_headless(wubi_pinyin(on), Some(&data_dir()));
            if phrases {
                coord.debug_install_phrases(zz_phrases());
            }
            press(&coord, "zhang");
            (coord.debug_input_buffer(), coord.debug_candidate_triples())
        };
        let on = run(true);
        assert_eq!(on.0, "zhang");
        assert!(
            on.1.iter().any(|(t, _, _)| t == "张"),
            "短语 {phrases}：zhang 应出「张」，实际 {:?}",
            on.1
        );
        assert_eq!(on, run(false), "短语 {phrases}：与关闭时相同");
    }
}

/// `azi` 撞车串：通配等长（`a?i` 的五笔字）在前，拼音精确「阿紫」随后，通配更长补全在其后。
#[test]
fn mixed_azi_wildcard_first_then_pinyin() {
    if !mixed_ready() {
        eprintln!("跳过：五笔 / 混输方案数据不存在");
        return;
    }
    let off = mixed_triples(false, "azi");
    assert!(
        off.iter().any(|(t, _, _)| t == "阿紫"),
        "前置：关闭时 azi 应出拼音「阿紫」，实际 {off:?}"
    );
    let on = mixed_triples(true, "azi");
    let azi = on
        .iter()
        .position(|(t, _, _)| t == "阿紫")
        .unwrap_or_else(|| panic!("开启后「阿紫」仍在，实际 {on:?}"));
    assert_eq!(
        on.iter().filter(|(t, _, _)| t == "阿紫").count(),
        1,
        "拼音不重复"
    );
    let last_equal = on
        .iter()
        .rposition(|(_, c, _)| wubi_hit("azi", c, true))
        .unwrap_or_else(|| panic!("应有 a?i 五笔命中，实际 {on:?}"));
    assert!(last_equal < azi, "等长通配全部排在「阿紫」之前：{on:?}");
    if let Some(first_longer) = on.iter().position(|(_, c, _)| wubi_hit("azi", c, false)) {
        assert!(azi < first_longer, "通配更长补全排在拼音精确之后：{on:?}");
    }
}

/// ★ Review Focus 4：拼音分段续转的剩余串按字面续转拼音，与关闭时逐条相同。
/// `woaizi` 选「我」后剩 `aizi`（4 码、z 非首位）——这条才测得到续转规则；
/// spec 给的 `woaizhongguo` 剩余串超码长，走的是超码长规则，一并保留。
#[test]
fn mixed_segment_continuation_stays_literal() {
    if !mixed_ready() {
        eprintln!("跳过：五笔 / 混输方案数据不存在");
        return;
    }
    for (keys, rest) in [("woaizi", "aizi"), ("woaizhongguo", "aizhongguo")] {
        let run = |on: bool| {
            let coord = Coordinator::new_headless(wubi_pinyin(on), Some(&data_dir()));
            press(&coord, keys);
            let texts = coord.debug_all_candidate_texts();
            let wo = texts
                .iter()
                .position(|t| t == "我")
                .unwrap_or_else(|| panic!("前置：{keys} 应有部分候选「我」，实际 {texts:?}"));
            let _ = coord.select_candidate(wo);
            (coord.debug_input_buffer(), coord.debug_candidate_triples())
        };
        let on = run(true);
        assert_eq!(on.0, rest, "选「我」后剩余 {rest}");
        assert!(
            !on.1.iter().any(|(_, c, _)| wubi_hit(rest, c, true)),
            "续转态不出五笔通配命中：{:?}",
            on.1
        );
        assert_eq!(on, run(false), "{keys}：续转与关闭时相同");
    }
}

/// 开着通配、但串里没有通配键 ⇒ 与关闭时逐条相同（引擎放开 `wildcard_key` 不得波及字面路径）。
#[test]
fn mixed_without_wildcard_key_is_identical_to_off() {
    if !mixed_ready() {
        eprintln!("跳过：五笔 / 混输方案数据不存在");
        return;
    }
    for keys in ["wo", "xian", "aawt", "yijga"] {
        assert_eq!(
            mixed_triples(true, keys),
            mixed_triples(false, keys),
            "{keys}：无通配键时与关闭相同"
        );
    }
}

// ─────────────────────────── 候选管线（Task 11） ───────────────────────────

/// 默认关闭 ⇒ 行为不变（`az` 无候选）；开启 ⇒ `az` 出候选、注释为全码、等长在前。
#[test]
fn wildcard_off_by_default_changes_nothing() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let off = Coordinator::new_headless(wubi(false, "z"), Some(&data_dir()));
    press(&off, "az");
    assert!(
        off.debug_all_candidate_texts().is_empty(),
        "对照：关闭时 az 是空码"
    );

    let on = Coordinator::new_headless(wubi(true, "z"), Some(&data_dir()));
    press(&on, "az");
    let tri = on.debug_candidate_triples();
    assert!(!tri.is_empty(), "开启后 az 应出候选");
    for (text, code, comment) in &tri {
        assert!(
            code.starts_with('a') && code.chars().count() >= 2,
            "{text} 的码 {code} 不匹配 a?"
        );
        assert_eq!(comment, code, "{text} 的注释应是完整编码");
    }
    assert_eq!(tri[0].1.chars().count(), 2, "等长档在前");
}

/// 多通配：`azzd` 只出第 1 位 a、第 4 位 d 的 4 码。
#[test]
fn multiple_wildcards_match_exactly_one_code_char_each() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi(true, "z"), Some(&data_dir()));
    press(&coord, "azzd");
    let tri = coord.debug_candidate_triples();
    assert!(!tri.is_empty(), "azzd 应有候选");
    for (text, code, _) in tri.iter().filter(|(_, c, _)| c.chars().count() == 4) {
        let cs: Vec<char> = code.chars().collect();
        assert!(
            cs[0] == 'a' && cs[3] == 'd',
            "{text} 的码 {code} 不匹配 a??d"
        );
    }
}

/// 首位无任何绑定时首位即可通配（退化全表扫描）：首批至多 100 条，翻到边界照常扩充
/// （spec §11；旧实现写死 100 条，扩容后可见条数不变即判到底）。
#[test]
fn leading_wildcard_when_unbound_scans_whole_table() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(no_relax(wubi(true, "z")), Some(&data_dir()));
    press(&coord, "z");
    let tri = coord.debug_candidate_triples();
    assert!(
        !tri.is_empty() && tri.len() <= 100,
        "首批至多 100 条：{}",
        tri.len()
    );
    assert!(tri.iter().all(|(_, c, m)| c == m));
    assert!(coord.debug_has_more(), "全表远超 100 条");
    let grown = page_until_more_than(&coord, tri.len());
    assert!(
        grown > tri.len(),
        "翻到边界应扩充：{} -> {grown}",
        tri.len()
    );
}

/// ★ Review Focus 1：首位 z 让位给 `zz*` 短语后，整轮都是字面——`zzbd` 仍出「、」。
/// 对照：同配置下 `az` 仍是通配（证明通配确实开着）。
#[test]
fn zz_phrases_survive_when_wildcard_on() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi(true, "z"), Some(&data_dir()));
    coord.debug_install_phrases(zz_phrases());
    press(&coord, "zzbd");
    assert_eq!(
        coord
            .debug_all_candidate_texts()
            .first()
            .map(String::as_str),
        Some("、"),
        "zzbd 短语应照常命中"
    );
    press_vk(&coord, 0x1B, false); // Esc 清空
    press(&coord, "az");
    assert!(
        !coord.debug_candidate_triples().is_empty(),
        "对照：az 仍是通配"
    );
}

/// 首位 z 让位给 `z_key_repeat`：首选是上一次上屏内容，不是通配结果。
#[test]
fn leading_z_yields_to_z_key_repeat() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut cfg = wubi(true, "z");
    cfg.schema.codetable.z_key_repeat = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press(&coord, "a");
    let first = committed(&press_vk(&coord, VK_SPACE, false))
        .expect("前置：空格上屏")
        .to_string();
    press(&coord, "z");
    assert_eq!(
        coord.debug_all_candidate_texts().first(),
        Some(&first),
        "首位 z 应让位给重复上屏"
    );
}

/// ★ Review Focus 2：开着通配，`z_key_action = temp_pinyin` 的 z 夺取仍然成立
/// （`has_code_prefix("zh")` 走字面 convert，不会被当成 `?h` 判活）。
#[test]
fn z_fallback_still_hijacks_when_wildcard_on() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut cfg = wubi(true, "z");
    cfg.schema.available.push("pinyin".into());
    cfg.schema.codetable.z_key_action = "temp_pinyin".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    coord.debug_install_phrases(zz_phrases()); // 首键 z 让位（活码），靠夺取进临拼
    press(&coord, "z");
    assert!(!coord.debug_in_temp_pinyin(), "前置：首键 z 让位");
    press(&coord, "h");
    assert!(coord.debug_in_temp_pinyin(), "zh 破活码前缀应被夺取进临拼");
}

/// §3.2：通配下超码长不顶字。对照：关闭时 `aaaa` + `a` 照常顶字。
#[test]
fn wildcard_never_top_commits() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut on = wubi(true, "z");
    on.schema.codetable.top_code_commit = true;
    let coord = Coordinator::new_headless(on, Some(&data_dir()));
    let act = press(&coord, "aaaza");
    assert!(committed(&act).is_none(), "通配组码不顶字，实际 {act:?}");
    assert_eq!(coord.debug_input_buffer(), "aaaza");

    let mut off = wubi(false, "z");
    off.schema.codetable.top_code_commit = true;
    let coord = Coordinator::new_headless(off, Some(&data_dir()));
    let act = press(&coord, "aaaaa");
    assert!(committed(&act).is_some(), "对照：字面超码长照常顶字");
}

/// §3.2：通配下满码唯一也不自动上屏（结果多于一条时本就不会上屏，故此处断言的是
/// 「最后一键没有上屏」这一弱形态；强形态见引擎单测 `wildcard_never_auto_commits`）。
#[test]
fn wildcard_full_length_does_not_auto_commit() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut cfg = wubi(true, "z");
    cfg.schema.codetable.auto_commit_at_full = true;
    cfg.schema.codetable.clear_on_empty_max = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    let act = press(&coord, "aaaz");
    assert!(
        committed(&act).is_none(),
        "通配满码不自动上屏，实际 {act:?}"
    );
    assert_eq!(coord.debug_input_buffer(), "aaaz", "也不清空");
}

/// ★ Review Focus 4：符号通配键同时在 `input.buffer_symbol_chars` 里，仍按通配查候选。
#[test]
fn symbol_wildcard_listed_in_buffer_symbol_chars_still_queries() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut cfg = wubi(true, "?");
    cfg.input.buffer_symbol_chars = "-?".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press(&coord, "a");
    press_vk(&coord, VK_SLASH, true);
    assert_eq!(coord.debug_input_buffer(), "a?");
    assert!(
        !coord.debug_candidate_triples().is_empty(),
        "通配键不是「字面符号」，不得清空候选"
    );
}

/// spec §3.1：同字不同码在通配结果里各留一条（学码时要看到每个码位）。
/// 「工」在五笔 86 里有 `aaa`（三简）与 `aaaa`（全码），`aaz` 的等长档与更长档各命中一条；
/// 协调器显示层若仍按 text 去重，后一条会被并进前一条。
#[test]
fn same_text_different_codes_both_listed() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(wubi(true, "z"), Some(&data_dir()));
    press(&coord, "aaz");
    let tri = coord.debug_candidate_triples();
    let gong: Vec<&str> = tri
        .iter()
        .filter(|(t, _, _)| t == "工")
        .map(|(_, c, _)| c.as_str())
        .collect();
    assert!(
        gong.contains(&"aaa") && gong.contains(&"aaaa"),
        "「工」应以 aaa 与 aaaa 各出一条，实际 {gong:?}"
    );
}

/// 出简让全在通配组码下不施加：通配结果是「查码」列表，不是某个码位的首选之争。
/// 现场：档位 3 下沿途记下 `a`/`aaa` 的首选「工」，`aaaz` 的首条（「工」，全码 `aaaa`）
/// 若照常让位就会被挪走。对照：档位 0 同一操作的首条。
#[test]
fn short_code_yield_skipped_under_wildcard() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let first_at = |level: usize| {
        let ov = std::env::temp_dir().join(format!("wind_wcard_scy_{level}"));
        std::fs::create_dir_all(&ov).expect("建 override 目录失败");
        std::fs::write(
            ov.join("wubi86.toml"),
            format!("[engine.codetable]\nshort_code_yield_level = {level}\n"),
        )
        .expect("写 override 失败");
        let coord =
            Coordinator::new_headless_with_override(wubi(true, "z"), Some(&data_dir()), Some(ov));
        press(&coord, "aaaz");
        coord.debug_all_candidate_texts().first().cloned()
    };
    let base = first_at(0);
    assert_eq!(
        base.as_deref(),
        Some("工"),
        "前置：档位 0 时 aaaz 首条是「工」"
    );
    assert_eq!(first_at(3), base, "通配组码不出简让全");
}

/// ★ Review Focus 3（端到端）：主输入路两个上屏出口（空格选词、标点顶屏）在通配组码下
/// 以**候选全码**记词频，不记 `aaaz` 这种查询串（读端永远查不中的孤儿键）。
/// 单测只钉 `main_freq_code` 本身；这里钉的是出口确实接上了它。
#[test]
fn commit_under_wildcard_records_freq_by_full_code() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    // 标点顶屏取 Shift+1（「！」）：出厂 `,` `.` 在组码中是翻页键；码表的标点顶屏
    // 开关（`punct_commit`）出厂关，这里打开。
    for (tag, vk, shift) in [("space", VK_SPACE, false), ("bang", 0x31, true)] {
        let base =
            std::env::temp_dir().join(format!("wind_wcard_freq_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let ov = base.join("override");
        std::fs::create_dir_all(&ov).unwrap();
        std::fs::write(
            ov.join("wubi86.toml"),
            "[engine.codetable.frequency]\nenabled = true\n",
        )
        .unwrap();
        let store = Arc::new(Store::open(base.join("user.redb")).unwrap());
        let mut cfg = wubi(true, "z");
        cfg.schema.codetable.punct_commit = true;
        let coord = Coordinator::new_headless_with_store_override(
            cfg,
            Some(&data_dir()),
            Arc::clone(&store),
            Some(ov),
        );
        press(&coord, "aaaz");
        let (text, code, _) = coord
            .debug_candidate_triples()
            .first()
            .cloned()
            .expect("前置：aaaz 有候选");
        let act = press_vk(&coord, vk, shift);
        assert!(
            committed(&act).is_some_and(|t| t.starts_with(&text)),
            "{tag}：前置：上屏首选，实际 {act:?}"
        );
        assert!(
            store.get_freq("wubi86", &code, &text).unwrap().is_some(),
            "{tag}：应按全码 {code} 记「{text}」"
        );
        assert!(
            store.get_freq("wubi86", "aaaz", &text).unwrap().is_none(),
            "{tag}：不得按通配串记账"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}

/// spec §5.3：短语不参与通配组码。现场：一条码恰为 `az` 的短语——通配下 `az` 是查询
/// `a?`，不是这条短语的码。对照：关闭时同一操作照出该短语。
#[test]
fn phrases_do_not_join_wildcard_results() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let seed = wind_phrase::PhraseSeed {
        code: "az".into(),
        text: "短语甲".into(),
        weight: 0,
        position: 0,
        is_system: true,
        category: String::new(),
    };
    for on in [false, true] {
        let coord = Coordinator::new_headless(wubi(on, "z"), Some(&data_dir()));
        coord.debug_install_phrases(vec![seed.clone()]);
        press(&coord, "az");
        let has = coord
            .debug_all_candidate_texts()
            .iter()
            .any(|t| t == "短语甲");
        assert_eq!(has, !on, "wildcard={on}：短语出现与否不符");
    }
}

/// §3.2 回归：出厂 `top_code_commit = true` 下，满码通配串再敲第 5 键不得把通配结果顶上屏，
/// 且通配开 / 关行为逐字一致（混输与纯五笔都钉）。
fn fifth_key_outcome(cfg: Config, keys: &str, fifth: &str) -> (Option<String>, String) {
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press(&coord, keys);
    let act = press(&coord, fifth);
    (
        committed(&act).map(str::to_string),
        coord.debug_input_buffer(),
    )
}

#[test]
fn mixed_wildcard_fifth_key_never_top_commits() {
    if !mixed_ready() {
        eprintln!("跳过：混输方案数据不存在");
        return;
    }
    for (keys, fifth) in [("wqvz", "x"), ("gzzz", "a")] {
        let mut results = vec![];
        for on in [true, false] {
            let mut cfg = wubi_pinyin(on);
            cfg.schema.codetable.top_code_commit = true;
            results.push(fifth_key_outcome(cfg, keys, fifth));
        }
        let want = (None, format!("{keys}{fifth}"));
        assert_eq!(results[0], want, "混输通配开：{keys}+{fifth} 不得顶字");
        assert_eq!(
            results[0], results[1],
            "混输通配开/关须一致：{keys}+{fifth}"
        );
    }
}

#[test]
fn pure_wubi_wildcard_fifth_key_never_top_commits() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    for (keys, fifth) in [("wqvz", "x"), ("gzzz", "a")] {
        let mut results = vec![];
        for on in [true, false] {
            let mut cfg = wubi(on, "z");
            cfg.schema.codetable.top_code_commit = true;
            results.push(fifth_key_outcome(cfg, keys, fifth));
        }
        let want = (None, format!("{keys}{fifth}"));
        assert_eq!(results[0], want, "纯五笔通配开：{keys}+{fifth}");
        assert_eq!(
            results[0], results[1],
            "纯五笔通配开/关须一致：{keys}+{fifth}"
        );
    }
}

// ─────────────────────── 常用字过滤与翻页扩充（spec §11） ───────────────────────

/// 显式钉住检索范围档与词豁免档（`Config::default()` 的出厂档将来变了，语义会静默换掉）。
fn with_filter(mut cfg: Config, mode: &str) -> Config {
    cfg.input.filter_mode = mode.into();
    cfg.input.rare_phrase = "keep".into();
    cfg
}

fn triples_with(cfg: Config, keys: &str) -> Vec<(String, String, String)> {
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press(&coord, keys);
    coord.debug_candidate_triples()
}

/// spec §11 契约 1：智能档把通配结果当一组——有常用字就滤掉生僻字，与「常用字」档结果相同。
/// 旧实现按 `(来源, 码)` 分组，`han?` / `a??` 下只含生僻字的码位当孤儿放行。
/// 对照：「全部字符」档条数更多，证明这些输入下首批确有生僻字可滤。
#[test]
fn pure_wildcard_smart_filters_like_general() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    for keys in ["hanz", "azz"] {
        let all = triples_with(with_filter(wubi(true, "z"), "gb18030"), keys);
        let general = triples_with(with_filter(wubi(true, "z"), "general"), keys);
        let smart = triples_with(with_filter(wubi(true, "z"), "smart"), keys);
        assert!(
            all.len() > general.len(),
            "前置：{keys} 首批应有生僻字（全部 {} / 常用 {}）",
            all.len(),
            general.len()
        );
        assert_eq!(smart, general, "{keys}: 智能档的通配结果应与常用字档相同");
    }
}

const VK_NEXT: u32 = 0x22; // PageDown，出厂翻页键组 "pageupdown"

/// 关掉末页放宽：翻页类用例只看扩容，末页多按一下的放宽会把被滤的字追加回来、把条数弄乱。
fn no_relax(mut cfg: Config) -> Config {
    cfg.input.scope_relax.page_end_key = false;
    cfg
}

/// 往后翻，直到候选总数超过 `than`，或已在末页且不再有更多（此时不再按，免得触发放宽）。
fn page_until_more_than(coord: &Coordinator, than: usize) -> usize {
    for _ in 0..400 {
        let n = coord.debug_candidate_count();
        if n > than {
            return n;
        }
        let (cur, _, total) = coord.debug_page_info();
        if cur + 1 >= total && !coord.debug_has_more() {
            return n;
        }
        press_vk(coord, VK_NEXT, false);
    }
    coord.debug_candidate_count()
}

/// ★ Review Focus 3：通配首批至多 100 条且 `has_more` 为真，翻到边界按 ×2 扩充。
/// 旧实现码长 ≥ 3 首批上限 1000、引擎却至多回 100 ⇒ `has_more` 恒假，永不扩充。
#[test]
fn pure_wildcard_first_batch_is_100_and_expands() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(no_relax(wubi(true, "z")), Some(&data_dir()));
    press(&coord, "azz");
    let first = coord.debug_candidate_count();
    assert!(first > 0 && first <= 100, "首批至多 100 条：{first}");
    assert!(
        coord.debug_has_more(),
        "a?? 远超 100 条，首批应标记还有更多"
    );
    let grown = page_until_more_than(&coord, first);
    assert!(grown > first, "翻到边界应扩充：{first} -> {grown}");
}

/// 控制者裁决：首批 100 只作用于纯码表；混输有通配 pattern 时维持原首批 300（缩到 100 会让
/// 拼音保底只剩 20 席）。「全部字符」档不滤，首批条数直接反映引擎上限：多于 100 即未被缩。
#[test]
fn mixed_wildcard_keeps_original_first_batch() {
    if !mixed_ready() {
        eprintln!("跳过：五笔拼音混输方案不存在");
        return;
    }
    let coord =
        Coordinator::new_headless(with_filter(wubi_pinyin(true), "gb18030"), Some(&data_dir()));
    press(&coord, "azz");
    let first = coord.debug_candidate_count();
    assert!(first > 100, "混输通配首批应沿用 300 档，实际 {first}");
}

/// 通配命中（等长或更长，码里没有 `z`——五笔词库无 z 码，带 z 的只可能是拼音 / 字面串）。
fn wc_hits(pattern: &str, tri: &[(String, String, String)]) -> Vec<(String, String)> {
    tri.iter()
        .filter(|(_, c, _)| wubi_hit(pattern, c, true) || wubi_hit(pattern, c, false))
        .map(|(t, c, _)| (t.clone(), c.clone()))
        .collect()
}

/// 混输同 `pure_wildcard_smart_filters_like_general`：通配那一截与「常用字」档相同，首选仍是
/// 等长通配。拼音候选按 `(拼音, 码)` 照旧分组，不在比较范围内。缺 Task 1 时红。
#[test]
fn mixed_wildcard_smart_filters_like_general() {
    if !mixed_ready() {
        eprintln!("跳过：五笔 / 混输方案数据不存在");
        return;
    }
    for keys in ["hanz", "azz"] {
        let all = wc_hits(
            keys,
            &triples_with(with_filter(wubi_pinyin(true), "gb18030"), keys),
        );
        let general = wc_hits(
            keys,
            &triples_with(with_filter(wubi_pinyin(true), "general"), keys),
        );
        let smart = triples_with(with_filter(wubi_pinyin(true), "smart"), keys);
        assert!(
            all.len() > general.len(),
            "前置：{keys} 首批通配结果应有生僻字（全部 {} / 常用 {}）",
            all.len(),
            general.len()
        );
        assert_eq!(
            wc_hits(keys, &smart),
            general,
            "{keys}: 通配那一截与常用字档相同"
        );
        assert!(
            wubi_hit(keys, &smart[0].1, true),
            "{keys}: 首选是等长通配，实际 {:?}",
            smart[0]
        );
    }
}

/// ★ Review Focus 5：混输首批回满、翻页扩充后，等长通配仍全部排在最前，字面候选仍在。
/// 控制者裁决：混输首批维持 300（非计划原文的 100），故上限断言按 300。
/// 键用 `yiz` 而非计划的 `azz`：真实数据里 `azz` 无任何拼音候选（`zz` 不成音节）；`yiz` 首批
/// 299 条、拼音 189 条、`has_more` 为真，拼音保底配额（300/5 = 60 席）确实在起作用。
/// 缺 Task 2 时红在 `has_more` 或扩充不生效；配额 / 合并序被破坏时红在后几条。
#[test]
fn mixed_expansion_keeps_equal_length_wildcard_first() {
    if !mixed_ready() {
        eprintln!("跳过：五笔 / 混输方案数据不存在");
        return;
    }
    let keys = "yiz";
    let coord = Coordinator::new_headless(no_relax(wubi_pinyin(true)), Some(&data_dir()));
    press(&coord, keys);
    let first = coord.debug_candidate_count();
    assert!(first <= 300, "混输首批至多 300 条：{first}");
    assert!(coord.debug_has_more(), "前置：混输首批回满，还有更多");
    let is_literal = |c: &str| !wubi_hit(keys, c, true) && !wubi_hit(keys, c, false);
    let literal_first = coord
        .debug_candidate_triples()
        .iter()
        .filter(|(_, c, _)| is_literal(c))
        .count();
    assert!(
        literal_first >= 300 / 5,
        "前置：拼音保底 60 席：{literal_first}"
    );
    let grown = page_until_more_than(&coord, first);
    assert!(grown > first, "翻到边界应扩充：{first} -> {grown}");
    let tri = coord.debug_candidate_triples();
    let first_other = tri
        .iter()
        .position(|(_, c, _)| !wubi_hit(keys, c, true))
        .unwrap_or(tri.len());
    assert!(first_other > 0, "扩充后首位是等长通配");
    assert!(
        tri[first_other..]
            .iter()
            .all(|(_, c, _)| !wubi_hit(keys, c, true)),
        "扩充后等长通配仍全部在最前"
    );
    let literal_grown = tri.iter().filter(|(_, c, _)| is_literal(c)).count();
    assert!(
        literal_grown >= literal_first,
        "字面（拼音）候选扩充后不减：{literal_first} -> {literal_grown}"
    );
}

/// 「常用字」档：首批被滤掉一部分，翻页照样扩充补足可见结果。缺 Task 2 时红在 `has_more`。
#[test]
fn general_mode_wildcard_pages_fill_up() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(
        no_relax(with_filter(wubi(true, "z"), "general")),
        Some(&data_dir()),
    );
    press(&coord, "azz");
    let first = coord.debug_candidate_count();
    assert!(first < 100, "前置：首批 100 条里有被滤的生僻字：{first}");
    assert!(coord.debug_has_more());
    let grown = page_until_more_than(&coord, first);
    assert!(grown > first, "翻页应补足可见结果：{first} -> {grown}");
}

/// 智能档被滤的通配生僻字经「翻到末页再按一次」追加在末尾，原有候选顺序不动。
/// `hanz` 纯码表只有 9 条（`han?`，4 码满码无更长补全），一页即末页。缺 Task 1 时红
/// （无被滤候选可追加）。
#[test]
fn page_end_relax_appends_filtered_wildcard_results() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let pair = |t: &(String, String, String)| (t.0.clone(), t.1.clone());
    let all: Vec<(String, String)> = triples_with(with_filter(wubi(true, "z"), "gb18030"), "hanz")
        .iter()
        .map(pair)
        .collect();
    let coord = Coordinator::new_headless(with_filter(wubi(true, "z"), "smart"), Some(&data_dir()));
    press(&coord, "hanz");
    let before: Vec<(String, String)> = coord.debug_candidate_triples().iter().map(pair).collect();
    assert!(all.len() > before.len(), "前置：hanz 有被滤的生僻字");
    assert!(!coord.debug_has_more(), "前置：9 条一批取完");
    for _ in 0..20 {
        let (cur, _, total) = coord.debug_page_info();
        if cur + 1 >= total {
            break;
        }
        press_vk(&coord, VK_NEXT, false);
    }
    press_vk(&coord, VK_NEXT, false); // 末页再按一次 = 放宽
    let after: Vec<(String, String)> = coord.debug_candidate_triples().iter().map(pair).collect();
    assert_eq!(&after[..before.len()], &before[..], "原有候选顺序不动");
    let mut appended = after[before.len()..].to_vec();
    appended.sort();
    let mut expected: Vec<(String, String)> =
        all.into_iter().filter(|p| !before.contains(p)).collect();
    expected.sort();
    assert_eq!(appended, expected, "被滤的通配生僻字全部追加在末尾");
}

// ─────────────────── 通配仅单字（reverse-mode spec §2） ───────────────────

fn single_only(mut cfg: Config) -> Config {
    cfg.schema.codetable.wildcard_single_only = true;
    cfg
}

fn is_single(t: &str) -> bool {
    wind_candidate::single_markable_char(t).is_some()
}

/// spec §5 列的冒烟串：`azz` 开仅单字后全是单字（`a??` 等长本就全是单字，首批不足以测截断先后）。
#[test]
fn single_only_azz_smoke() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(single_only(wubi(true, "z")), Some(&data_dir()));
    press(&coord, "azz");
    let texts = coord.debug_all_candidate_texts();
    assert!(!texts.is_empty());
    assert!(texts.iter().all(|t| is_single(t)), "{texts:?}");
}

/// ★ Review Focus 2（真实数据）：`azzz`（`a???`）按权重前 100 条只有 ~38 个单字；
/// 先滤后截才能让首批凑满 100 个单字、`has_more` 为真、翻页可扩。缺 Task 2 时红在条数。
#[test]
fn single_only_azzz_first_batch_is_full_of_singles() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let base = Coordinator::new_headless(no_relax(wubi(true, "z")), Some(&data_dir()));
    press(&base, "azzz");
    let base_texts = base.debug_all_candidate_texts();
    assert!(
        base_texts.iter().filter(|t| is_single(t)).count() < 100,
        "前置：不过滤时首批里单字不足 100"
    );
    let coord =
        Coordinator::new_headless(no_relax(single_only(wubi(true, "z"))), Some(&data_dir()));
    press(&coord, "azzz");
    let texts = coord.debug_all_candidate_texts();
    assert!(texts.iter().all(|t| is_single(t)), "{texts:?}");
    assert!(coord.debug_has_more(), "a??? 单字 1342 个，首批应回满");
    let first = coord.debug_candidate_count();
    let grown = page_until_more_than(&coord, first);
    assert!(grown > first, "翻到边界应扩充：{first} -> {grown}");
    assert!(
        coord
            .debug_all_candidate_texts()
            .iter()
            .all(|t| is_single(t))
    );
}

/// spec §2 混输：只作用于通配那一侧，拼音「汉字」照出。
#[test]
fn mixed_single_only_keeps_pinyin_words() {
    if !mixed_ready() {
        eprintln!("跳过：五笔 / 混输方案数据不存在");
        return;
    }
    let coord = Coordinator::new_headless(single_only(wubi_pinyin(true)), Some(&data_dir()));
    press(&coord, "hanz");
    let tri = coord.debug_candidate_triples();
    assert!(
        tri.iter()
            .filter(|(_, c, _)| wubi_hit("hanz", c, true) || wubi_hit("hanz", c, false))
            .all(|(t, _, _)| is_single(t)),
        "通配侧全是单字：{tri:?}"
    );
    assert!(
        tri.iter()
            .any(|(t, c, _)| is_single(t)
                && (wubi_hit("hanz", c, true) || wubi_hit("hanz", c, false))),
        "通配侧至少有一条单字命中（防过滤后为空而空过）：{tri:?}"
    );
    assert!(
        tri.iter().any(|(t, _, _)| t == "汉字"),
        "拼音侧词组照出：{tri:?}"
    );
}

// ─────────────── 未启用扩展词库只给反查模式 ───────────────
// 样本：「门头沟区 uuia」只在未启用的 wubi86_xzqy（default_enabled = false），主库 / extra 无 uuia 码。
// 开关 `input.reverse.lookup_disabled_dicts` 开着时，行内通配与普通打字都必须与关时逐条相同；
// 反查模式那一侧见 tests/codetable_reverse.rs。

fn lookup_disabled(mut cfg: Config) -> Config {
    cfg.input.reverse.lookup_disabled_dicts = true;
    cfg
}

/// 同一份配置下进反查模式（`\` 绑反查）查 `zuia`：给「行内通配不含 xzqy」当「影子层确实挂上过」的对照。
fn reverse_has_xzqy(mut cfg: Config) -> bool {
    cfg.keys
        .key_actions
        .insert("backslash".into(), "reverse".into());
    cfg.input.reverse.lookup_disabled_dicts = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press_vk(&coord, 0xDC, false);
    press(&coord, "zuia");
    coord
        .debug_candidate_triples()
        .iter()
        .any(|(t, c, _)| t == "门头沟区" && c == "uuia")
}

fn tri_of(cfg: Config, keys: &str) -> Vec<(String, String, String)> {
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    press(&coord, keys);
    coord.debug_candidate_triples()
}

/// 真实数据：开关开着，行内通配在已启用结果之后追加未启用库的「门头沟区」；关着则不出。
#[test]
fn lookup_disabled_dicts_inline_wildcard_excludes_xzqy() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let off = tri_of(wubi(true, "z"), "uuiz");
    let on = tri_of(lookup_disabled(wubi(true, "z")), "uuiz");
    assert!(!off.is_empty(), "前置：行内通配有结果（防空过）");
    assert!(
        off.iter().all(|(t, _, _)| t != "门头沟区"),
        "关时不查：{off:?}"
    );
    assert_eq!(&on[..off.len()], &off[..], "已启用的在前、次序不变");
    assert!(
        on.iter().any(|(t, _, c)| t == "门头沟区" && c == "*uuia"),
        "开时行内通配查到未启用库，注释带 *：{on:?}"
    );
    assert!(
        reverse_has_xzqy(wubi(true, "z")),
        "对照：同一配置下反查模式查得到门头沟区（影子层确实挂上过）"
    );
}

/// ★ Review Focus 1（真实数据）：开关开着，普通打字的候选逐条不变（含前缀补全与活码字母）。
#[test]
fn lookup_disabled_dicts_leaves_plain_typing_identical() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    for keys in ["uuia", "uui", "djma", "a", "zz"] {
        assert_eq!(
            tri_of(lookup_disabled(wubi(true, "z")), keys),
            tri_of(wubi(true, "z"), keys),
            "{keys}"
        );
    }
}

/// 混输：开关开着，主码表子引擎的行内通配同样查到未启用库。
#[test]
fn mixed_inline_wildcard_excludes_xzqy() {
    if !mixed_ready() {
        eprintln!("跳过：五笔 / 混输方案数据不存在");
        return;
    }
    let on = tri_of(lookup_disabled(wubi_pinyin(true)), "uuiz");
    assert!(!on.is_empty(), "前置：混输行内通配有结果（防空过）");
    assert!(on.iter().any(|(t, _, _)| t == "门头沟区"), "{on:?}");
    assert!(
        reverse_has_xzqy(wubi_pinyin(true)),
        "对照：同一混输配置下反查模式查得到门头沟区"
    );
}
