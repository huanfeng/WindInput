//! 中英 / 中数切换处补空格（`input.spacing.*`）端到端测试。
//!
//! 设计见 `docs/design/auto-spacing-and-latin-smart-punct.md`。空格是**切换后那一笔上屏的前缀**，
//! 「上一笔末字符」来自客户端的输入序列记账（`seq_char`），这里由 [`Host`] 模拟。按键一律经
//! `handle_key_event_policed`——补空格在那个出口。
//!
//! 主方案取五笔：中文上屏一条键路就到，不依赖拼音整句的排序。
//!
//! ⚠️ 词典缺失时整族静默跳过（判据是**耗时 0.00s**，不是通过条数）。

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};

const VK_SPACE: u32 = 0x20;
const VK_OEM_COMMA: u32 = 0xBC;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn ready() -> bool {
    let d = data_dir();
    d.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists()
        && d.join("schemas/english.schema.toml").exists()
        && d.join("schemas/english").is_dir()
}

fn config(cjk_latin: bool, cjk_digit: bool) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "english".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.temp_english.enabled = true;
    cfg.input.spacing.cjk_latin = cjk_latin;
    cfg.input.spacing.cjk_digit = cjk_digit;
    cfg
}

/// 模拟客户端：只做 DLL 那份输入序列记账（`seq_char` = 上一笔进文档的末字符）。
struct Host {
    coord: Arc<Coordinator>,
    seq: Cell<u16>,
}

impl Host {
    fn new(cfg: Config) -> Self {
        Self {
            coord: Coordinator::new_headless(cfg, Some(&data_dir())),
            seq: Cell::new(0),
        }
    }

    fn key(&self, vk: u32, modifiers: u32) -> KeyAction {
        let act = self.coord.handle_key_event_policed(&KeyEventData {
            key_code: vk,
            scan_code: 0,
            modifiers,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
            seq_char: self.seq.get(),
        });
        if let KeyAction::InsertText { text, .. } = &act
            && let Some(c) = text.chars().last()
        {
            let mut buf = [0u16; 2];
            self.seq.set(c.encode_utf16(&mut buf)[0]);
        }
        act
    }

    /// 透传进宿主的一个字符（数字等）：客户端记账，服务端看不见。
    fn passthrough(&self, c: char) {
        self.seq.set(c as u16);
    }

    fn letter(&self, c: char) -> KeyAction {
        self.key((c.to_ascii_uppercase() as u32) & 0xFF, 0)
    }

    /// 打一段五笔码再按空格，返回这次上屏的文本（满码自动上屏时取那一键的上屏）。
    fn wubi(&self, code: &str) -> String {
        let mut last = None;
        for c in code.chars() {
            if let KeyAction::InsertText { text, .. } = self.letter(c) {
                last = Some(text);
            }
        }
        if last.is_none()
            && let KeyAction::InsertText { text, .. } = self.key(VK_SPACE, 0)
        {
            last = Some(text);
        }
        last.expect("五笔应有上屏")
    }

    /// Shift+首字母进临英、打完余下字母，空格上屏原文。
    fn temp_english(&self, word: &str) -> String {
        let mut chars = word.chars();
        let first = chars.next().expect("至少一个字母");
        self.key((first.to_ascii_uppercase() as u32) & 0xFF, MOD_SHIFT);
        for c in chars {
            self.letter(c);
        }
        match self.key(VK_SPACE, 0) {
            KeyAction::InsertText { text, .. } => text,
            other => panic!("临英空格应上屏，实际 {other:?}"),
        }
    }
}

/// `中 en 中`：汉字后的英文词前补、英文词后的汉字前补。
#[test]
fn spaces_between_cjk_and_latin_both_ways() {
    if !ready() {
        eprintln!("跳过：缺少五笔或英文词库");
        return;
    }
    let h = Host::new(config(true, false));
    let zh = h.wubi("aaaa");
    assert!(!zh.starts_with(' '), "开头前一字符不可知，不补: {zh:?}");

    let en = h.temp_english("Python");
    assert_eq!(en, " Python", "汉字后的英文词前补空格");

    let zh2 = h.wubi("aaaa");
    assert_eq!(
        zh2,
        format!(" {}", zh.trim_start()),
        "英文词后的汉字前补空格"
    );
}

#[test]
fn no_space_when_switch_off() {
    if !ready() {
        return;
    }
    let h = Host::new(config(false, false));
    h.wubi("aaaa");
    assert_eq!(h.temp_english("Python"), "Python");
}

/// 标点永不前补：英文词后直接按逗号顶屏，得到 `Python，`/`Python,` 而不是 `Python ,`。
#[test]
fn punctuation_never_gets_a_leading_space() {
    if !ready() {
        return;
    }
    let h = Host::new(config(true, false));
    h.wubi("aaaa");
    h.temp_english("Python");
    let act = h.key(VK_OEM_COMMA, 0);
    if let KeyAction::InsertText { text, .. } = &act {
        assert!(!text.starts_with(' '), "标点前不补空格: {text:?}");
    }
}

/// 数字有自己的开关（默认关）：开了才在「数字 → 汉字」之间补。数字是透传进宿主的，
/// 服务端只能经客户端记账知道它。
#[test]
fn digit_rule_has_its_own_switch() {
    if !ready() {
        return;
    }
    let h = Host::new(config(true, false));
    h.passthrough('3');
    assert!(!h.wubi("aaaa").starts_with(' '), "cjk_digit 关着不补");

    let h = Host::new(config(false, true));
    h.passthrough('3');
    assert!(h.wubi("aaaa").starts_with(' '), "cjk_digit 开着补");
}

/// 用户自己打的空格（透传）之后不再补。
#[test]
fn user_typed_space_suppresses_auto_space() {
    if !ready() {
        return;
    }
    let h = Host::new(config(true, false));
    h.wubi("aaaa");
    h.passthrough(' ');
    assert_eq!(h.temp_english("Python"), "Python");
}

/// 客户端报不出光标前是什么时，中英交界**不**退回服务端记录（宁可少补）：焦点切换、
/// 鼠标挪过光标之后，上一笔是什么与这里无关。
#[test]
fn unknown_previous_char_gets_no_cjk_space() {
    if !ready() {
        return;
    }
    let h = Host::new(config(true, false));
    h.wubi("aaaa");
    h.seq.set(0);
    assert_eq!(h.temp_english("Python"), "Python");
}

/// 开了临英的 `commit_space`：英文词上屏就带尾随空格（看得见、可预期），切到中文时前一字符
/// 已是空格，不再重复补——得到 `中 Hello World 中`。
#[test]
fn trailing_word_space_is_immediate_and_not_doubled() {
    if !ready() {
        return;
    }
    let mut cfg = config(true, false);
    cfg.input.temp_english.commit_space = true;
    let h = Host::new(cfg);
    let zh = h.wubi("aaaa");
    assert_eq!(h.temp_english("Hello"), " Hello ");
    assert_eq!(h.temp_english("World"), "World ");
    assert_eq!(h.wubi("aaaa"), zh.trim_start(), "尾随空格之后不再前补");
}

/// 没开 `commit_space`：连续的英文之间不管，切回中文时才补。
#[test]
fn consecutive_english_left_alone_without_commit_space() {
    if !ready() {
        return;
    }
    let h = Host::new(config(true, false));
    let zh = h.wubi("aaaa");
    assert_eq!(h.temp_english("Hello"), " Hello");
    assert_eq!(h.temp_english("World"), "World");
    assert_eq!(h.wubi("aaaa"), format!(" {}", zh.trim_start()));
}

/// 汉字后打数字：DLL 把这个数字吃下交过来（`seq_char` 是汉字），服务端恒出字；开着中数间
/// 空格就在前面补一个。
#[test]
fn digit_after_cjk_is_output_and_spaced() {
    if !ready() {
        return;
    }
    let h = Host::new(config(false, true));
    h.wubi("aaaa");
    match h.key(0x33, 0) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, " 3"),
        other => panic!("汉字后的数字应由服务端出字，实际 {other:?}"),
    }
    // 开关关着：
    // - Windows：到得了这里说明 DLL 已经吃下（开关慢了一拍），照样出字、只是不补——不能丢键；
    // - macOS / Linux：客户端每个键都发过来，必须照旧透传，交给宿主（数字当快捷键）。
    let h = Host::new(config(false, false));
    h.wubi("aaaa");
    let act = h.key(0x33, 0);
    if cfg!(windows) {
        assert!(
            matches!(&act, KeyAction::InsertText { text, .. } if text == "3"),
            "吃下来的数字必须出字，实际 {act:?}"
        );
    } else {
        assert!(
            matches!(act, KeyAction::PassThrough),
            "开关关着，汉字后的数字应透传，实际 {act:?}"
        );
    }
}

/// 小键盘数字同主键盘：两种小键盘档位（direct 出厂 / follow_main）汉字后都出字补空格；
/// 开关关着时 macOS / Linux 照旧透传。运算符不在其列：开关开着关着产出一致。
#[test]
fn numpad_digit_after_cjk_is_output_and_spaced() {
    if !ready() {
        return;
    }
    const VK_NUMPAD3: u32 = 0x63;
    const VK_ADD: u32 = 0x6B;
    for behavior in ["direct", "follow_main"] {
        let mut cfg = config(false, true);
        cfg.input.numpad_behavior = behavior.into();
        let h = Host::new(cfg.clone());
        h.wubi("aaaa");
        match h.key(VK_NUMPAD3, 0) {
            KeyAction::InsertText { text, .. } => assert_eq!(text, " 3", "{behavior}"),
            other => panic!("{behavior}: 汉字后的小键盘数字应出字，实际 {other:?}"),
        }
        // 运算符与开关无关：开着时的产出必须和关着时一模一样（不补空格，也不改吃 / 透传）。
        let op_with = |on: bool| {
            let mut c = cfg.clone();
            c.input.spacing.cjk_digit = on;
            let h = Host::new(c);
            h.wubi("aaaa");
            format!("{:?}", h.key(VK_ADD, 0))
        };
        assert_eq!(
            op_with(true),
            op_with(false),
            "{behavior}: 运算符不受开关影响"
        );
        if !cfg!(windows) {
            cfg.input.spacing.cjk_digit = false;
            let h = Host::new(cfg);
            h.wubi("aaaa");
            assert!(
                matches!(h.key(VK_NUMPAD3, 0), KeyAction::PassThrough),
                "{behavior}: 开关关着应透传"
            );
        }
    }
}

/// 上一笔不是汉字时，空闲数字照旧透传（DLL 也不会交过来）。
#[test]
fn digit_after_latin_still_passes_through() {
    if !ready() {
        return;
    }
    let h = Host::new(config(true, true));
    h.temp_english("Hello");
    assert!(matches!(h.key(0x33, 0), KeyAction::PassThrough));
}

/// 透传的数字进了宿主，宿主照样发「选区变了」（时间上离上一次自己上屏很远，服务端会判成
/// 「用户移动光标」）。这不能作废记下的上一笔：数字后的汉字照样要补。鼠标挪光标由客户端
/// 自己识别（按着鼠标、且这次编辑没改文本），报 `SEQ_CHAR_UNKNOWN`。
#[test]
fn selection_echo_after_passthrough_digit_keeps_previous_char() {
    if !ready() {
        return;
    }
    let h = Host::new(config(false, true));
    h.passthrough('3');
    std::thread::sleep(std::time::Duration::from_millis(400));
    h.coord.handle_selection_changed(0);
    assert!(h.wubi("aaaa").starts_with(' '), "数字后的汉字前补空格");
}

/// 客户端报「鼠标挪过光标」（`SEQ_CHAR_UNKNOWN`）：不补。
#[test]
fn client_reported_unknown_gets_no_space() {
    if !ready() {
        return;
    }
    let h = Host::new(config(true, false));
    h.wubi("aaaa");
    h.seq.set(wind_ipc::protocol::SEQ_CHAR_UNKNOWN);
    assert_eq!(h.temp_english("Python"), "Python");
}

/// 五笔主路径里的英文候选（`english_merge`）：跟随临英的 `commit_space`，与临英、快捷输入一致。
#[test]
fn english_candidate_in_codetable_follows_temp_english_switch() {
    if !ready() {
        return;
    }
    let run = |commit_space: bool| -> String {
        let mut cfg = config(true, false);
        cfg.schema.codetable.english_merge.enable = true;
        cfg.input.temp_english.commit_space = commit_space;
        let h = Host::new(cfg);
        h.wubi("aaaa");
        for c in "git".chars() {
            h.letter(c);
        }
        match h.key(VK_SPACE, 0) {
            KeyAction::InsertText { text, .. } => text,
            other => panic!("空格应上屏英文候选 git，实际 {other:?}"),
        }
    };
    assert_eq!(run(true), " git ", "开着：上屏即补尾随空格");
    assert_eq!(run(false), " git", "关着：只有切换处的前缀");
}

/// 英文词后直接按标点顶屏（临英、五笔 / 拼音的英文候选）：标点紧贴单词，不带尾随空格——
/// 开着 `commit_space` 也一样（它只在选词上屏时补）。标点形态跟中文标点状态，开了英文后智能
/// 标点时列表内的 `.` 出英文。
#[test]
fn punct_top_commit_after_english_word_has_no_space() {
    if !ready() {
        return;
    }
    const VK_OEM_PERIOD: u32 = 0xBE;
    let text = |a: KeyAction| match a {
        KeyAction::InsertText { text, .. } => text,
        other => panic!("标点应顶屏上屏，实际 {other:?}"),
    };
    for commit_space in [false, true] {
        for smart in [false, true] {
            let mut cfg = config(false, false);
            cfg.input.temp_english.commit_space = commit_space;
            cfg.input.punct.smart_after_latin = smart;
            cfg.schema.codetable.english_merge.enable = true;
            cfg.schema.codetable.punct_commit = true; // 码表标点顶屏出厂关
            let mut py = cfg.clone();
            py.schema.available.push("pinyin".into());
            py.schema.active = "pinyin".into();
            py.schema.pinyin.english_merge.enable = true;
            let period = if smart { "." } else { "。" };
            for (vk, punct) in [(VK_OEM_COMMA, "，"), (VK_OEM_PERIOD, period)] {
                let tag = format!("commit_space={commit_space} smart={smart} {punct}");
                let h = Host::new(cfg.clone());
                h.key(('H' as u32) & 0xFF, MOD_SHIFT);
                for c in "ello".chars() {
                    h.letter(c);
                }
                assert_eq!(text(h.key(vk, 0)), format!("Hello{punct}"), "临英 {tag}");
                let h = Host::new(cfg.clone());
                for c in "git".chars() {
                    h.letter(c);
                }
                assert_eq!(text(h.key(vk, 0)), format!("git{punct}"), "五笔 {tag}");
                let h = Host::new(py.clone());
                for c in "hello".chars() {
                    h.letter(c);
                }
                assert_eq!(text(h.key(vk, 0)), format!("hello{punct}"), "拼音 {tag}");
            }
        }
    }
}
