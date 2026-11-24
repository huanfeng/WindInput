//! 联想的端到端验证：真实词库 + 真实按键 + 真实上屏路径。
//!
//! `handle_assoc::wiring_tests` 用 headless（无词库）协调器，词语联想在那里恒空，只能验状态机与
//! 按键闸门；「打完一个字到底出不出联想」只有这里能答（`maybe_enter_assoc` 有几个静默的
//! early-return）。词典缺失时自动跳过——`build_dev/data` 不存在时整族静默跳过而计数照绿，看耗时判断。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_ALT, MOD_CTRL, MOD_SHIFT};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn dict_ready(d: &std::path::Path) -> bool {
    d.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists()
}

fn key_event(key_code: u32) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

/// 按用户真机上的配置造协调器：活跃方案是混输，这正是词语联想差点整个失效的场景。
fn coord(kind: &str, schema: &str) -> std::sync::Arc<Coordinator> {
    coord_mode(kind, schema, "one_shot")
}

/// 同 [`coord`]，但可再改几项配置（联想态的按键行为开关）。
fn coord_tweak(
    kind: &str,
    schema: &str,
    tweak: impl FnOnce(&mut Config),
) -> std::sync::Arc<Coordinator> {
    let dir = data_dir();
    let mut cfg = Config::default();
    cfg.schema.available = vec![schema.to_string()];
    cfg.schema.active = schema.to_string();
    cfg.input.default.chinese_mode = true;
    cfg.input.symbol.smart_mode = false;
    cfg.input.association.kind = kind.to_string();
    cfg.input.association.mode = "one_shot".to_string();
    tweak(&mut cfg);
    let c = Coordinator::new_headless(cfg, Some(&dir));
    prewarm(&c);
    c
}

/// 词语联想的数据源是反查索引，生产里由启动后的后台预热线程建好（`construct.rs` →
/// `prewarm_indexes`），headless 协调器不跑那个线程。
///
/// 索引没就绪时返回空、绝不在按键线程上现建，测试不自己预热就测的是夹具缺陷。生产/测试/移动端
/// 共用同一个 `prewarm_indexes`。智能档的下一词表也由它派后台加载，这里等它装好。
fn prewarm(c: &Coordinator) {
    c.prewarm_indexes();
    c.debug_wait_assoc_bigram(std::time::Duration::from_secs(10));
}

fn coord_mode(kind: &str, schema: &str, mode: &str) -> std::sync::Arc<Coordinator> {
    let dir = data_dir();
    let mut cfg = Config::default();
    cfg.schema.available = vec![schema.to_string()];
    cfg.schema.active = schema.to_string();
    cfg.input.default.chinese_mode = true;
    cfg.input.symbol.smart_mode = false;
    cfg.input.association.kind = kind.to_string();
    cfg.input.association.mode = mode.to_string();
    let c = Coordinator::new_headless(cfg, Some(&dir));
    prewarm(&c);
    c
}

fn press(c: &Coordinator, code: &str) {
    for ch in code.chars() {
        c.handle_key_event(&key_event((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
}

/// 空格上屏当前高亮候选，返回真正写进宿主的文本。两种形态都算数：
///   - `InsertText`                 —— 没进联想态（或 `top_commit_mode = pre_confirm`）
///   - `CommitThenDeferComposition` —— 进了联想态：真提交 + 延到 keyup 才开占位组合
///     （联想能收到后续按键的关键，见 `Coordinator::assoc_composition`）
fn commit_with_space(c: &Coordinator) -> String {
    match c.handle_key_event(&key_event(0x20)) {
        KeyAction::InsertText { text, .. } => text,
        KeyAction::CommitThenDeferComposition { commit_text, .. } => commit_text,
        other => panic!("空格该上屏，实得 {other:?}"),
    }
}

/// 上屏动作是否带了占位组合——联想态的命门。
fn opened_composition(act: &KeyAction) -> bool {
    match act {
        KeyAction::CommitThenDeferComposition {
            deferred_composition,
            ..
        } => !deferred_composition.is_empty(),
        KeyAction::InsertText {
            new_composition, ..
        } => new_composition.as_deref().is_some_and(|c| !c.is_empty()),
        _ => false,
    }
}

fn assoc_texts(c: &Coordinator) -> Vec<String> {
    c.debug_assoc_texts()
}

/// 混输方案下打一个字上屏，词语联想要出得来。只要求「上屏了一个汉字 ⇒ 联想非空」，
/// 不钉死具体字，否则词库一更新就假红。
#[test]
fn word_assoc_appears_after_commit_on_mixed_schema() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    press(&c, "q");
    let committed = commit_with_space(&c);
    assert!(!committed.is_empty(), "前提：空格确实上屏了东西");
    let hits = assoc_texts(&c);
    assert!(
        !hits.is_empty(),
        "上屏 {committed:?} 之后词语联想为空——真机上就是这个现象"
    );
    for h in &hits {
        assert!(
            h.starts_with(&committed),
            "联想候选 {h:?} 应以刚上屏的 {committed:?} 开头"
        );
    }
    println!("上屏 {committed:?} → 联想 {hits:?}");
}

/// 受控对照：同样的操作，`kind = "off"` 时不该有联想。
///
/// 少了这条，上一条可能只是因为联想态被别的什么东西填上了。
#[test]
fn off_kind_yields_no_assoc_on_mixed_schema() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("off", "wubi86_pinyin");
    press(&c, "q");
    let committed = commit_with_space(&c);
    assert!(!committed.is_empty());
    assert!(assoc_texts(&c).is_empty(), "关闭时不该有联想");
}

/// 纯码表方案（非混输）同样要出——两条一起把「词源方案解析」的两个分支都盖住。
#[test]
fn word_assoc_appears_on_codetable_schema() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86");
    press(&c, "q");
    let committed = commit_with_space(&c);
    assert!(!committed.is_empty());
    assert!(
        !assoc_texts(&c).is_empty(),
        "码表方案上屏 {committed:?} 之后也该有联想"
    );
}

/// 选中联想候选后，只补出剩余部分。
#[test]
fn selecting_assoc_commits_only_the_suffix() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    press(&c, "q");
    let first = commit_with_space(&c);
    let hits = assoc_texts(&c);
    assert!(!hits.is_empty(), "前提：联想非空");
    let word = hits[0].clone();
    // 按 1 选中第一条联想。一次性档选完即止，故无新组合，是普通 InsertText。
    let out = match c.handle_key_event(&key_event(0x31)) {
        KeyAction::InsertText { text, .. } => text,
        KeyAction::CommitThenDeferComposition { commit_text, .. } => commit_text,
        other => panic!("数字键该上屏联想候选，实得 {other:?}"),
    };
    assert_eq!(
        out,
        word.strip_prefix(&first).unwrap(),
        "上屏的该是整词 {word:?} 去掉已在屏上的 {first:?} 之后那半截"
    );
}

/// 智能联想里前缀延伸候选只显示补出的那一截（「中」→「国」，不是「中国」），与同栏的下一词、
/// 标点同一口径；选中上屏的正是显示的那一截。
///
/// 对照同一份词库的词语联想：整词去掉已在屏上的上文、按智能联想口径筛过（补出部分不超过
/// `SMART_PREFIX_TAIL_MAX` 字、最多 `SMART_PREFIX_CAP` 条）后应逐条相同（桌面基线两档都只出前缀词）。
#[test]
fn smart_assoc_shows_only_the_continuation() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let word = coord("word", "wubi86_pinyin");
    press(&word, "q");
    let first = commit_with_space(&word);
    let full = assoc_texts(&word);
    assert!(!full.is_empty(), "前提：词语联想非空");
    let expected: Vec<String> = full
        .iter()
        .map(|w| {
            w.strip_prefix(&first)
                .expect("词语联想以上文开头")
                .to_string()
        })
        .filter(|t| t.chars().count() <= wind_assoc::SMART_PREFIX_TAIL_MAX)
        .take(wind_assoc::SMART_PREFIX_CAP)
        .collect();

    // 关掉下一词源：本条只测前缀词的显示口径（下一词另见 `smart_assoc_brings_next_words`）
    let smart = coord_tweak("smart", "wubi86_pinyin", |c| {
        c.input.association.bigram = false
    });
    press(&smart, "q");
    assert_eq!(commit_with_space(&smart), first, "前提：两档上屏同一个字");
    let shown = assoc_texts(&smart);
    assert_eq!(shown, expected, "智能联想只显示补出的那一截");

    let out = match smart.handle_key_event(&key_event(0x31)) {
        KeyAction::InsertText { text, .. } => text,
        KeyAction::CommitThenDeferComposition { commit_text, .. } => commit_text,
        other => panic!("数字键该上屏联想候选，实得 {other:?}"),
    };
    assert_eq!(out, shown[0], "上屏的就是显示的那一截");
}

/// 智能联想出「下一词」（离线蒸馏表 `data/assoc/assoc_bigram.txt`）：上屏「我」后首位是
/// 词内接续「们」（逐字打字最常要的），紧跟着是下一词「的」，名额用满。
#[test]
fn smart_assoc_brings_next_words() {
    let dir = data_dir();
    if !dict_ready(&dir) || !dir.join("assoc/assoc_bigram.txt").exists() {
        eprintln!("!!! 跳过：build_dev 词库或下一词表不存在");
        return;
    }
    let c = coord("smart", "wubi86_pinyin");
    press(&c, "q");
    assert_eq!(commit_with_space(&c), "我", "前提：q 上屏「我」");
    let shown = assoc_texts(&c);
    assert_eq!(shown.len(), 9, "名额用满：{shown:?}");
    assert_eq!(
        &shown[..2],
        ["们", "的"],
        "接续与下一词按概率混排：{shown:?}"
    );
    assert!(
        !shown.iter().any(|t| t.starts_with('我')),
        "智能联想不显示带上文的整词：{shown:?}"
    );
}

/// 宿主报了光标前文：逐字上屏「今」「天」，下一词按「今天」接，而不是按「天」。
#[test]
fn next_words_borrow_from_preceding_text() {
    let dir = data_dir();
    if !dict_ready(&dir) || !dir.join("assoc/assoc_bigram.txt").exists() {
        eprintln!("!!! 跳过：build_dev 词库或下一词表不存在");
        return;
    }
    let c = coord("smart", "wubi86_pinyin");
    press(&c, "gdi");
    assert_eq!(commit_with_space(&c), "天", "前提：gdi 上屏「天」");
    let alone = assoc_texts(&c);
    assert!(
        !alone.iter().any(|t| t == "早上"),
        "没有前文：按「天」接 {alone:?}"
    );

    let c = coord("smart", "wubi86_pinyin");
    c.set_preceding_text("好的，今");
    press(&c, "gdi");
    assert_eq!(commit_with_space(&c), "天");
    let joined = assoc_texts(&c);
    assert!(
        joined.iter().any(|t| t == "早上"),
        "按「今天」接：{joined:?}"
    );
}

/// 混输方案打 `lwty`（2 个候选）空格上屏，联想要出得来。
#[test]
fn reproduce_user_lwty() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    for code in ["lwty", "q", "wq", "trhy", "nt"] {
        let c = coord("word", "wubi86_pinyin");
        press(&c, code);
        let out = match c.handle_key_event(&key_event(0x20)) {
            KeyAction::InsertText { text, .. } => text,
            other => format!("<{other:?}>"),
        };
        println!("{code:<6} 上屏 {out:<10} → 联想 {:?}", assoc_texts(&c));
    }
}

/// 上屏进入联想态时必须带占位组合——整条链的命门。
///
/// 没有它，TSF 的 `OnTestKeyDown` 判定「无会话」，退格/Esc 不会转发进服务端，联想窗关不掉。
/// 让宿主的 `_hasCandidates` 保持真不行：它异步回填，赢不了同一拍的同步判定。
#[test]
fn entering_assoc_opens_placeholder_composition() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    press(&c, "q");
    let act = c.handle_key_event(&key_event(0x20));
    assert!(!assoc_texts(&c).is_empty(), "前提：进了联想态");
    assert!(
        opened_composition(&act),
        "进联想态的上屏动作必须带占位组合，实得 {act:?}"
    );
}

/// 受控对照：没进联想态时不该平白多出一个组合，否则上一条可能只是因为「所有上屏都带组合」。
#[test]
fn plain_commit_opens_no_composition() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("off", "wubi86_pinyin");
    press(&c, "q");
    let act = c.handle_key_event(&key_event(0x20));
    assert!(assoc_texts(&c).is_empty(), "前提：没进联想态");
    assert!(
        !opened_composition(&act),
        "关闭联想时普通上屏不该开组合，实得 {act:?}"
    );
}

/// 退格：结束占位组合并吞键（联想窗随之关掉）。
#[test]
fn backspace_closes_assoc_window() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    press(&c, "q");
    c.handle_key_event(&key_event(0x20));
    assert!(!assoc_texts(&c).is_empty(), "前提：进了联想态");
    let act = c.handle_key_event(&key_event(0x08));
    assert!(assoc_texts(&c).is_empty(), "退格该关掉联想窗");
    assert!(
        matches!(act, KeyAction::ClearComposition),
        "退格该结束占位组合，实得 {act:?}"
    );
}

/// Esc 同理。
#[test]
fn escape_closes_assoc_window() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    press(&c, "q");
    c.handle_key_event(&key_event(0x20));
    assert!(!assoc_texts(&c).is_empty());
    let act = c.handle_key_event(&key_event(0x1B));
    assert!(assoc_texts(&c).is_empty(), "Esc 该关掉联想窗");
    assert!(matches!(act, KeyAction::ClearComposition), "实得 {act:?}");
}

/// 字母键照常放行——它会用新编码替换掉占位组合，不需要我们收尾。
#[test]
fn letter_falls_through_and_starts_new_input() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    press(&c, "q");
    c.handle_key_event(&key_event(0x20));
    assert!(!assoc_texts(&c).is_empty());
    let act = c.handle_key_event(&key_event(0x51)); // Q
    assert!(assoc_texts(&c).is_empty(), "字母键该退出联想态");
    assert!(
        matches!(act, KeyAction::UpdateComposition { .. }),
        "字母键该落回正常输入并替换组合，实得 {act:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 联想态 = 正常输入态，只是没有输入缓冲
//
// 这组测的是联想没有被做成特例：候选住在 `state.candidates` 里，主输入路的既有能力天然适用。
// ─────────────────────────────────────────────────────────────────────────────

/// 进入联想态并返回候选列表（够长才好验翻页/二三候选）。
fn enter_assoc(c: &Coordinator) -> Vec<String> {
    press(c, "q");
    c.handle_key_event(&key_event(0x20));
    let hits = assoc_texts(c);
    assert!(!hits.is_empty(), "前提：进了联想态");
    hits
}

/// 二三候选键（`;` / `'`）在联想态照常选词。
#[test]
fn select_keys_work_in_assoc() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    let hits = enter_assoc(&c);
    assert!(hits.len() >= 2, "前提：至少两条候选，实得 {hits:?}");
    let want = hits[1].clone(); // 第 2 条
    // VK_OEM_1 = `;` = 二候选键（出厂 select_key_groups = ["semicolon_quote"]）
    let act = c.handle_key_event(&key_event(0xBA));
    match act {
        KeyAction::InsertText { text, .. }
        | KeyAction::CommitThenDeferComposition {
            commit_text: text, ..
        } => {
            // 词语联想只补剩余部分，故拿整词去掉已上屏的前缀来比。
            assert!(
                want.ends_with(&text) && !text.is_empty(),
                "`;` 该选第 2 条 {want:?}，实得上屏 {text:?}"
            );
        }
        other => panic!("`;` 在联想态该选第 2 条候选，实得 {other:?}"),
    }
}

/// 上下移高亮在联想态照常工作，且空格上屏的是移动后的那条。
#[test]
fn highlight_move_works_in_assoc() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    let hits = enter_assoc(&c);
    assert!(hits.len() >= 2);
    // 下移一格（VK_DOWN）
    c.handle_key_event(&key_event(0x28));
    let act = c.handle_key_event(&key_event(0x20)); // 空格上屏高亮
    let text = match act {
        KeyAction::InsertText { text, .. }
        | KeyAction::CommitThenDeferComposition {
            commit_text: text, ..
        } => text,
        other => panic!("空格该上屏高亮候选，实得 {other:?}"),
    };
    assert!(
        hits[1].ends_with(&text) && !text.is_empty(),
        "下移后空格该上屏第 2 条 {:?}，实得 {text:?}",
        hits[1]
    );
}

/// 翻页键（PageDown）在联想态照常工作。候选不足一页时翻不动是正常的，只断言按键被消费而不是
/// 漏给宿主。
#[test]
fn paging_keys_are_consumed_in_assoc() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    enter_assoc(&c);
    let act = c.handle_key_event(&key_event(0x22)); // VK_NEXT = PageDown
    assert!(
        !matches!(act, KeyAction::PassThrough | KeyAction::NotHandled),
        "翻页键在联想态不该漏给宿主，实得 {act:?}"
    );
    assert!(!assoc_texts(&c).is_empty(), "翻页不该退出联想态");
}

/// 持续档：选中一条之后接着给下一轮（「我」→「我们」→ 以「我们」开头的词）。
#[test]
fn continuous_mode_chains_in_assoc() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord_mode("word", "wubi86_pinyin", "continuous");
    press(&c, "q");
    c.handle_key_event(&key_event(0x20));
    let first = assoc_texts(&c);
    assert!(!first.is_empty(), "前提：进了联想态");
    let picked = first[0].clone();
    c.handle_key_event(&key_event(0x31)); // 选第 1 条
    let second = assoc_texts(&c);
    // 续不续得上取决于词库里有没有更长的词；有就必须以整词为前缀，没有就该干净收窗。
    if second.is_empty() {
        println!("{picked:?} 之后无更长的词，收窗（合法）");
    } else {
        for w in &second {
            assert!(
                w.starts_with(&picked),
                "续联想该以整词 {picked:?} 为前缀，实得 {w:?}"
            );
        }
    }
}

/// 受控对照：一次性档同样操作不续轮。
#[test]
fn one_shot_does_not_chain_in_assoc() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin"); // one_shot
    press(&c, "q");
    c.handle_key_event(&key_event(0x20));
    assert!(!assoc_texts(&c).is_empty());
    c.handle_key_event(&key_event(0x31));
    assert!(assoc_texts(&c).is_empty(), "一次性档选完即止");
}

/// 联想候选不记词频——它没有编码，记进去是一条永远查不到的空码行。
#[test]
fn assoc_commit_records_no_frequency() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    enter_assoc(&c);
    // 选中联想候选。没有 store 的 headless 下本就不会写，这里验的是不 panic 且
    // 上屏正确——真正的守门在 `commit_selected` 里那句 `if !from_assoc`。
    let act = c.handle_key_event(&key_event(0x31));
    assert!(
        matches!(
            act,
            KeyAction::InsertText { .. } | KeyAction::CommitThenDeferComposition { .. }
        ),
        "实得 {act:?}"
    );
}

/// 联想态下按标点，不该顶屏联想候选。
///
/// 顶屏的前提是「用户打了码、按标点意味着就选高亮那个」；联想态没有码，高亮是输入法猜的。
/// 何况联想显示的是整词，顶屏会把已在屏上的字重复一遍（「我」+「我们」=「我我们」）。
#[test]
fn punctuation_does_not_top_commit_assoc() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    press(&c, "q");
    let first = commit_with_space(&c); // 「我」
    let hits = assoc_texts(&c);
    assert!(!hits.is_empty(), "前提：进了联想态，首条 {hits:?}");
    // VK_OEM_PERIOD = 0xBE = 「。」
    let act = c.handle_key_event(&key_event(0xBE));
    let out = match &act {
        KeyAction::InsertText { text, .. }
        | KeyAction::CommitThenDeferComposition {
            commit_text: text, ..
        } => text.clone(),
        other => panic!("标点该上屏，实得 {other:?}"),
    };
    assert!(
        !out.contains(&first),
        "上屏的 {out:?} 里混进了已经在屏幕上的 {first:?}——顶屏了联想候选"
    );
    for h in &hits {
        assert!(
            !out.contains(h.as_str()),
            "上屏的 {out:?} 里混进了联想候选 {h:?}"
        );
    }
    assert!(assoc_texts(&c).is_empty(), "标点该收掉联想窗");
}

/// 回车：收窗 + 结束占位组合，不上屏高亮联想（回车是终结性动作，替用户选一个是越权）。
///
/// 只管「不上屏」，刻意接受两种收窗动作：吃键还是交还宿主由 `enter_cancels_only` 定，由
/// `assoc_enter_*` 两条钉。
#[test]
fn enter_dismisses_assoc_without_committing() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    press(&c, "q");
    c.handle_key_event(&key_event(0x20));
    assert!(!assoc_texts(&c).is_empty(), "前提：进了联想态");
    let act = c.handle_key_event(&key_event(0x0D)); // VK_RETURN
    assert!(assoc_texts(&c).is_empty(), "回车该收掉联想窗");
    assert!(
        matches!(
            act,
            KeyAction::ClearComposition | KeyAction::ClearCompositionThenPassThrough
        ),
        "回车该结束占位组合而不是上屏联想，实得 {act:?}"
    );
}

/// `space_commits = false` 时空格不选联想，只收窗并出空格。配置还在而消费点被删时它静默失效。
#[test]
fn space_commits_false_outputs_space_not_candidate() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86_pinyin".to_string()];
    cfg.schema.active = "wubi86_pinyin".to_string();
    cfg.input.default.chinese_mode = true;
    cfg.input.symbol.smart_mode = false;
    cfg.input.association.kind = "word".to_string();
    cfg.input.association.space_commits = false;
    let c = Coordinator::new_headless(cfg, Some(&data_dir()));
    prewarm(&c); // 本例自己造协调器，没走 coord_* 夹具，须显式预热（理由见 prewarm）
    press(&c, "q");
    c.handle_key_event(&key_event(0x20)); // 第一次空格：正常选词上屏「我」
    let hits = assoc_texts(&c);
    assert!(!hits.is_empty(), "前提：进了联想态");
    let act = c.handle_key_event(&key_event(0x20)); // 第二次空格：联想态
    assert!(assoc_texts(&c).is_empty(), "该收窗");
    if let KeyAction::InsertText { text, .. } = &act {
        for h in &hits {
            assert!(
                !text.contains(h.as_str()),
                "不该选中联想 {h:?}，实得 {text:?}"
            );
        }
    }
}

/// 受控对照：默认（`space_commits = true`）时空格确实选中联想。
/// 少了它，上一条可能只是因为空格在联想态压根不工作。
#[test]
fn space_commits_true_selects_assoc() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    press(&c, "q");
    let first = commit_with_space(&c);
    let hits = assoc_texts(&c);
    assert!(!hits.is_empty());
    let want = hits[0].strip_prefix(&first).unwrap().to_string();
    let act = c.handle_key_event(&key_event(0x20));
    match act {
        KeyAction::InsertText { text, .. }
        | KeyAction::CommitThenDeferComposition {
            commit_text: text, ..
        } => assert_eq!(text, want, "默认该选中首条联想并只补剩余部分"),
        other => panic!("实得 {other:?}"),
    }
}

/// 联想态按引导键进模式不顶屏联想候选（理由同标点）。
///
/// 标点那条路（`commit_highlight_then_char`）有同款守卫，但进模式顶屏走的是
/// `take_committed_with_highlight`，判据必须自己带。
#[test]
fn entering_mode_does_not_commit_assoc_candidate() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    let hits = enter_assoc(&c);
    // 反引号 = 临时拼音引导键：只开引导符新组合，不把联想候选顶上屏。
    match c.handle_key_event(&key_event(0xC0)) {
        KeyAction::UpdateComposition { text, .. } => {
            assert_eq!(text, "`", "联想态进临拼应只开引导符组合，实得 {text:?}");
        }
        KeyAction::InsertText { text, .. }
        | KeyAction::CommitThenDeferComposition {
            commit_text: text, ..
        } => panic!("联想态按引导键不该顶屏联想候选，却上屏了 {text:?}（联想候选 {hits:?}）"),
        other => panic!("联想态按反引号应进临时拼音，实得 {other:?}"),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 联想态的回车 / 退格：吃键还是交还宿主
//
// 两个键默认相反（回车透传、退格吃键），理由见 `AssociationConfig` 的字段文档。
// 四条用例把两个开关的两种取值都钉死，只测一半会让「开关根本没接线」照样全绿。
// ─────────────────────────────────────────────────────────────────────────────

/// 默认：联想态回车交还宿主（收窗 + 照常换行/发送），不必按两次。
#[test]
fn assoc_enter_passes_through_by_default() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    enter_assoc(&c);
    match c.handle_key_event(&key_event(0x0D)) {
        KeyAction::ClearCompositionThenPassThrough => {}
        other => panic!("联想态回车默认应收窗 + 交还宿主，实得 {other:?}"),
    }
    assert!(assoc_texts(&c).is_empty(), "回车后联想窗该收起");
}

/// `enter_cancels_only = true`：回车只收窗，键被吃掉（要按第二次才生效）。
#[test]
fn assoc_enter_cancels_only_when_configured() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    // 两个开关同时设成与各自默认相反：这样「回车读了退格那份配置」这类接错线
    // 也会被抓到——只翻一个开关时，另一个键的默认值恰好能让错误的读取蒙对。
    let c = coord_tweak("word", "wubi86_pinyin", |cfg| {
        cfg.input.association.enter_cancels_only = true;
        cfg.input.association.backspace_cancels_only = false;
    });
    enter_assoc(&c);
    match c.handle_key_event(&key_event(0x0D)) {
        KeyAction::ClearComposition => {}
        other => panic!("开 enter_cancels_only 后回车应只收窗吃键，实得 {other:?}"),
    }
    assert!(assoc_texts(&c).is_empty(), "回车后联想窗该收起");
}

/// 默认：联想态退格吃键——与回车相反，透传会删掉刚上屏的字（不可逆）。
#[test]
fn assoc_backspace_is_eaten_by_default() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    enter_assoc(&c);
    match c.handle_key_event(&key_event(0x08)) {
        KeyAction::ClearComposition => {}
        other => panic!("联想态退格默认应只收窗吃键，实得 {other:?}"),
    }
    assert!(assoc_texts(&c).is_empty(), "退格后联想窗该收起");
}

/// `backspace_cancels_only = false`：退格连同收窗一起交还宿主。
#[test]
fn assoc_backspace_passes_through_when_configured() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    // 同上，两个开关一起翻，堵住「退格读了回车那份配置」的假绿。
    let c = coord_tweak("word", "wubi86_pinyin", |cfg| {
        cfg.input.association.backspace_cancels_only = false;
        cfg.input.association.enter_cancels_only = true;
    });
    enter_assoc(&c);
    match c.handle_key_event(&key_event(0x08)) {
        KeyAction::ClearCompositionThenPassThrough => {}
        other => panic!("关 backspace_cancels_only 后退格应交还宿主，实得 {other:?}"),
    }
    assert!(assoc_texts(&c).is_empty(), "退格后联想窗该收起");
}

// ── 联想态下的「其它按键」 ────────────────────────────────────────────────────
//
// 退格与回车各有专门分支，本节管在既有分支里落到 `PassThrough` 的那一大类键（Del/Home/End/
// 左右/Insert 等）。名单数不清，故收口按动作判，见 `handle_assoc::assoc_release_on_passthrough`。
//
// 这一族必须走 `handle_key_event_policed`（bridge 的真入口）：收口挂在那个唯一出口上，走内层
// `handle_key_event` 拿到的 `PassThrough` 正是修复前的症状。

/// 联想态下会落到透传的键。刻意不含 ↑↓/PageUp/PageDown：那四个在联想态是翻页与移高亮。
/// Tab 也不在列：它是辅助码触发键（`enter_aux_code`），会被干净地 `Consumed`，不属本收口——
/// 这正是「判据取动作、不取键名」的理由。
const ASSOC_PASSTHROUGH_KEYS: &[(u32, &str)] = &[
    (0x2E, "Delete"),
    (0x24, "Home"),
    (0x23, "End"),
    (0x25, "Left"),
    (0x27, "Right"),
    (0x2D, "Insert"),
];

/// 带指定修饰键的按键事件。
fn key_event_mods(key_code: u32, modifiers: u32) -> KeyEventData {
    KeyEventData {
        modifiers,
        ..key_event(key_code)
    }
}

/// 带 Shift 的按键事件（Shift+字母进临时英文用）。
fn key_event_shift(key_code: u32) -> KeyEventData {
    key_event_mods(key_code, MOD_SHIFT)
}

/// 联想窗弹出时 Del / Home / End 等不得被吞。
///
/// 联想态的占位组合让 TSF 的 `_HasInputSession()` 为真，`OnTestKeyDown` 把这批键（`HotkeyType::CursorKey`）
/// 吃下转发；协调器缓冲为空而回 `PassThrough`，「吃了再吐」时不补发 `WM_KEYDOWN` 的宿主直接丢键。
/// 正解是收组合 + 重放按键，顺带收窗：这些键动了光标或文档，联想赖以成立的上文已经断了。
#[test]
fn assoc_cursor_and_edit_keys_close_window_and_reach_host() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    for (vk, name) in ASSOC_PASSTHROUGH_KEYS {
        enter_assoc(&c);
        match c.handle_key_event_policed(&key_event(*vk)) {
            KeyAction::ClearCompositionThenPassThrough => {}
            other => panic!("联想态 {name} 应收组合 + 交还宿主，实得 {other:?}"),
        }
        assert!(assoc_texts(&c).is_empty(), "{name} 之后联想窗该收起");
    }
}

/// 反向守卫：光按下修饰键不该收窗。Shift+字母是「先 Shift keydown、再字母 keydown」，第一下就收窗
/// 会让临英入口被自己撞掉。Windows 侧这类 keydown 不转发，这条钉的是薄宿主与未来的接线。
#[test]
fn assoc_survives_bare_modifier_keydown() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    let hits = enter_assoc(&c);
    // 值域必须写全（`handle_assoc::is_modifier_like_vk`）：0xA0-0xA3、CapsLock(0x14)、左右 Alt(0xA4/0xA5)
    // 与笼统的 VK_SHIFT/VK_CONTROL/VK_MENU(0x10/0x11/0x12)。后几个桌面 TSF 不转发，但薄宿主最可能
    // 上报它们。守卫按 vk 判，带修饰位只是为了复现真机形态（Shift keydown 必带 MOD_SHIFT）。
    for (vk, mods) in [
        (0xA0u32, MOD_SHIFT),
        (0xA1, MOD_SHIFT),
        (0xA2, MOD_CTRL),
        (0xA3, MOD_CTRL),
        (0x14, 0),
        (0xA4, MOD_ALT),
        (0xA5, MOD_ALT),
        (0x10, MOD_SHIFT),
        (0x11, MOD_CTRL),
        (0x12, MOD_ALT),
    ] {
        c.handle_key_event_policed(&key_event_mods(vk, mods));
        assert_eq!(
            assoc_texts(&c),
            hits,
            "修饰键 keydown (vk=0x{vk:02X}) 不该动联想窗"
        );
    }
}

/// Ctrl/Alt 组合刻意也走改判（`assoc_release_on_passthrough`）：它们在联想态同样落 `PassThrough`，
/// 同样被 TSF 的 `ctrl_alt_cleanup` 分支吃下转发。改判后 C++ 走 `_ReplayKeyToHost`，重放时物理
/// 修饰键仍按着，宿主 `GetKeyState` 能还原 Ctrl+A 语义。
#[test]
fn assoc_ctrl_combo_closes_window_and_reaches_host() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    enter_assoc(&c);
    match c.handle_key_event_policed(&key_event_mods(0x41, MOD_CTRL)) {
        KeyAction::ClearCompositionThenPassThrough => {}
        other => panic!("联想态 Ctrl+A 应收组合 + 交还宿主，实得 {other:?}"),
    }
    assert!(assoc_texts(&c).is_empty(), "Ctrl+A 之后联想窗该收起");
}

/// 联想态的候选热键（出厂置顶 Ctrl+数字、删除 Ctrl+Shift+数字）同样收组合 + 交还宿主。
/// 这组键被 TSF 用 RegisterHotKey 拦成系统热键，TSF 只在「交还宿主」类响应时才注入原键，
/// 回 `Consumed` 就是丢键（浏览器 Ctrl+1 切标签没反应）。
#[test]
fn assoc_candidate_hotkeys_close_window_and_reach_host() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord_tweak("word", "wubi86_pinyin", |cfg| {
        cfg.keys.pin_candidate = "ctrl+number".into();
        cfg.keys.delete_candidate = "ctrl+shift+number".into();
    });
    for (mods, name) in [(MOD_CTRL, "Ctrl+1"), (MOD_CTRL | MOD_SHIFT, "Ctrl+Shift+1")] {
        enter_assoc(&c);
        match c.handle_key_event_policed(&key_event_mods(0x31, mods)) {
            KeyAction::ClearCompositionThenPassThrough => {}
            other => panic!("联想态 {name} 应收组合 + 交还宿主，实得 {other:?}"),
        }
        assert!(assoc_texts(&c).is_empty(), "{name} 之后联想窗该收起");
    }
}

/// 对照：联想态开着 `input.alt_commit` 时 Alt+数字 仍归输入法（上屏该候选的拼音），
/// 不交还宿主——TSF 的注入只认「交还宿主」类响应，这条不受影响。
#[test]
fn assoc_alt_digit_stays_with_alt_commit() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord_tweak("word", "wubi86_pinyin", |cfg| {
        cfg.input.alt_commit = "pinyin".into();
        cfg.keys.pin_candidate = "ctrl+number".into();
        cfg.keys.delete_candidate = "ctrl+shift+number".into();
    });
    enter_assoc(&c);
    match c.handle_key_event_policed(&key_event_mods(0x31, MOD_ALT)) {
        KeyAction::InsertText { text, .. } => {
            assert!(!text.is_empty(), "联想态 Alt+1 应上屏该候选的拼音")
        }
        other => panic!("联想态 Alt+1 应由 alt_commit 上屏，实得 {other:?}"),
    }
    assert!(assoc_texts(&c).is_empty(), "Alt+1 上屏后联想窗该收起");
}

/// 联想窗弹出时按 Shift+字母要进临英，不能落进中文码表缓冲。
///
/// `try_activate_mode` 开头的联想让位只为保住 `;`/`'` 的二三候选键（见 `select_keys_work_in_assoc`），
/// 不能把临英 Shift+字母分支一起挡掉；那个分支的「无候选」门也不能被联想候选拦住。
#[test]
fn assoc_shift_letter_enters_temp_english() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    enter_assoc(&c);
    // 走 policed（生产入口）而非内层：验的虽是 `try_activate_mode`，但顺带钉住
    // `assoc_release_on_passthrough` 不会误伤这条入口——它返回的不是透传，①本就不该触发。
    match c.handle_key_event_policed(&key_event_shift(0x48)) {
        KeyAction::UpdateComposition { .. } => {}
        other => panic!("联想态 Shift+H 应进临时英文，实得 {other:?}"),
    }
    assert_eq!(
        c.debug_active_mode(),
        Some("temp_english"),
        "Shift+H 该落在临时英文，而不是把 h 塞进中文码表缓冲"
    );
    assert!(assoc_texts(&c).is_empty(), "进临英后联想窗该收起");
}

/// 同一入口的另一档：`shift_behavior = "direct_commit"` 时直接上屏大写字母。两档在同一个 `if` 里
/// 分叉，只测一档时另一档漏了收联想窗也照绿。
#[test]
fn assoc_shift_letter_direct_commits_when_configured() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord_tweak("word", "wubi86_pinyin", |cfg| {
        cfg.input.temp_english.shift_behavior = "direct_commit".to_string();
    });
    enter_assoc(&c);
    let text = match c.handle_key_event_policed(&key_event_shift(0x48)) {
        KeyAction::InsertText { text, .. }
        | KeyAction::CommitThenDeferComposition {
            commit_text: text, ..
        } => text,
        other => panic!("direct_commit 档下 Shift+H 应直接上屏，实得 {other:?}"),
    };
    assert_eq!(text, "H", "direct_commit 档该上屏大写 H");
    assert_eq!(c.debug_active_mode(), None, "direct_commit 档不进临英");
    assert!(assoc_texts(&c).is_empty(), "上屏后联想窗该收起");
}

// ─────────────────────────────────────────────────────────────────────────────
// `select_keys_commit`：联想态让出二三候选键
//
// 出厂 `true`（照常选第 2/3 条，由 `select_keys_work_in_assoc` 钉）。`false` 时 `;` `'` 不选联想，
// 收窗后按「联想窗不在」处理。判据拿同一个键在空闲态的应答来比，不写死某个标点（`;` 出厂另绑了
// 快捷输入）。
// ─────────────────────────────────────────────────────────────────────────────

/// 上屏 / 组合动作中「对宿主可见的那部分」：写进宿主的文本与新开的组合。联想态挂着占位组合，
/// 空闲态没有，动作变体不必相同。
fn host_visible(act: &KeyAction) -> (String, String) {
    match act {
        KeyAction::InsertText {
            text,
            new_composition,
            ..
        } => (text.clone(), new_composition.clone().unwrap_or_default()),
        KeyAction::CommitThenDeferComposition {
            commit_text,
            deferred_composition,
            ..
        } => (commit_text.clone(), deferred_composition.clone()),
        KeyAction::UpdateComposition { text, .. } => (String::new(), text.clone()),
        other => panic!("意外的动作 {other:?}"),
    }
}

/// 空闲态（联想关、刚上屏一个字）按 `vk` 的应答——「联想窗不在」时这个键本来的样子。
fn idle_response(vk: u32) -> (String, String) {
    let c = coord_tweak("off", "wubi86_pinyin", |cfg| {
        cfg.input.association.select_keys_commit = false;
    });
    press(&c, "q");
    c.handle_key_event(&key_event(0x20));
    assert!(assoc_texts(&c).is_empty(), "前提：参照组不进联想态");
    host_visible(&c.handle_key_event(&key_event(vk)))
}

fn yield_coord() -> std::sync::Arc<Coordinator> {
    coord_tweak("word", "wubi86_pinyin", |cfg| {
        cfg.input.association.select_keys_commit = false;
    })
}

/// `select_keys_commit = false`：联想态按 `'`（三选键）不选联想，收窗并照空闲态出符号。
#[test]
fn assoc_yields_quote_when_select_keys_commit_off() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = yield_coord();
    let hits = enter_assoc(&c);
    assert!(
        hits.len() >= 3,
        "前提：至少三条联想，`'` 才选得到，实得 {hits:?}"
    );
    let act = c.handle_key_event(&key_event(0xDE)); // VK_OEM_7 = `'`
    let got = host_visible(&act);
    for h in &hits {
        assert!(
            !got.0.contains(h.as_str()),
            "不该选中联想 {h:?}，实得 {act:?}"
        );
    }
    assert!(!got.0.is_empty(), "`'` 该出符号，实得 {act:?}");
    assert_eq!(got, idle_response(0xDE), "该与联想窗不在时按 `'` 一致");
    assert!(assoc_texts(&c).is_empty(), "联想窗该收起");
}

/// 同上，`;`（二选键）。出厂 `;` 另绑快捷输入引导，空闲态按它进模式——让出后也该进。
#[test]
fn assoc_yields_semicolon_when_select_keys_commit_off() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = yield_coord();
    let hits = enter_assoc(&c);
    assert!(hits.len() >= 2, "前提：至少两条联想，实得 {hits:?}");
    let act = c.handle_key_event(&key_event(0xBA)); // VK_OEM_1 = `;`
    let got = host_visible(&act);
    for h in &hits {
        assert!(
            !got.0.contains(h.as_str()),
            "不该选中联想 {h:?}，实得 {act:?}"
        );
    }
    assert_eq!(got, idle_response(0xBA), "该与联想窗不在时按 `;` 一致");
    assert!(assoc_texts(&c).is_empty(), "联想窗该收起");
}

/// 让出只管联想态：有码时 `;` 照常选第 2 个候选。
#[test]
fn select_keys_commit_off_keeps_select_keys_outside_assoc() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = yield_coord();
    press(&c, "q");
    let page = c.debug_page_texts();
    assert!(page.len() >= 2, "前提：q 至少两个候选，实得 {page:?}");
    let (text, _) = host_visible(&c.handle_key_event(&key_event(0xBA)));
    assert_eq!(text, page[1], "非联想态 `;` 该照常选第 2 个候选");
}

/// 让出不碰数字键：联想态按 `2` 照常选第 2 条联想。
#[test]
fn select_keys_commit_off_keeps_digit_selection_in_assoc() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = yield_coord();
    let hits = enter_assoc(&c);
    assert!(hits.len() >= 2, "前提：至少两条联想，实得 {hits:?}");
    let (text, _) = host_visible(&c.handle_key_event(&key_event(0x32)));
    assert!(
        hits[1].ends_with(&text) && !text.is_empty(),
        "数字 2 该选第 2 条 {:?}，实得 {text:?}",
        hits[1]
    );
}

/// 受控对照：默认（`true`）时 `'` 在联想态确实选第 3 条——少了它，上面的让出用例
/// 可能只是因为 `'` 在联想态压根不选词。
#[test]
fn quote_selects_third_assoc_by_default() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    let hits = enter_assoc(&c);
    assert!(hits.len() >= 3, "前提：至少三条联想，实得 {hits:?}");
    let (text, _) = host_visible(&c.handle_key_event(&key_event(0xDE)));
    assert!(
        hits[2].ends_with(&text) && !text.is_empty(),
        "默认 `'` 该选第 3 条 {:?}，实得 {text:?}",
        hits[2]
    );
}

/// 让出的口径是「选词键」而非写死 `;` `'`：用户把字母 `z` 配成二选键时，联想态按 `z`
/// 同样不选联想，照空闲态处理（字母臂与符号臂是两个消费点，两处都要让）。
#[test]
fn assoc_yields_custom_letter_select_key() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let tweak = |cfg: &mut Config| {
        cfg.input.association.select_keys_commit = false;
        cfg.keys
            .session_actions
            .insert("z".into(), "select_candidate:2".into());
    };
    let c = coord_tweak("word", "wubi86_pinyin", tweak);
    let hits = enter_assoc(&c);
    assert!(hits.len() >= 2, "前提：至少两条联想，实得 {hits:?}");
    let act = c.handle_key_event(&key_event(0x5A)); // `z`
    let got = host_visible(&act);
    for h in &hits {
        assert!(
            !got.0.contains(h.as_str()),
            "不该选中联想 {h:?}，实得 {act:?}"
        );
    }
    assert!(assoc_texts(&c).is_empty(), "联想窗该收起");

    let idle = coord_tweak("off", "wubi86_pinyin", tweak);
    press(&idle, "q");
    idle.handle_key_event(&key_event(0x20));
    assert_eq!(
        got,
        host_visible(&idle.handle_key_event(&key_event(0x5A))),
        "该与联想窗不在时按 `z` 一致"
    );
}

/// 常用单字上屏后的联想（手机配置：智能、持续、带标点）。测量工具，手动跑看输出：
///
/// ```text
/// cargo test -p wind-coordinator --test assoc_end_to_end common_char_dump -- --ignored --nocapture
/// ```
#[test]
#[ignore = "测量工具：打印常用字的联想，手动跑"]
fn common_char_dump() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        return;
    }
    let c = coord_tweak("smart", "pinyin", |cfg| {
        cfg.input.association.mode = "continuous".to_string();
        cfg.input.association.punct = true;
    });
    for (code, want) in [
        ("wo", "我"),
        ("de", "的"),
        ("da", "大"),
        ("shi", "是"),
        ("ni", "你"),
        ("ta", "他"),
        ("zai", "在"),
        ("le", "了"),
        ("bu", "不"),
        ("you", "有"),
        ("yi", "一"),
        ("zhe", "这"),
        ("hao", "好"),
        ("jiu", "就"),
        ("ye", "也"),
        ("dou", "都"),
        ("hen", "很"),
        ("yao", "要"),
        ("hui", "会"),
        ("shuo", "说"),
        ("ren", "人"),
        ("dao", "到"),
        ("shang", "上"),
        ("ge", "个"),
        ("zhong", "中"),
        ("lai", "来"),
        ("xiao", "小"),
        ("men", "们"),
        ("na", "那"),
        ("he", "和"),
    ] {
        c.handle_key_event(&key_event(0x1B));
        press(&c, code);
        let out = commit_with_space(&c);
        let mark = if out == want { "" } else { " ⚠" };
        println!("{out}{mark}\t{}", assoc_texts(&c).join(" "));
    }
}
