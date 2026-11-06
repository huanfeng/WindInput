//! 临拼 / 快捷输入 / 临英剩余码**无候选**时按标点：与主路空码标点同一套语义
//! （`input.punct_on_empty_behavior`，唯一解释器 `punct_empty_code_policy`）。
//!
//! - `commit`：上屏「引导字母 + 已选段 + 剩余原码」再接标点；原码按本模式来源记统计、
//!   进上屏历史（转换前形态、不含标点），标点按 `Punctuation` 记。
//! - `clear`（出厂）：已选段与剩余原码都丢，只出标点。
//! - `clear_no_input`：连标点也不出，整键当没按过（退出模式、清组合）。
//!
//! 现场：临拼兜底标点臂无候选时 `exit + ClearComposition`，已选段 / 剩余码 / 标点三样
//! 全丢；快捷输入 ⑥ 无高亮候选时只上屏已选段 + 标点，剩余原码与字母引导符静默消失。
//! 两者都不读 `punct_on_empty_behavior`。临英无候选时恒上屏原文 + 标点，而英文方案主路
//! 同样的无候选状态按该开关处置。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_config::config::FreeInputMode;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};
use wind_store::stats::CommitSource;

const VK_BACKTICK: u32 = 0xC0;
const VK_SEMICOLON: u32 = 0xBA;
const VK_COMMA: u32 = 0xBC;
const VK_NEXT: u32 = 0x22;

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
        coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
}

/// 五笔主方案；快捷输入去掉英文成员（它恒给出「所打原文」候选，造不出无候选），
/// 关掉自由输入（开着时标点在 ⑤ 作字面进缓冲，到不了 ⑥）。
fn open(policy: &str, edit: impl FnOnce(&mut Config)) -> Arc<Coordinator> {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.input.temp_english.enabled = true;
    c.input.punct_on_empty_behavior = policy.into();
    c.schema.mix_modes[0].members.retain(|m| m != "english");
    c.schema.mix_modes[0].free_input = FreeInputMode::Off;
    edit(&mut c);
    Coordinator::new_headless(c, Some(&data_dir()))
}

/// 真机出厂 `zz*` 短语的最小替身：装上后首键 z 是活码前缀、让位，第二键破前缀才夺取。
fn install_zz(coord: &Coordinator) {
    coord.debug_install_phrases(vec![wind_phrase::PhraseSeed {
        code: "zzbd".into(),
        text: "、".into(),
        weight: 0,
        position: 0,
        is_system: true,
        category: String::new(),
    }]);
}

/// 翻页找「你」并数字键选中（分步，留在组合区）。
fn pick_ni(coord: &Coordinator) {
    for _ in 0..40 {
        let t = coord.debug_page_texts();
        if let Some(p) = t.iter().position(|x| x == "你") {
            let act = coord.handle_key_event(&key(0x31 + p as u32, 0));
            assert!(
                !matches!(act, KeyAction::InsertText { .. }),
                "前提：选「你」应留在组合区分步，实际: {act:?}"
            );
            return;
        }
        coord.handle_key_event(&key(VK_NEXT, 0));
    }
    panic!("前提：候选里应有「你」");
}

fn assert_no_candidates(coord: &Coordinator) {
    assert!(
        coord.debug_page_texts().is_empty(),
        "前提：剩余码不应有候选，实际: {:?}",
        coord.debug_page_texts()
    );
}

/// 按 `,`，返回上屏文本（`ClearComposition` 为 `None`）。
fn comma(coord: &Coordinator) -> Option<String> {
    match coord.handle_key_event(&key(VK_COMMA, 0)) {
        KeyAction::InsertText { text, .. } => Some(text),
        KeyAction::ClearComposition => None,
        other => panic!("无候选按标点应上屏或清空，实际: {other:?}"),
    }
}

/// 进快捷输入（空缓冲）断言重复上屏候选就是 `want`（即它进了上屏历史）。
fn assert_repeat(coord: &Coordinator, want: &str) {
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some(want),
        "上屏的原码应进上屏历史（`;` 重复上屏取得到）"
    );
}

// ── 临时拼音 ──

/// 反引号进临拼、打 `nivvvvvvvv`、分步选「你」，剩余 `vvvvvvvv` 无候选。
fn temp_pinyin_ni_vvv(coord: &Coordinator) {
    coord.handle_key_event(&key(VK_BACKTICK, 0));
    assert!(coord.debug_in_temp_pinyin(), "前提：反引号进临拼");
    type_str(coord, "nivvvvvvvv");
    pick_ni(coord);
    assert_no_candidates(coord);
}

#[test]
fn temp_pinyin_commit_keeps_segments_raw_and_punct() {
    skip_without_data!();
    let coord = open("commit", |_| {});
    temp_pinyin_ni_vvv(&coord);
    coord.debug_capture_stat_events();
    assert_eq!(
        comma(&coord).as_deref(),
        Some("你vvvvvvvv，"),
        "commit：已选段 + 剩余原码 + 标点"
    );
    assert_eq!(
        coord.debug_take_stat_events(),
        vec![
            (CommitSource::TempPinyin, "vvvvvvvv".to_string()),
            (CommitSource::Punctuation, "，".to_string()),
        ],
        "剩余原码按临拼来源记（已选段选词时已记），标点单独记"
    );
    assert!(!coord.debug_in_temp_pinyin(), "上屏后应退出临拼");
    assert_repeat(&coord, "你vvvvvvvv");
}

#[test]
fn temp_pinyin_commit_returns_letter_guide() {
    skip_without_data!();
    let coord = open("commit", |c| {
        c.schema.codetable.z_key_action = "temp_pinyin".into()
    });
    install_zz(&coord);
    type_str(&coord, "zvvv");
    assert!(coord.debug_in_temp_pinyin(), "前提：`zv` 破前缀夺取进临拼");
    assert_no_candidates(&coord);
    assert_eq!(
        comma(&coord).as_deref(),
        Some("zvvv，"),
        "字母引导符 z 应归还（同空格 / 回车）"
    );
}

#[test]
fn temp_pinyin_clear_outputs_punct_only() {
    skip_without_data!();
    let coord = open("clear", |_| {});
    temp_pinyin_ni_vvv(&coord);
    coord.debug_capture_stat_events();
    assert_eq!(
        comma(&coord).as_deref(),
        Some("，"),
        "clear：丢已选段与剩余码，标点照出（同主路）"
    );
    assert_eq!(
        coord.debug_take_stat_events(),
        vec![(CommitSource::Punctuation, "，".to_string())]
    );
    assert!(!coord.debug_in_temp_pinyin(), "应退出临拼");
}

#[test]
fn temp_pinyin_clear_no_input_drops_everything() {
    skip_without_data!();
    let coord = open("clear_no_input", |_| {});
    temp_pinyin_ni_vvv(&coord);
    assert_eq!(comma(&coord), None, "clear_no_input：标点也不出");
    assert!(!coord.debug_in_temp_pinyin(), "应退出临拼");
}

/// 刚进临拼、一个码都没打就按标点：不是空码，标点照出（主路空闲按标点同理）。
#[test]
fn temp_pinyin_empty_buffer_punct_is_output() {
    skip_without_data!();
    let coord = open("clear_no_input", |_| {});
    coord.handle_key_event(&key(VK_BACKTICK, 0));
    assert!(coord.debug_in_temp_pinyin(), "前提：反引号进临拼");
    assert_eq!(comma(&coord).as_deref(), Some("，"));
    assert!(!coord.debug_in_temp_pinyin(), "应退出临拼");
}

// ── 快捷输入 ──

fn quick_ni_vvv(coord: &Coordinator) {
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    assert_eq!(
        coord.debug_active_mode(),
        Some("mix"),
        "前提：`;` 进快捷输入"
    );
    type_str(coord, "nivvvvvvvv");
    pick_ni(coord);
    assert_no_candidates(coord);
}

#[test]
fn quick_input_commit_keeps_segments_raw_and_punct() {
    skip_without_data!();
    let coord = open("commit", |_| {});
    quick_ni_vvv(&coord);
    coord.debug_capture_stat_events();
    assert_eq!(
        comma(&coord).as_deref(),
        Some("你vvvvvvvv，"),
        "commit：已选段 + 剩余原码 + 标点"
    );
    assert_eq!(
        coord.debug_take_stat_events(),
        vec![
            (CommitSource::Mix, "vvvvvvvv".to_string()),
            (CommitSource::Punctuation, "，".to_string()),
        ]
    );
    assert_eq!(coord.debug_active_mode(), None, "应退出快捷输入");
    assert_repeat(&coord, "你vvvvvvvv");
}

#[test]
fn quick_input_commit_returns_letter_guide() {
    skip_without_data!();
    let coord = open("commit", |c| {
        c.schema.codetable.z_key_action = "mix:quick_mix".into()
    });
    install_zz(&coord);
    type_str(&coord, "zvvv");
    assert_eq!(
        coord.debug_active_mode(),
        Some("mix"),
        "前提：`zv` 破前缀夺取进快捷输入"
    );
    assert_no_candidates(&coord);
    assert_eq!(comma(&coord).as_deref(), Some("zvvv，"));
}

#[test]
fn quick_input_clear_outputs_punct_only() {
    skip_without_data!();
    let coord = open("clear", |_| {});
    quick_ni_vvv(&coord);
    assert_eq!(
        comma(&coord).as_deref(),
        Some("，"),
        "clear：连已选段一起丢，标点照出（同主路）"
    );
    assert_eq!(coord.debug_active_mode(), None, "应退出快捷输入");
}

#[test]
fn quick_input_clear_no_input_drops_everything() {
    skip_without_data!();
    let coord = open("clear_no_input", |_| {});
    quick_ni_vvv(&coord);
    assert_eq!(comma(&coord), None, "clear_no_input：标点也不出");
    assert_eq!(coord.debug_active_mode(), None, "应退出快捷输入");
}

// ── 临时英文（对照英文方案主路） ──

/// 临英关掉词库候选、原文候选与大小写变形 ⇒ 恒无候选。
fn no_temp_english_candidates(c: &mut Config) {
    c.input.temp_english.show_candidates = false;
    c.input.temp_english.raw_candidate = wind_config::config::RawCandidateMode::Off;
    c.input.temp_english.case_variants = false;
}

fn temp_english_helo(coord: &Coordinator) {
    coord.handle_key_event(&key(u32::from(b'H'), MOD_SHIFT));
    type_str(coord, "elo");
    assert_eq!(
        coord.debug_active_mode(),
        Some("temp_english"),
        "前提：Shift+H 进临英"
    );
    assert_no_candidates(coord);
}

#[test]
fn temp_english_commit_keeps_raw_and_punct() {
    skip_without_data!();
    let coord = open("commit", no_temp_english_candidates);
    temp_english_helo(&coord);
    assert_eq!(comma(&coord).as_deref(), Some("Helo，"));
}

#[test]
fn temp_english_clear_outputs_punct_only() {
    skip_without_data!();
    let coord = open("clear", no_temp_english_candidates);
    temp_english_helo(&coord);
    coord.debug_capture_stat_events();
    assert_eq!(
        comma(&coord).as_deref(),
        Some("，"),
        "clear：同英文方案主路，丢原文、标点照出"
    );
    assert_eq!(
        coord.debug_take_stat_events(),
        vec![(CommitSource::Punctuation, "，".to_string())]
    );
    assert_eq!(coord.debug_active_mode(), None, "应退出临英");
}

#[test]
fn temp_english_clear_no_input_drops_everything() {
    skip_without_data!();
    let coord = open("clear_no_input", no_temp_english_candidates);
    temp_english_helo(&coord);
    assert_eq!(comma(&coord), None);
    assert_eq!(coord.debug_active_mode(), None, "应退出临英");
}

/// 对照组：英文方案主路同一状态（原文候选关、词库无命中 ⇒ 无候选）按同一开关处置。
/// 它不是本次改动的对象，只钉住「临英对齐的是什么」。
#[test]
fn english_schema_main_path_reference() {
    skip_without_data!();
    for (policy, want) in [
        ("commit", Some("qzxv,")),
        ("clear", Some(",")),
        ("clear_no_input", None),
    ] {
        let mut c = Config::default();
        c.schema.available = vec!["english".into()];
        c.schema.active = "english".into();
        c.input.default.chinese_mode = true;
        c.input.punct_on_empty_behavior = policy.into();
        c.schema.english.raw_candidate = wind_config::config::RawCandidateMode::Off;
        let coord = Coordinator::new_headless(c, Some(&data_dir()));
        type_str(&coord, "qzxv");
        assert_no_candidates(&coord);
        assert_eq!(comma(&coord).as_deref(), want, "英文方案主路 {policy}");
    }
}

// ── 字母引导符算码：z 进模式、空缓冲按标点 ──
//
// headless 无 store ⇒ 短语层空、`z` 不是活码前缀，首键直接进模式（前缀 `z`、缓冲空）。

fn z_then_comma(policy: &str, z_action: &str, only_pinyin_member: bool) -> Option<String> {
    let coord = open(policy, |c| {
        c.schema.codetable.z_key_action = z_action.into();
        if only_pinyin_member {
            // 去掉算式类成员：否则空缓冲的 `,` 会开数字透镜作表达式字符入缓冲，到不了 ⑥。
            c.schema.mix_modes[0].members =
                vec![wind_config::config::MIX_MEMBER_PRIMARY_PINYIN.to_string()];
        }
    });
    type_str(&coord, "z");
    assert!(
        coord.debug_active_mode().is_some(),
        "前提：z 直接进模式（{z_action}）"
    );
    let out = comma(&coord);
    assert_eq!(coord.debug_active_mode(), None, "应退出模式");
    out
}

#[test]
fn temp_pinyin_letter_guide_counts_as_code() {
    skip_without_data!();
    assert_eq!(
        z_then_comma("commit", "temp_pinyin", false).as_deref(),
        Some("z，"),
        "commit：归还引导符 z 再接标点"
    );
    assert_eq!(
        z_then_comma("clear", "temp_pinyin", false).as_deref(),
        Some("，"),
        "clear：z 算废码丢掉，标点照出"
    );
    assert_eq!(
        z_then_comma("clear_no_input", "temp_pinyin", false),
        None,
        "clear_no_input：z 与标点都不出"
    );
}

#[test]
fn quick_input_letter_guide_counts_as_code() {
    skip_without_data!();
    assert_eq!(
        z_then_comma("commit", "mix:quick_mix", true).as_deref(),
        Some("z，")
    );
    assert_eq!(
        z_then_comma("clear", "mix:quick_mix", true).as_deref(),
        Some("，")
    );
    assert_eq!(z_then_comma("clear_no_input", "mix:quick_mix", true), None);
}

// ── 临拼无候选时的二三候选键：同主路 `keys.overflow.select_key` 的无候选分支 ──

#[test]
fn temp_pinyin_select_key_without_candidate_follows_overflow_policy() {
    skip_without_data!();
    // 出厂 `ignore`：吞键，组合留着继续打（此前一律退出清空，已选段与剩余码丢掉）。
    let coord = open("clear", |_| {});
    temp_pinyin_ni_vvv(&coord);
    assert!(matches!(
        coord.handle_key_event(&key(VK_SEMICOLON, 0)),
        KeyAction::Consumed
    ));
    assert!(coord.debug_in_temp_pinyin(), "ignore：留在临拼");
    // `commit_and_input`：丢码并出该键字符（同主路无候选分支）。
    let coord = open("clear", |c| {
        c.keys.overflow.select_key = "commit_and_input".into()
    });
    temp_pinyin_ni_vvv(&coord);
    match coord.handle_key_event(&key(VK_SEMICOLON, 0)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "；"),
        other => panic!("commit_and_input 应出该键字符，实际: {other:?}"),
    }
    assert!(!coord.debug_in_temp_pinyin(), "应退出临拼");
}
