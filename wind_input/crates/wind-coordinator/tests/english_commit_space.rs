//! 英文方案「词间自动补空格」（`schema.english.commit_space`）端到端测试。
//!
//! 该配置项曾经**四层就位、零处生效**：结构体 / schema 注册 / config.toml / 设置页 GUI
//! 全有，消费点却只接在 `commit_candidate` 上——而英文引擎恒 `should_commit = false`、
//! 自动上屏又只认码表来源，那三个调用点在英文方案下**一个都走不到**。开关打开毫无反应。
//!
//! 所以本文件的断言重心不是「补了空格」，而是**逐条通路各自补没补**：真正的英文上屏出口
//! 是 `commit_selected`（六类触发汇于一处）、「空格上屏原码」与「回车上屏原码」。少测一条，
//! 下一次重构就可能把某条通路悄悄改回不生效。
//!
//! # 按需补（2026-11-06 起）
//!
//! 空格不再紧跟在上屏的词**后面**，而是补在**下一个词前面**（设计见
//! `docs/design/auto-spacing-and-latin-smart-punct.md`）：每条用例都断言两件事——
//! 上屏文本本身不带尾随空格；紧接着的下一个英文词带前缀空格。「上一笔末字符」来自客户端的
//! 输入序列记账（`seq_char`），这里由 [`Host`] 模拟：它记住上一笔上屏的末字符，随下一键带回。
//! 按键一律经 `handle_key_event_policed`——补空格在那个出口，直调 `handle_key_event` 看不到。
//!
//! ⚠️ 词典缺失时整族静默跳过（判据是**耗时 0.00s**，不是通过条数）——见
//! `build_dev/data` 相关记录。worktree 中需自备 `build_dev` 链接。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{CommitRequestData, KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_config::config::RawCandidateMode;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, EVENT_KEY_UP, MOD_SHIFT};

const VK_SPACE: u32 = 0x20;
const VK_RETURN: u32 = 0x0D;
const VK_2: u32 = 0x32;
const VK_3: u32 = 0x33;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

/// 英文方案就绪判据：方案定义与**词库**都要在（词库由构建 assemble 注入，不进 git）。
fn has_english_schema() -> bool {
    let d = data_dir();
    d.join("schemas/english.schema.toml").exists() && d.join("schemas/english").is_dir()
}

fn key_event(key_code: u32, event_type: u8) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

fn english_config(commit_space: bool) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["english".into(), "wubi86".into()];
    cfg.schema.active = "english".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.english.commit_space = commit_space;
    cfg
}

/// 模拟客户端：只做 DLL 那份输入序列记账（`seq_char` = 上一笔上屏文本的末字符）。
struct Host {
    coord: Arc<Coordinator>,
    seq: std::cell::Cell<u16>,
}

impl Host {
    fn new(cfg: Config) -> Self {
        Self {
            coord: Coordinator::new_headless(cfg, Some(&data_dir())),
            seq: std::cell::Cell::new(0),
        }
    }

    fn note(&self, action: &KeyAction) {
        if let KeyAction::InsertText { text, .. } = action
            && let Some(c) = text.chars().last()
        {
            let mut buf = [0u16; 2];
            self.seq.set(c.encode_utf16(&mut buf)[0]);
        }
    }

    fn key_mods(&self, vk: u32, modifiers: u32, event_type: u8) -> KeyAction {
        let mut ev = key_event(vk, event_type);
        ev.modifiers = modifiers;
        ev.seq_char = self.seq.get();
        let act = self.coord.handle_key_event_policed(&ev);
        self.note(&act);
        act
    }

    fn key(&self, vk: u32) -> KeyAction {
        self.key_mods(vk, 0, EVENT_KEY_DOWN)
    }

    fn letter(&self, c: char) -> KeyAction {
        self.key((c.to_ascii_uppercase() as u32) & 0xFF)
    }

    /// Shift+字母：英文方案下走主路字母臂，大写落进影子串 `input_buffer_cased`，
    /// `input_buffer` 保持小写（见 `handle_lifecycle` 临英门禁处的 ★ 英文方案下不进临英）。
    fn letter_shift(&self, c: char) -> KeyAction {
        self.key_mods(
            (c.to_ascii_uppercase() as u32) & 0xFF,
            MOD_SHIFT,
            EVENT_KEY_DOWN,
        )
    }

    fn type_word(&self, s: &str) {
        for c in s.chars() {
            self.letter(c);
        }
    }

    fn top(&self) -> String {
        self.coord
            .debug_page_texts()
            .first()
            .cloned()
            .expect("应有候选")
    }

    /// 先上屏一个词垫底（空格选首选），返回它的上屏文本。
    fn commit_first_word(&self) -> String {
        self.type_word("hel");
        commit_text(&self.key(VK_SPACE))
    }
}

fn commit_text(action: &KeyAction) -> String {
    match action {
        KeyAction::InsertText { text, .. } => text.clone(),
        other => panic!("应为 InsertText 上屏，实际: {other:?}"),
    }
}

/// 空格选中首选 → 上屏首选本身（不带尾随空格）；下一个词前面补一个空格。
///
/// 断言的是**相对关系**（首选文本前一个空格）而非硬编码 `" hello"`：词库内容与排序会随
/// 版本变，绑死具体单词只会让测试在无关改动上误报。
#[test]
fn space_select_spaces_before_next_word() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let h = Host::new(english_config(true));
    assert_eq!(h.coord.active_schema_id(), "english");

    let first = h.commit_first_word();
    assert!(!first.ends_with(' '), "不再立即补尾随空格: {first:?}");

    h.type_word("hel");
    let top = h.top();
    let text = commit_text(&h.key(VK_SPACE));
    assert_eq!(text, format!(" {top}"), "第二个词前面应补一个空格");
}

/// 首字母大写时**选中首选**上屏，同样要补空格（t122）。
///
/// 它落在 `english_appends_space` 上（有候选 → 选中上屏）。此刻的首选是
/// `english_head_candidates` 用「所打原码」直接构造的原文候选（`source` 为 `None`），
/// 于是第一分支不命中；第二分支比 `text == input`，`input` 若传成全小写的
/// `state.input_buffer` 就永不相等 —— 两头落空。
#[test]
fn capitalized_word_gets_space_before_it() {
    if !has_english_schema() {
        return;
    }
    let h = Host::new(english_config(true));
    h.commit_first_word();

    h.letter_shift('h');
    h.type_word("el");
    let top = h.top();
    assert!(
        top.starts_with('H'),
        "前提：首选应已按输入投影成首字母大写，实际: {top}"
    );
    let text = commit_text(&h.key(VK_SPACE));
    assert_eq!(text, format!(" {top}"), "首字母大写的词前面也应补空格");
}

/// 头部候选里的大小写变形（GH#152）：选它上屏也算「一个英文词」。
#[test]
fn case_variant_select_gets_space_before_it() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = english_config(true);
    cfg.schema.english.case_variants = true;
    let h = Host::new(cfg);
    h.commit_first_word();

    h.type_word("qzxv");
    let page = h.coord.debug_page_texts();
    assert_eq!(
        page.get(..3),
        Some(&["qzxv".to_string(), "Qzxv".to_string(), "QZXV".to_string()][..]),
        "前提：头部应为原文 + 两条变形，实际 {page:?}"
    );
    assert_eq!(commit_text(&h.key(VK_2)), " Qzxv");

    h.type_word("qzxv");
    assert_eq!(commit_text(&h.key(VK_3)), " QZXV");
}

/// CapsLock 全大写档：整列套形、含头部候选，同样补。
#[test]
fn capslock_upper_tier_gets_space_before_it() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = english_config(true);
    cfg.input.english_case_cycle_key = "capslock".into();
    let h = Host::new(cfg);
    h.commit_first_word();

    h.type_word("qzxv");
    assert!(h.coord.debug_cycle_english_case(), "前提：应切到全大写档");
    assert_eq!(h.top(), "QZXV", "前提：全大写档首选应是套形后的原文");
    assert_eq!(commit_text(&h.key(VK_SPACE)), " QZXV");
}

#[test]
fn no_space_when_disabled() {
    if !has_english_schema() {
        return;
    }
    let h = Host::new(english_config(false));
    h.commit_first_word();

    h.type_word("hel");
    let top = h.top();
    assert_eq!(commit_text(&h.key(VK_SPACE)), top, "开关关闭时不得补空格");
}

#[test]
fn number_key_select_gets_space_before_it() {
    if !has_english_schema() {
        return;
    }
    let h = Host::new(english_config(true));
    h.commit_first_word();

    h.type_word("hel");
    let page = h.coord.debug_page_texts();
    assert!(
        page.len() >= 2,
        "需至少两个候选才能测数字键 2，实际 {page:?}"
    );
    let second = page[1].clone();
    assert_eq!(commit_text(&h.key(VK_2)), format!(" {second}"));
}

/// 鼠标点选不经按键出口，补空格在 `select_candidate_at` 推送之前。
#[test]
fn mouse_select_gets_space_before_it() {
    if !has_english_schema() {
        return;
    }
    let h = Host::new(english_config(true));
    h.commit_first_word();

    h.type_word("hel");
    let top = h.top();
    let act = h
        .coord
        .debug_mouse_select(0)
        .expect("鼠标点选应返回主输入路 action");
    assert_eq!(commit_text(&act), format!(" {top}"));
}

/// 「空格上屏原码」（词库里没有的词）：无候选可依，按方案口径。
#[test]
fn raw_code_space_commit_gets_space_before_it() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = english_config(true);
    cfg.schema.english.raw_candidate = RawCandidateMode::Off;
    let h = Host::new(cfg);
    h.commit_first_word();

    // 前提：该串在词库中无候选。若日后词库收录了它，前提失效 → 显式失败而非静默变成
    // 「测了另一条通路」。
    let nonsense = "qwxzjv";
    h.type_word(nonsense);
    assert_eq!(
        h.coord.debug_candidate_count(),
        0,
        "测试前提失效：{nonsense} 现在有候选了，请换一个无候选的串"
    );
    assert_eq!(commit_text(&h.key(VK_SPACE)), format!(" {nonsense}"));
}

/// 同上、首字母大写输入（t122 的原码分支）。
#[test]
fn raw_code_space_commit_capitalized_gets_space_before_it() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = english_config(true);
    cfg.schema.english.raw_candidate = RawCandidateMode::Off;
    let h = Host::new(cfg);
    h.commit_first_word();

    let nonsense = "qwxzjv";
    h.letter_shift('q');
    h.type_word(&nonsense[1..]);
    assert_eq!(
        h.coord.debug_candidate_count(),
        0,
        "测试前提失效：{nonsense} 现在有候选了，请换一个无候选的串"
    );
    assert_eq!(commit_text(&h.key(VK_SPACE)), " Qwxzjv");
}

/// 回车上屏原码：旧语义「回车不补尾随空格」在按需补下变成「回车上屏的词前面照样补」——
/// 否则「hello（空格）world（回车）」会从 `hello world` 退化成 `helloworld`。
#[test]
fn raw_code_enter_commit_gets_space_before_it() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = english_config(true);
    cfg.schema.english.raw_candidate = RawCandidateMode::Off;
    let h = Host::new(cfg);
    h.commit_first_word();

    let nonsense = "qwxzjv";
    h.type_word(nonsense);
    assert_eq!(
        h.coord.debug_candidate_count(),
        0,
        "测试前提失效：{nonsense} 现在有候选了"
    );
    assert_eq!(commit_text(&h.key(VK_RETURN)), format!(" {nonsense}"));
}

/// 客户端报不出光标前是什么（旧版 DLL 不带 `seq_char`、焦点刚切过来）：英文词间空格退回
/// 服务端自己记的上一笔。立即补尾随空格的旧实现是无条件的，新 core 配旧 DLL 时不能整个失效。
#[test]
fn legacy_client_without_seq_char_keeps_word_space() {
    if !has_english_schema() {
        return;
    }
    let h = Host::new(english_config(true));
    h.commit_first_word();
    h.seq.set(0);

    h.type_word("hel");
    let top = h.top();
    assert_eq!(commit_text(&h.key(VK_SPACE)), format!(" {top}"));
}

/// 客户端在记账、但不知道光标前是什么（回车换行、方向键之后，报 `SEQ_CHAR_UNKNOWN`）：
/// 不退回服务端记录——否则行首的第一个词前面会多一个空格。
#[test]
fn known_unknown_previous_char_gets_no_space() {
    if !has_english_schema() {
        return;
    }
    let h = Host::new(english_config(true));
    h.commit_first_word();
    h.seq.set(wind_ipc::protocol::SEQ_CHAR_UNKNOWN);

    h.type_word("hel");
    let top = h.top();
    assert_eq!(commit_text(&h.key(VK_SPACE)), top);
}

#[test]
fn non_english_schema_never_spaces_words() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = english_config(true);
    cfg.schema.active = "wubi86".into(); // 开关照开，只换方案
    let h = Host::new(cfg);
    assert_eq!(h.coord.active_schema_id(), "wubi86");

    h.type_word("aaaa");
    h.key(VK_SPACE);
    h.type_word("aaaa");
    let top = h.top();
    assert_eq!(
        commit_text(&h.key(VK_SPACE)),
        top,
        "非英文方案下即便开关为真也不得补空格"
    );
}

/// IPC barrier 通路（`handle_commit_request`，当前 C++ 侧未接线）与按键路径同口径。
#[test]
fn commit_request_space_gets_space_before_it() {
    if !has_english_schema() {
        return;
    }
    let h = Host::new(english_config(true));
    h.commit_first_word();
    h.type_word("hel");
    let top = h.top();

    let res = h
        .coord
        .handle_commit_request(&CommitRequestData {
            barrier_seq: 1,
            trigger_key: VK_SPACE as u16,
            modifiers: 0,
            input_buffer: "hel".into(),
        })
        .expect("有输入缓冲时应返回上屏结果");
    assert_eq!(res.text, format!(" {top}"));
}

#[test]
fn commit_request_enter_gets_space_before_it() {
    if !has_english_schema() {
        return;
    }
    let h = Host::new(english_config(true));
    h.commit_first_word();
    let nonsense = "qwxzjv";
    h.type_word(nonsense);

    let res = h
        .coord
        .handle_commit_request(&CommitRequestData {
            barrier_seq: 1,
            trigger_key: VK_RETURN as u16,
            modifiers: 0,
            input_buffer: nonsense.into(),
        })
        .expect("有输入缓冲时应返回上屏结果");
    assert_eq!(res.text, format!(" {nonsense}"));
}

/// 前缀空格不得混进词频记账键（`hello ` / ` hello` 都是读端永远查不中的孤儿键）。
#[test]
fn space_does_not_pollute_freq_key() {
    if !has_english_schema() {
        return;
    }
    let path = std::env::temp_dir().join("wind_english_commit_space_freq.redb");
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(wind_store::Store::open(&path).expect("建 store 失败"));

    let mut cfg = english_config(true);
    cfg.schema.english.frequency.enabled = true;
    cfg.schema.english.frequency.strategy = "top".into(); // 一次到顶，位次变化最好判读
    let h = Host {
        coord: Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store),
        seq: std::cell::Cell::new(0),
    };
    h.commit_first_word();

    h.type_word("hel");
    let page = h.coord.debug_page_texts();
    assert!(page.len() >= 3, "需原文 + 至少两个词库候选，实际 {page:?}");
    // page[0] 恒是所打原文（raw_candidate 默认开），词库段从 1 开始。
    let second = page[2].clone();
    assert_eq!(commit_text(&h.key(VK_3)), format!(" {second}"));

    // 再打一次同样的码：记账键若干净，该词应升到**词库段**首位。
    //
    // ⚠️ 断言落在 `after[1]` 而不是 `after[0]`——首位恒是原文，调频只在词库段内部生效
    // （原文与变形被 `split_off(dict_start)` 挡在重排之外，见设计文档 §5.2）。
    h.type_word("hel");
    let after = h.coord.debug_page_texts();
    assert_eq!(
        after.first().map(String::as_str),
        Some("hel"),
        "首位恒是所打原文"
    );
    assert_eq!(
        after.get(1),
        Some(&second),
        "选过的词应因调频升到词库段首位；纹丝不动说明记账键被空格污染成了孤儿键。实际候选: {after:?}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn keyup_does_not_emit_extra_space() {
    if !has_english_schema() {
        return;
    }
    let h = Host::new(english_config(true));
    h.commit_first_word();

    h.type_word("hel");
    h.key(VK_SPACE);
    let up = h.key_mods(VK_SPACE, 0, EVENT_KEY_UP);
    assert!(
        !matches!(up, KeyAction::InsertText { .. }),
        "空格 keyup 不应再次上屏，实际: {up:?}"
    );
}
