//! 正确拼音提示的显示正文：协调器按 `meta.pinyin_hints`、来源勾选与 `hint.scope` 生成
//! `CandidateItem.text_styled`（「苹(ping)果」，`(ping)` 段角色 `pinyin_hint`），上屏文字不变。
//!
//! 自造夹具，不依赖 `build_dev/data`：`pinguo` 精确命中「拼过」，in/ing 模糊命中「苹果」「平过」。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use wind_bridge::handler::{CaretData, KeyAction, KeyEventData, MessageHandler};
use wind_config::{Config, HintScope};
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, caret_source};
use wind_ui_types::{CandidateItem, UiCommand};

const DICT: &str = "拼过\tpin guo\t5000\n苹果\tping guo\t3000\n平过\tping guo\t100\n";

struct Fixture {
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn fixture(tag: &str) -> Fixture {
    fixture_with(tag, DICT)
}

fn fixture_with(tag: &str, dict: &str) -> Fixture {
    let dir = std::env::temp_dir().join(format!("wind_pinyin_hint_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(schemas.join("fh")).unwrap();
    std::fs::write(
        schemas.join("fh/mini.dict.yaml"),
        format!("---\nname: mini\nversion: \"1\"\nsort: by_weight\n...\n{dict}"),
    )
    .unwrap();
    std::fs::write(
        schemas.join("fh.schema.toml"),
        "[schema]\nid = \"fh\"\nname = \"fh\"\n[engine]\ntype = \"pinyin\"\n\
         [[dictionaries]]\nid = \"mini\"\npath = \"fh/mini.dict.yaml\"\n\
         type = \"rime_pinyin\"\ndefault = true\n",
    )
    .unwrap();
    Fixture { dir }
}

fn open(f: &Fixture, hint: bool, scope: HintScope) -> (Arc<Coordinator>, Receiver<UiCommand>) {
    open_with(f, hint, scope, |_| {})
}

fn open_with(
    f: &Fixture,
    hint: bool,
    scope: HintScope,
    edit: impl FnOnce(&mut Config),
) -> (Arc<Coordinator>, Receiver<UiCommand>) {
    let mut c = Config::default();
    c.schema.available = vec!["fh".into()];
    c.schema.active = "fh".into();
    c.input.default.chinese_mode = true;
    let fz = &mut c.schema.pinyin.fuzzy;
    fz.enabled = true;
    fz.in_ing = true;
    let h = &mut c.input.correction.hint;
    h.enabled = hint;
    h.scope = scope;
    edit(&mut c);
    Coordinator::new_headless_with_ui(c, Some(&f.dir))
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

fn type_str(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        coord.handle_key_event(&key(ch.to_ascii_uppercase() as u32));
    }
}

/// 放行首显闸门后取最近一次下发的候选页。
fn shown(coord: &Coordinator, rx: &Receiver<UiCommand>) -> Vec<CandidateItem> {
    coord.handle_caret_update(&CaretData {
        x: 100,
        y: 200,
        height: 20,
        composition_start_x: 100,
        composition_start_y: 200,
        source: caret_source::TSF_SELECTION,
        composition_rect: None,
    });
    rx.try_iter()
        .filter_map(|cmd| match cmd {
            UiCommand::UpdateCandidates { candidates, .. } => Some(candidates),
            _ => None,
        })
        .last()
        .expect("应下发候选")
}

fn item<'a>(items: &'a [CandidateItem], text: &str) -> &'a CandidateItem {
    items
        .iter()
        .find(|c| c.text == text)
        .unwrap_or_else(|| panic!("应有候选「{text}」：{:?}", texts(items)))
}

fn texts(items: &[CandidateItem]) -> Vec<&str> {
    items.iter().map(|c| c.text.as_str()).collect()
}

fn pinguo(tag: &str, hint: bool, scope: HintScope) -> Vec<CandidateItem> {
    let f = fixture(tag);
    let (coord, rx) = open(&f, hint, scope);
    type_str(&coord, "pinguo");
    shown(&coord, &rx)
}

fn hint_span(c: &CandidateItem) -> Vec<(&str, Option<&str>)> {
    let s = c.text_styled.as_str();
    c.text_styled
        .spans()
        .iter()
        .map(|sp| (&s[sp.start as usize..sp.end as usize], sp.role))
        .collect()
}

#[test]
fn scope_first_marks_only_first_fuzzy() {
    let items = pinguo("first", true, HintScope::First);
    assert_eq!(
        items[0].text,
        "拼过",
        "前提：首选是精确命中：{:?}",
        texts(&items)
    );
    assert!(items[0].text_styled.is_empty(), "精确命中不标");
    let pg = item(&items, "苹果");
    assert_eq!(pg.text_styled.as_str(), "苹(ping)果");
    assert_eq!(hint_span(pg), vec![("(ping)", Some("pinyin_hint"))]);
    let later = item(&items, "平过");
    assert!(
        later.text_styled.is_empty(),
        "first：第一个之后的模糊候选不标：{:?}",
        later.text_styled
    );
}

#[test]
fn scope_all_marks_every_fuzzy() {
    let items = pinguo("all", true, HintScope::All);
    assert!(item(&items, "拼过").text_styled.is_empty());
    assert_eq!(item(&items, "苹果").text_styled.as_str(), "苹(ping)果");
    assert_eq!(item(&items, "平过").text_styled.as_str(), "平(ping)过");
}

#[test]
fn hint_off_text_styled_empty() {
    let items = pinguo("off", false, HintScope::All);
    assert!(
        items.iter().any(|c| c.text == "苹果"),
        "{:?}",
        texts(&items)
    );
    assert!(items.iter().all(|c| c.text_styled.is_empty()));
}

#[test]
fn commit_text_unchanged() {
    let f = fixture("commit");
    let (coord, rx) = open(&f, true, HintScope::First);
    type_str(&coord, "pinguo");
    let items = shown(&coord, &rx);
    let pos = items
        .iter()
        .position(|c| c.text == "苹果")
        .expect("应有「苹果」");
    assert_eq!(
        items[pos].text_styled.as_str(),
        "苹(ping)果",
        "前提：带提示"
    );
    let action = coord.handle_key_event(&key('1' as u32 + pos as u32));
    match action {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "苹果"),
        other => panic!("应上屏：{other:?}"),
    }
}

/// `first` 按**协调器能显示出提示**判第一条：首个带提示的候选（「平品」，提示在第 2 字）被
/// `max_chars = 1` 截成「平…」、提示随之丢掉，顺延给下一条（「品平」，提示在第 1 字）。
#[test]
fn scope_first_skips_candidate_whose_hint_is_truncated_away() {
    let f = fixture_with(
        "first_trunc",
        "平平\tping ping\t5000\n平品\tping pin\t3000\n品平\tpin ping\t100\n",
    );
    let (coord, rx) = open_with(&f, true, HintScope::First, |c| {
        c.ui.candidate.max_chars = 1;
    });
    type_str(&coord, "pingping");
    let items = shown(&coord, &rx);
    assert_eq!(texts(&items), vec!["平…", "平…", "品…"], "前提：排序与截断");
    assert!(items[0].text_styled.is_empty(), "精确命中不标");
    assert!(items[1].text_styled.is_empty(), "提示所在的字被截掉");
    assert_eq!(items[2].text_styled.as_str(), "品(pin)…");
}

/// `first` 在整条排序列表上找第一个带提示的候选，翻页后它不在当前页，之后各页都不带提示。
#[test]
fn scope_first_later_pages_have_no_hint() {
    let f = fixture("first_pages");
    let (coord, rx) = open_with(&f, true, HintScope::First, |c| {
        c.ui.candidate.per_page = 2;
    });
    type_str(&coord, "pinguo");
    let page1 = shown(&coord, &rx);
    assert_eq!(
        texts(&page1),
        vec!["拼过", "苹果"],
        "前提：第 1 页含首个提示"
    );
    assert_eq!(page1[1].text_styled.as_str(), "苹(ping)果");

    const VK_NEXT: u32 = 0x22;
    coord.handle_key_event(&key(VK_NEXT));
    let page2 = shown(&coord, &rx);
    assert_eq!(texts(&page2), vec!["平过"], "前提：翻到第 2 页");
    assert!(
        page2.iter().all(|c| c.text_styled.is_empty()),
        "first：之后的页不带提示：{:?}",
        page2
            .iter()
            .map(|c| c.text_styled.as_str())
            .collect::<Vec<_>>()
    );
}

/// 读音表（pinyin-data 格式），带调提示与 `${correct_pinyin}` 带调值取自它。
fn write_pinyin_map(f: &Fixture) {
    std::fs::write(
        f.dir.join("pinyin_map.txt"),
        "U+4E2D: zhōng,zhòng  # 中\nU+56FD: guó  # 国\nU+5E73: píng  # 平\n\
         U+62FC: pīn  # 拼\nU+82F9: píng  # 苹\nU+8FC7: guò,guō  # 过\n",
    )
    .unwrap();
}

/// `hint.tone`（§9.1）：内嵌提示用词库音节筛读音表取调。
#[test]
fn tone_marks_toned_syllable() {
    let f = fixture("tone");
    write_pinyin_map(&f);
    let (coord, rx) = open_with(&f, true, HintScope::All, |c| {
        c.input.correction.hint.tone = true;
    });
    type_str(&coord, "pinguo");
    let items = shown(&coord, &rx);
    let pg = item(&items, "苹果");
    assert_eq!(pg.text_styled.as_str(), "苹(píng)果");
    assert_eq!(hint_span(pg), vec![("(píng)", Some("pinyin_hint"))]);
}

/// 读音表里查不到该字的读音：退回不带调的词库音节（宁少调不错音）。
#[test]
fn tone_falls_back_to_plain_syllable_without_reading() {
    let f = fixture("tone_fb");
    let (coord, rx) = open_with(&f, true, HintScope::All, |c| {
        c.input.correction.hint.tone = true;
    });
    type_str(&coord, "pinguo");
    let items = shown(&coord, &rx);
    assert_eq!(item(&items, "苹果").text_styled.as_str(), "苹(ping)果");
}

const ZG: &str = "中国\tzhong guo\t3000\n宗\tzong\t100\n";

/// 拼音类型分配一个只写 `${correct_pinyin}` 的模板（竖排没写时同横排）。
fn assign_correct_pinyin(c: &mut Config) {
    c.ui.comment
        .templates
        .push(wind_config::comment_lib::CommentTemplateDef {
            id: "fz".into(),
            name: "fz".into(),
            text: "${correct_pinyin}".into(),
        });
    // 夹具是拼音方案：拼音类型出厂默认态 none，分到类型上。
    c.ui.comment.assign.by_type.insert(
        "pinyin".into(),
        wind_config::comment_lib::CommentCells {
            default_horizontal: Some("fz".into()),
            ..Default::default()
        },
    );
}

/// `zongguo`（开 zh↔z）、`hint=false`、注释模板只写 `${correct_pinyin}`。
fn zongguo(tag: &str, tone: bool) -> Vec<CandidateItem> {
    let f = fixture_with(tag, ZG);
    write_pinyin_map(&f);
    let (coord, rx) = open_with(&f, false, HintScope::First, |c| {
        c.schema.pinyin.fuzzy.zh_z = true;
        c.input.correction.hint.tone = tone;
        assign_correct_pinyin(c);
    });
    type_str(&coord, "zongguo");
    shown(&coord, &rx)
}

/// §9.2：只写模板、不开 `hint`，注释出整词正确拼音，正文不内嵌。
#[test]
fn correct_pinyin_var_fills_comment_without_inline_hint() {
    let items = zongguo("var", false);
    let zg = item(&items, "中国");
    assert_eq!(zg.comment.as_str(), "zhong guo");
    let s = zg.comment.as_str();
    let roles: Vec<_> = zg
        .comment
        .spans()
        .iter()
        .map(|sp| (&s[sp.start as usize..sp.end as usize], sp.role))
        .collect();
    assert_eq!(roles, vec![("zhong guo", Some("correct_pinyin"))]);
    assert!(zg.text_styled.is_empty(), "hint 关：正文不内嵌");
}

/// 带调随 `hint.tone`。
#[test]
fn correct_pinyin_var_toned() {
    let items = zongguo("var_tone", true);
    assert_eq!(item(&items, "中国").comment.as_str(), "zhōng guó");
}

/// 非模糊候选（精确命中的「宗」）该变量为空。
#[test]
fn correct_pinyin_var_empty_for_exact_candidate() {
    let items = zongguo("var_exact", false);
    assert!(item(&items, "宗").comment.is_empty());
}

// ───────────────────── 来源过滤（fuzzy / misread / correction） ─────────────────────

const GP: &str = "公平\tgong ping\t3000\n";

/// `gognping`（`gn_ng` 出厂关，这里打开）纠成「公平」，`edit` 再调提示配置。
fn gognping(tag: &str, edit: impl FnOnce(&mut Config)) -> Vec<CandidateItem> {
    let f = fixture_with(tag, GP);
    let (coord, rx) = open_with(&f, true, HintScope::All, |c| {
        c.input.correction.rules.gn_ng = true;
        edit(c);
    });
    type_str(&coord, "gognping");
    shown(&coord, &rx)
}

/// 勾上纠错来源：纠错候选内嵌正确拼音，标记在提示之前。
#[test]
fn correction_source_inlines_correct_pinyin() {
    let items = gognping("corr_on", |c| c.input.correction.hint.correction = true);
    let gp = item(&items, "*公平");
    assert_eq!(gp.text_styled.as_str(), "*公(gong)平");
    assert_eq!(hint_span(gp), vec![("(gong)", Some("pinyin_hint"))]);
}

/// 出厂来源不含纠错：纠错候选只带标记、不内嵌。
#[test]
fn correction_source_off_by_default() {
    let items = gognping("corr_def", |_| {});
    assert!(item(&items, "*公平").text_styled.is_empty());
}

/// 不勾模糊音来源：模糊候选不内嵌。
#[test]
fn fuzzy_source_unchecked_hides_fuzzy_hint() {
    let f = fixture("fuzzy_off");
    let (coord, rx) = open_with(&f, true, HintScope::All, |c| {
        c.input.correction.hint.fuzzy = false;
    });
    type_str(&coord, "pinguo");
    let items = shown(&coord, &rx);
    assert!(item(&items, "苹果").text_styled.is_empty());
}

/// `${correct_pinyin}` 按来源过滤：只勾误读时，模糊候选的变量为空。
#[test]
fn template_var_respects_sources() {
    let f = fixture_with("var_src", ZG);
    let (coord, rx) = open_with(&f, false, HintScope::First, |c| {
        c.schema.pinyin.fuzzy.zh_z = true;
        let h = &mut c.input.correction.hint;
        h.fuzzy = false;
        h.misread = true;
        h.correction = false;
        assign_correct_pinyin(c);
    });
    type_str(&coord, "zongguo");
    let items = shown(&coord, &rx);
    assert!(item(&items, "中国").comment.is_empty());
}

/// `${correct_pinyin}` 不看总开关与 `scope`：`hint.enabled` 关、`scope=first`，第二个模糊候选也出值。
#[test]
fn template_var_ignores_master_switch() {
    let f = fixture_with(
        "var_master",
        "中国\tzhong guo\t3000\n中过\tzhong guo\t100\n",
    );
    let (coord, rx) = open_with(&f, false, HintScope::First, |c| {
        c.schema.pinyin.fuzzy.zh_z = true;
        assign_correct_pinyin(c);
    });
    type_str(&coord, "zongguo");
    let items = shown(&coord, &rx);
    assert_eq!(item(&items, "中国").comment.as_str(), "zhong guo");
    assert_eq!(item(&items, "中过").comment.as_str(), "zhong guo");
    assert!(items.iter().all(|c| c.text_styled.is_empty()));
}

/// 纠错来源的 `${correct_pinyin}`：勾上即出纠正后的整词拼音。
#[test]
fn template_var_correction_source() {
    let items = gognping("var_corr", |c| {
        c.input.correction.hint.enabled = false;
        c.input.correction.hint.correction = true;
        assign_correct_pinyin(c);
    });
    assert_eq!(item(&items, "*公平").comment.as_str(), "gong ping");
}
