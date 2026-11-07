//! 快捷输入里打大写英文（Shift+字母）：英文段对齐临英——大小写变形 + 词库候选，词库候选
//! **跟随输入大小写**（读 `input.temp_english.case_follow_input`，用户已定：快捷输入英文一律
//! 读临英开关）。
//!
//! 现场：Shift+字母让缓冲带上大写，按透镜判据越界进 `Free`，候选只剩所打原文一条——
//! 英文成员根本没被问到，`case_follow_input` 无从生效。
//!
//! 最小改法（不动透镜判据，A2-23 / t157 的 Free 语义原样）：缓冲是**纯 ASCII 字母**、本实例
//! 含英文成员且 `free_input = auto` 时，Free 透镜在原文之后追加英文段。原文仍钉首位，
//! 数字 / 符号照旧字面入缓冲，拼音 / 数字透镜不受影响（缓冲里一有大写就不是它们的编码）。
//!
//! `free_input = off` 的实例（A2-50 / t235）：含英文成员时大写同样进缓冲、候选是同一份英文段，
//! 但数字 / 符号键保持功能键身份（选词、翻页、顶屏）；不含英文成员时恒小写，维持原状。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_config::config::FreeInputMode;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};
use wind_store::Store;

const VK_SEMICOLON: u32 = 0xBA;
const VK_SPACE: u32 = 0x20;
const VK_DOWN: u32 = 0x28;

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

fn type_str(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        let m = if ch.is_ascii_uppercase() {
            MOD_SHIFT
        } else {
            0
        };
        coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF, m));
    }
}

fn open(tag: &str, edit: impl FnOnce(&mut Config)) -> (Arc<Coordinator>, Arc<Store>, PathBuf) {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.schema.english.frequency.enabled = true;
    edit(&mut c);
    let db = std::env::temp_dir().join(format!(
        "wind_quick_en_case_{tag}_{}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), Arc::clone(&store));
    (coord, store, db)
}

fn quick_hel(coord: &Coordinator) -> Vec<String> {
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    assert_eq!(
        coord.debug_active_mode(),
        Some("mix"),
        "前提：`;` 进快捷输入"
    );
    type_str(coord, "Hel");
    coord.debug_all_candidate_texts()
}

#[test]
fn uppercase_input_projects_dict_candidates() {
    skip_without_data!();
    let (coord, _store, db) = open("follow", |_| {});
    let all = quick_hel(&coord);
    assert_eq!(all.first().map(String::as_str), Some("Hel"), "原文仍钉首位");
    assert!(
        all.iter().any(|t| t == "Hello"),
        "词库 `hello` 应跟随输入投影成 `Hello`：{all:?}"
    );
    assert!(
        !all.iter().any(|t| t == "hello"),
        "投影后不该再留小写那条：{all:?}"
    );
    let _ = std::fs::remove_file(&db);
}

#[test]
fn case_follow_off_keeps_dict_case() {
    skip_without_data!();
    let (coord, _store, db) = open("nofollow", |c| {
        c.input.temp_english.case_follow_input = false;
    });
    let all = quick_hel(&coord);
    assert_eq!(all.first().map(String::as_str), Some("Hel"));
    assert!(
        all.iter().any(|t| t == "hello"),
        "关掉跟随时词库原文照旧：{all:?}"
    );
    let _ = std::fs::remove_file(&db);
}

/// 选中投影后的词库词：上屏所见形态，词频按词库原文记（同临英）。
#[test]
fn selecting_projected_word_records_dict_freq() {
    skip_without_data!();
    let (coord, store, db) = open("select", |_| {});
    let all = quick_hel(&coord);
    let pos = all
        .iter()
        .position(|t| t == "Hello")
        .unwrap_or_else(|| panic!("前提：有 `Hello`：{all:?}"));
    for _ in 0..pos {
        coord.handle_key_event(&key(VK_DOWN, 0));
    }
    match coord.handle_key_event(&key(VK_SPACE, 0)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "Hello"),
        other => panic!("空格应上屏高亮，实际: {other:?}"),
    }
    assert!(
        store
            .get_freq("english", "hello", "hello")
            .unwrap()
            .is_some(),
        "词频按词库原文 `hello` 记"
    );
    let _ = std::fs::remove_file(&db);
}

/// 含非字母字符的字面输入照旧只有原文一条（A2-23 的 Free 语义不动）。
#[test]
fn literal_with_symbols_stays_single_candidate() {
    skip_without_data!();
    let (coord, _store, db) = open("literal", |_| {});
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    type_str(&coord, "Hel");
    coord.handle_key_event(&key(0xBD, 0)); // `-`
    type_str(&coord, "lo");
    assert_eq!(
        coord.debug_all_candidate_texts(),
        vec!["Hel-lo".to_string()]
    );
    let _ = std::fs::remove_file(&db);
}

// ── 大小写档位循环（`input.english_case_cycle_key`）对齐临英 ──
//
// 真实触发点 CapsLock 走全局钩子（headless 不可达），经 `debug_cycle_english_case` 调同一函数。

/// 文本透镜（全小写缓冲）：英文段随档位整段套形；中文候选不动。
#[test]
fn case_cycle_applies_to_english_segment_in_text_lens() {
    skip_without_data!();
    let (coord, _store, db) = open("cycle_text", |c| {
        c.input.english_case_cycle_key = "capslock".into();
    });
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    type_str(&coord, "hello");
    let before = coord.debug_all_candidate_texts();
    let pos = before
        .iter()
        .position(|t| t == "hello")
        .unwrap_or_else(|| panic!("前提：{before:?}"));
    assert!(
        pos == 0 || !coord.debug_cycle_english_case(),
        "高亮中文候选时不夺取"
    );
    // 夺取判据只看高亮：移到英文候选上才切档。
    for _ in 0..pos {
        coord.handle_key_event(&key(VK_DOWN, 0));
    }
    assert!(
        coord.debug_cycle_english_case(),
        "高亮英文候选时应夺取档位键"
    );
    let upper = coord.debug_all_candidate_texts();
    assert!(upper.iter().any(|t| t == "HELLO"), "全大写档：{upper:?}");
    assert!(!upper.iter().any(|t| t == "hello"), "全大写档：{upper:?}");
    let cn: Vec<&String> = before.iter().filter(|t| !t.is_ascii()).collect();
    assert!(
        cn.iter().all(|t| upper.contains(t)),
        "中文候选不受档位影响：{before:?} → {upper:?}"
    );
    let _ = std::fs::remove_file(&db);
}

/// 自由透镜（带大写的纯字母缓冲）：英文段同样随档位循环；退出后档位复位。
#[test]
fn case_cycle_applies_in_free_lens_and_resets_on_exit() {
    skip_without_data!();
    let (coord, _store, db) = open("cycle_free", |c| {
        c.input.english_case_cycle_key = "capslock".into();
    });
    quick_hel(&coord);
    assert!(coord.debug_cycle_english_case());
    let upper = coord.debug_all_candidate_texts();
    assert!(upper.iter().any(|t| t == "HELLO"), "全大写档：{upper:?}");
    assert!(coord.debug_cycle_english_case());
    let lower = coord.debug_all_candidate_texts();
    assert!(lower.iter().any(|t| t == "hello"), "全小写档：{lower:?}");
    assert!(!lower.iter().any(|t| t == "Hello"), "全小写档：{lower:?}");
    // Esc 退出再进：档位属于那一次组合，不得串到下一次。
    coord.handle_key_event(&key(0x1B, 0));
    let again = quick_hel(&coord);
    assert!(
        again.iter().any(|t| t == "Hello"),
        "档位应已复位：{again:?}"
    );
    let _ = std::fs::remove_file(&db);
}

/// 没有英文段（纯中文候选）时不夺取：按键归原语义。
#[test]
fn case_cycle_not_taken_without_english_segment() {
    skip_without_data!();
    let (coord, _store, db) = open("cycle_none", |c| {
        c.input.english_case_cycle_key = "capslock".into();
        c.schema.mix_modes[0].members.retain(|m| m != "english");
    });
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    type_str(&coord, "nihao");
    assert!(!coord.debug_all_candidate_texts().is_empty(), "前提");
    assert!(!coord.debug_cycle_english_case());
    let _ = std::fs::remove_file(&db);
}

// ── 档位键夺取收窄：只在高亮英文候选时夺取 ──

/// 档位键配成空格：高亮中文候选时空格仍是选词。
#[test]
fn space_cycle_key_still_selects_chinese_highlight() {
    skip_without_data!();
    let (coord, _store, db) = open("space_cn", |c| {
        c.input.english_case_cycle_key = "space".into();
    });
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    type_str(&coord, "nihao");
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some("你好"),
        "前提：`;nihao` 高亮「你好」"
    );
    match coord.handle_key_event(&key(VK_SPACE, 0)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "你好"),
        other => panic!("中文高亮时空格应选词，实际: {other:?}"),
    }
    let _ = std::fs::remove_file(&db);
}

/// 档位键配成空格：高亮英文候选时空格切档，不上屏。
#[test]
fn space_cycle_key_cycles_on_english_highlight() {
    skip_without_data!();
    let (coord, _store, db) = open("space_en", |c| {
        c.input.english_case_cycle_key = "space".into();
    });
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    type_str(&coord, "hello");
    let all = coord.debug_all_candidate_texts();
    let pos = all
        .iter()
        .position(|t| t == "hello")
        .unwrap_or_else(|| panic!("前提：有英文候选 `hello`：{all:?}"));
    for _ in 0..pos {
        coord.handle_key_event(&key(VK_DOWN, 0));
    }
    assert!(
        matches!(
            coord.handle_key_event(&key(VK_SPACE, 0)),
            KeyAction::Consumed
        ),
        "高亮英文时空格应被档位循环夺取"
    );
    let after = coord.debug_all_candidate_texts();
    assert!(
        after.iter().any(|t| t == "HELLO"),
        "应切到全大写档：{after:?}"
    );
    let _ = std::fs::remove_file(&db);
}

/// Free 透镜里非纯字母的字面输入（`;C++`）不是英文词：不夺取。
#[test]
fn free_literal_with_symbols_is_not_taken() {
    skip_without_data!();
    let (coord, _store, db) = open("free_lit", |c| {
        c.input.english_case_cycle_key = "capslock".into();
    });
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    type_str(&coord, "C");
    coord.handle_key_event(&key(0xBB, MOD_SHIFT)); // `+`
    coord.handle_key_event(&key(0xBB, MOD_SHIFT));
    assert_eq!(
        coord.debug_all_candidate_texts(),
        vec!["C++".to_string()],
        "前提"
    );
    assert!(
        !coord.debug_cycle_english_case(),
        "字面输入不该被档位循环夺取"
    );
    let _ = std::fs::remove_file(&db);
}

// ── 自由输入关闭（`free_input = off`，A2-50 / t235）──
//
// off 的语义是「数字 / 符号键保持功能键身份」。含英文成员的实例里 Shift+字母仍以大写进
// 缓冲，候选只给英文段（与 auto 下同一份逻辑），但数字键照旧选词、`-`/`=` 照旧翻页——
// 不能因为缓冲进了大写就把整个会话拖进字面输入。

fn open_off(tag: &str, edit: impl FnOnce(&mut Config)) -> (Arc<Coordinator>, Arc<Store>, PathBuf) {
    open(tag, |c| {
        c.schema.mix_modes[0].free_input = FreeInputMode::Off;
        edit(c);
    })
}

/// off + 含英文成员：Shift 打 `Hel` 出英文段，原文钉首位、词库词跟随大小写；中文成员不参与。
#[test]
fn off_uppercase_projects_english_segment() {
    skip_without_data!();
    let (coord, _store, db) = open_off("off_follow", |_| {});
    let all = quick_hel(&coord);
    assert_eq!(
        all.first().map(String::as_str),
        Some("Hel"),
        "原文钉首位：{all:?}"
    );
    assert!(
        all.iter().any(|t| t == "Hello"),
        "词库词跟随大小写：{all:?}"
    );
    assert!(all.iter().all(|t| t.is_ascii()), "中文成员不参与：{all:?}");
    assert!(coord.debug_preedit().ends_with("Hel"), "缓冲保留大写");
    let _ = std::fs::remove_file(&db);
}

/// off 下大写缓冲按数字键是**选词**，不是字面输入。
#[test]
fn off_uppercase_digit_still_selects() {
    skip_without_data!();
    let (coord, _store, db) = open_off("off_digit", |_| {});
    quick_hel(&coord);
    let page = coord.debug_page_texts();
    assert!(page.len() >= 2, "前提：至少两条候选：{page:?}");
    match coord.handle_key_event(&key(0x32, 0)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text.trim_end(), page[1]),
        other => panic!("数字键应选第 2 候选，实际: {other:?}"),
    }
    let _ = std::fs::remove_file(&db);
}

/// off 下大写缓冲按 `-`：翻页键，不是字面（缓冲不变）。
#[test]
fn off_uppercase_minus_is_not_literal() {
    skip_without_data!();
    let (coord, _store, db) = open_off("off_minus", |_| {});
    let before = quick_hel(&coord);
    coord.handle_key_event(&key(0xBD, 0)); // `-`
    assert_eq!(
        coord.debug_active_mode(),
        Some("mix"),
        "`-` 不该顶屏退出（它是翻页键）"
    );
    assert!(coord.debug_preedit().ends_with("Hel"), "缓冲不变");
    assert_eq!(coord.debug_all_candidate_texts(), before);
    let _ = std::fs::remove_file(&db);
}

/// off 且不含英文成员：维持现状，Shift 被丢弃、缓冲恒小写。
#[test]
fn off_without_english_member_stays_lowercase() {
    skip_without_data!();
    let (coord, _store, db) = open_off("off_noen", |c| {
        c.schema.mix_modes[0].members.retain(|m| m != "english");
    });
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    type_str(&coord, "Nihao");
    let preedit = coord.debug_preedit();
    assert!(
        !preedit.chars().any(|c| c.is_ascii_uppercase()),
        "缓冲恒小写：{preedit}"
    );
    assert!(
        coord
            .debug_all_candidate_texts()
            .iter()
            .any(|t| t == "你好"),
        "照旧出中文候选"
    );
    let _ = std::fs::remove_file(&db);
}

/// 大小写混合（先小写 `m` 再 Shift+A）：与 auto 下同样输入的候选逐条一致。
#[test]
fn off_mixed_case_matches_auto() {
    skip_without_data!();
    let run = |coord: &Coordinator| {
        coord.handle_key_event(&key(VK_SEMICOLON, 0));
        type_str(coord, "mA");
        coord.debug_all_candidate_texts()
    };
    let (off, _s1, db1) = open_off("off_mixed", |_| {});
    let (auto, _s2, db2) = open("auto_mixed", |_| {});
    let (a, b) = (run(&off), run(&auto));
    assert_eq!(a.first().map(String::as_str), Some("mA"), "{a:?}");
    assert_eq!(a, b, "off 与 auto 的英文段应一致");
    let _ = std::fs::remove_file(&db1);
    let _ = std::fs::remove_file(&db2);
}

/// 退格删到没有大写：回到普通文本透镜（中文成员重新参与）。
#[test]
fn off_backspace_to_lowercase_restores_text_lens() {
    skip_without_data!();
    let (coord, _store, db) = open_off("off_back", |_| {});
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    type_str(&coord, "niH");
    assert!(
        coord
            .debug_all_candidate_texts()
            .iter()
            .all(|t| t.is_ascii())
    );
    coord.handle_key_event(&key(0x08, 0));
    let all = coord.debug_all_candidate_texts();
    assert!(all.iter().any(|t| !t.is_ascii()), "中文候选应回来：{all:?}");
    let _ = std::fs::remove_file(&db);
}

/// 对照：auto 下大写缓冲按数字键仍是字面（Free 语义不变）。
#[test]
fn auto_uppercase_digit_is_literal() {
    skip_without_data!();
    let (coord, _store, db) = open("auto_digit", |_| {});
    quick_hel(&coord);
    coord.handle_key_event(&key(0x32, 0));
    assert!(
        coord.debug_preedit().ends_with("Hel2"),
        "{}",
        coord.debug_preedit()
    );
    let _ = std::fs::remove_file(&db);
}

/// off 下选中投影词：词频按词库原文（小写码）记进英文桶，与 auto 同口径。
#[test]
fn off_selecting_projected_word_records_dict_freq() {
    skip_without_data!();
    let (coord, store, db) = open_off("off_select", |_| {});
    let all = quick_hel(&coord);
    let pos = all
        .iter()
        .position(|t| t == "Hello")
        .unwrap_or_else(|| panic!("前提：有 `Hello`：{all:?}"));
    for _ in 0..pos {
        coord.handle_key_event(&key(VK_DOWN, 0));
    }
    match coord.handle_key_event(&key(VK_SPACE, 0)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text.trim_end(), "Hello"),
        other => panic!("空格应上屏高亮，实际: {other:?}"),
    }
    assert!(
        store
            .get_freq("english", "hello", "hello")
            .unwrap()
            .is_some(),
        "词频按词库原文 `hello` 记"
    );
    let _ = std::fs::remove_file(&db);
}

/// off 下大写缓冲按 `,`：照旧顶屏高亮候选 + 标点并退出（不是字面）。
#[test]
fn off_uppercase_punct_commits_highlight() {
    skip_without_data!();
    let (coord, _store, db) = open_off("off_punct", |_| {});
    quick_hel(&coord);
    match coord.handle_key_event(&key(0xBC, 0)) {
        KeyAction::InsertText { text, .. } => assert!(text.starts_with("Hel"), "{text}"),
        other => panic!("`,` 应顶屏，实际: {other:?}"),
    }
    assert_ne!(coord.debug_active_mode(), Some("mix"));
    let _ = std::fs::remove_file(&db);
}
