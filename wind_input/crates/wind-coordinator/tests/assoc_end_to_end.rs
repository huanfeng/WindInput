//! 联想的**端到端**验证：真实词库 + 真实按键 + 真实上屏路径。
//!
//! # 为什么单元测试不够
//!
//! `handle_assoc::wiring_tests` 用的是 headless（无词库）协调器，词语联想在那里恒空
//! ——它能验状态机与按键闸门，验不了「真机上打完一个字到底出不出联想」。
//!
//! 而真机第一次跑起来正是**什么都没出**，且日志一行都没有（`maybe_enter_assoc` 有三个
//! 静默的 early-return）。本文件就是为了把那种情况变成一条会红的用例。
//!
//! 词典缺失时自动跳过 —— ⚠️ `build_dev/data` 不存在时**整族静默跳过而计数照绿**，
//! 判据是耗时。见 project_build_dev_data_missing。

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
    }
}

/// 按用户真机上的配置造协调器：**活跃方案是混输**，这正是词语联想差点整个失效的场景。
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

/// 词语联想的数据源是**反查索引**，而它在生产里由启动后的后台预热线程建好
/// （`construct.rs` → `prewarm_indexes`）。headless 协调器不跑那个线程。
///
/// 索引改成「没就绪就返回空、绝不在按键线程上现建」之后（避免大词库把整机卡住），
/// 测试若不自己预热，`assoc_prefix_words` 会如实给出「无联想」——那测的是夹具缺陷，
/// 不是产品行为。生产/测试/移动端共用同一个 `prewarm_indexes`，三处不会漂移。
fn prewarm(c: &Coordinator) {
    c.prewarm_indexes();
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

/// 空格上屏当前高亮候选，返回真正写进宿主的文本。
///
/// ★ 两种形态都算数：
///   - `InsertText`                 —— 没进联想态（或 `top_commit_mode = pre_confirm`）
///   - `CommitThenDeferComposition` —— **进了联想态**：真提交 + 延到 keyup 才开占位组合
///
/// 后者正是联想能收到后续按键的关键（见 `handle_assoc::ASSOC_COMPOSITION`）。
/// 本函数刻意两种都接：它要回答的是「上屏了什么」，而不是「走了哪条时序」。
fn commit_with_space(c: &Coordinator) -> String {
    match c.handle_key_event(&key_event(0x20)) {
        KeyAction::InsertText { text, .. } => text,
        KeyAction::CommitThenDeferComposition { commit_text, .. } => commit_text,
        other => panic!("空格该上屏，实得 {other:?}"),
    }
}

/// 上屏动作是否**带了占位组合**——联想态的命门。
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

/// ★★★ **真机复现**：混输方案下打一个字上屏，词语联想要出得来。
///
/// `q` 在五笔 86 是「我」的首选码位之一；这里不钉死具体是哪个字，只要求
/// 「上屏了一个汉字 ⇒ 联想非空」。钉死具体字会让词库一更新就假红。
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

/// **受控对照**：同样的操作，`kind = "off"` 时不该有联想。
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

/// 选中联想候选后，**只补出剩余部分**。
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

/// 真机那次的确切复现：混输方案打 `lwty` 空格上屏，看联想出不出。
///
/// 用户 2026-08-16 11:51 的日志里就是这一串（buf='lwty' → 2 候选 → 空格）。
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

/// ★★★ **上屏进入联想态时必须带占位组合**——整条链的命门。
///
/// 没有它，TSF 的 `OnTestKeyDown` 判定「无会话」，退格/Esc 根本不会转发进服务端，
/// 联想窗永远关不掉（2026-08-16 真机实测）。曾试过让宿主的 `_hasCandidates` 保持真，
/// 但那是异步回填的，赢不了同一拍的同步判定。
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

/// **受控对照**：没进联想态时**不该**平白多出一个组合。
///
/// 少了这条，上一条可能只是因为「所有上屏都带组合」——那会在每次普通上屏后
/// 都留一个悬空组合，是比联想失效严重得多的回归。
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

/// 字母键**照常放行**——它会用新编码替换掉占位组合，不需要我们收尾。
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
// 联想态 = **正常输入态，只是没有输入缓冲**
//
// 下面这组不是在测「我为联想写了什么」，而是在测「联想**没有**被做成特例」：
// 候选就住在 `state.candidates` 里，于是主输入路的既有能力天然全部适用。
// 每多一条通过的用例，就多一份「不需要为联想单独接线」的证据。
// ─────────────────────────────────────────────────────────────────────────────

/// 进入联想态并返回候选列表（够长才好验翻页/二三候选）。
fn enter_assoc(c: &Coordinator) -> Vec<String> {
    press(c, "q");
    c.handle_key_event(&key_event(0x20));
    let hits = assoc_texts(c);
    assert!(!hits.is_empty(), "前提：进了联想态");
    hits
}

/// ★ 二三候选键（`;` / `'`）在联想态照常选词。
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

/// ★ 上下移高亮在联想态照常工作，且空格上屏的是移动后的那条。
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

/// ★ 翻页键（PageDown）在联想态照常工作。
///
/// 候选不足一页时翻不动是正常的，故只断言「按键被消费掉而不是漏给宿主」——
/// 漏给宿主才是联想被做成特例的症状。
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

/// ★ 持续档：选中一条之后接着给下一轮（「我」→「我们」→ 以「我们」开头的词）。
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

/// **受控对照**：一次性档同样操作**不**续轮。
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

/// ★ 联想候选**不记词频**——它没有编码，记进去是一条永远查不到的空码行。
#[test]
fn assoc_commit_records_no_frequency() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    enter_assoc(&c);
    // 选中联想候选。没有 store 的 headless 下本就不会写，这里验的是**不 panic 且
    // 上屏正确**——真正的守门在 `commit_selected` 里那句 `if !from_assoc`。
    let act = c.handle_key_event(&key_event(0x31));
    assert!(
        matches!(
            act,
            KeyAction::InsertText { .. } | KeyAction::CommitThenDeferComposition { .. }
        ),
        "实得 {act:?}"
    );
}

/// ★★★ **联想态下按标点，不该顶屏联想候选。**
///
/// 真机复现（2026-08-16）：打「我」上屏 → 联想首条「我们」→ 按「。」→ 得到「我我们。」
///
/// 两层错：
///   ① 顶屏本身不该发生。顶码/顶屏的语义前提是「用户打了码、还没选词，按标点意味着
///      『就选高亮那个吧』」。联想态**没有码**——高亮那条是输入法猜的，不是用户在选。
///      用户按「。」的意图就是打个句号。
///   ② 就算顶屏，也拿错了文本：联想的显示文本是整词「我们」，而屏幕上已经有「我」，
///      真正该补的只有「们」。于是「我」+「我们」=「我我们」。
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

/// 回车：收窗 + 结束占位组合，**不**上屏高亮联想。
///
/// 回车是终结性动作（换行/发送），联想的高亮是输入法猜的，替用户选一个是越权。
///
/// 本条只管「不上屏」这一件事，**刻意接受两种收窗动作**：吃键还是把回车交还宿主由
/// `enter_cancels_only` 定，各自由 `assoc_enter_*` 两条钉。断言写死成某一个变体，
/// 会让这条在开关默认值翻转时误报成「上屏了联想」——那是它压根没在测的事。
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

/// ★ `space_commits = false` 时空格不选联想，只收窗并出空格。
///
/// 这一条曾在重构里**静默失效**——读它的那段代码被删掉了，配置还在但没有消费点。
/// 属本仓反复出现的「配置就位、消费点不可达 ⇒ 开关毫无反应」。
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

/// **受控对照**：默认（`space_commits = true`）时空格**确实**选中联想。
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

/// ★ 联想态按引导键进模式**不顶屏**联想候选。
///
/// 顶屏的语义前提是「用户打了码、还没选词，按这个键意味着『就选高亮那条吧』」。联想态
/// 没有码——高亮那条是输入法猜的，此刻按引导键的意图就是进模式。
///
/// 标点键那条路（`commit_highlight_then_char`）早有同款守卫，但进模式顶屏走的是另一条
/// （`take_committed_with_highlight`），判据必须自己带——它此前没有，联想候选会被顶上屏。
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
// 两个键**默认相反**（回车透传、退格吃键），理由见 `AssociationConfig` 的字段文档。
// 四条用例把两个开关的两种取值都钉死——只测一半会让「开关根本没接线」照样全绿。
// ─────────────────────────────────────────────────────────────────────────────

/// ★ 默认：联想态回车**交还宿主**（收窗 + 照常换行/发送），不必按两次。
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

/// ★ `enter_cancels_only = true`：回车只收窗，键被吃掉（要按第二次才生效）。
#[test]
fn assoc_enter_cancels_only_when_configured() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    // 两个开关**同时设成与各自默认相反**：这样「回车读了退格那份配置」这类接错线
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

/// ★ 默认：联想态退格**吃键**——与回车相反，透传会删掉刚上屏的字（不可逆）。
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

/// ★ `backspace_cancels_only = false`：退格连同收窗一起交还宿主。
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
// 退格与回车各有专门分支（上面四条），本节管的是**剩下那一大类**：在既有分支里落到
// `PassThrough` 的键。用户报的是「Del / Home / End 按了没反应」，但名单数不清——
// 左右/Home/End 一条臂、Del 一条臂、Insert 与 Tab 落兜底臂——故收口是按**动作**判的，
// 见 `handle_assoc::assoc_release_on_passthrough`。
//
// ⚠️⚠️ 这一族**必须走 `handle_key_event_policed`**（bridge 的真入口，server.rs 调的就是
// 它）。收口挂在那个唯一出口上，改走内层 `handle_key_event` 会如实拿到 `PassThrough`
// ——那正是修复前的症状，拿它写断言等于把 bug 钉成规范。

/// 联想态下会落到透传的键。刻意**不含** ↑↓/PageUp/PageDown：那四个在联想态是翻页与
/// 移高亮（`apply_session_action` 早就接住了），本就不该收窗。
/// ⛔ **Tab 刻意不在列**——审查时按 C++ 的吃键表（`session_select_or_page` 同样「有
/// session 就吃」）把它加进来过，实测拿到的是 `Consumed` 而非 `PassThrough`：Tab 是
/// **辅助码触发键**（`enter_aux_code`），在协调器里有自己的归属。
/// 用户看到的表象同样是「按了没反应」，但机制不同（键被干净吃掉，没有「吃了再吐」、
/// 不丢键），该不该让它在联想态做点别的是另一个问题，不归本收口管。
/// ★ 这正是「判据取动作、不取键名」的又一次兑现：按键名列表会把它错收进来。
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

/// ★★★ **用户报障**：联想窗弹出时「退格能删，Del / Home / End 却按了没反应」。
///
/// 根因不在这些键各自的分支里，而在联想态挂着的占位组合：它让 TSF 的
/// `_HasInputSession()` 为真 ⇒ `OnTestKeyDown` 把这批键（C++ 归 `HotkeyType::CursorKey`）
/// 全吃下转发；而协调器这边缓冲与已转换段皆空，一路落到 `PassThrough` ⇒ 「吃了再吐」
/// 翻转，不补发 `WM_KEYDOWN` 的宿主直接丢键。
///
/// 正解是收组合 + 重放按键，顺带收窗——这些键要么移动宿主光标、要么改动光标后的文档，
/// 联想赖以成立的上文已经断了，那批候选此刻就是错的。
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

/// ★ 反向守卫：**光按下修饰键不该收窗**。
///
/// Shift+字母的物理形态是「先 Shift keydown、再字母 keydown」。若收口不排除纯修饰键，
/// 第一下就把联想窗干掉了，第二下看到的已经是空闲态——修好的临英入口会被自己撞掉。
/// Windows 侧这类 keydown 压根不转发，故这条钉的是薄宿主与未来的接线。
#[test]
fn assoc_survives_bare_modifier_keydown() {
    let dir = data_dir();
    if !dict_ready(&dir) {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = coord("word", "wubi86_pinyin");
    let hits = enter_assoc(&c);
    // 值域必须写全（`handle_assoc::is_modifier_like_vk`）：0xA0-0xA3 四个纯修饰键与
    // CapsLock(0x14) 之外，还有左右 Alt(0xA4/0xA5) 与未拆左右的笼统
    // VK_SHIFT/VK_CONTROL/VK_MENU(0x10/0x11/0x12)。后五个在桌面 TSF 上到不了服务端
    // （C++ `isModifierKeyItself` 不转发），本条钉的是**薄宿主**——而薄宿主恰恰是最
    // 可能上报它们的那一类，守卫要是只覆盖桌面值域，就会在它唯一要保的场景下失效。
    // 修饰键自己按下时，那一位修饰位就是按着的——真机上 Shift keydown 必带 MOD_SHIFT。
    // 守卫按 vk 判，带不带位结论都一样；带上只是为了让用例复现的确实是它写的那个场景。
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

/// ★ Ctrl/Alt 组合**刻意也走改判**（`assoc_release_on_passthrough` 里那段星标）：
/// 它们在联想态同样落 `PassThrough`（`MOD_SHORTCUT` 那条臂，缓冲与已转换段皆空），
/// 同样被 TSF 的 `ctrl_alt_cleanup` 分支吃下转发 ⇒ 同一个「吃了再吐」。
///
/// 这是本次触及面最大的一格，故单独钉住：改判后 C++ 走 `_ReplayKeyToHost`，重放时物理
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

/// ★★★ **用户报障**：联想窗弹出时按 Shift+字母想打英文，字母却落进了中文码表缓冲。
///
/// 根因是 `try_activate_mode` 开头那句联想让位**让过头了**：它本意只为保住 `;`/`'` 的
/// 二三候选键（见 `select_keys_work_in_assoc`），却把同一函数里的临英 Shift+字母分支
/// 一起挡掉；即使不挡，那个分支的「无候选」门也会被联想候选拦住。
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

/// ★ 同一入口的另一档：`shift_behavior = "direct_commit"` 时直接上屏大写字母。
///
/// 两档都测，是因为它们在同一个 `if` 里分叉：只测一档时，另一档漏了收联想窗
/// （候选与临英候选住同一个 `state.candidates`）也照绿。
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
