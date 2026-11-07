//! 中英 / 中数间按需补空格（`input.spacing.*`）端到端测试。
//!
//! 设计见 `docs/design/auto-spacing-and-latin-smart-punct.md`。空格是**下一笔上屏的前缀**，
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

/// 中文语境里连打两个英文词（中 en en 中）：开了中英间空格就隔开，不必另开临英的词间空格。
#[test]
fn two_english_words_in_chinese_text_are_separated() {
    if !ready() {
        return;
    }
    let mut cfg = config(true, false);
    cfg.input.temp_english.commit_space = false;
    let h = Host::new(cfg);
    h.wubi("aaaa");
    assert_eq!(h.temp_english("Hello"), " Hello");
    assert_eq!(h.temp_english("World"), " World");
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
    // 开关关着（DLL 手上的开关慢了一拍还是吃了）：照样出字，只是不补——不能丢键。
    let h = Host::new(config(false, false));
    h.wubi("aaaa");
    match h.key(0x33, 0) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "3"),
        other => panic!("吃下来的数字必须出字，实际 {other:?}"),
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

/// 鼠标把光标点到别处（`selection_changed` 判成用户移动）：下一笔不再按旧的「上一笔」补。
#[test]
fn cursor_moved_by_mouse_forgets_previous_char() {
    if !ready() {
        return;
    }
    let h = Host::new(config(true, false));
    h.wubi("aaaa");
    // 超过自提交回声的宽限期，才会被判成用户移动光标。
    std::thread::sleep(std::time::Duration::from_millis(400));
    h.coord.handle_selection_changed(0);
    assert_eq!(h.temp_english("Python"), "Python");
}
