//! 码表用户词里的 `$` 模板（`aadg` → `$Y年$M月$D日`）经**顶码 / 满码自动上屏 / 特殊模式**
//! 三个出口上屏时，词频同样记在展开前的源文本上（GH#177）。
//!
//! 读端 `apply_freq_rerank_in` 按 `Candidate::freq_text()`（模板源文本）查词频；这几个出口的
//! 写端此前直接拿上屏文本（当天的展开结果）记账，读写永不同键，且逐日留下孤儿行。
//! 主选词出口另见 `pinyin_user_word_abbrev_code.rs`。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;

const DATE_TEMPLATE: &str = "$Y年$M月$D日";
const VK_BACKSLASH: u32 = 0xDC;
const VK_COMMA: u32 = 0xBC;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    ["wubi86", "pinyin", "english"]
        .iter()
        .all(|s| d.join(format!("schemas/{s}.schema.toml")).exists())
}

fn key(k: u32) -> KeyEventData {
    KeyEventData {
        key_code: k,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

fn type_str(coord: &Coordinator, s: &str) -> KeyAction {
    let mut last = KeyAction::Consumed;
    for ch in s.chars() {
        last = coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
    last
}

fn inserted(act: &KeyAction) -> String {
    match act {
        KeyAction::InsertText { text, .. } => text.clone(),
        // 顶码：首选上屏、余码留作组合。
        KeyAction::CommitThenDeferComposition { commit_text, .. } => commit_text.clone(),
        other => panic!("前提：应上屏，实际: {other:?}"),
    }
}

fn is_date(t: &str) -> bool {
    t.contains('年') && t.contains('月') && t.contains('日') && !t.contains('$')
}

/// 断言 `(schema, code)` 的词频记在模板源文本上、没有按展开文本 `shown` 记。
fn assert_freq_on_source(store: &Store, schema: &str, code: &str, shown: &str) {
    assert_eq!(
        store
            .get_freq(schema, code, DATE_TEMPLATE)
            .unwrap()
            .map(|r| r.count),
        Some(1),
        "词频应在模板源文本 `{DATE_TEMPLATE}` 上记一次"
    );
    assert!(
        store.get_freq(schema, code, shown).unwrap().is_none(),
        "不应按展开文本「{shown}」记词频（次日即成孤儿行）"
    );
}

/// 主方案 wubi86；`code` 下加一条高权重用户词 `text`。码表行为（调频、顶码、自动上屏）经
/// override 层写进方案名下——方案文件的 `[engine.codetable]` 盖过全局 `schema.codetable`。
fn open_main(
    tag: &str,
    code: &str,
    text: &str,
    codetable_toml: &str,
) -> (Arc<Coordinator>, Arc<Store>, PathBuf) {
    let base =
        std::env::temp_dir().join(format!("wind_tmpl_freq_exits_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let ov = base.join("override");
    std::fs::create_dir_all(&ov).unwrap();
    std::fs::write(
        ov.join("wubi86.toml"),
        format!(
            "[engine.codetable]\n{codetable_toml}\n[engine.codetable.frequency]\nenabled = true\n"
        ),
    )
    .unwrap();
    let store = Arc::new(Store::open(base.join("user.redb")).unwrap());
    store
        .add_user_word("wubi86", code, text, 2_000_000_000, 0)
        .unwrap();
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    let coord = Coordinator::new_headless_with_store_override(
        c,
        Some(&data_dir()),
        Arc::clone(&store),
        Some(ov),
    );
    (coord, store, base)
}

/// 顶码：满码后再打一码，首选（模板词）被顶上屏。
#[test]
fn top_code_commit_records_template_source() {
    if !has_data() {
        return;
    }
    let (coord, store, base) = open_main(
        "top",
        "aadg",
        DATE_TEMPLATE,
        "top_code_commit = true\nauto_commit_at_full = false",
    );
    type_str(&coord, "aadg");
    let first = coord
        .debug_page_texts()
        .first()
        .cloned()
        .unwrap_or_default();
    assert!(
        is_date(&first),
        "前提：模板词应在 `aadg` 首选，实际首选「{first}」"
    );
    let text = inserted(&type_str(&coord, "a"));
    assert_eq!(text, first, "前提：第五码把首选顶上屏");
    assert_freq_on_source(&store, "wubi86", "aadg", &first);
    let _ = std::fs::remove_dir_all(&base);
}

/// 满码唯一自动上屏：`vbnm` 系统词库无字，模板词是唯一候选。
///
/// 顺带钉住：引擎的上屏意向是词条原文 `$Y年$M月$D日`，协调器复核须按 `template_source`
/// 认它——此前只比展开后的 `text`，模板词条恒被否决、从不满码自动上屏。
#[test]
fn auto_commit_at_full_records_template_source() {
    if !has_data() {
        return;
    }
    let (coord, store, base) =
        open_main("auto", "vbnm", DATE_TEMPLATE, "auto_commit_at_full = true");
    let text = inserted(&type_str(&coord, "vbnm"));
    assert!(
        is_date(&text),
        "前提：`vbnm` 满码唯一自动上屏模板词，实际「{text}」"
    );
    assert_freq_on_source(&store, "wubi86", "vbnm", &text);
    let _ = std::fs::remove_dir_all(&base);
}

/// 主方案拼音，`\\` 进以 wubi86 为码表的特殊模式；`code` 下加一条高权重 `text` 用户词。
/// `codetable_toml` 写进特殊方案名下的 `[engine.codetable]`（overlay 方案不继承全局码表配置）。
fn open_special(
    tag: &str,
    code: &str,
    text: &str,
    codetable_toml: &str,
) -> (Arc<Coordinator>, Arc<Store>, PathBuf) {
    let base =
        std::env::temp_dir().join(format!("wind_tmpl_freq_exits_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let ov = base.join("override");
    std::fs::create_dir_all(&ov).unwrap();
    std::fs::write(
        ov.join("wubi86.toml"),
        format!(
            "[overlay]\nkind = \"special\"\n[engine.codetable]\n{codetable_toml}\n\
             [engine.codetable.frequency]\nenabled = true\n"
        ),
    )
    .unwrap();
    let store = Arc::new(Store::open(base.join("user.redb")).unwrap());
    store
        .add_user_word("wubi86", code, text, 2_000_000_000, 0)
        .unwrap();
    let mut c = Config::default();
    c.schema.available = vec!["pinyin".into(), "wubi86".into(), "english".into()];
    c.schema.active = "pinyin".into();
    c.input.default.chinese_mode = true;
    c.keys
        .key_actions
        .insert("backslash".into(), "special:wubi86".into());
    let coord = Coordinator::new_headless_with_store_override(
        c,
        Some(&data_dir()),
        Arc::clone(&store),
        Some(ov),
    );
    coord.handle_key_event(&key(VK_BACKSLASH));
    assert_eq!(
        coord.debug_active_mode(),
        Some("special"),
        "前提：`\\` 进特殊模式"
    );
    (coord, store, base)
}

/// 特殊模式标点顶屏：选词 / 顶屏 / 自动上屏同走 `record_special_selection`。
#[test]
fn special_mode_records_template_source() {
    if !has_data() {
        return;
    }
    let (coord, store, base) = open_special("special", "aa", DATE_TEMPLATE, "");
    type_str(&coord, "aa");
    let first = coord
        .debug_page_texts()
        .first()
        .cloned()
        .unwrap_or_default();
    assert!(
        is_date(&first),
        "前提：模板词应在 `aa` 首选，实际首选「{first}」"
    );
    let text = inserted(&coord.handle_key_event(&key(VK_COMMA)));
    assert_eq!(text, format!("{first}，"), "前提：标点顶屏首选");
    assert_freq_on_source(&store, "wubi86", "aa", &first);
    let _ = std::fs::remove_dir_all(&base);
}

/// 特殊模式满码唯一自动上屏：复核同样按 `template_source` 认引擎意向。
#[test]
fn special_mode_auto_commit_records_template_source() {
    if !has_data() {
        return;
    }
    let (coord, store, base) = open_special(
        "special_auto",
        "vbnm",
        DATE_TEMPLATE,
        "auto_commit_at_full = true",
    );
    let text = inserted(&type_str(&coord, "vbnm"));
    assert!(
        is_date(&text),
        "前提：特殊模式 `vbnm` 满码唯一自动上屏模板词，实际「{text}」"
    );
    assert_freq_on_source(&store, "wubi86", "vbnm", &text);
    let _ = std::fs::remove_dir_all(&base);
}

/// `{..}` 插值（剪贴板 / 反查）**不**满码自动上屏：内容取自外部状态，须经候选窗让用户
/// 看过再选。主路与特殊模式同一判据（`auto_commit_target_matches`），这里钉主路。
///
/// 用 `{code()}`（求值即当前编码）：不依赖剪贴板等外部状态，headless 下稳定非空，候选必在。
#[test]
fn interpolation_user_word_does_not_auto_commit() {
    if !has_data() {
        return;
    }
    let (coord, _, base) = open_main(
        "interp_auto",
        "vbnm",
        "{code()}",
        "auto_commit_at_full = true",
    );
    let act = type_str(&coord, "vbnm");
    let page = coord.debug_page_texts();
    assert_eq!(
        page.len(),
        1,
        "前提：`vbnm` 只有这一条插值候选，实际: {page:?}"
    );
    assert!(
        !page[0].contains('{'),
        "前提：插值已展开，实际「{}」",
        page[0]
    );
    assert!(
        !matches!(act, KeyAction::InsertText { .. }),
        "`{{..}}` 插值词条满码唯一时不应自动上屏，实际: {act:?}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// 码表词库里的 `$CC` 纯文本命令：手动选中按显示标签记词频——与读端重排、满码自动上屏
/// （首候选的 `freq_text`）同键。按求值文本记的话读端永远查不中。
#[test]
fn dict_command_select_records_label() {
    if !has_data() {
        return;
    }
    let (coord, store, base) = open_main(
        "cmd_select",
        "vbnm",
        r#"$CC("标签", type("命令文本"))"#,
        "auto_commit_at_full = false",
    );
    type_str(&coord, "vbnm");
    let page = coord.debug_page_texts();
    assert_eq!(
        page,
        vec!["标签".to_string()],
        "前提：`vbnm` 只有这条命令候选"
    );
    let act = coord.select_candidate(0);
    assert!(
        format!("{act:?}").contains("命令文本"),
        "前提：选中上屏命令求值文本，实际: {act:?}"
    );
    assert_eq!(
        store
            .get_freq("wubi86", "vbnm", "标签")
            .unwrap()
            .map(|r| r.count),
        Some(1),
        "命令词条应按显示标签记一次词频"
    );
    assert!(
        store
            .get_freq("wubi86", "vbnm", "命令文本")
            .unwrap()
            .is_none(),
        "不应按求值文本记词频（读端按标签查，永不命中）"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// 显示态复评（`recheck_auto_commit`）这条来路：引擎首轮见同码两条（系统「工厂」+ 用户词）
/// 判不唯一，shadow 删掉「工厂」后只剩用户词，复评按**展开后的**候选给出意向。
///
/// 插值词条在这条路上同样不放行（复评返回的是展开文本，只比 `text` 会放过它）；
/// `$` 模板词条照常放行——对照组，证明拦的是插值而不是复评整条路。
#[test]
fn recheck_path_blocks_interpolation_but_not_template() {
    if !has_data() {
        return;
    }
    for (tag, text, should_commit) in [
        ("recheck_interp", "{code()}", false),
        ("recheck_tmpl", DATE_TEMPLATE, true),
    ] {
        let (coord, store, base) = open_main(tag, "aadg", text, "auto_commit_at_full = true");
        store.delete_shadow("wubi86", "aadg", "工厂").unwrap();
        let act = type_str(&coord, "aadg");
        let page = coord.debug_page_texts();
        let committed = matches!(act, KeyAction::InsertText { .. });
        assert_eq!(
            committed,
            should_commit,
            "`{text}`：shadow 删掉「工厂」后复评{}自动上屏，实际 act={act:?} page={page:?}",
            if should_commit { "应" } else { "不应" }
        );
        if !should_commit {
            assert_eq!(page.len(), 1, "前提：只剩这一条插值候选，实际: {page:?}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
