//! 按键翻译与英文自动空格（`press` 接口）。
//!
//! 这些语义原先写在 Android 宿主的 Kotlin 里（`RustSession.toVk`、`Punct.US_LAYOUT`、
//! `WindInputImeService.engineKey` 等），随第二个平台（iOS）出现就得再抄一份。收进核心之后
//! 宿主只剩「把按下的键报上来、执行返回的编辑指令」。**行为逐条照搬，不做改进。**

use std::sync::atomic::Ordering;

use wind_host::{EditOp, KeyOutcome};
use wind_keys::keymap;

use crate::MobileCore;

/// 按键来源。只在一处有区别：数字键。触摸键盘打数字就是要数字，物理键盘的数字照电脑习惯选候选。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Touch,
    Physical,
}

/// 宿主报上来的按键（尚未翻译成 VK）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressKey {
    /// 字符键。`shifted` = 宿主处于 Shift 态；`ch` 是未 Shift 的键面字符（字母小写）
    Char {
        ch: char,
        shifted: bool,
    },
    Space,
    Backspace,
    Enter,
}

/// 英文词后补的空格，遇到这些句末标点时挪到标点后面（`word ` + `.` → `word. `）
const AUTO_SPACE_PUNCT: [char; 6] = ['.', ',', '?', '!', ';', ':'];

/// 美式键盘：字符 → (VK, 需要 Shift)。与 Android `Punct.US_LAYOUT` 逐项一致。
pub(crate) fn us_layout_vk(c: char) -> Option<(u32, bool)> {
    match c {
        '0'..='9' => Some((0x30 + (c as u32 - '0' as u32), false)),
        ')' => Some((0x30, true)),
        '!' => Some((0x31, true)),
        '@' => Some((0x32, true)),
        '#' => Some((0x33, true)),
        '$' => Some((0x34, true)),
        '%' => Some((0x35, true)),
        '^' => Some((0x36, true)),
        '&' => Some((0x37, true)),
        '*' => Some((0x38, true)),
        '(' => Some((0x39, true)),
        ';' => Some((0xBA, false)),
        ':' => Some((0xBA, true)),
        '=' => Some((0xBB, false)),
        '+' => Some((0xBB, true)),
        ',' => Some((0xBC, false)),
        '<' => Some((0xBC, true)),
        '-' => Some((0xBD, false)),
        '_' => Some((0xBD, true)),
        '.' => Some((0xBE, false)),
        '>' => Some((0xBE, true)),
        '/' => Some((0xBF, false)),
        '?' => Some((0xBF, true)),
        '`' => Some((0xC0, false)),
        '~' => Some((0xC0, true)),
        '[' => Some((0xDB, false)),
        '{' => Some((0xDB, true)),
        '\\' => Some((0xDC, false)),
        '|' => Some((0xDC, true)),
        ']' => Some((0xDD, false)),
        '}' => Some((0xDD, true)),
        '\'' => Some((0xDE, false)),
        '"' => Some((0xDE, true)),
        _ => None,
    }
}

/// 压制某键的引导键身份，析构时清除（任何返回路径都不会漏清）
struct TriggerSuppress<'a>(&'a wind_coordinator::Coordinator);

impl<'a> TriggerSuppress<'a> {
    fn new(coord: &'a wind_coordinator::Coordinator, vk: u32) -> Self {
        coord.set_trigger_suppressed(vk);
        Self(coord)
    }
}

impl Drop for TriggerSuppress<'_> {
    fn drop(&mut self) {
        self.0.set_trigger_suppressed(0);
    }
}

impl MobileCore {
    /// 触摸键盘的 `;` 是否进快捷输入（默认关：当普通标点）。物理键盘不受影响。
    pub fn set_touch_quick_trigger(&self, on: bool) {
        self.touch_quick_trigger.store(on, Ordering::Relaxed);
    }

    /// 英文词后是否自动补空格（默认开）。关掉后不补空格、不换位。
    pub fn set_auto_space(&self, on: bool) {
        self.auto_space.store(on, Ordering::Relaxed);
    }

    /// 按键：字符/空格/退格/回车，连同翻译与英文自动空格。
    ///
    /// 返回的 `ops` 已包含补的空格与标点换位，宿主只管按序执行；`consumed == false`
    /// 时执行 `ops`（通常为空）后走默认行为。
    pub fn press(&self, key: PressKey, source: KeySource) -> KeyOutcome {
        let ch = match key {
            PressKey::Char { ch, .. } => Some(ch),
            _ => None,
        };

        // 触摸键盘的数字不选候选（物理键盘仍选）。条件取自 Kotlin `onKeyAction`：有候选才处理
        let mut leading: Option<KeyOutcome> = None;
        if source == KeySource::Touch
            && let Some(d) = ch.filter(char::is_ascii_digit)
            && self.candidates(0, 1).total > 0
        {
            if self.coord.is_composing() {
                if !self.coord.latin_input() && !self.digits_are_input() {
                    // 中文组码：首选上屏，再出数字（同打标点）。不清 auto_spaced（Kotlin 在此直接返回）
                    let mut out = self.select_candidate_plain(0);
                    let ops = vec![EditOp::Commit(d.to_string())];
                    self.coord.note_host_edits(&ops);
                    out.ops.extend(ops);
                    out.consumed = true;
                    return out;
                }
                // 英文组码 / 数字属于输入的模式：数字是缓冲的一部分，交给引擎
            } else {
                // 联想态：先收起联想，再走引擎
                leading = Some(self.key_down(keymap::VK_ESCAPE, 0));
            }
        }

        let outcome = self.engine_press(key, ch, source);
        match leading {
            None => outcome,
            Some(mut first) => {
                first.ops.extend(outcome.ops);
                first.consumed = outcome.consumed;
                first.mode_changed |= outcome.mode_changed;
                first.hint = first.hint.or(outcome.hint);
                first
            }
        }
    }

    /// 宿主的「原样上屏」键（九宫数字页的数字、句点、左列符号，123 层的半角句点）。
    ///
    /// 这些键平时绕过引擎：中文模式下句点不能变「。」，有编码时数字不能变成选词。但在
    /// 文本缓冲模式（见 [`Self::digits_are_input`]，如快捷输入的 `;12+3`）里它们是缓冲的
    /// 一部分，这里照触摸字符键交给引擎。其余情况返回 `consumed == false`、`ops` 为空，
    /// 宿主维持原样上屏（有编码先顶首选）。只收单个字符，多字符项一律原样上屏。
    pub fn press_literal(&self, text: &str) -> KeyOutcome {
        let mut chars = text.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            return KeyOutcome::passthrough();
        };
        if !self.digits_are_input() {
            return KeyOutcome::passthrough();
        }
        self.engine_press(
            PressKey::Char {
                ch: c,
                shifted: false,
            },
            Some(c),
            KeySource::Touch,
        )
    }

    /// 当前模式的缓冲是**文本**而不是编码，数字由引擎自己判定是入缓冲还是选词：
    /// 快捷输入等融合模式（`;12+3`，算式透镜下数字是输入）、临时英文（`win10`）、
    /// 网址 / 邮箱（原样累积）、Unicode（十六进制码点）。
    ///
    /// 这些模式里候选一进来就有（快捷输入的「重复上屏」），触摸数字若照普通组码
    /// 「首选上屏 + 数字」，进快捷输入后上滑数字就把重复候选顶上屏了（真机反馈）。
    /// 编码类模式（临时拼音、特殊码表、生僻字、反查、辅助码）数字不是合法编码，维持原状。
    fn digits_are_input(&self) -> bool {
        matches!(
            self.coord.debug_active_mode(),
            Some("mix" | "temp_english" | "url" | "email" | "unicode")
        )
    }

    /// 对应 Kotlin `engineKey`：句末标点换位、清标记、翻译 VK、进引擎、英文空格补空格
    fn engine_press(&self, key: PressKey, ch: Option<char>, source: KeySource) -> KeyOutcome {
        let english = self.coord.latin_input();
        let composing_before = self.coord.is_composing();

        // 自动补的空格遇到句末标点：挪到标点后面。空格还在不在由 `set_preceding_text` 把关
        if english
            && !composing_before
            && self.auto_spaced.load(Ordering::Relaxed)
            && let Some(c) = ch.filter(|c| AUTO_SPACE_PUNCT.contains(c))
        {
            let ops = vec![EditOp::ReplaceBackward {
                count: 1,
                text: format!("{c} "),
            }];
            self.coord.note_host_edits(&ops);
            return KeyOutcome {
                consumed: true,
                ops,
                ..KeyOutcome::default()
            };
        }
        self.auto_spaced.store(false, Ordering::Relaxed);

        let Some((vk, modifiers)) = to_vk(key, english) else {
            return KeyOutcome::passthrough();
        };
        // 触摸 `;` 默认不是引导键：压制期间核心按「该键未绑定」处理（空码出标点，
        // 有码仍是次选键）。守卫析构即清除，`should_handle_key` 探测与分发都在其覆盖下
        let _suppress = (source == KeySource::Touch
            && ch == Some(';')
            && !self.touch_quick_trigger.load(Ordering::Relaxed))
        .then(|| TriggerSuppress::new(&self.coord, vk));
        let mut outcome = self.key_down(vk, modifiers);
        drop(_suppress);
        // 英文方案：有编码时空格上屏原文（引擎只出词本身），这里补上词后的空格
        if outcome.consumed && key == PressKey::Space && english && composing_before {
            self.append_auto_space(&mut outcome);
        }
        outcome
    }

    /// 英文词后补空格：追加 `Commit(" ")` 并置 `auto_spaced`。关着自动空格时什么都不做。
    pub(crate) fn append_auto_space(&self, outcome: &mut KeyOutcome) {
        if !self.auto_space.load(Ordering::Relaxed) {
            return;
        }
        let ops = vec![EditOp::Commit(" ".to_string())];
        self.coord.note_host_edits(&ops);
        outcome.ops.extend(ops);
        self.auto_spaced.store(true, Ordering::Relaxed);
    }
}

/// 对应 Kotlin `RustSession.toVk`。`None` = 不进引擎（宿主走默认行为）
fn to_vk(key: PressKey, english: bool) -> Option<(u32, u32)> {
    const SHIFT: u32 = wind_host::Modifiers::SHIFT;
    match key {
        PressKey::Space => Some((keymap::VK_SPACE, 0)),
        PressKey::Backspace => Some((keymap::VK_BACK, 0)),
        PressKey::Enter => Some((keymap::VK_RETURN, 0)),
        PressKey::Char { ch, shifted } => match ch {
            'a'..='z' if !shifted => Some((0x41 + (ch as u32 - 'a' as u32), 0)),
            // 英文方案：大写字母也进组合（句首自动大写后补全不能断）
            'a'..='z' if english => Some((0x41 + (ch as u32 - 'a' as u32), SHIFT)),
            // 中文方案：Shift 态大写字母绕过引擎直输（引擎的 Shift+字母 是「临时英文」语义）
            'a'..='z' => None,
            _ => us_layout_vk(ch).map(|(vk, shift)| (vk, if shift { SHIFT } else { 0 })),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use wind_config::Config;
    use wind_coordinator::{Coordinator, InputProfile, SessionEvent};

    fn data_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
    }

    /// 造一个移动 Profile 的核心（结构体字面量，不经 `MobileCore::new`：它写进程环境变量）。
    /// 接收端必须留在作用域里，否则会话事件通道断开。缺数据：CI 跳过，本地失败。
    fn core(
        tag: &str,
        schema: &str,
        tweak: impl FnOnce(&mut Config),
    ) -> Option<(MobileCore, std::sync::mpsc::Receiver<SessionEvent>)> {
        let data = data_dir();
        if !data.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists()
            || !data.join("schemas/english").is_dir()
        {
            if std::env::var_os("CI").is_some() {
                eprintln!("跳过：缺少出厂数据（CI 环境）");
                return None;
            }
            panic!("缺少出厂数据 {}，本地不允许跳过", data.display());
        }
        let user =
            std::env::temp_dir().join(format!("wind_mobile_touch_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&user);
        std::fs::create_dir_all(&user).unwrap();
        let mut cfg = Config::default();
        cfg.schema.available = vec![schema.into()];
        cfg.schema.active = schema.into();
        cfg.input.default.chinese_mode = true;
        cfg.schema.codetable.auto_commit_at_full = false;
        tweak(&mut cfg);
        let (coord, rx) =
            Coordinator::new_session(cfg, Some(&data), Some(&user), InputProfile::mobile());
        let m = MobileCore {
            coord,
            warming: Arc::new(AtomicBool::new(false)),
            data_dir: user,
            auto_spaced: AtomicBool::new(false),
            auto_space: AtomicBool::new(true),
            touch_quick_trigger: AtomicBool::new(false),
        };
        Some((m, rx))
    }

    /// 关联想：上屏后核心会开一个占位组合（`SetComposition(" ")`），断言精确 ops 时要避开它
    fn no_assoc(c: &mut Config) {
        c.input.association.kind = "off".into();
        c.mobile.association.kind = "off".into();
    }

    fn ch(c: char) -> PressKey {
        PressKey::Char {
            ch: c,
            shifted: false,
        }
    }

    fn type_str(m: &MobileCore, s: &str) {
        for c in s.chars() {
            m.press(ch(c), KeySource::Touch);
        }
    }

    fn commit(s: &str) -> EditOp {
        EditOp::Commit(s.to_string())
    }

    fn has_replace(out: &KeyOutcome) -> bool {
        out.ops
            .iter()
            .any(|o| matches!(o, EditOp::ReplaceBackward { .. }))
    }

    /// 英文词 + 空格，空格后 `auto_spaced` 为真
    fn english_word_space(m: &MobileCore) -> KeyOutcome {
        type_str(m, "hello");
        m.press(PressKey::Space, KeySource::Touch)
    }

    #[test]
    fn lowercase_letter_composes() {
        let Some((m, _rx)) = core("lower", "wubi86", |_| {}) else {
            return;
        };
        let out = m.press(ch('w'), KeySource::Touch);
        assert!(out.consumed);
        assert!(m.coord.is_composing());
    }

    #[test]
    fn shifted_letter_passes_through_in_chinese() {
        let Some((m, _rx)) = core("shiftzh", "wubi86", |_| {}) else {
            return;
        };
        let out = m.press(
            PressKey::Char {
                ch: 'a',
                shifted: true,
            },
            KeySource::Touch,
        );
        assert!(!out.consumed);
        assert!(out.ops.is_empty());
        assert!(!m.coord.is_composing());
    }

    #[test]
    fn shifted_letter_composes_in_english() {
        let Some((m, _rx)) = core("shiften", "english", |_| {}) else {
            return;
        };
        let out = m.press(
            PressKey::Char {
                ch: 'a',
                shifted: true,
            },
            KeySource::Touch,
        );
        assert!(out.consumed, "{out:?}");
        assert!(m.coord.is_composing());
    }

    #[test]
    fn punct_maps_like_us_layout() {
        let Some((m, _rx)) = core("punct", "wubi86", |_| {}) else {
            return;
        };
        let out = m.press(ch(','), KeySource::Touch);
        assert!(out.consumed);
        assert_eq!(out.ops, vec![commit("，")], "逗号走中文标点");
        // '?' 走 Shift + '/'
        let out = m.press(ch('?'), KeySource::Touch);
        assert_eq!(out.ops, vec![commit("？")], "{out:?}");
        // 美式键盘上没有的字符不进引擎
        let out = m.press(ch('，'), KeySource::Touch);
        assert!(!out.consumed && out.ops.is_empty());
    }

    #[test]
    fn touch_digit_commits_first_then_digit() {
        let Some((m, _rx)) = core("tdigit", "wubi86", no_assoc) else {
            return;
        };
        type_str(&m, "wqvb");
        let first = m.candidates(0, 1).items[0].clone();
        let out = m.press(ch('1'), KeySource::Touch);
        assert!(out.consumed);
        assert_eq!(out.ops, vec![commit(&first), commit("1")]);
        assert!(!m.coord.is_composing());
    }

    #[test]
    fn touch_digit_closes_association() {
        let Some((m, _rx)) = core("assoc", "wubi86", |c| {
            c.input.association.kind = "word".into();
            c.input.association.mode = "continuous".into();
        }) else {
            return;
        };
        m.coord.prewarm_indexes();
        type_str(&m, "q");
        let out = m.press(PressKey::Space, KeySource::Touch);
        assert!(out.consumed);
        assert!(!m.coord.is_composing(), "上屏后不在组码中");
        assert!(m.candidates(0, 1).total > 0, "前提：上屏后有联想候选");
        let out = m.press(ch('1'), KeySource::Touch);
        assert_eq!(m.candidates(0, 1).total, 0, "联想已收起: {out:?}");
        assert!(
            !out.ops
                .iter()
                .any(|o| matches!(o, EditOp::Commit(t) if t.chars().count() > 1)),
            "数字不能选走联想词: {out:?}"
        );
    }

    #[test]
    fn physical_digit_still_selects() {
        let Some((m, _rx)) = core("pdigit", "wubi86", no_assoc) else {
            return;
        };
        type_str(&m, "wqvb");
        let first = m.candidates(0, 1).items[0].clone();
        let out = m.press(ch('1'), KeySource::Physical);
        assert!(out.consumed);
        assert_eq!(out.ops, vec![commit(&first)], "物理键盘数字只选词");
    }

    #[test]
    fn english_digit_enters_buffer() {
        let Some((m, _rx)) = core("endigit", "english", |_| {}) else {
            return;
        };
        type_str(&m, "mp");
        assert!(m.candidates(0, 1).total > 0, "前提：mp 有候选");
        let out = m.press(ch('3'), KeySource::Touch);
        assert!(out.consumed, "{out:?}");
        assert!(m.coord.is_composing());
        let comp = out
            .ops
            .iter()
            .find_map(|o| match o {
                EditOp::SetComposition { text, .. } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        assert!(comp.to_lowercase().contains("mp3"), "{out:?}");
    }

    #[test]
    fn space_after_english_word_appends_space() {
        let Some((m, _rx)) = core("sp", "english", |_| {}) else {
            return;
        };
        let out = english_word_space(&m);
        assert!(out.consumed);
        assert_eq!(out.ops.last(), Some(&commit(" ")), "{out:?}");
        assert!(m.auto_spaced.load(Ordering::Relaxed));
    }

    #[test]
    fn physical_space_also_auto_spaces() {
        let Some((m, _rx)) = core("psp", "english", |_| {}) else {
            return;
        };
        for c in "hello".chars() {
            m.press(ch(c), KeySource::Physical);
        }
        let out = m.press(PressKey::Space, KeySource::Physical);
        assert_eq!(out.ops.last(), Some(&commit(" ")), "{out:?}");
    }

    #[test]
    fn select_candidate_in_english_appends_space() {
        let Some((m, _rx)) = core("selen", "english", |_| {}) else {
            return;
        };
        type_str(&m, "hel");
        assert!(m.candidates(0, 1).total > 0);
        let out = m.select_candidate(0);
        assert!(out.consumed);
        assert_eq!(out.ops.last(), Some(&commit(" ")), "{out:?}");
        assert!(m.auto_spaced.load(Ordering::Relaxed));
    }

    #[test]
    fn select_candidate_plain_adds_no_space() {
        let Some((m, _rx)) = core("selplain", "english", |_| {}) else {
            return;
        };
        type_str(&m, "hel");
        assert!(m.candidates(0, 1).total > 0);
        let out = m.select_candidate_plain(0);
        assert!(out.consumed);
        assert!(!out.ops.contains(&commit(" ")), "{out:?}");
        assert!(!m.auto_spaced.load(Ordering::Relaxed));
        let dot = m.press(ch('.'), KeySource::Touch);
        assert!(
            !dot.ops
                .iter()
                .any(|o| matches!(o, EditOp::ReplaceBackward { .. })),
            "没补空格就不该换位: {dot:?}"
        );
    }

    #[test]
    fn touch_digit_with_association_keeps_ops_order() {
        let Some((m, _rx)) = core("digassoc", "wubi86", |_| {}) else {
            return;
        };
        type_str(&m, "aa");
        let out = m.press(ch('1'), KeySource::Touch);
        assert!(out.consumed);
        assert_eq!(out.ops.len(), 3, "{out:?}");
        assert!(
            matches!(&out.ops[0], EditOp::Commit(t) if t != "1" && !t.is_empty()),
            "{out:?}"
        );
        assert!(
            matches!(&out.ops[1], EditOp::SetComposition { text, .. } if text == " "),
            "{out:?}"
        );
        assert_eq!(out.ops[2], commit("1"), "{out:?}");
    }

    #[test]
    fn select_candidate_in_chinese_appends_nothing() {
        let Some((m, _rx)) = core("selzh", "wubi86", |_| {}) else {
            return;
        };
        type_str(&m, "wqvb");
        let out = m.select_candidate(0);
        assert!(!out.ops.contains(&commit(" ")), "{out:?}");
        assert!(!m.auto_spaced.load(Ordering::Relaxed));
    }

    #[test]
    fn punct_swaps_with_auto_space() {
        let Some((m, _rx)) = core("swap", "english", |_| {}) else {
            return;
        };
        english_word_space(&m);
        let out = m.press(ch('.'), KeySource::Touch);
        assert!(out.consumed);
        assert_eq!(
            out.ops,
            vec![EditOp::ReplaceBackward {
                count: 1,
                text: ". ".into()
            }]
        );
    }

    #[test]
    fn second_punct_keeps_swapping() {
        let Some((m, _rx)) = core("swap2", "english", |_| {}) else {
            return;
        };
        english_word_space(&m);
        m.press(ch('.'), KeySource::Touch);
        let out = m.press(ch(','), KeySource::Touch);
        assert_eq!(
            out.ops,
            vec![EditOp::ReplaceBackward {
                count: 1,
                text: ", ".into()
            }],
            "和现有 Kotlin 一致：换位后标记仍在"
        );
    }

    #[test]
    fn auto_space_off_appends_nothing() {
        let Some((m, _rx)) = core("off", "english", |_| {}) else {
            return;
        };
        m.set_auto_space(false);
        let out = english_word_space(&m);
        assert!(!out.ops.contains(&commit(" ")), "{out:?}");
        assert!(!m.auto_spaced.load(Ordering::Relaxed));
        type_str(&m, "ab");
        let out = m.select_candidate(0);
        assert!(!out.ops.contains(&commit(" ")), "{out:?}");
    }

    #[test]
    fn cursor_move_cancels_punct_swap() {
        let Some((m, _rx)) = core("cursor", "english", |_| {}) else {
            return;
        };
        english_word_space(&m);
        m.set_preceding_text("abc".into());
        let out = m.press(ch('.'), KeySource::Touch);
        assert!(!has_replace(&out), "{out:?}");
    }

    #[test]
    fn preceding_space_keeps_punct_swap() {
        let Some((m, _rx)) = core("keep", "english", |_| {}) else {
            return;
        };
        english_word_space(&m);
        m.set_preceding_text("hello ".into());
        let out = m.press(ch('.'), KeySource::Touch);
        assert!(has_replace(&out), "{out:?}");
    }

    #[test]
    fn focus_change_cancels_punct_swap() {
        let Some((m, _rx)) = core("focus", "english", |_| {}) else {
            return;
        };
        english_word_space(&m);
        m.focus_gained();
        let out = m.press(ch('.'), KeySource::Touch);
        assert!(!has_replace(&out), "{out:?}");
    }

    #[test]
    fn other_key_cancels_punct_swap() {
        let Some((m, _rx)) = core("other", "english", |_| {}) else {
            return;
        };
        english_word_space(&m);
        m.press(PressKey::Backspace, KeySource::Touch);
        let out = m.press(ch('.'), KeySource::Touch);
        assert!(!has_replace(&out), "{out:?}");
    }

    /// Kotlin `Punct.US_LAYOUT` 的全部条目（逐项抄录，共 42 项）
    #[test]
    fn us_layout_matches_kotlin() {
        let mut expected: Vec<(char, u32, bool)> = Vec::new();
        for d in 0..10u32 {
            expected.push((char::from_digit(d, 10).unwrap(), 0x30 + d, false));
        }
        for (i, c) in ")!@#$%^&*(".chars().enumerate() {
            expected.push((c, 0x30 + i as u32, true));
        }
        let oem: [(u32, char, char); 11] = [
            (0xBA, ';', ':'),
            (0xBB, '=', '+'),
            (0xBC, ',', '<'),
            (0xBD, '-', '_'),
            (0xBE, '.', '>'),
            (0xBF, '/', '?'),
            (0xC0, '`', '~'),
            (0xDB, '[', '{'),
            (0xDC, '\\', '|'),
            (0xDD, ']', '}'),
            (0xDE, '\'', '"'),
        ];
        for (vk, plain, shifted) in oem {
            expected.push((plain, vk, false));
            expected.push((shifted, vk, true));
        }
        assert_eq!(expected.len(), 42);
        for (c, vk, shift) in &expected {
            assert_eq!(us_layout_vk(*c), Some((*vk, *shift)), "字符 {c:?}");
        }
        let hits = (0u32..0x250)
            .filter_map(char::from_u32)
            .filter(|c| us_layout_vk(*c).is_some())
            .count();
        assert_eq!(hits, 42, "不能多出 Kotlin 表里没有的字符");
    }

    /// 账本遗留项：缓冲空但留着已转换前缀（`handle_candidate.rs` 的 `refill_candidates` 空缓冲分支）。
    ///
    /// `is_composing()` 要求 `input_buffer` 非空，所以该状态下它为 false，而 `preedit` 还显示前缀。
    /// 这里尝试从拼音分步选词出发，用退格到达该状态（Home+前删试过：`VK_DELETE`
    /// 在移动端 `key_down` 直接放行（consumed=false），到不了），并在每一步检查不变量：
    /// `is_composing()` 为 false 时，宿主组合区必须已被清空（最近一次组合区文本为空），
    /// 否则就是「组合区还挂着前缀、按键却被放行」。
    #[test]
    fn leading_converted_prefix_composing() {
        fn last_comp(out: &wind_host::KeyOutcome, cur: &mut String) {
            for op in &out.ops {
                if let EditOp::SetComposition { text, .. } = op {
                    *cur = text.clone();
                }
            }
        }
        {
            let tag = "prefix_bs";
            let Some((m, _rx)) = core(tag, "pinyin", |_| {}) else {
                return;
            };
            m.coord.prewarm_indexes();
            type_str(&m, "nihao");
            let win = m.candidates(0, 50);
            // 只覆盖前缀的单字候选 = 分步上屏，剩余编码继续留在缓冲
            let idx = win
                .items
                .iter()
                .position(|t| t.chars().count() == 1)
                .expect("前提：nihao 有单字候选");
            let mut comp = String::new();
            last_comp(&m.select_candidate(idx), &mut comp);
            assert!(
                comp.starts_with('你') || !comp.is_empty(),
                "前提：分步上屏后组合区有内容: {comp:?}"
            );
            assert!(m.coord.is_composing(), "前提：分步选词后剩余编码仍在组合中");
            let mut saw_empty_buffer_with_prefix = false;
            for step in 0..12 {
                let out = m.press(PressKey::Backspace, KeySource::Touch);
                last_comp(&out, &mut comp);
                let composing = m.coord.is_composing();
                eprintln!("[{tag}] step {step}: composing={composing} comp={comp:?} out={out:?}");
                if !composing {
                    // 不在组码中：宿主组合区必须已清空，回车 / 退格才可以放行
                    saw_empty_buffer_with_prefix |= !comp.is_empty();
                    assert!(
                        comp.is_empty(),
                        "[{tag}] 组合区挂着 {comp:?} 而 is_composing()=false"
                    );
                    break;
                }
            }
            assert!(!saw_empty_buffer_with_prefix);
            // 此刻回车：组合区已空，放行换行才对
            let enter = m.press(PressKey::Enter, KeySource::Touch);
            eprintln!(
                "[{tag}] enter -> {enter:?}, composing={}",
                m.coord.is_composing()
            );
            assert!(!m.coord.is_composing());
        }
    }

    // ---- 触摸 `;` 默认不进快捷输入 ----

    #[test]
    fn touch_semicolon_is_punct_by_default() {
        let Some((m, _rx)) = core("semi_off", "wubi86", no_assoc) else {
            return;
        };
        let out = m.press(ch(';'), KeySource::Touch);
        assert!(out.consumed);
        assert!(out.ops.contains(&commit("；")), "ops={:?}", out.ops);
        assert!(!m.coord.is_composing());
    }

    #[test]
    fn touch_semicolon_enters_quick_input_when_enabled() {
        let Some((m, _rx)) = core("semi_on", "wubi86", no_assoc) else {
            return;
        };
        m.set_touch_quick_trigger(true);
        let out = m.press(ch(';'), KeySource::Touch);
        assert!(out.consumed);
        assert!(!out.ops.contains(&commit("；")), "ops={:?}", out.ops);
        assert!(m.coord.is_composing());
    }

    #[test]
    fn physical_semicolon_always_enters_quick_input() {
        let Some((m, _rx)) = core("semi_phys", "wubi86", no_assoc) else {
            return;
        };
        let out = m.press(ch(';'), KeySource::Physical);
        assert!(out.consumed);
        assert!(!out.ops.contains(&commit("；")), "ops={:?}", out.ops);
        assert!(m.coord.is_composing());
    }

    #[test]
    fn touch_semicolon_still_selects_second_candidate() {
        let Some((m, _rx)) = core("semi_sel", "wubi86", no_assoc) else {
            return;
        };
        type_str(&m, "a");
        let second = m.candidates(0, 2).items[1].clone();
        let out = m.press(ch(';'), KeySource::Touch);
        assert!(out.consumed);
        assert!(out.ops.contains(&commit(&second)), "ops={:?}", out.ops);
    }

    // ---- 快捷输入等「数字属于输入」的模式：触摸数字进缓冲 ----

    /// 组合区文本（最后一条 SetComposition）
    fn composition(out: &KeyOutcome) -> Option<String> {
        out.ops.iter().rev().find_map(|o| match o {
            EditOp::SetComposition { text, .. } => Some(text.clone()),
            _ => None,
        })
    }

    #[test]
    fn touch_digit_enters_quick_input_buffer_with_repeat_candidate() {
        let Some((m, _rx)) = core("quick_digit", "wubi86", no_assoc) else {
            return;
        };
        m.set_touch_quick_trigger(true);
        // 先上屏一个字：快捷输入一进来就有「重复上屏」候选（真机反馈的触发条件）
        type_str(&m, "wqvb");
        m.press(PressKey::Space, KeySource::Touch);
        m.press(ch(';'), KeySource::Touch);
        assert!(
            m.candidates(0, 1).total > 0,
            "前提：快捷输入里有重复上屏候选"
        );
        let mut last = KeyOutcome::default();
        for c in "12+3".chars() {
            last = m.press(ch(c), KeySource::Touch);
            assert!(
                !last.ops.iter().any(|o| matches!(o, EditOp::Commit(_))),
                "{c} 不能上屏: {last:?}"
            );
        }
        assert!(m.coord.is_composing());
        let comp = composition(&last).unwrap_or_default();
        assert!(comp.contains("12+3"), "{last:?}");
        assert!(
            m.candidates(0, 5).items.iter().any(|t| t == "15"),
            "算式算得出: {:?}",
            m.candidates(0, 5).items
        );
    }

    /// 九宫数字页 / 123 层的「原样上屏」键在快捷输入里进缓冲（真机反馈：切到数字页打
    /// `12+3`，数字直接顶码上屏，算不了式子）。小数点也是缓冲，不变「。」
    #[test]
    fn literal_keys_enter_quick_input_buffer() {
        let Some((m, _rx)) = core("literal_quick", "wubi86", no_assoc) else {
            return;
        };
        m.set_touch_quick_trigger(true);
        type_str(&m, "wqvb");
        m.press(PressKey::Space, KeySource::Touch);
        m.press(ch(';'), KeySource::Touch);
        assert!(m.candidates(0, 1).total > 0, "前提：有重复上屏候选");
        let mut last = KeyOutcome::default();
        for c in "(1.5+3)*2".chars() {
            last = m.press_literal(&c.to_string());
            assert!(last.consumed, "{c} 要被引擎收下: {last:?}");
            assert!(
                !last.ops.iter().any(|o| matches!(o, EditOp::Commit(_))),
                "{c} 不能上屏: {last:?}"
            );
        }
        let comp = composition(&last).unwrap_or_default();
        assert!(comp.contains("(1.5+3)*2"), "{last:?}");
        assert!(
            m.candidates(0, 5).items.iter().any(|t| t == "9"),
            "算式算得出: {:?}",
            m.candidates(0, 5).items
        );
    }

    /// 普通组码 / 无编码时原样上屏键不进引擎：宿主照旧（有编码先顶首选，再接字符）
    #[test]
    fn literal_keys_pass_through_outside_text_buffer() {
        let Some((m, _rx)) = core("literal_plain", "wubi86", no_assoc) else {
            return;
        };
        let out = m.press_literal("1");
        assert!(!out.consumed && out.ops.is_empty(), "无编码: {out:?}");
        let out = m.press_literal(".");
        assert!(!out.consumed && out.ops.is_empty(), "无编码句点: {out:?}");
        type_str(&m, "wqvb");
        for t in ["1", ".", "+"] {
            let out = m.press_literal(t);
            assert!(!out.consumed && out.ops.is_empty(), "组码中 {t}: {out:?}");
            assert!(m.coord.is_composing(), "组码不被动");
        }
        // 多字符项在快捷输入里也原样上屏
        m.set_touch_quick_trigger(true);
        m.press(PressKey::Space, KeySource::Touch);
        m.press(ch(';'), KeySource::Touch);
        let out = m.press_literal("()");
        assert!(!out.consumed && out.ops.is_empty(), "{out:?}");
    }
}
