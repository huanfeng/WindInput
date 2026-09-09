//! 英文候选**跟随输入大小写**的端到端测试（`schema.english.case_follow_input` /
//! `input.temp_english.case_follow_input` / `input.capslock.english_case_cycle`）。
//!
//! # 这组测试要钉住的四件事
//!
//! 1. **逐位投影且单向**：打 `Hi` 出 `Hill`、打 `WoW` 出 `WoWed`；打小写不动词库原文。
//! 2. ★ **英文方案读的是影子串**：`state.input_buffer` 恒为全小写，用它取输入形态的话
//!    整个功能在英文方案下**一条也不会投影，且毫无报错**。这是实现期最容易漏的一处，
//!    `english_schema_projects_with_shift` 是它的守门断言。
//! 3. ★ **英文方案下 Shift+字母不再进临英**：用户已经在英文方案里，Shift 的意思是
//!    「打个大写字母」而不是「换一个模式」。
//! 4. ★ **词频记的是词库原文**：投影后的文本若进了词频，写 `Hill`、读 `hill`（读端排在
//!    投影之前），两端永不相交 —— 英文词频整体静默失效。
//!
//! # ⚠️ 假绿源
//!
//! 词典缺失时整族**静默跳过**（判据是耗时而非通过条数），worktree 需自备 `build_dev`。
//! 同 `english_head_candidates.rs`。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};
use wind_store::Store;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_english_schema() -> bool {
    let d = data_dir();
    d.join("schemas/english.schema.toml").exists() && d.join("schemas/english").is_dir()
}

fn key(key_code: u32, modifiers: u32) -> KeyEventData {
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

fn english_config() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "english".into()];
    cfg.schema.active = "english".into();
    cfg.input.default.chinese_mode = true;
    cfg
}

/// 临英（主方案取五笔：归属恒是内置英文方案，与 active 无关）。
fn temp_english_config() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "english".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.temp_english.enabled = true;
    cfg
}

fn store_at(tag: &str) -> Arc<Store> {
    let path = std::env::temp_dir().join(format!("wind_en_case_{tag}.redb"));
    let _ = std::fs::remove_file(&path);
    Arc::new(Store::open(&path).unwrap())
}

fn coord_with(cfg: Config, tag: &str) -> Arc<Coordinator> {
    Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store_at(tag))
}

/// 按 `word` 逐键输入，大写字母带 Shift。首字母大写时**不**特殊处理——英文方案下它就该
/// 落进主路字母臂（本文件的 `shift_letter_stays_in_english_schema` 正是钉这一条）。
fn type_word_cased(coord: &Coordinator, word: &str) {
    for c in word.chars() {
        let shift = if c.is_ascii_uppercase() { MOD_SHIFT } else { 0 };
        coord.handle_key_event(&key((c.to_ascii_uppercase() as u32) & 0xFF, shift));
    }
}

// ───────────────────── 英文方案 ─────────────────────

/// ★ 英文方案下打 `Hi`：词库候选整体投影成首字母大写。
///
/// 这条同时是「输入形态取自影子串」的守门断言——取 `input_buffer`（恒全小写）的话，
/// 页面里一条大写也不会有。
#[test]
fn english_schema_projects_with_shift() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let coord = coord_with(english_config(), "en_shift");
    type_word_cased(&coord, "Hi");
    let page = coord.debug_page_texts();
    assert_eq!(
        page.first().map(String::as_str),
        Some("Hi"),
        "首候选恒是所打原文（含大小写），实际: {page:?}"
    );
    // 判据落在**具体词**上：按首字母扫全列表会被词库自带大写的条目（`Hi-Fi`）搅乱，
    // 而那类条目正是单向规则要保护的对象，不该出现在这条断言的判据里。
    assert!(
        page.iter().any(|t| t == "Hibernate"),
        "词库的 hibernate 应投影成 Hibernate，实际: {page:?}"
    );
    assert!(
        !page.iter().any(|t| t == "hibernate"),
        "投影后不该还留着原形态，实际: {page:?}"
    );
}

/// 反向对照：关掉开关后词库候选保持词库原文（小写）。
///
/// 没有这条，「恒投影」与「按开关投影」两种实现都能让上面的正向断言通过。
#[test]
fn english_schema_case_follow_can_be_turned_off() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let mut cfg = english_config();
    cfg.schema.english.case_follow_input = false;
    let coord = coord_with(cfg, "en_off");
    type_word_cased(&coord, "Hi");
    let page = coord.debug_page_texts();
    assert!(
        page.iter().any(|t| t == "hibernate"),
        "关掉后词库候选应保持词库原文（小写），实际: {page:?}"
    );
    assert!(
        !page.iter().any(|t| t == "Hibernate"),
        "关掉后不该有任何投影产物，实际: {page:?}"
    );
}

/// 全小写输入不改动任何候选（单向规则：小写不覆盖词库自带的大写）。
#[test]
fn lowercase_input_leaves_dict_case_alone() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let coord = coord_with(english_config(), "en_lower");
    type_word_cased(&coord, "hi");
    let page = coord.debug_page_texts();
    assert!(
        page.iter().any(|t| t == "hibernate"),
        "词库原文原样出现，实际: {page:?}"
    );
    // ★ 单向规则的正面证据：词库里 `Hi-Fi` 自带大写，全小写输入**不得**把它压成 `hi-fi`。
    // 这正是 `China` / `iPhone` 那条取舍在真实词库里的样子。
    assert!(
        page.iter().any(|t| t == "Hi-Fi"),
        "词库自带的大写不该被小写输入压掉，实际: {page:?}"
    );
}

/// ★ 英文方案下 Shift+字母**留在英文方案**，不再被换进临时英文。
#[test]
fn shift_letter_stays_in_english_schema() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let coord = coord_with(english_config(), "en_no_temp");
    coord.handle_key_event(&key(b'H' as u32, MOD_SHIFT));
    assert_eq!(
        coord.debug_active_mode(),
        None,
        "英文方案下 Shift+字母不得进入任何 overlay 模式"
    );
    let page = coord.debug_page_texts();
    assert_eq!(
        page.first().map(String::as_str),
        Some("H"),
        "大写应进影子串并如实显示为原文候选，实际: {page:?}"
    );
}

/// 非英文方案（五笔）下 Shift+字母仍进临英——上一条的反向对照，防止判据加宽。
#[test]
fn shift_letter_still_enters_temp_english_elsewhere() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let coord = coord_with(temp_english_config(), "wubi_temp");
    coord.handle_key_event(&key(b'H' as u32, MOD_SHIFT));
    assert_eq!(
        coord.debug_active_mode(),
        Some("temp_english"),
        "五笔方案下 Shift+字母仍是临英的进入方式"
    );
}

// ───────────────────── 临时英文 ─────────────────────

/// 临英由 Shift+字母进入 ⇒ 缓冲首字母恒大写 ⇒ 候选恒为首字母大写形态。
///
/// 这是 2026-09-09 重新裁定的行为（旧 `adapt_en_case` 曾因整串套形被删除）。
#[test]
fn temp_english_projects_from_entry_capital() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let coord = coord_with(temp_english_config(), "te_on");
    type_word_cased(&coord, "Hi");
    let page = coord.debug_page_texts();
    assert!(
        page.iter().any(|t| t == "Hibernate"),
        "临英词库候选应随进入时的大写投影，实际: {page:?}"
    );
}

/// 临英侧开关独立可关（两个作用域各一份，与 `raw_candidate` / `case_variants` 同形制）。
#[test]
fn temp_english_case_follow_can_be_turned_off() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let mut cfg = temp_english_config();
    cfg.input.temp_english.case_follow_input = false;
    let coord = coord_with(cfg, "te_off");
    type_word_cased(&coord, "Hi");
    let page = coord.debug_page_texts();
    assert!(
        page.iter().any(|t| t == "hibernate"),
        "关掉后词库候选应保留词库原文（小写），实际: {page:?}"
    );
}

/// 候选列表里不得出现两条一模一样的文本——投影会让词库的 `hi` 撞上头部原文候选。
#[test]
fn projection_does_not_duplicate_candidates() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let coord = coord_with(english_config(), "en_dedup");
    type_word_cased(&coord, "Hi");
    let page = coord.debug_page_texts();
    let mut seen = std::collections::HashSet::new();
    for t in &page {
        assert!(seen.insert(t.clone()), "候选重复: {t} —— 整页: {page:?}");
    }
}

// ───────────────────── 投影 × 词频 ─────────────────────

/// ★★ 选中被投影过的候选：词频记的是**词库原文**，不是屏幕上那个形态。
///
/// 存反了的后果是完全静默的——读端 `apply_freq_rerank_in` 排在投影之前、按原文查，
/// 于是写 `Hello`、读 `hello`，两端永不相交，英文词频整体失效而没有任何报错。
#[test]
fn freq_records_dict_original_not_projected_text() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let mut cfg = english_config();
    cfg.schema.english.frequency.enabled = true; // 出厂是 false，不开就是在测一个关着的功能
    let store = store_at("freq_orig");
    let coord =
        Coordinator::new_headless_with_store(cfg, Some(&data_dir()), std::sync::Arc::clone(&store));

    type_word_cased(&coord, "Hi");
    let page = coord.debug_page_texts();
    let shown = page
        .iter()
        .find(|t| t.as_str() == "Hibernate")
        .expect("应有投影后的 Hibernate");
    let idx = page.iter().position(|t| t == shown).expect("上面刚找到");
    // 数字键选中它（页内第 idx+1 条）。
    coord.handle_key_event(&key(b'1' as u32 + idx as u32, 0));

    assert!(
        store
            .get_freq("english", "hibernate", "hibernate")
            .unwrap()
            .is_some(),
        "词频应按词库原文 hibernate 记账"
    );
    assert!(
        store
            .get_freq("english", "hibernate", "Hibernate")
            .unwrap()
            .is_none(),
        "不得按屏幕上的投影形态记账——那样读端永远查不中"
    );
}

/// 反过来：库里已有词频记录时，投影**不妨碍**它把词顶到前面。
///
/// 重排排在投影之前，故它比对的是词库原文；这条钉住「顺序没被人调换」。
#[test]
fn projection_does_not_break_freq_rerank() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let mut cfg = english_config();
    cfg.schema.english.frequency.enabled = true;
    cfg.schema.english.case_variants = false; // 变形候选会占位，扰乱「词库段首条」的判定
    let store = store_at("freq_rerank");

    // 先用全小写输入学一次，再用带大写的输入验证同一条记录仍然生效。
    let coord = Coordinator::new_headless_with_store(
        cfg.clone(),
        Some(&data_dir()),
        std::sync::Arc::clone(&store),
    );
    type_word_cased(&coord, "hi");
    let page = coord.debug_page_texts();
    let idx = page
        .iter()
        .position(|t| t == "hibernation")
        .expect("词库应有 hibernation");
    coord.handle_key_event(&key(b'1' as u32 + idx as u32, 0));

    let coord2 =
        Coordinator::new_headless_with_store(cfg, Some(&data_dir()), std::sync::Arc::clone(&store));
    type_word_cased(&coord2, "Hi");
    let page2 = coord2.debug_page_texts();
    assert_eq!(
        page2.get(1).map(String::as_str),
        Some("Hibernation"),
        "小写时学到的词，带大写重打时仍应被顶到词库段首位（并投影成大写），实际: {page2:?}"
    );
}

// ───────────────────── CapsLock 档位循环 ─────────────────────

/// 真实触发点是全局键盘钩子（headless 里不可达），故经 `debug_cycle_english_case`
/// 走同一个函数。三档循环闭合 + 每档的候选形态逐格钉住。
#[test]
fn capslock_cycles_three_variants() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let mut cfg = english_config();
    cfg.input.capslock.english_case_cycle = true;
    let coord = coord_with(cfg, "cycle");
    type_word_cased(&coord, "Hi");
    assert!(
        coord.debug_page_texts().iter().any(|t| t == "Hibernate"),
        "默认档：按输入形态投影"
    );

    assert!(
        coord.debug_cycle_english_case(),
        "有候选且是英文态 ⇒ 应夺取本键"
    );
    assert!(
        coord.debug_page_texts().iter().any(|t| t == "HIBERNATE"),
        "第一档：全大写，实际: {:?}",
        coord.debug_page_texts()
    );

    assert!(coord.debug_cycle_english_case());
    assert!(
        coord.debug_page_texts().iter().any(|t| t == "hibernate"),
        "第二档：全小写，实际: {:?}",
        coord.debug_page_texts()
    );

    assert!(coord.debug_cycle_english_case());
    assert!(
        coord.debug_page_texts().iter().any(|t| t == "Hibernate"),
        "循环回默认档 —— 档位切换必须可逆，实际: {:?}",
        coord.debug_page_texts()
    );
}

/// 开关关闭时不夺取（出厂即此状态）。
#[test]
fn capslock_not_hijacked_when_disabled() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let coord = coord_with(english_config(), "cycle_off");
    type_word_cased(&coord, "Hi");
    assert!(
        !coord.debug_cycle_english_case(),
        "出厂关 ⇒ CapsLock 保持系统原生语义"
    );
}

/// ★ 非英文语境不夺取：中文方案下 CapsLock 仍归系统 / 用户绑定。
#[test]
fn capslock_not_hijacked_outside_english() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let mut cfg = temp_english_config(); // active = wubi86
    cfg.input.capslock.english_case_cycle = true;
    let coord = coord_with(cfg, "cycle_cn");
    // 打五笔码，出的是中文候选。
    for c in "wq".chars() {
        coord.handle_key_event(&key((c.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
    assert!(
        !coord.debug_page_texts().is_empty(),
        "五笔应有候选，否则这条测试测的是「没候选所以不夺取」"
    );
    assert!(
        !coord.debug_cycle_english_case(),
        "中文输入语境下不得夺取 CapsLock"
    );
}

/// ★ 空闲（无候选）时不夺取——否则用户按 CapsLock 连大写锁定都切不了。
#[test]
fn capslock_not_hijacked_without_candidates() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let mut cfg = english_config();
    cfg.input.capslock.english_case_cycle = true;
    let coord = coord_with(cfg, "cycle_idle");
    assert!(
        !coord.debug_cycle_english_case(),
        "空闲时 CapsLock 必须留给系统"
    );
}

/// 临英同样受用（两条路径共用同一套判据）。
#[test]
fn capslock_cycle_works_in_temp_english() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let mut cfg = temp_english_config();
    cfg.input.capslock.english_case_cycle = true;
    let coord = coord_with(cfg, "cycle_te");
    type_word_cased(&coord, "Hi");
    assert!(coord.debug_cycle_english_case(), "临英也是英文语境");
    assert!(
        coord.debug_page_texts().iter().any(|t| t == "HIBERNATE"),
        "实际: {:?}",
        coord.debug_page_texts()
    );
}

/// ★ 档位属于**这一次**组合：上屏后复位，不串到下一个词。
#[test]
fn variant_resets_after_commit() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let mut cfg = english_config();
    cfg.input.capslock.english_case_cycle = true;
    let coord = coord_with(cfg, "cycle_reset");
    type_word_cased(&coord, "Hi");
    assert!(coord.debug_cycle_english_case());
    assert!(coord.debug_page_texts().iter().any(|t| t == "HIBERNATE"));
    // 空格上屏首候选，结束这一次组合。
    coord.handle_key_event(&key(0x20, 0));
    type_word_cased(&coord, "Hi");
    assert!(
        coord.debug_page_texts().iter().any(|t| t == "Hibernate"),
        "下一次组合应回到默认档，实际: {:?}",
        coord.debug_page_texts()
    );
}
