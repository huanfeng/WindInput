//! 快捷输入历史（成员 `quick_input.history`，t46）：在快捷输入里上屏过的字面文本，
//! 下次按前缀补全。设计见 `src/quick_history.rs` 模块文档。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库（快捷输入的成员方案要能加载）；缺失时**静默跳过**
//! （判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};
use wind_store::Store;
use wind_store::completion::CompletionKind;

const VK_SEMICOLON: u32 = 0xBA;
const VK_SPACE: u32 = 0x20;
const VK_RETURN: u32 = 0x0D;
const VK_OEM_MINUS: u32 = 0xBD;
const VK_OEM_PLUS: u32 = 0xBB;
const VK_OEM_PERIOD: u32 = 0xBE;

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

fn store_at(tag: &str) -> Arc<Store> {
    let path = std::env::temp_dir().join(format!(
        "wind_quick_history_{}_{tag}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    Arc::new(Store::open(&path).unwrap())
}

/// 五笔主方案（`;` 空码进快捷输入）；`history` 控制是否把历史成员加进 `quick_mix`。
fn open(tag: &str, history: bool) -> (Arc<Coordinator>, Arc<Store>) {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.schema.english.frequency.enabled = true;
    if history {
        let members = &mut c.schema.mix_modes[0].members;
        let at = members
            .iter()
            .position(|m| m == wind_quick_input::MEMBER_REPEAT)
            .map_or(members.len(), |i| i + 1);
        members.insert(at, wind_quick_input::MEMBER_HISTORY.to_string());
    }
    let store = store_at(tag);
    let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), store.clone());
    (coord, store)
}

/// 逐键敲入：小写字母 / 数字原样，大写字母带 Shift，`_` `-` `$` `{` `}` `.` `+` 按美式键位。
fn type_str(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        let (vk, m) = match ch {
            'a'..='z' | '0'..='9' => ((ch.to_ascii_uppercase() as u32) & 0xFF, 0),
            'A'..='Z' => (ch as u32, MOD_SHIFT),
            '_' => (VK_OEM_MINUS, MOD_SHIFT),
            '-' => (VK_OEM_MINUS, 0),
            '+' => (VK_OEM_PLUS, MOD_SHIFT),
            '.' => (VK_OEM_PERIOD, 0),
            '$' => (u32::from(b'4'), MOD_SHIFT),
            '{' => (0xDB, MOD_SHIFT),
            '}' => (0xDD, MOD_SHIFT),
            _ => panic!("type_str 不认识 {ch:?}"),
        };
        coord.handle_key_event(&key(vk, m));
    }
}

fn enter_quick(coord: &Coordinator) {
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    assert_eq!(
        coord.debug_active_mode(),
        Some("mix"),
        "前提：`;` 进快捷输入"
    );
}

fn committed(act: &KeyAction) -> Option<String> {
    match act {
        KeyAction::InsertText { text, .. } => Some(text.clone()),
        _ => None,
    }
}

fn count_of(store: &Store, text: &str) -> u32 {
    store
        .get_completion(CompletionKind::QuickHistory, text)
        .unwrap()
        .map_or(0, |r| r.count)
}

fn history_len(store: &Store) -> usize {
    store
        .list_completions(CompletionKind::QuickHistory, "", 0, 0)
        .unwrap()
        .1
}

/// 回车上屏的自由输入原文进历史，下次打前缀（小写、文本透镜）就能补出来。
#[test]
fn literal_commit_is_remembered_and_recalled() {
    skip_without_data!();
    let (coord, store) = open("recall", true);
    enter_quick(&coord);
    type_str(&coord, "reset_time");
    let act = coord.handle_key_event(&key(VK_RETURN, 0));
    assert_eq!(committed(&act).as_deref(), Some("reset_time"), "前提");
    assert_eq!(
        count_of(&store, "reset_time"),
        1,
        "回车上屏的原文应记进历史"
    );

    enter_quick(&coord);
    type_str(&coord, "res");
    let texts = coord.debug_page_texts();
    assert!(
        texts.iter().any(|t| t == "reset_time"),
        "打 res 应补出历史 reset_time，实际 {texts:?}"
    );
}

/// 纯小写前缀（文本透镜）下历史排在成员顺序里自己的位置：成员列表里排在拼音、英文前面，
/// `reset_time` / `font-size` 打前缀即首位；排在末尾则被拼音、英文压到首页之外（论坛 t298）。
#[test]
fn text_lens_history_rank_follows_member_order() {
    skip_without_data!();
    for (tag, history_last) in [("rank_head", false), ("rank_tail", true)] {
        let (coord, _store) = open_with(tag, |c| {
            if history_last {
                let m = &mut c.schema.mix_modes[0].members;
                m.retain(|x| x != wind_quick_input::MEMBER_HISTORY);
                m.push(wind_quick_input::MEMBER_HISTORY.to_string());
            }
        });
        for word in ["reset_time", "font-size"] {
            enter_quick(&coord);
            type_str(&coord, word);
            let act = coord.handle_key_event(&key(VK_RETURN, 0));
            assert_eq!(committed(&act).as_deref(), Some(word), "前提");
        }
        for (prefix, word) in [("res", "reset_time"), ("font", "font-size")] {
            enter_quick(&coord);
            type_str(&coord, prefix);
            let texts = coord.debug_page_texts();
            coord.handle_key_event(&key(0x1B, 0));
            assert_eq!(
                texts.first().map(String::as_str) == Some(word),
                !history_last,
                "{tag}：{prefix} 首位，实际 {texts:?}"
            );
        }
    }
}

/// 带符号的缓冲（Free 透镜）：历史排首位，空格上屏它并次数 +1；回车恒上屏原文。
#[test]
fn free_lens_puts_history_first_and_enter_keeps_raw() {
    skip_without_data!();
    let (coord, store) = open("free", true);
    store
        .record_completion(CompletionKind::QuickHistory, "${reset_time}")
        .unwrap();

    enter_quick(&coord);
    type_str(&coord, "${re");
    let texts = coord.debug_page_texts();
    assert_eq!(
        texts.first().map(String::as_str),
        Some("${reset_time}"),
        "Free 透镜里历史应置首，实际 {texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t == "${re"),
        "带符号的缓冲有历史命中时不再单列原文（组合区里看得见，回车上屏它）: {texts:?}"
    );
    let act = coord.handle_key_event(&key(VK_SPACE, 0));
    assert_eq!(committed(&act).as_deref(), Some("${reset_time}"));
    assert_eq!(count_of(&store, "${reset_time}"), 2, "选中历史应次数 +1");

    enter_quick(&coord);
    type_str(&coord, "${re");
    let act = coord.handle_key_event(&key(VK_RETURN, 0));
    assert_eq!(
        committed(&act).as_deref(),
        Some("${re"),
        "回车恒上屏原文，不被历史顶掉"
    );
}

/// 存储区分大小写、匹配不区分：两条都补得出来。
#[test]
fn match_ignores_case_but_storage_keeps_both() {
    skip_without_data!();
    let (coord, store) = open("case", true);
    for t in ["RESET_TIME", "reset_time"] {
        store
            .record_completion(CompletionKind::QuickHistory, t)
            .unwrap();
    }
    enter_quick(&coord);
    type_str(&coord, "res");
    let texts = coord.debug_page_texts();
    for t in ["RESET_TIME", "reset_time"] {
        assert!(texts.iter().any(|x| x == t), "缺 {t}: {texts:?}");
    }
}

/// 历史文本不经词库特殊语法展开：含 `{…}` 的串原样出，不被改写也不被丢掉。
#[test]
fn history_text_is_not_expanded_as_template() {
    skip_without_data!();
    let (coord, store) = open("tmpl", true);
    store
        .record_completion(CompletionKind::QuickHistory, "ab{clip()}")
        .unwrap();
    enter_quick(&coord);
    type_str(&coord, "ab");
    let texts = coord.debug_page_texts();
    assert!(
        texts.iter().any(|t| t == "ab{clip()}"),
        "历史文本应原样出现，实际 {texts:?}"
    );
}

/// 没把历史成员加进列表：什么都不记、什么都不补（出厂即如此）。
#[test]
fn nothing_happens_without_the_member() {
    skip_without_data!();
    let (coord, store) = open("off", false);
    enter_quick(&coord);
    type_str(&coord, "reset_time");
    coord.handle_key_event(&key(VK_RETURN, 0));
    assert_eq!(history_len(&store), 0);
}

/// 数字透镜的产出（计算结果、数字原文）不进历史。
#[test]
fn numeric_lens_is_not_remembered() {
    skip_without_data!();
    let (coord, store) = open("num", true);
    enter_quick(&coord);
    type_str(&coord, "1+2");
    coord.handle_key_event(&key(VK_SPACE, 0));
    enter_quick(&coord);
    type_str(&coord, "12.5");
    coord.handle_key_event(&key(VK_RETURN, 0));
    assert_eq!(history_len(&store), 0, "计算 / 数字不该进历史");
}

/// 英文词库词不进历史（英文词只记英文词频，t46 楼主说英文不需要）。
#[test]
fn english_dictionary_word_is_not_remembered() {
    skip_without_data!();
    let (coord, store) = open("en", true);
    enter_quick(&coord);
    type_str(&coord, "Hello");
    let texts = coord.debug_page_texts();
    assert_eq!(texts.first().map(String::as_str), Some("Hello"), "前提");
    coord.handle_key_event(&key(VK_SPACE, 0));
    assert_eq!(history_len(&store), 0, "词库词不该进历史");
}

/// 容量上限：超出后裁掉，刚写入的那条保住。
#[test]
fn history_is_pruned_to_the_cap() {
    skip_without_data!();
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.schema.mix_modes[0]
        .members
        .push(wind_quick_input::MEMBER_HISTORY.to_string());
    c.schema.quick_input.history_max = 2;
    let store = store_at("cap");
    let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), store.clone());
    for w in ["aa_one", "bb_two", "cc_three"] {
        enter_quick(&coord);
        type_str(&coord, w);
        coord.handle_key_event(&key(VK_RETURN, 0));
    }
    assert_eq!(history_len(&store), 2);
    assert_eq!(count_of(&store, "cc_three"), 1, "刚写入的那条不能被裁掉");
}

/// 带配置钩子的 `open`：历史成员恒加在 `quick_input.repeat` 之后。
fn open_with(tag: &str, edit: impl FnOnce(&mut Config)) -> (Arc<Coordinator>, Arc<Store>) {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    let members = &mut c.schema.mix_modes[0].members;
    let at = members
        .iter()
        .position(|m| m == wind_quick_input::MEMBER_REPEAT)
        .map_or(members.len(), |i| i + 1);
    members.insert(at, wind_quick_input::MEMBER_HISTORY.to_string());
    edit(&mut c);
    let store = store_at(tag);
    let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), store.clone());
    (coord, store)
}

/// 文本透镜（纯小写字母）回车上屏的原文不记：它本身就是拼音 / 英文的合法编码，记进来
/// 下次打前缀时会顶掉拼音首选。
#[test]
fn text_lens_raw_commit_is_not_remembered() {
    skip_without_data!();
    let (coord, store) = open("textraw", true);
    enter_quick(&coord);
    type_str(&coord, "nihao");
    coord.handle_key_event(&key(VK_RETURN, 0));
    assert_eq!(history_len(&store), 0, "纯小写原文不该进历史");
}

/// 标点顶屏高亮的历史候选：次数 +1，且**不**往任何方案写词频（它没有编码）。
#[test]
fn punct_top_commit_of_history_writes_no_freq() {
    skip_without_data!();
    let (coord, store) = open_with("punct", |c| {
        c.schema.mix_modes[0].free_input = wind_config::config::FreeInputMode::Off;
        c.schema.codetable.frequency.enabled = true;
        c.schema.pinyin.frequency.enabled = true;
        c.schema.english.frequency.enabled = true;
    });
    store
        .record_completion(CompletionKind::QuickHistory, "reset_time")
        .unwrap();
    enter_quick(&coord);
    type_str(&coord, "res");
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some("reset_time"),
        "前提：历史排在成员列表里它的位置（文本透镜下 repeat 之后即首位）"
    );
    let act = coord.handle_key_event(&key(0xBC, 0)); // `,`
    let out = committed(&act).unwrap_or_default();
    assert!(out.starts_with("reset_time"), "应顶屏历史，实际 {act:?}");
    assert_eq!(count_of(&store, "reset_time"), 2, "顶屏也算选中一次");
    for schema in ["wubi86", "pinyin", "english"] {
        let (rows, _) = store.list_freq_paged(schema, "", 0, 0).unwrap();
        assert!(
            !rows.iter().any(|(_, text, _)| text == "reset_time"),
            "历史候选不该记进 {schema} 的词频: {rows:?}"
        );
    }
}

/// 历史置首（高亮停在历史上）时大小写档位键仍然生效（改动前 Free 透镜首位恒是原文，
/// 档位键可用）。关掉临英「原文候选」才能让英文词缓冲的历史置首。
#[test]
fn case_cycle_key_still_works_when_history_is_first() {
    skip_without_data!();
    let (coord, store) = open_with("cycle", |c| {
        c.input.english_case_cycle_key = "tab".into();
        c.input.temp_english.raw_candidate = wind_config::config::RawCandidateMode::Off;
    });
    store
        .record_completion(CompletionKind::QuickHistory, "Help_me")
        .unwrap();
    enter_quick(&coord);
    type_str(&coord, "Hel");
    let before = coord.debug_page_texts();
    assert_eq!(before.first().map(String::as_str), Some("Help_me"), "前提");
    coord.handle_key_event(&key(0x09, 0)); // Tab
    let after = coord.debug_page_texts();
    assert_ne!(
        before, after,
        "档位键应改写英文段的大小写，实际未变: {after:?}"
    );
    assert_eq!(
        after.first().map(String::as_str),
        Some("Help_me"),
        "历史仍在首位"
    );
}

/// 英文词缓冲（纯字母带大写）有历史命中时，原文跟随临英「原文候选」：出厂 Always ⇒
/// 原文首位、历史紧随；关掉 ⇒ 去掉原文、历史置首。
///
/// 两组词：`RSTQ` 不在词库里，原文格是纯头部候选；`RESET` 在词库里，原文格被同名词库词
/// 占据（投影成 `RESET`）——保留原文时它同样要排首位。关掉原文候选时词库词照常出（临英
/// 同样如此），所以「不单列原文」只拿不在词库里的那组验。
#[test]
fn english_word_raw_follows_temp_english_raw_candidate() {
    skip_without_data!();
    use wind_config::config::RawCandidateMode;
    let run = |mode: RawCandidateMode, typed: &str, hist: &str| {
        let (coord, store) = open_with(&format!("raw_{mode:?}_{typed}"), |c| {
            c.input.temp_english.raw_candidate = mode;
        });
        store
            .record_completion(CompletionKind::QuickHistory, hist)
            .unwrap();
        enter_quick(&coord);
        type_str(&coord, typed);
        coord.debug_page_texts()
    };
    for (typed, hist) in [("RSTQ", "RSTQ_TIME"), ("RESET", "RESET_TIME")] {
        let t = run(RawCandidateMode::Always, typed, hist);
        assert_eq!(
            t.iter().take(2).map(String::as_str).collect::<Vec<_>>(),
            [typed, hist],
            "Always 下原文首位、历史紧随: {t:?}"
        );
    }
    let t = run(RawCandidateMode::Off, "RSTQ", "RSTQ_TIME");
    assert_eq!(t.first().map(String::as_str), Some("RSTQ_TIME"), "{t:?}");
    assert!(!t.iter().any(|x| x == "RSTQ"), "Off 下不单列原文: {t:?}");
}

/// 自由输入（Free 透镜）下候选窗照常显示序号（维护者 2026-10-07 定）。
#[test]
fn free_lens_shows_index_labels() {
    skip_without_data!();
    use wind_bridge::handler::CaretData;
    use wind_ipc::protocol::caret_source;
    use wind_ui_types::UiCommand;
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    let (coord, rx) = Coordinator::new_headless_with_ui(c, Some(&data_dir()));
    enter_quick(&coord);
    type_str(&coord, "a_b");
    coord.handle_caret_update(&CaretData {
        x: 100,
        y: 200,
        height: 20,
        composition_start_x: 100,
        composition_start_y: 200,
        source: caret_source::TSF_SELECTION,
        composition_rect: None,
    });
    let items = rx
        .try_iter()
        .filter_map(|cmd| match cmd {
            UiCommand::UpdateCandidates { candidates, .. } => Some(candidates),
            _ => None,
        })
        .last()
        .expect("应下发候选");
    assert!(!items.is_empty(), "前提：有候选");
    assert!(
        items.iter().all(|i| !i.no_index),
        "Free 透镜下应照常显示序号: {:?}",
        items
            .iter()
            .map(|i| (&i.text, i.no_index))
            .collect::<Vec<_>>()
    );
}

/// 把一条记过的历史完整打出来：它排首位（就是原文本身），候选不为空；其余历史仍有 5 条名额。
#[test]
fn typing_a_whole_history_entry_keeps_it_first() {
    skip_without_data!();
    let (coord, store) = open("exact", true);
    for t in [
        "${reset_time}",
        "${reset_time}_a",
        "${reset_time}_b",
        "${reset_time}_c",
        "${reset_time}_d",
        "${reset_time}_e",
    ] {
        store
            .record_completion(CompletionKind::QuickHistory, t)
            .unwrap();
    }
    enter_quick(&coord);
    type_str(&coord, "${reset_time}");
    let texts = coord.debug_page_texts();
    assert_eq!(
        texts.first().map(String::as_str),
        Some("${reset_time}"),
        "{texts:?}"
    );
    assert_eq!(
        texts
            .iter()
            .filter(|t| t.starts_with("${reset_time}_"))
            .count(),
        5,
        "逐字相同的那条不占配额: {texts:?}"
    );
    let act = coord.handle_key_event(&key(VK_SPACE, 0));
    assert_eq!(committed(&act).as_deref(), Some("${reset_time}"));
    assert_eq!(count_of(&store, "${reset_time}"), 2, "按历史候选记次数");
}

/// 按大小写档位键后，原文（已被改写成别的大小写）仍按「原文候选」开关处理：Always 下
/// 首格仍是原文那一格，不会掉到历史后面。
#[test]
fn raw_stays_first_after_case_cycle() {
    skip_without_data!();
    let (coord, store) = open_with("cycle_raw", |c| {
        c.input.english_case_cycle_key = "tab".into();
    });
    store
        .record_completion(CompletionKind::QuickHistory, "RSTQ_TIME")
        .unwrap();
    enter_quick(&coord);
    type_str(&coord, "Rstq");
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some("Rstq"),
        "前提"
    );
    coord.handle_key_event(&key(0x09, 0)); // Tab
    let texts = coord.debug_page_texts();
    let first = texts.first().cloned().unwrap_or_default();
    assert!(
        first.eq_ignore_ascii_case("rstq"),
        "档位循环后首格仍应是原文（换了大小写）: {texts:?}"
    );
    assert_eq!(
        texts.get(1).map(String::as_str),
        Some("RSTQ_TIME"),
        "{texts:?}"
    );
}

/// 右键历史候选：菜单只有「删除此历史」与复制；点删除后这条从库里消失、候选立即刷新。
#[test]
fn right_click_deletes_a_history_entry() {
    skip_without_data!();
    use wind_ui_types::UiCommand;
    let user_dir = std::env::temp_dir().join(format!("wind_qh_menu_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&user_dir);
    std::fs::create_dir_all(&user_dir).unwrap();
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.schema.mix_modes[0]
        .members
        .push(wind_quick_input::MEMBER_HISTORY.to_string());
    let (coord, rx) = Coordinator::new_headless_with_ui_at(c, Some(&data_dir()), Some(&user_dir));

    enter_quick(&coord);
    type_str(&coord, "RESET_TIME");
    coord.handle_key_event(&key(VK_RETURN, 0));
    enter_quick(&coord);
    type_str(&coord, "RESET_");
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some("RESET_TIME"),
        "前提：历史已记下并置首"
    );

    let _ = rx.try_iter().count();
    coord.debug_show_candidate_menu(0);
    let items = rx
        .try_iter()
        .find_map(|m| match m {
            UiCommand::ShowCandidateMenu { items, .. } => Some(items),
            _ => None,
        })
        .expect("候选菜单没弹");
    let labels: Vec<&str> = items
        .iter()
        .map(|i| i.label.as_str())
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(labels, ["删除此历史", "复制"]);

    // 点菜单里的「删除此历史」：走 UI 回传点击的同一入口（读打开菜单时记下的目标下标）。
    let delete = items
        .iter()
        .find(|i| i.label == "删除此历史")
        .expect("缺删除项")
        .kind;
    coord.debug_menu_action(delete);
    let texts = coord.debug_page_texts();
    assert!(
        !texts.iter().any(|t| t == "RESET_TIME"),
        "删除后应立即从候选里消失: {texts:?}"
    );
    let _ = std::fs::remove_dir_all(&user_dir);
}

/// 删除候选热键（出厂 Ctrl+Shift+数字）同样能删历史——与右键同一能力、同一判据。
#[test]
fn delete_hotkey_removes_a_history_entry() {
    skip_without_data!();
    use wind_ipc::protocol::MOD_CTRL;
    let (coord, store) = open("hotkey", true);
    store
        .record_completion(CompletionKind::QuickHistory, "RESET_TIME")
        .unwrap();
    enter_quick(&coord);
    type_str(&coord, "RESET_");
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some("RESET_TIME"),
        "前提"
    );
    let act = coord.handle_key_event(&key(0x31, MOD_CTRL | MOD_SHIFT)); // Ctrl+Shift+1
    assert!(matches!(act, KeyAction::Consumed), "热键应被消费: {act:?}");
    assert_eq!(count_of(&store, "RESET_TIME"), 0, "应已从库里删掉");
    assert!(
        !coord.debug_page_texts().iter().any(|t| t == "RESET_TIME"),
        "候选应立即刷新"
    );
}

/// 数字透镜里**只有历史**命中时，字母不是选词键：`.` 起手召回了 `.txt`，接着打 `xlsx`
/// 照样字面入缓冲。历史是字面前缀补全、不是这个透镜算出来的结果，它不该让空候选那条
/// 「选词键归字面」的判据失效（GH 回归：开历史成员后 `;.xlsx` 停在那个点）。
#[test]
fn numeric_lens_history_only_does_not_eat_letters() {
    skip_without_data!();
    let (coord, store) = open("dot_ext", true);
    store
        .record_completion(CompletionKind::QuickHistory, ".txt")
        .unwrap();
    enter_quick(&coord);
    type_str(&coord, ".");
    assert_eq!(
        coord.debug_page_texts(),
        vec![".txt".to_string()],
        "前提：光一个点只召回历史"
    );
    type_str(&coord, "x");
    assert_eq!(coord.debug_preedit(), ";.x", "字母不该被当选词键吃掉");
    type_str(&coord, "lsx");
    assert_eq!(coord.debug_preedit(), ";.xlsx");
    let act = coord.handle_key_event(&key(VK_RETURN, 0));
    assert_eq!(committed(&act).as_deref(), Some(".xlsx"));
}

/// 同上，数字在前：`.7` 只召回历史 `.7z`，接着的 `z` 照样字面入缓冲。
#[test]
fn numeric_lens_history_only_after_digit_does_not_eat_letters() {
    skip_without_data!();
    let (coord, store) = open("dot_7z", true);
    store
        .record_completion(CompletionKind::QuickHistory, ".7z")
        .unwrap();
    enter_quick(&coord);
    type_str(&coord, ".7");
    assert_eq!(
        coord.debug_page_texts(),
        vec![".7z".to_string()],
        "前提：`.7` 只召回历史"
    );
    type_str(&coord, "z");
    assert_eq!(coord.debug_preedit(), ";.7z", "字母不该被当选词键吃掉");
    let act = coord.handle_key_event(&key(VK_RETURN, 0));
    assert_eq!(committed(&act).as_deref(), Some(".7z"));
}

/// ★反向对照：数字透镜有**本透镜的**候选时，即便历史也命中，字母仍是选词键
/// （`a` 选首选，越界的 `z` 仍被吃掉、不污染缓冲）。
#[test]
fn numeric_lens_letters_still_select_with_history_alongside() {
    skip_without_data!();
    let (coord, store) = open("num_hist_sel", true);
    store
        .record_completion(CompletionKind::QuickHistory, "1+2ab")
        .unwrap();
    enter_quick(&coord);
    type_str(&coord, "1+2");
    let texts = coord.debug_page_texts();
    assert_eq!(
        texts.first().map(String::as_str),
        Some("3"),
        "前提: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t == "1+2ab") && texts.len() < 26,
        "前提：历史也命中、z 越界: {texts:?}"
    );
    let act = coord.handle_key_event(&key(u32::from(b'Z'), 0));
    assert!(
        matches!(act, KeyAction::Consumed),
        "越界选词键应被吃掉: {act:?}"
    );
    assert_eq!(coord.debug_preedit(), ";1+2", "缓冲不该被 z 污染");
    let act = coord.handle_key_event(&key(u32::from(b'A'), 0));
    assert_eq!(committed(&act).as_deref(), Some("3"), "a 应选走首选");
}

/// t253：历史排在英文之后时，用过几次的 `font-size` 与英文词一起按热度重排——打前缀即上首位。
#[test]
fn hot_history_joins_english_segment_and_reaches_top() {
    skip_without_data!();
    let (coord, store) = open_with("hot_hist", |c| {
        // 同 GH#253 楼主的配置：只留英文、历史，历史在后；原文与变形候选不显示。
        c.schema.english.frequency.enabled = true;
        c.input.temp_english.raw_candidate = wind_config::config::RawCandidateMode::Off;
        c.input.temp_english.case_variants = false;
        c.schema.mix_modes[0].members = vec![
            "english".to_string(),
            wind_quick_input::MEMBER_HISTORY.to_string(),
        ];
    });
    for _ in 0..4 {
        store
            .record_completion(CompletionKind::QuickHistory, "font-size")
            .unwrap();
    }
    enter_quick(&coord);
    type_str(&coord, "fo");
    let texts = coord.debug_page_texts();
    assert_eq!(
        texts.first().map(String::as_str),
        Some("font-size"),
        "用过 4 次的历史应与英文词按热度比、上到首位，实际 {texts:?}"
    );
}

/// t253 反向：英文词 `font` 用得更多时仍在 `font-size` 之前（热度翻转，不是历史一律置顶）。
#[test]
fn hotter_english_word_stays_above_history() {
    skip_without_data!();
    let (coord, store) = open_with("hot_en", |c| {
        c.schema.english.frequency.enabled = true;
        c.schema.mix_modes[0].members = vec![
            "english".to_string(),
            wind_quick_input::MEMBER_HISTORY.to_string(),
        ];
    });
    for _ in 0..2 {
        store
            .record_completion(CompletionKind::QuickHistory, "font-size")
            .unwrap();
    }
    for _ in 0..8 {
        enter_quick(&coord);
        type_str(&coord, "font");
        let texts = coord.debug_page_texts();
        let k = texts
            .iter()
            .position(|t| t == "font")
            .expect("前提：英文段里有 font")
            + 1;
        coord.handle_key_event(&key(0x30 + k as u32, 0));
    }
    enter_quick(&coord);
    type_str(&coord, "fo");
    let texts = coord.debug_page_texts();
    let pos = |w: &str| texts.iter().position(|t| t == w);
    assert!(
        pos("font").is_some() && pos("font") < pos("font-size"),
        "用得更多的 font 应在历史 font-size 之前，实际 {texts:?}"
    );
}

/// 英文调频选「一次到顶」时，Free / 大写英文透镜里的历史也按它排：最近用过的在前，
/// 不再只看累计次数（反馈：`${ResetTime}` 输了几次仍排在老条目 `${reset_time}` 后面）。
#[test]
fn free_lens_history_follows_english_top_strategy() {
    skip_without_data!();
    for (tag, old, new, typed) in [
        ("top_sym", "${reset_time}", "${ResetTime}", "${R"),
        ("top_cap", "Reset_time", "ResetTime", "Res"),
    ] {
        let (coord, store) = open_with(tag, |c| {
            c.schema.english.frequency.enabled = true;
            c.schema.english.frequency.strategy = "top".into();
            c.input.temp_english.raw_candidate = wind_config::config::RawCandidateMode::Off;
            c.input.temp_english.case_variants = false;
        });
        for _ in 0..3 {
            store
                .record_completion(CompletionKind::QuickHistory, old)
                .unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(1100));
        store
            .record_completion(CompletionKind::QuickHistory, new)
            .unwrap();
        enter_quick(&coord);
        type_str(&coord, typed);
        let texts = coord.debug_page_texts();
        assert_eq!(
            texts.first().map(String::as_str),
            Some(new),
            "{tag}: 一次到顶下最近用过的历史应居首，实际 {texts:?}"
        );
    }
}

/// 英文调频「一次到顶」下，与缓冲逐字相同的历史仍恒首位，不被更近用过的别条顶下去。
#[test]
fn free_lens_exact_history_stays_first_under_top_strategy() {
    skip_without_data!();
    let (coord, store) = open_with("top_exact", |c| {
        c.schema.english.frequency.enabled = true;
        c.schema.english.frequency.strategy = "top".into();
        c.input.temp_english.raw_candidate = wind_config::config::RawCandidateMode::Off;
        c.input.temp_english.case_variants = false;
    });
    store
        .record_completion(CompletionKind::QuickHistory, "${ResetTime}")
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    store
        .record_completion(CompletionKind::QuickHistory, "${ResetTime}_x")
        .unwrap();
    enter_quick(&coord);
    type_str(&coord, "${ResetTime}");
    let texts = coord.debug_page_texts();
    assert_eq!(
        texts.first().map(String::as_str),
        Some("${ResetTime}"),
        "逐字相同的历史应恒首位，实际 {texts:?}"
    );
}
