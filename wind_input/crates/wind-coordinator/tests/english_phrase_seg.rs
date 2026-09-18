//! 英文词组分词输入（论坛 t42）端到端：`'` 切段，每段只打前缀。
//!
//! 设计与判据见 `wind_engine::english_phrase`。
//!
//! # ⚠️ 假绿源
//!
//! 词典缺失时整族**静默跳过**（判据是耗时而非通过条数），worktree 需自备 `build_dev`。
//! 见 `has_english_schema`。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_config::config::FreeInputMode;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};
use wind_store::Store;

/// `'` 键。
const VK_QUOTE: u32 = 0xDE;
/// `;` 键（快捷输入出厂引导键）。
const VK_SEMICOLON: u32 = 0xBA;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_english_schema() -> bool {
    let d = data_dir();
    d.join("schemas/english.schema.toml").exists() && d.join("schemas/english").is_dir()
}

fn key(key_code: u32) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

fn english_config(phrase_seg: bool) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "english".into()];
    cfg.schema.active = "english".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.english.phrase_seg = phrase_seg;
    cfg
}

fn coord_with(cfg: Config, tag: &str) -> Arc<Coordinator> {
    let path = std::env::temp_dir().join(format!("wind_en_seg_{tag}.redb"));
    let _ = std::fs::remove_file(&path);
    Coordinator::new_headless_with_store(
        cfg,
        Some(&data_dir()),
        Arc::new(Store::open(&path).unwrap()),
    )
}

/// 按串打字，`'` 走 VK_QUOTE，其余按字母键。
fn type_input(coord: &Coordinator, s: &str) {
    for c in s.chars() {
        if c == '\'' {
            coord.handle_key_event(&key(VK_QUOTE));
        } else {
            coord.handle_key_event(&key((c.to_ascii_uppercase() as u32) & 0xFF));
        }
    }
}

/// 拼接式编码（`Buenos Aires` → code `buenosaires`）。
#[test]
fn matches_concatenated_encoding() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let p = page_of("bue'air", "concat", true);
    assert!(
        p.iter().any(|t| t == "Buenos Aires"),
        "`bue'air` 应命中 Buenos Aires，实际: {p:?}"
    );
}

/// 共同前缀式编码（`iPhone 15 Pro Max` → code `iphone`，code 里没有后段词）。
/// 这是 t42 原帖 `p1..p10` 分列方案够不着的那一类。
#[test]
fn matches_shared_prefix_encoding() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let p = page_of("ip'max", "shared", true);
    assert!(
        p.iter().any(|t| t == "iPhone 15 Pro Max"
            || t == "iPhone 16 Pro Max"
            || t == "iPhone 17 Pro Max"),
        "`ip'max` 应命中某个 iPhone … Pro Max，实际: {p:?}"
    );
}

/// 跳词：`ip'pro` 越过中间的型号数字命中 `Pro`（2026-09-18 拍板「优先保效果」）。
#[test]
fn skips_intervening_words_end_to_end() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let p = page_of("ip'pro", "skip", true);
    assert!(
        p.iter()
            .any(|t| t.starts_with("iPhone") && t.contains("Pro")),
        "`ip'pro` 应跳过型号数字命中 iPhone … Pro，实际: {p:?}"
    );
}

/// ★ 反向对照：开关关闭时 `'` 仍是第三候选键，不进缓冲。
///
/// 没有这条，「恒夺取」与「按开关夺取」两种实现都能让上面几条通过。
#[test]
fn quote_stays_a_select_key_when_disabled() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let c = coord_with(english_config(false), "off");
    type_input(&c, "ip");
    let before = c.debug_page_texts();
    assert!(
        before.len() >= 3,
        "前提：打 ip 至少要有 3 条候选，实际: {before:?}"
    );
    let third = before[2].clone();
    // 关闭时 `'` 是三选键 ⇒ 选中第 3 条上屏，而不是进缓冲。
    match c.handle_key_event(&key(VK_QUOTE)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, third, "关闭分词时 `'` 应选中第 3 候选上屏")
        }
        other => panic!("关闭分词时 `'` 应作三选键上屏，实际: {other:?}"),
    }
}

/// 开启时同一串按 `'` 不上屏、而是进缓冲继续组词。与上一条构成对照的两半。
#[test]
fn quote_enters_buffer_when_enabled() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let c = coord_with(english_config(true), "on");
    type_input(&c, "ip");
    if let KeyAction::InsertText { text, .. } = c.handle_key_event(&key(VK_QUOTE)) {
        panic!("开启分词时 `'` 不该上屏候选，实际上屏了 {text:?}");
    }
    // 缓冲里已含 `'`，再打一段应能命中词组。
    type_input(&c, "pro");
    let p = c.debug_page_texts();
    assert!(
        p.iter()
            .any(|t| t.starts_with("iPhone") && t.contains("Pro")),
        "`'` 进缓冲后应能续打并命中词组，实际: {p:?}"
    );
}

/// ★ 打到分词符那一刻候选不得变空。
///
/// `ip'` 切出的是**一段**。原路径此时必然落空（词库里没有 code 以 `ip'` 开头的条目），
/// 所以分词路径必须照查——单段查询就是「列出首词以 ip 开头的词组」，正是用户按下
/// 分词符时想看到的东西。
///
/// 这条是实测反馈修出来的：`phrase_candidates` 原先有一条 `segs.len() < 2` 早退，
/// 理由「单段等价于普通前缀补全，原路径已经做了」——那个理由只对**不含分词符**的输入
/// 成立，而不含分词符的输入在更上面就被 `!input.contains(sep)` 挡掉了，根本走不到那条
/// 早退。于是它实际只在 `ip'` 这一种情形生效，且效果恰好是让候选整片消失。
#[test]
fn candidates_do_not_vanish_right_after_the_separator() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let p = page_of("ip'", "sep_only", true);
    assert!(
        p.iter().any(|t| t.starts_with("iPhone")),
        "打到 `ip'` 时应列出首词以 ip 开头的词组，不该是空候选，实际: {p:?}"
    );
}

/// ★ 顺序断言：weight 是主键，跨度只在同权重时说话。
///
/// 审查点名的缺口——此前 12 条 e2e **一条顺序断言都没有**，全是 `iter().any(...)`，
/// 于是引擎内部按什么排都测不出来，而协调器的 `candidate_display_order` 会按 weight
/// 统一重排（AGENTS.md 硬约定）。单测里的顺序断言测的是用户看不到的中间态。
///
/// 判据取 `ip'max`：`iPhone XS Max` 是**跨度 2**（ip→iPhone、max→Max，跳过 XS），
/// 比 `iPhone 15 Pro Max` 的跨度 3 更紧凑。跨度当主键时它会排在前面；weight 当主键时
/// 则是 `iPhone … Pro Max` 那批在前。后者才是用户眼前的真实顺序。
#[test]
fn weight_outranks_span_end_to_end() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let p = page_of("ip'max", "order", true);
    let phrases: Vec<&String> = p.iter().filter(|t| t.contains(' ')).collect();
    assert!(
        phrases.len() >= 2,
        "前提：应有多条词组候选可比较，实际: {p:?}"
    );
    let pro_max = phrases
        .iter()
        .position(|t| t.as_str() == "iPhone 15 Pro Max");
    let xs_max = phrases.iter().position(|t| t.as_str() == "iPhone XS Max");
    if let (Some(a), Some(b)) = (pro_max, xs_max) {
        assert!(
            a < b,
            "weight 高的 `iPhone 15 Pro Max` 应排在跨度更小但 weight 低的 `iPhone XS Max` 之前，\
             实际顺序: {phrases:?}"
        );
    } else {
        panic!("前提不成立：词库里应同时有 iPhone 15 Pro Max 与 iPhone XS Max，实际: {p:?}");
    }
}

/// 不含分词符时逐条回归原路径——本功能不得改动普通英文输入。
#[test]
fn plain_input_is_untouched() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    assert_eq!(
        page_of("hel", "plain_on", true),
        page_of("hel", "plain_off", false),
        "不含分词符时开/关两档的候选必须逐条相同"
    );
}

/// ★ 合并而非劫持：同一串输入要**同时**拿到原路径与分词路径的候选。
///
/// 这条钉住 `EnglishEngine::convert` 的「原路径照查 + 分词候选追加」。改成
/// 「见到分词符就改走分词路径」的话，词库里 57 条含撇号的 code 会在打全码时集体失踪。
///
/// ⚠️ 输入必须选**两条路都非空**的那种，否则这条护栏是空的：先前用 `let`（不含分词符）
/// ⇒ 分词候选恒空 ⇒ 劫持分支根本不执行，变异验证里实测不变红。
/// `o'c` 才同时踩到两边：
/// - 分词路径 → `OS X El Capitan`（还顺带验了跳词：`o`→OS、`c`→Capitan，越过 `X`/`El`）
/// - 原路径   → `o'clock`（code 自带撇号，靠前缀匹配）
#[test]
fn both_paths_contribute_and_neither_is_dropped() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let p = page_of("o'c", "apos", true);
    assert!(
        p.iter().any(|t| t == "OS X El Capitan"),
        "分词路径应命中 OS X El Capitan，实际: {p:?}"
    );
    assert!(
        p.iter().any(|t| t == "o'clock"),
        "原路径的含撇号词 o'clock 不得被分词候选挤掉，实际: {p:?}"
    );
}

fn page_of(s: &str, tag: &str, phrase_seg: bool) -> Vec<String> {
    let c = coord_with(english_config(phrase_seg), tag);
    type_input(&c, s);
    c.debug_page_texts()
}

// ───────────────────── 临时英文 ─────────────────────

/// 临英配置：主方案取五笔（临英的归属必须是内置英文方案，与 active 无关）。
fn temp_english_config(phrase_seg: bool) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "english".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.temp_english.enabled = true;
    cfg.input.temp_english.phrase_seg = phrase_seg;
    cfg
}

/// Shift+首字母进临英，其余按串打（`'` 走 VK_QUOTE）。
fn type_temp_english(coord: &Coordinator, s: &str) {
    let mut chars = s.chars();
    let first = chars.next().expect("至少一个字母");
    coord.handle_key_event(&KeyEventData {
        key_code: (first.to_ascii_uppercase() as u32) & 0xFF,
        modifiers: MOD_SHIFT,
        ..key(0)
    });
    for c in chars {
        if c == '\'' {
            coord.handle_key_event(&key(VK_QUOTE));
        } else {
            coord.handle_key_event(&key((c.to_ascii_uppercase() as u32) & 0xFF));
        }
    }
}

fn temp_page(s: &str, tag: &str, phrase_seg: bool) -> Vec<String> {
    let c = coord_with(temp_english_config(phrase_seg), tag);
    type_temp_english(&c, s);
    c.debug_page_texts()
}

/// 临英下分词照样命中，且跳词有效。
#[test]
fn temp_english_matches_phrases_with_skipping() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let p = temp_page("ip'pro", "te_on", true);
    assert!(
        p.iter()
            .any(|t| t.starts_with("iPhone") && t.contains("Pro")),
        "临英下 `ip'pro` 应跳词命中 iPhone … Pro，实际: {p:?}"
    );
}

/// ★ 临英的开关是**独立**的：英文方案那份开着也不影响临英。
///
/// 反向对照不可省——没有它，「读同一个开关」与「各读各的」两种实现都能过上一条。
#[test]
fn temp_english_scope_has_its_own_switch() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let mut cfg = temp_english_config(false);
    // 英文方案那份**开着**，临英那份关着 ⇒ 临英下 `'` 仍是三选键。
    cfg.schema.english.phrase_seg = true;
    let c = coord_with(cfg, "te_scope");
    type_temp_english(&c, "ip");
    let before = c.debug_page_texts();
    assert!(
        before.len() >= 3,
        "前提：临英打 Ip 要有 3 条候选，实际: {before:?}"
    );
    match c.handle_key_event(&key(VK_QUOTE)) {
        KeyAction::InsertText { .. } => {}
        other => panic!(
            "临英开关关闭时 `'` 应作三选键上屏（不受 schema.english.phrase_seg 影响），实际: {other:?}"
        ),
    }
}

// ───────────────────── 快捷输入（mix） ─────────────────────

fn quick_config(phrase_seg: bool) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "english".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.english.phrase_seg = phrase_seg;
    cfg
}

fn quick_page(s: &str, tag: &str, phrase_seg: bool) -> Vec<String> {
    let c = coord_with(quick_config(phrase_seg), tag);
    c.handle_key_event(&key(VK_SEMICOLON));
    for ch in s.chars() {
        if ch == '\'' {
            c.handle_key_event(&key(VK_QUOTE));
        } else {
            c.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
        }
    }
    c.debug_page_texts()
}

/// 快捷输入的英文成员同样支持分词（跟随 `schema.english.phrase_seg`）。
#[test]
fn quick_input_english_matches_phrases() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let p = quick_page("ip'pro", "q_on", true);
    assert!(
        p.iter()
            .any(|t| t.starts_with("iPhone") && t.contains("Pro")),
        "快捷输入里 `ip'pro` 应命中 iPhone … Pro，实际: {p:?}"
    );
}

/// ★★ 分词符不得把透镜推进 Free —— 否则词组打得进去却选不出来。
///
/// 出厂 `free_input = Auto`：缓冲里出现「越界字符」就切 [`MixLens::Free`]，而 Free 透镜
/// 一个选词键都没有、候选窗连序号都不画。分词符必须同时被按键分派收下**和**被
/// `MixLens::accepts` 认可，两处各写一份判据的表现就是「打得进、选不出」且不报错。
///
/// 判据取「数字键仍能选词」：Free 透镜下数字键是字面输入，会继续进缓冲而不是选中候选。
#[test]
fn quick_input_separator_keeps_select_keys_alive() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let c = coord_with(quick_config(true), "q_lens");
    c.handle_key_event(&key(VK_SEMICOLON));
    for ch in "ip".chars() {
        c.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
    c.handle_key_event(&key(VK_QUOTE));
    for ch in "pro".chars() {
        c.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
    let page = c.debug_page_texts();
    assert!(!page.is_empty(), "前提：应有词组候选，实际: {page:?}");
    let first = page[0].clone();
    // 数字键 1 选首候选。Free 透镜下它会被当字面输入进缓冲，候选不会上屏。
    match c.handle_key_event(&key(0x31)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, first, "数字键应选中首候选上屏（说明透镜仍是 Text）")
        }
        other => {
            panic!("分词符把透镜推进 Free 了：数字键不再选词，实际: {other:?}（候选 {page:?}）")
        }
    }
}

/// ★★ H1 回归护栏：开启分词**不得**打散快捷输入里含撇号的自由输入。
///
/// 第一版把分词符并进了 `MixLens::Text` 的接受集，于是 `don't` 留在「编码域」被喂给拼音
/// 成员：实测候选变成 `["东欧","斗殴",…]`，空格上屏得到 `;东欧n't`，整串输入被打散。
/// `input_flow.rs` 的 `quick_input_free_apostrophe_word` 当初正是为防这个事故而写，
/// 它没变红只因为用的是出厂 `phrase_seg = false`——**开关一开，它钉住的不变量就没人守了**。
///
/// 判据取「开/关两档逐条一致」：这类词本就不是词组，分词开着与关着都该原样上屏。
#[test]
fn phrase_seg_does_not_break_apostrophe_free_input() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    for (i, word) in ["don't", "rock'n'roll"].iter().enumerate() {
        let off = quick_page(word, &format!("apo_off{i}"), false);
        let on = quick_page(word, &format!("apo_on{i}"), true);
        assert_eq!(
            on, off,
            "`;{word}` 的候选不该因为开启词组分词而改变（开={on:?} 关={off:?}）"
        );
        assert_eq!(
            on.first().map(String::as_str),
            Some(*word),
            "`;{word}` 应原样作为候选，实际: {on:?}"
        );
    }
}

/// ★ M1 回归护栏：分词符不得在**数字透镜**下被收进缓冲。
///
/// `accepts` 只在 Text/Phrase 下认分词符；写入侧若不带同样的 `lens` 守卫，两者就不是同一个
/// 谓词了。实测 `free_input = off` 的实例里 `;12` 按 `'` 会从「选第 3 候选」变成缓冲 `12'`
/// 且候选整片清空——仓里 `input_flow.rs` 的
/// `quick_input_numeric_lens_off_keeps_quote_as_third_select_key` 钉的正是前一种行为，
/// 而它同样只在出厂开关下才守得住。
#[test]
fn phrase_seg_keeps_quote_as_select_key_in_numeric_lens() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let mut cfg = quick_config(true);
    cfg.schema.mix_modes[0].free_input = FreeInputMode::Off;
    let c = coord_with(cfg, "num_lens");
    c.handle_key_event(&key(VK_SEMICOLON));
    c.handle_key_event(&key(0x31));
    c.handle_key_event(&key(0x32));
    let before = c.debug_page_texts();
    assert!(
        before.len() >= 3,
        "前提：`;12` 应有 3 条以上候选，实际: {before:?}"
    );
    let third = before[2].clone();
    match c.handle_key_event(&key(VK_QUOTE)) {
        KeyAction::InsertText { text, .. } => assert_eq!(
            text, third,
            "数字透镜下 `'` 必须仍是第三候选键，不得被分词符臂收走"
        ),
        other => panic!("数字透镜下 `'` 应选中第 3 候选上屏，实际: {other:?}"),
    }
}

/// ★ 词组透镜只查 english 成员：拼音成员不得掺进来。
///
/// 实测（修复前）`;mac'sn` 出的是 `["吗","嘛","骂","马",…]` —— 整串被喂给拼音成员，
/// 分词符当噪声处理。修复后应当是那条词组。
#[test]
fn phrase_lens_queries_english_member_only() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let p = quick_page("mac'sn", "only_en", true);
    assert_eq!(
        p.first().map(String::as_str),
        Some("Mac OS X Snow Leopard"),
        "词组透镜下应只有英文词组候选，不该掺拼音字，实际: {p:?}"
    );
}

/// ★ M6 补缺：英文方案的作用域护栏（审查指出此前完全没测）。
///
/// `english_phrase_separator_key` 承诺「只在英文引擎下夺取 `'`」，但把那条判据换成 `true`
/// 时整个测试族仍然全绿——即「phrase_seg 开着时五笔/拼音方案下 `'` 仍是三选键」没人守。
/// 临英与快捷输入各自都写了作用域对照，就差这一处。
#[test]
fn quote_stays_a_select_key_in_non_english_schema() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let mut cfg = english_config(true);
    cfg.schema.active = "wubi86".into(); // 开关开着，但活跃方案不是英文
    let c = coord_with(cfg, "non_en");
    // 五笔打两码，拿到候选。
    c.handle_key_event(&key(u32::from(b'W')));
    c.handle_key_event(&key(u32::from(b'W')));
    let before = c.debug_page_texts();
    assert!(
        before.len() >= 3,
        "前提：五笔 `ww` 应有 3 条以上候选，实际: {before:?}"
    );
    let third = before[2].clone();
    match c.handle_key_event(&key(VK_QUOTE)) {
        KeyAction::InsertText { text, .. } => assert_eq!(
            text, third,
            "非英文方案下 `'` 必须仍是第三候选键，哪怕 schema.english.phrase_seg 开着"
        ),
        other => panic!("非英文方案下 `'` 应作三选键上屏，实际: {other:?}"),
    }
}

/// ★ M4：分词符是输入语法，不得作为「原文候选」被带上屏。
///
/// 出厂 `raw_candidate = always` 会把所打原文恒插在首位。含分词符时那条候选是字面
/// `ip'pro`——不是任何人想要的内容，而且它占着首位，把真正想要的词组挤到第二条起。
///
/// 三档都该如此：`in_dict` 本来就不产（带分词符的串不可能是词库词），`off` 更不产，
/// 这条钉的是 `always` 档也对齐。
#[test]
fn separator_is_syntax_not_content() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let p = page_of("ip'pro", "m4", true);
    assert!(
        !p.iter().any(|t| t.contains('\'')),
        "候选里不该出现带分词符的原文，实际: {p:?}"
    );
    assert!(
        p.first().is_some_and(|t| t.starts_with("iPhone")),
        "首候选应直接是词组，实际: {p:?}"
    );

    // 临英同理（它有独立的一份 raw_candidate，出厂同样是 always）。
    let t = temp_page("ip'pro", "m4_te", true);
    assert!(
        !t.iter().any(|x| x.contains('\'')),
        "临英候选里同样不该出现带分词符的原文，实际: {t:?}"
    );

    // ★ 反向对照：不含分词符时原文候选照常在（别把 always 档整个关掉了）。
    let plain = page_of("hel", "m4_plain", true);
    assert_eq!(
        plain.first().map(String::as_str),
        Some("hel"),
        "不含分词符时 always 档仍应把原文放首位，实际: {plain:?}"
    );
}
