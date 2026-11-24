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

        let Some((vk, modifiers)) = to_vk(key) else {
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
fn to_vk(key: PressKey) -> Option<(u32, u32)> {
    const SHIFT: u32 = wind_host::Modifiers::SHIFT;
    match key {
        PressKey::Space => Some((keymap::VK_SPACE, 0)),
        PressKey::Backspace => Some((keymap::VK_BACK, 0)),
        PressKey::Enter => Some((keymap::VK_RETURN, 0)),
        PressKey::Char { ch, shifted } => match ch {
            'a'..='z' if !shifted => Some((0x41 + (ch as u32 - 'a' as u32), 0)),
            // Shift 态字母一律进引擎：英文方案里进组合（句首自动大写后补全不能断）；
            // 中文方案里由 `input.temp_english.shift_behavior` 定——进临时英文（出厂）或直接上屏大写
            'a'..='z' => Some((0x41 + (ch as u32 - 'a' as u32), SHIFT)),
            _ => us_layout_vk(ch).map(|(vk, shift)| (vk, if shift { SHIFT } else { 0 })),
        },
    }
}

#[cfg(test)]
#[path = "touch_tests.rs"]
mod tests;
