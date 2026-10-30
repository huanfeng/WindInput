//! 反查模式端到端（docs/design/codetable-reverse-mode.md §3）。真实数据：build_dev/data 的 wubi86 / wubi86_pinyin。
//! ⚠️ 数据缺失时整族静默跳过而计数照绿，判据是耗时（≥1s）与输出里有没有「跳过」。

use std::path::PathBuf;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};

const VK_BACKSLASH: u32 = 0xDC;
const VK_ESCAPE: u32 = 0x1B;
const VK_NEXT: u32 = 0x22;
const VK_BACK: u32 = 0x08;
const VK_OEM_MINUS: u32 = 0xBD;
const VK_OEM_1: u32 = 0xBA;
const VK_RSHIFT: u32 = 0xA1;
const VK_SPACE: u32 = 0x20;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}
fn dict_ready() -> bool {
    data_dir()
        .join("schemas/wubi86/wubi86_jidian.dict.yaml")
        .exists()
}
/// 走生产出口 `handle_key_event_policed`（`wind-bridge` server 调的就是它）：放宽态的失效
/// （`expire_scope_override`）挂在这个出口上，裸 `handle_key_event` 绕过它，测不到。
fn key(c: &Coordinator, vk: u32, shift: bool) {
    key_act(c, vk, shift);
}
/// 同 `key`，但取回 `KeyAction`（要断言上屏内容时用）。
fn key_act(c: &Coordinator, vk: u32, shift: bool) -> wind_bridge::handler::KeyAction {
    c.handle_key_event_policed(&KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: if shift { MOD_SHIFT } else { 0 },
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    })
}
fn letters(c: &Coordinator, s: &str) {
    for ch in s.chars() {
        key(c, ch.to_ascii_uppercase() as u32, false);
    }
}

/// 五笔 86 + `\` 绑反查（绑键即可用，没有总开关）。通配主开关默认关（反查不看它）。
fn wubi_rev() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.keys
        .key_actions
        .insert("backslash".into(), "reverse".into());
    cfg.input.scope_relax.page_end_key = false;
    cfg
}
fn with_filter(mut cfg: Config, mode: &str) -> Config {
    cfg.input.filter_mode = mode.into();
    cfg.input.rare_phrase = "keep".into();
    cfg
}
fn rev_triples(cfg: Config, code: &str) -> Vec<(String, String, String)> {
    let c = Coordinator::new_headless(cfg, Some(&data_dir()));
    key(&c, VK_BACKSLASH, false);
    letters(&c, code);
    c.debug_candidate_triples()
}
fn page_until_more_than(c: &Coordinator, than: usize) -> usize {
    for _ in 0..400 {
        let n = c.debug_candidate_count();
        if n > than {
            return n;
        }
        let (cur, _, total) = c.debug_page_info();
        if cur + 1 >= total && !c.debug_has_more() {
            return n;
        }
        key(c, VK_NEXT, false);
    }
    c.debug_candidate_count()
}

/// spec §3.2 + codetable-wildcard §11：首批 100、`has_more`、翻到边界 ×2 扩充。翻页扩充若漏了反查
/// 模式那一臂，红在扩充。
#[test]
fn reverse_azzz_first_batch_100_and_expands() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let c = Coordinator::new_headless(with_filter(wubi_rev(), "gb18030"), Some(&data_dir()));
    key(&c, VK_BACKSLASH, false);
    letters(&c, "azzz");
    assert_eq!(c.debug_candidate_count(), 100);
    assert!(c.debug_has_more());
    let grown = page_until_more_than(&c, 100);
    assert!(grown > 100, "应扩充：{grown}");
    assert_eq!(c.debug_active_mode(), Some("reverse"), "翻页不退模式");
}

/// §11 契约 1：智能档把反查结果整份当一组，与常用字档相同。
#[test]
fn reverse_smart_matches_general() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let all = rev_triples(with_filter(wubi_rev(), "gb18030"), "hanz");
    let general = rev_triples(with_filter(wubi_rev(), "general"), "hanz");
    let smart = rev_triples(with_filter(wubi_rev(), "smart"), "hanz");
    assert!(all.len() > general.len(), "前置：han? 有生僻字可滤");
    assert_eq!(smart, general);
}

/// 智能档 + 末页放宽：反查模式里 `input_buffer` 恒空，放宽须看 `special_buffer`。
fn smart_relax() -> Config {
    let mut cfg = with_filter(wubi_rev(), "smart");
    cfg.input.scope_relax.page_end_key = true;
    cfg
}

/// 在反查模式里一路向后翻，直到放宽生效或确实翻不动。返回是否放宽了。
fn page_until_relaxed(c: &Coordinator) -> bool {
    for _ in 0..50 {
        key(c, VK_NEXT, false);
        if c.debug_scope_relaxed() {
            return true;
        }
    }
    false
}

/// 翻到末页再按一次 ⇒ 被智能档滤掉的反查生僻字追加在末尾，原有顺序不动、模式不退；
/// 放宽态在本次按键结束后仍在（`expire_scope_override` 看的是 `special_buffer`，不是恒空的
/// `input_buffer`——看错了会在同一键里静默撤销）。末页放宽若漏了反查模式那一臂，红在「应放宽」。
#[test]
fn reverse_page_end_relax_appends_filtered() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let pair = |t: &(String, String, String)| (t.0.clone(), t.1.clone());
    let all: Vec<(String, String)> = rev_triples(with_filter(wubi_rev(), "gb18030"), "hanz")
        .iter()
        .map(pair)
        .collect();
    let c = Coordinator::new_headless(smart_relax(), Some(&data_dir()));
    key(&c, VK_BACKSLASH, false);
    letters(&c, "hanz");
    let before: Vec<(String, String)> = c.debug_candidate_triples().iter().map(pair).collect();
    assert!(all.len() > before.len(), "前置：hanz 有被滤的生僻字");
    assert!(!c.debug_has_more(), "前置：一批取完");
    assert!(page_until_relaxed(&c), "末页再按应放宽");
    assert_eq!(c.debug_active_mode(), Some("reverse"));
    let after: Vec<(String, String)> = c.debug_candidate_triples().iter().map(pair).collect();
    assert_eq!(&after[..before.len()], &before[..], "原有候选顺序不动");
    let mut appended = after[before.len()..].to_vec();
    appended.sort();
    let mut expected: Vec<(String, String)> =
        all.into_iter().filter(|p| !before.contains(p)).collect();
    expected.sort();
    assert_eq!(appended, expected, "被滤的反查生僻字全部追加在末尾");

    // 放宽期间改码（退格）：缓冲非空 ⇒ 放宽态保持，同主路「找生僻字常要改几次编码」。
    key(&c, VK_BACK, false);
    assert!(c.debug_scope_relaxed(), "缓冲非空时放宽保持");
    assert_eq!(c.debug_preedit(), "\\han");
}

/// 放宽后 Esc：放宽 / 翻页位全部复位，退出后主路径的下一次组码不处于放宽态。
#[test]
fn reverse_exit_after_relax_leaves_no_residue() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let c = Coordinator::new_headless(smart_relax(), Some(&data_dir()));
    key(&c, VK_BACKSLASH, false);
    letters(&c, "hanz");
    assert!(page_until_relaxed(&c), "前置：已放宽");
    key(&c, VK_ESCAPE, false);
    assert_eq!(c.debug_active_mode(), None);
    assert!(!c.debug_scope_relaxed());
    assert!(!c.debug_has_more());
    letters(&c, "a");
    assert_eq!(c.debug_input_buffer(), "a", "主路径照常组码");
    assert!(!c.debug_scope_relaxed(), "主路径组码不继承放宽");
}

/// 扩充后 Esc：`has_more` 复位；退出后主路径翻页不会拿残留位去扩充。
#[test]
fn reverse_exit_after_expand_leaves_no_residue() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let c = Coordinator::new_headless(with_filter(wubi_rev(), "gb18030"), Some(&data_dir()));
    key(&c, VK_BACKSLASH, false);
    letters(&c, "azzz");
    assert!(page_until_more_than(&c, 100) > 100, "前置：已扩充");
    key(&c, VK_ESCAPE, false);
    assert_eq!(c.debug_active_mode(), None);
    assert!(!c.debug_has_more());
    assert_eq!(c.debug_candidate_count(), 0);
    // 页码本身随 special 族一样留在退出前的值（候选已空、无可见影响）；主路径下一次装填
    // 经 `reset_candidate_view` 归零，这里锁的是「主路径从第 1 页开始」。
    letters(&c, "a");
    assert_eq!(c.debug_input_buffer(), "a");
    assert_eq!(c.debug_page_info().0, 0, "主路径从第 1 页开始");
}

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

#[test]
fn reverse_symbol_trigger_enters_when_bound() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let c = Coordinator::new_headless(wubi_rev(), Some(&data_dir()));
    key(&c, VK_BACKSLASH, false);
    assert_eq!(c.debug_active_mode(), Some("reverse"));
    let mut unbound = wubi_rev();
    unbound.keys.key_actions.remove("backslash");
    let c = Coordinator::new_headless(unbound, Some(&data_dir()));
    key(&c, VK_BACKSLASH, false);
    assert_eq!(c.debug_active_mode(), None, "没绑反查键：不进");
}

/// ★ Review Focus 3：字母触发键是本方案首码（活码前缀）⇒ 让位作正常码；符号键不让位；
/// 冲突体检认得反查键。
#[test]
fn reverse_letter_trigger_yields_to_live_code_prefix() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut cfg = wubi_rev();
    cfg.keys.key_actions.insert("a".into(), "reverse".into());
    let c = Coordinator::new_headless(cfg, Some(&data_dir()));
    letters(&c, "a");
    assert_eq!(c.debug_active_mode(), None, "a 是五笔活码，让位");
    assert_eq!(c.debug_input_buffer(), "a");
    key(&c, VK_ESCAPE, false);
    key(&c, VK_BACKSLASH, false);
    assert_eq!(c.debug_active_mode(), Some("reverse"), "符号键不让位");

    let mut conflict = wubi_rev();
    conflict.schema.codetable.input_chars = "a-y\\".into();
    let c = Coordinator::new_headless(conflict, Some(&data_dir()));
    let rep = c.code_char_conflicts();
    assert!(
        rep.iter()
            .any(|(ch, owners)| *ch == '\\' && owners.contains(&"反查模式触发键")),
        "{rep:?}"
    );
}

/// spec §3.1：非码表 / 混输方案让位。
#[test]
fn reverse_yields_in_pinyin_schema() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut cfg = wubi_rev();
    cfg.schema.available = vec!["pinyin".into(), "wubi86".into()];
    cfg.schema.active = "pinyin".into();
    let c = Coordinator::new_headless(cfg, Some(&data_dir()));
    key(&c, VK_BACKSLASH, false);
    assert_eq!(c.debug_active_mode(), None);
}

/// GH#146 同构：`z_key_action = "reverse"` 在出厂 `zz*` 短语下经 z 夺取进入。
#[test]
fn z_key_action_reverse_enters_via_z_fallback() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut cfg = wubi_rev();
    cfg.schema.codetable.z_key_action = "reverse".into();
    let c = Coordinator::new_headless(cfg, Some(&data_dir()));
    c.debug_install_phrases(zz_phrases());
    letters(&c, "z");
    assert_eq!(c.debug_active_mode(), None, "首键 z 让位给 zz* 活码");
    letters(&c, "uia");
    assert_eq!(c.debug_active_mode(), Some("reverse"));
    assert_eq!(c.debug_preedit(), "zuia");
    assert!(
        !c.debug_all_candidate_texts().is_empty(),
        "uia 的等长 + 前缀结果"
    );
}

/// 放宽后失焦：`reset_exclusive_modes` 必须一并复位 `scope_relaxed`。它的失效点
/// `expire_scope_override` 只挂在按键出口上，失焦清缓冲不经过它；不复位的话回来后
/// 打的第一个码（缓冲非空）就继承了上一个焦点里的放宽态。主路 / 临拼同受此缺口影响。
#[test]
fn reverse_focus_lost_after_relax_leaves_no_residue() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let c = Coordinator::new_headless(smart_relax(), Some(&data_dir()));
    key(&c, VK_BACKSLASH, false);
    letters(&c, "hanz");
    assert!(page_until_relaxed(&c), "前置：已放宽");
    c.handle_focus_lost(0, wind_bridge::handler::FocusLostReason::Thread);
    assert_eq!(c.debug_active_mode(), None, "失焦退出反查模式");
    letters(&c, "a");
    assert_eq!(c.debug_input_buffer(), "a", "主路径照常组码");
    assert!(!c.debug_scope_relaxed(), "失焦后的新组码不继承放宽");
}

/// 反查模式里 `handle_candidate_nav` 排在通配键进缓冲之前：通配键配成
/// 翻页 / 选词键时模式内它先被导航吃掉，通配进不了缓冲。不改按键顺序，由体检报出。
/// 对照：出厂通配键 `z` 不报；没绑反查键时不报（模式进不去，谈不上模式内冲突）。
#[test]
fn reverse_wildcard_key_on_nav_key_is_reported() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let with_key = |k: &str, bound: bool| {
        let mut cfg = wubi_rev();
        cfg.schema.codetable.wildcard_key = k.into();
        if !bound {
            cfg.keys.key_actions.remove("backslash");
        }
        Coordinator::new_headless(cfg, Some(&data_dir())).reverse_wildcard_conflicts()
    };
    assert_eq!(with_key("-", true), vec!["翻页键"]);
    // 选词键不是冲突：`apply_session_action` 对选词 / 以词定字 / 独立辅助码返回 None 不吃键，
    // 随后反查的符号通配键臂排在选词臂之前，`;` 照常进缓冲（下方实跑）。
    assert!(with_key(";", true).is_empty(), "次选键不吃键，不报");
    assert!(with_key("z", true).is_empty(), "对照：出厂 z 无冲突");
    assert!(with_key("-", false).is_empty(), "对照：没绑反查键不报");

    // 报的是真冲突：模式内按 `-` 被翻页吃掉，不进缓冲。
    let mut cfg = wubi_rev();
    cfg.schema.codetable.wildcard_key = "-".into();
    let c = Coordinator::new_headless(cfg, Some(&data_dir()));
    key(&c, VK_BACKSLASH, false);
    letters(&c, "a");
    key(&c, VK_OEM_MINUS, false);
    assert_eq!(c.debug_active_mode(), Some("reverse"));
    assert_eq!(c.debug_preedit(), "\\a", "`-` 被导航吃掉，没进缓冲");

    // 对照：次选键 `;` 作通配键时在模式内照常进缓冲。
    let mut cfg = wubi_rev();
    cfg.schema.codetable.wildcard_key = ";".into();
    let c = Coordinator::new_headless(cfg, Some(&data_dir()));
    key(&c, VK_BACKSLASH, false);
    letters(&c, "a");
    key(&c, VK_OEM_1, false);
    assert_eq!(c.debug_active_mode(), Some("reverse"));
    assert_eq!(c.debug_preedit(), "\\a;", "`;` 作通配进了缓冲");
}

/// 修饰键绑反查、在拼音方案里按：门卫没过必须**不吞键**，落回全局链照常切中英
/// （出厂 `toggle_mode_keys` 含 rshift）。反查让位判据若排在修饰键早退之前，keyup 通路会把
/// Yield 当「显式 none」吞掉，RShift 在拼音方案里彻底没反应。
#[test]
fn reverse_modifier_binding_in_pinyin_still_toggles_mode() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut cfg = wubi_rev();
    cfg.schema.available = vec!["pinyin".into(), "wubi86".into()];
    cfg.schema.active = "pinyin".into();
    cfg.keys
        .key_actions
        .insert("rshift".into(), "reverse".into());
    assert!(
        cfg.keys.toggle_mode_keys.iter().any(|k| k == "rshift"),
        "前置：rshift 出厂是 toggle_mode 键"
    );
    let c = Coordinator::new_headless(cfg, Some(&data_dir()));
    assert!(c.is_chinese_mode());
    c.handle_key_event(&KeyEventData {
        key_code: VK_RSHIFT,
        scan_code: 0,
        modifiers: 0,
        event_type: wind_ipc::protocol::EVENT_KEY_UP,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    });
    assert!(!c.is_chinese_mode(), "门卫没过不吞键：RShift 照常切中英");
}

fn mixed_ready() -> bool {
    dict_ready()
        && data_dir()
            .join("schemas/wubi86_pinyin.schema.toml")
            .exists()
}

/// spec §3.2：首位通配（`?uia`），通配主开关关着也生效；注释是全码。
#[test]
fn reverse_leading_wildcard_zuia_ignores_inline_switch() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut cfg = wubi_rev();
    cfg.schema.codetable.wildcard = false;
    let tri = rev_triples(cfg, "zuia");
    assert!(
        tri.iter().any(|(t, c, _)| t == "平江" && c == "guia"),
        "{tri:?}"
    );
    assert!(
        tri.iter()
            .all(|(_, c, m)| c.len() == 4 && &c[1..] == "uia" && c == m),
        "{tri:?}"
    );
}

/// spec §3.2 + §4：模式内同样查未启用扩展库，排在已启用之后。
#[test]
fn reverse_mode_sees_disabled_dicts_when_on() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let off = rev_triples(wubi_rev(), "zuia");
    assert!(off.iter().all(|(t, _, _)| t != "门头沟区"));
    let mut cfg = wubi_rev();
    cfg.input.reverse.lookup_disabled_dicts = true;
    let on = rev_triples(cfg, "zuia");
    let pos = on
        .iter()
        .position(|(t, _, _)| t == "门头沟区")
        .unwrap_or_else(|| panic!("{on:?}"));
    assert!(pos >= off.len(), "已启用的 {} 条全在前：{on:?}", off.len());
}

/// spec §2 作用于反查模式。
#[test]
fn reverse_mode_single_only() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut cfg = with_filter(wubi_rev(), "gb18030");
    cfg.schema.codetable.wildcard_single_only = true;
    let tri = rev_triples(cfg, "azzz");
    assert_eq!(tri.len(), 100);
    assert!(
        tri.iter()
            .all(|(t, _, _)| wind_candidate::single_markable_char(t).is_some())
    );
}

/// spec §3.2：混输下模式内只查主码表，没有拼音候选。
#[test]
fn reverse_mode_has_no_pinyin_in_mixed() {
    if !mixed_ready() {
        eprintln!("跳过：五笔 / 混输方案数据不存在");
        return;
    }
    let mut cfg = wubi_rev();
    cfg.schema.available = vec!["wubi86_pinyin".into(), "wubi86".into(), "pinyin".into()];
    cfg.schema.active = "wubi86_pinyin".into();
    // 正向对照：同配置不进反查、走主路径时「汉字」确实在候选里，否则下面的「没有拼音」可能空过。
    let main = Coordinator::new_headless(cfg.clone(), Some(&data_dir()));
    letters(&main, "hanz");
    assert!(
        main.debug_all_candidate_texts().iter().any(|t| t == "汉字"),
        "前置：主路径 hanz 含「汉字」"
    );
    let tri = rev_triples(cfg, "hanz");
    assert!(!tri.is_empty());
    assert!(
        tri.iter().all(|(t, c, _)| !c.contains('z') && t != "汉字"),
        "{tri:?}"
    );
}

/// ★ Review Focus 5（真实数据）：翻过页、放宽过再 Esc，残留全清；随后主路径打字与新开的 coordinator 逐条相同。
#[test]
fn reverse_esc_exits_without_residue() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut cfg = with_filter(wubi_rev(), "smart");
    cfg.input.scope_relax.page_end_key = true;
    let c = Coordinator::new_headless(cfg.clone(), Some(&data_dir()));
    key(&c, VK_BACKSLASH, false);
    letters(&c, "hanz");
    for _ in 0..5 {
        key(&c, VK_NEXT, false); // 翻到底再按 ⇒ 放宽
    }
    assert!(c.debug_scope_relaxed(), "前置：Esc 前放宽确实发生了");
    key(&c, VK_ESCAPE, false);
    assert_eq!(c.debug_active_mode(), None);
    assert!(!c.debug_has_more() && !c.debug_scope_relaxed());
    letters(&c, "hanz");
    let fresh = Coordinator::new_headless(cfg, Some(&data_dir()));
    letters(&fresh, "hanz");
    assert_eq!(c.debug_candidate_triples(), fresh.debug_candidate_triples());
}

/// spec §3.2：选中上屏候选文本（不上屏编码），并退出。
#[test]
fn reverse_space_commits_highlight() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let c = Coordinator::new_headless(wubi_rev(), Some(&data_dir()));
    key(&c, VK_BACKSLASH, false);
    letters(&c, "zuia");
    let first = c.debug_all_candidate_texts()[0].clone();
    let act = key_act(&c, VK_SPACE, false);
    assert!(format!("{act:?}").contains(&first), "上屏的是候选：{act:?}");
    assert_eq!(c.debug_active_mode(), None);
}

// ─────────────── 真实数据验收（反查专属的「含扩展词库」） ───────────────

/// 同一开关下，反查模式出门头沟区（排在已启用之后），行内通配不出。
#[test]
fn reverse_only_lookup_zuia_vs_inline_uuiz() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut cfg = wubi_rev();
    cfg.input.reverse.lookup_disabled_dicts = true;
    cfg.schema.codetable.wildcard = true;
    let rev = rev_triples(cfg.clone(), "zuia");
    let off = rev_triples(wubi_rev(), "zuia");
    let pos = rev
        .iter()
        .position(|(t, c, _)| t == "门头沟区" && c == "uuia")
        .unwrap_or_else(|| panic!("{rev:?}"));
    assert!(pos >= off.len(), "已启用的 {} 条全在前：{rev:?}", off.len());
    let c = Coordinator::new_headless(cfg, Some(&data_dir()));
    letters(&c, "uuiz");
    let inline = c.debug_candidate_triples();
    assert!(
        inline
            .iter()
            .any(|(_, c, _)| c.len() == 4 && c.starts_with("uui")),
        "前置：行内通配确实生效：{inline:?}"
    );
    assert!(
        inline
            .iter()
            .any(|(t, _, c)| t == "门头沟区" && c == "*uuia"),
        "行内通配也查未启用库，注释带 *：{inline:?}"
    );
}

/// 混输：反查与行内通配（码长内）都查主码表的未启用库。
/// 同时钉住 `build_engine` 对混输 primary 递归透传全局开关（透传丢了，反查这侧就查不到）。
#[test]
fn mixed_reverse_sees_xzqy_inline_does_not() {
    if !mixed_ready() {
        eprintln!("跳过：五笔 / 混输方案数据不存在");
        return;
    }
    let mut cfg = wubi_rev();
    cfg.schema.available = vec!["wubi86_pinyin".into(), "wubi86".into(), "pinyin".into()];
    cfg.schema.active = "wubi86_pinyin".into();
    cfg.schema.codetable.wildcard = true;
    cfg.input.reverse.lookup_disabled_dicts = true;
    let rev = rev_triples(cfg.clone(), "zuia");
    assert!(
        rev.iter().any(|(t, c, _)| t == "门头沟区" && c == "uuia"),
        "{rev:?}"
    );
    let c = Coordinator::new_headless(cfg, Some(&data_dir()));
    letters(&c, "uuiz");
    assert!(
        c.debug_candidate_triples()
            .iter()
            .any(|(_, c, _)| c.len() == 4 && c.starts_with("uui")),
        "前置：混输行内通配确实生效"
    );
    assert!(
        c.debug_all_candidate_texts()
            .iter()
            .any(|t| t == "门头沟区"),
        "混输行内通配也查未启用库"
    );
}

/// 真实数据：方案覆盖文件里残留旧的方案级键，反查与行内通配都不查未启用库。
#[test]
fn stale_schema_override_leaks_nowhere_real_data() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let ov = std::env::temp_dir().join(format!("wind_rev_stale_ov_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ov);
    std::fs::create_dir_all(&ov).unwrap();
    // 覆盖文件按 `<override_dir>/<方案 id>.toml` 命名（`EngineManager::persist_schema_override`）。
    std::fs::write(
        ov.join("wubi86.toml"),
        "[engine.codetable]\nlookup_disabled_dicts = true\nwildcard = true\n",
    )
    .unwrap();
    let mk =
        || Coordinator::new_headless_with_override(wubi_rev(), Some(&data_dir()), Some(ov.clone()));
    let c = mk();
    key(&c, VK_BACKSLASH, false);
    letters(&c, "zuia");
    let rev = c.debug_candidate_triples();
    assert!(!rev.is_empty(), "前置：反查有结果");
    assert!(rev.iter().all(|(t, _, _)| t != "门头沟区"), "{rev:?}");
    let c = mk();
    letters(&c, "uuiz");
    let inline = c.debug_candidate_triples();
    assert!(
        inline
            .iter()
            .any(|(_, c, _)| c.len() == 4 && c.starts_with("uui")),
        "前置：覆盖文件里的 wildcard = true 生效，行内通配有结果：{inline:?}"
    );
    assert!(inline.iter().all(|(t, _, _)| t != "门头沟区"), "{inline:?}");
    let _ = std::fs::remove_dir_all(&ov);
}

/// 没有任何开关——`z_key_action = "reverse"` 经 z 夺取进入（z 是引导键，`zuia` 实际查 `uia`），照常出结果。
#[test]
fn z_key_action_reverse_lists_zuia_without_any_switch() {
    if !dict_ready() {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let mut cfg = wubi_rev();
    cfg.keys.key_actions.remove("backslash");
    cfg.schema.codetable.z_key_action = "reverse".into();
    let c = Coordinator::new_headless(cfg, Some(&data_dir()));
    c.debug_install_phrases(zz_phrases());
    letters(&c, "zuia");
    assert_eq!(c.debug_active_mode(), Some("reverse"));
    // 实测：z 夺取进入反查后，z 是「引导键」而非首位通配符——
    // 预编辑是 zuia，查询的是 `uia`（等长 + 前缀补全），不是 `?uia`。
    assert_eq!(c.debug_preedit(), "zuia");
    let tri = c.debug_candidate_triples();
    assert!(!tri.is_empty(), "无任何开关也有结果");
    assert!(
        tri.iter().all(|(_, c, m)| c.starts_with("uia") && c == m),
        "{tri:?}"
    );
    assert!(tri.iter().any(|(_, c, _)| c.len() == 4), "{tri:?}");
}
