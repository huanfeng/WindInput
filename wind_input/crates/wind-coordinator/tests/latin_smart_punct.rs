//! 英文后智能标点（`input.punct.smart_after_latin` / `latin_smart_list`）端到端测试。
//!
//! 设计见 `docs/design/auto-spacing-and-latin-smart-punct.md` §4。「英文后」的两个来源：
//! - 顶屏：被顶出的那段文本（临英原文）的末字符；
//! - 空缓冲直接按标点：客户端输入序列记账（`seq_char`），由 [`Host`] 模拟。
//!
//! 主方案取五笔，英文从临英（Shift+字母）进。
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
const VK_OEM_PERIOD: u32 = 0xBE;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn ready() -> bool {
    let d = data_dir();
    d.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists()
        && d.join("schemas/english.schema.toml").exists()
        && d.join("schemas/english").is_dir()
}

fn config(on: bool) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "english".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.default.chinese_punct = true;
    cfg.input.temp_english.enabled = true;
    cfg.input.punct.smart_after_latin = on;
    cfg
}

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
        let text = match &act {
            KeyAction::InsertText { text, .. } | KeyAction::ReplaceBackward { text, .. } => {
                Some(text)
            }
            _ => None,
        };
        if let Some(c) = text.and_then(|t| t.chars().last()) {
            let mut buf = [0u16; 2];
            self.seq.set(c.encode_utf16(&mut buf)[0]);
        }
        act
    }

    fn passthrough(&self, c: char) {
        self.seq.set(c as u16);
    }

    /// Shift+首字母进临英、打完余下字母（不上屏）。
    fn type_temp_english(&self, word: &str) {
        let mut chars = word.chars();
        let first = chars.next().expect("至少一个字母");
        self.key((first.to_ascii_uppercase() as u32) & 0xFF, MOD_SHIFT);
        for c in chars {
            self.key((c.to_ascii_uppercase() as u32) & 0xFF, 0);
        }
    }

    fn wubi(&self, code: &str) {
        for c in code.chars() {
            self.key((c.to_ascii_uppercase() as u32) & 0xFF, 0);
        }
        self.key(VK_SPACE, 0);
    }
}

fn text(act: &KeyAction) -> String {
    match act {
        KeyAction::InsertText { text, .. } => text.clone(),
        other => panic!("应为 InsertText 上屏，实际: {other:?}"),
    }
}

/// 顶屏：临英原文 + 句号 ⇒ `Node.`；开关关着则仍是 `Node。`。
#[test]
fn top_commit_after_english_word_goes_half_width() {
    if !ready() {
        eprintln!("跳过：缺少五笔或英文词库");
        return;
    }
    let h = Host::new(config(true));
    h.type_temp_english("Node");
    assert_eq!(text(&h.key(VK_OEM_PERIOD, 0)), "Node.");

    let h = Host::new(config(false));
    h.type_temp_english("Node");
    assert_eq!(text(&h.key(VK_OEM_PERIOD, 0)), "Node。");
}

/// 列表外的标点照出中文：英文词后接中文逗号是中文句子的常态。
#[test]
fn comma_is_not_in_default_list() {
    if !ready() {
        return;
    }
    let h = Host::new(config(true));
    h.type_temp_english("Python");
    assert_eq!(text(&h.key(VK_OEM_COMMA, 0)), "Python，");
}

/// 空缓冲直接按标点：上一笔是英文词（客户端记账）⇒ 半角。
#[test]
fn bare_punct_after_committed_english_word() {
    if !ready() {
        return;
    }
    let h = Host::new(config(true));
    h.type_temp_english("a");
    h.key(VK_SPACE, 0); // 上屏 `A`
    assert_eq!(text(&h.key(VK_OEM_PERIOD, 0)), ".");
}

/// 上一笔是汉字 ⇒ 不受影响。
#[test]
fn bare_punct_after_cjk_stays_chinese() {
    if !ready() {
        return;
    }
    let h = Host::new(config(true));
    h.wubi("aaaa");
    assert_eq!(text(&h.key(VK_OEM_PERIOD, 0)), "。");
}

/// 用户自己按了空格（透传）⇒ 英文语境结束，按默认中文标点。
#[test]
fn user_typed_space_ends_english_context() {
    if !ready() {
        return;
    }
    let h = Host::new(config(true));
    h.type_temp_english("a");
    h.key(VK_SPACE, 0);
    h.passthrough(' ');
    assert_eq!(text(&h.key(VK_OEM_PERIOD, 0)), "。");
}

/// 智能符号开着：英文后与数字后一样反向——press1 出半角，同键再按换回中文。
#[test]
fn smart_symbol_reverses_after_english() {
    if !ready() {
        return;
    }
    let mut cfg = config(true);
    cfg.input.symbol.smart_mode = true;
    let h = Host::new(cfg);
    h.type_temp_english("a");
    h.key(VK_SPACE, 0);
    assert_eq!(text(&h.key(VK_OEM_PERIOD, 0)), ".");
    match h.key(VK_OEM_PERIOD, 0) {
        KeyAction::ReplaceBackward { count, text } => {
            assert_eq!((count, text.as_str()), (1, "。"));
        }
        other => panic!("同键再按应换回中文句号，实际 {other:?}"),
    }
}
