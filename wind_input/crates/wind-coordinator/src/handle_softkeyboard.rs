//! 软键盘：常驻符号面板的状态与按键接管。
//!
//! # 它和别的模式不是一回事
//!
//! 软键盘不组码、不产候选、不进 [`ModeKind`](crate::pipeline::ModeKind)——它只把键位
//! 直接换成符号，语义与自定义标点同族，只是范围扩大到整个主键区。所以它与 `State.active`
//! **正交**，状态也不住在 `State` 里（理由见 `Coordinator::softkeyboard_active` 的注释）。
//!
//! # 两种面
//!
//! | | 符号面（默认） | 键盘面（`send_keys`） |
//! |---|---|---|
//! | 画的是 | 键盘打不出的字符 | 键盘本来就有的键 |
//! | 点键帽 | 查表 → 上屏那个字符 | **合成一次真实按键** |
//! | 敲物理键 | 查表 → 上屏 | **完全不接管**，落回常规输入链路 |
//! | 打 `nihao` | 出五个符号 | 出「你好」 |
//!
//! 键盘面存在的理由：用户在标准 PC 键盘面上点 `n` `i` `h` `a` `o`，期待的是打出中文，
//! 不是往文档里塞五个字母。两条路（点击、物理键）都必须汇进常规输入链路，所以
//! **C++ 侧也要知道当前是哪种面**（`STATUS_SOFT_KEYBOARD_KEYS`）：键盘面时不启用
//! 软键盘总闸，否则英文模式下会「吃了不发」——总闸吃掉键，常规链路却 PassThrough。
//!
//! # 谁被接管，谁不被接管（符号面）
//!
//! | 键 | 处置 |
//! |---|---|
//! | 布局里的**符号键位**（字母 / 数字 / 符号） | 接管 → 查表上屏；无映射则吃掉忽略 |
//! | 布局里的**特殊键**（Tab/Enter/退格/空格/Ins/Del/方向键） | 透传（封闭集，不可映射） |
//! | 布局**之外**的一切（F1–F12、小键盘、Ctrl/Alt 组合） | 透传 |
//! | Esc / 翻页键 | 面板自己的控制键 |
//!
//! 特殊键**硬编码透传、不接受映射**：放开可配会带来一族判据（映射过的退格还能不能长按
//! 连删？映射过的回车还换不换行？），而它买到的灵活性没人要。本模块不列举它们——
//! **凡是不在布局表里的键一律不接管**，透传是「没被拦下」的自然结果而不是一条规则，
//! 于是不存在第二份需要同步的清单。
//!
//! # 长按重复不在这里
//!
//! 物理键长按由系统 auto-repeat 连发 keydown，逐次走本模块，**不要去抖**；特殊键既然
//! 透传，它们的重复干脆由宿主自己处理，我们完全不参与。只有面板上的鼠标长按需要定时器，
//! 那是 UI 侧的事（见 `docs/design/soft-keyboard.md` §5）。

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::Ordering;

use tracing::{debug, warn};
use wind_bridge::handler::{KeyAction, KeyEventData};
use wind_ipc::protocol::{MOD_SHIFT, MOD_SHORTCUT};
use wind_keys::keymap;

use wind_ui_types::{SoftKeyCap, UiCommand, slot_layer};

use crate::coordinator::{Coordinator, State};

/// 虚拟键码 → 软键盘键位名。
///
/// 由 [`wind_softkeyboard::all_slots`] 反查构建，**不手写第二份映射**——手写的那份
/// 与布局表迟早分叉，症状是「某个键在面板上画着符号，敲下去却没反应」。
fn vk_to_slot() -> &'static HashMap<u32, &'static str> {
    static MAP: OnceLock<HashMap<u32, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = HashMap::with_capacity(wind_softkeyboard::SLOT_COUNT);
        for slot in wind_softkeyboard::all_slots() {
            // 数字键不在 wind-keys 的 KEY_TABLE 里（那张表只收符号键），ASCII 直映射。
            let vk = if slot.len() == 1 && slot.as_bytes()[0].is_ascii_digit() {
                Some(0x30 + u32::from(slot.as_bytes()[0] - b'0'))
            } else {
                keymap::key_name_to_vk_with_letters(slot)
            };
            match vk {
                Some(vk) => {
                    m.insert(vk, slot);
                }
                None => {
                    // 布局里出现了 wind-keys 不认识的键名。`tests/key_name_parity.rs` 会在
                    // CI 里先一步拦下，这里只是运行期兜底。
                    warn!("软键盘: 键位名 {slot:?} 解析不出虚拟键码，该键位不可用");
                }
            }
        }
        m
    })
}

impl Coordinator {
    /// 软键盘是否开着。
    pub(crate) fn softkeyboard_is_open(&self) -> bool {
        self.softkeyboard_active.load(Ordering::Relaxed)
    }

    /// 当前面的下标（已按表长夹取）。
    pub(crate) fn softkeyboard_page_idx(&self) -> usize {
        let n = self.softkeyboard.len();
        if n == 0 {
            return 0;
        }
        self.softkeyboard_page.load(Ordering::Relaxed).min(n - 1)
    }

    /// 当前面是不是**键盘面**（按键交还输入法）。
    pub(crate) fn softkeyboard_send_keys(&self) -> bool {
        self.softkeyboard
            .pages()
            .get(self.softkeyboard_page_idx())
            .is_some_and(|p| p.send_keys)
    }

    /// 当前面名（日志与 UI 用；表为空时给空串）。
    pub(crate) fn softkeyboard_page_name(&self) -> String {
        self.softkeyboard
            .pages()
            .get(self.softkeyboard_page_idx())
            .map(|p| p.name.clone())
            .unwrap_or_default()
    }

    /// 开关软键盘。
    ///
    /// `page` 非空时**无论开关状态都切到那一面**（直通车语义）：按一次进数学面，再按
    /// 一次仍是数学面而不是关掉——否则用户给两个面各配一个直通键，两键就会互相打架。
    /// 想关就用不带面的那条绑定，或 Esc。
    pub(crate) fn toggle_softkeyboard(&self, page: Option<&str>) -> KeyAction {
        match page {
            Some(id) if self.softkeyboard_is_open() => {
                if self.softkeyboard_goto(id) {
                    KeyAction::Consumed
                } else {
                    // 面不存在：退化成普通开关，别让这个键什么都不做。
                    self.close_softkeyboard()
                }
            }
            _ if self.softkeyboard_is_open() => self.close_softkeyboard(),
            _ => self.open_softkeyboard(page),
        }
    }

    /// 热键入口：先处置正在打的编码，再开关面板。
    ///
    /// ★ **必须先收掉未上屏的编码**。软键盘一开就接管主键区，用户没法再往下打，
    /// 而候选窗还挂着旧候选——看起来就是「卡住了」。处置策略直接沿用切换那一套
    /// （`keys.commit_on_switch`：开则上屏原码，关则丢弃），因为对「这串码还要不要」
    /// 这个问题，开软键盘与切方案的答案相同。
    ///
    /// ⚠️ **空文本也要走 `InsertText`**：C++ 的 `CommitText` 即便文本为空也会
    /// `EndComposition`，清掉宿主里残留的编码；只发状态更新是清不掉的——那正是
    /// 「切了方案编码还挂在应用里」的根因。
    ///
    /// ⚠️ 内部要取 `State` 锁，**只能在不持锁时调用**（热键分派点正好在取锁之前）。
    pub(crate) fn softkeyboard_hotkey(&self, page: Option<&str>) -> KeyAction {
        // 关闭方向不必处置：软键盘态下本就没有输入会话。
        if self.softkeyboard_is_open() && page.is_none() {
            return self.close_softkeyboard();
        }
        let commit = self.take_input_on_schema_switch();
        self.toggle_softkeyboard(page);
        if commit.text.is_empty() && !commit.had_pending {
            return KeyAction::Consumed;
        }
        let chinese_mode = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.chinese_mode
        };
        KeyAction::InsertText {
            text: commit.text,
            new_composition: None,
            // 软键盘不改中英模式，别让图标白刷一次。
            mode_changed: false,
            chinese_mode,
            has_new_composition: false,
        }
    }

    /// 开启软键盘。表为空时不开——弹一个一个符号都打不出的面板只会让用户以为坏了。
    pub(crate) fn open_softkeyboard(&self, page: Option<&str>) -> KeyAction {
        if self.softkeyboard.is_empty() {
            warn!("软键盘: 映射表为空，忽略开启请求");
            return KeyAction::Consumed;
        }
        if let Some(id) = page {
            self.softkeyboard_goto(id);
        }
        self.softkeyboard_active.store(true, Ordering::Relaxed);
        self.softkeyboard_dirty.store(true, Ordering::Relaxed);
        *self
            .softkeyboard_opened_at
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
        debug!(
            "softkeyboard: opened at page {} ({})",
            self.softkeyboard_page_idx(),
            self.softkeyboard_page_name()
        );
        self.push_softkeyboard_view();
        KeyAction::Consumed
    }

    /// 关闭软键盘。
    ///
    /// 保留当前面下标：再开时停在同一面，符合「面板是把工具，放下再拿起来还是刚才那把」。
    pub(crate) fn close_softkeyboard(&self) -> KeyAction {
        if self.softkeyboard_active.swap(false, Ordering::Relaxed) {
            self.softkeyboard_dirty.store(true, Ordering::Relaxed);
            debug!("softkeyboard: closed");
            let _ = self.ui_tx.send(UiCommand::HideSoftKeyboard);
        }
        KeyAction::Consumed
    }

    /// 焦点切换时关闭。
    ///
    /// ⚠️ 与工具栏的失焦**隐藏**不同，这里是**关闭**：切回来不自动恢复。复用的是
    /// 「什么算失焦」那套判定，不是「失焦后做什么」的动作——两者分不清就会写成
    /// 「切回应用软键盘自己弹出来」。
    ///
    /// ★ 这条行为完全依赖面板不抢焦点（`WS_EX_NOACTIVATE`）：面板一旦可激活，
    /// 用户点它上面任何一个键都是在改变焦点，它会把自己关掉。
    pub(crate) fn close_softkeyboard_on_focus_change(&self, why: &str) {
        if !self.softkeyboard_is_open() {
            return;
        }
        // 守卫期：跨宿主切换时旧宿主的 focus_lost 实测晚约 100ms 到达，而「从工具栏图标
        // 点开面板」正好落在这个窗口里。不设守卫的表现是「点一下弹出即消失」——菜单为
        // 这件事踩过一轮，这里直接复用它的守卫期常量。
        if let Some(at) = *self
            .softkeyboard_opened_at
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            && at.elapsed() < crate::handle_menu::MENU_FOCUS_GUARD
        {
            debug!(
                "softkeyboard: 焦点事件({why})距打开 {:?} < 守卫期，忽略",
                at.elapsed()
            );
            return;
        }
        debug!("softkeyboard: closed by focus change ({why})");
        self.close_softkeyboard();
        self.push_state_update();
    }

    /// 切到指定 id 的面。面不存在返回 `false` 并告警——**不静默忽略**：用户配了
    /// `softkeyboard:math` 而表里没有 math 时，静默会表现成「按了没反应」，
    /// 与「这个键根本没绑上」完全同形，用户无从分辨。
    fn softkeyboard_goto(&self, id: &str) -> bool {
        match self.softkeyboard.index_of(id) {
            Some(idx) => {
                self.softkeyboard_page.store(idx, Ordering::Relaxed);
                // 切面可能把键盘面换成符号面（或反之），`STATUS_SOFT_KEYBOARD_KEYS`
                // 随之改变，C++ 的吃键判定要跟上。
                self.softkeyboard_dirty.store(true, Ordering::Relaxed);
                true
            }
            None => {
                warn!(
                    "软键盘: 面 {:?} 不存在（可用: {}）",
                    id,
                    self.softkeyboard
                        .pages()
                        .iter()
                        .map(|p| p.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                false
            }
        }
    }

    /// 相对切面（翻页键用）。空表时空转。
    fn softkeyboard_cycle_page(&self, delta: i32) {
        let n = self.softkeyboard.len();
        if n == 0 {
            return;
        }
        let next = (self.softkeyboard_page_idx() as i32 + delta).rem_euclid(n as i32) as usize;
        self.softkeyboard_page.store(next, Ordering::Relaxed);
        // 见 `softkeyboard_goto`：面的类型变了，C++ 的吃键判定要跟上。
        self.softkeyboard_dirty.store(true, Ordering::Relaxed);
        debug!(
            "softkeyboard: page -> {next} ({})",
            self.softkeyboard_page_name()
        );
        self.push_softkeyboard_view();
    }

    /// 软键盘态的按键处理。
    ///
    /// 返回 `None` = 本模块不管这个键，调用方继续走常规链路。**特殊键与布局外按键的
    /// 处置就在这里**：不拦，它们自然回到宿主。
    pub(crate) fn handle_softkeyboard_key(
        &self,
        state: &State,
        data: &KeyEventData,
    ) -> Option<KeyAction> {
        if !self.softkeyboard_is_open() {
            return None;
        }
        let vk = data.key_code;
        let send_keys = self.softkeyboard_send_keys();
        // 正在打字（有编码或有候选）时，面板的控制键要让位给输入本身：Esc 是「清空这串
        // 码」、翻页是「翻候选页」，都比「关面板 / 切面」更贴近用户当下在做的事。
        // 只有键盘面会出现这种局面——符号面根本不组码。
        let typing = send_keys && (!state.input_buffer.is_empty() || !state.candidates.is_empty());

        // ── 面板自己的控制键 ──
        if vk == keymap::VK_ESCAPE {
            if typing {
                return None;
            }
            return Some(self.close_softkeyboard());
        }
        if vk == keymap::VK_PRIOR && !typing {
            self.softkeyboard_cycle_page(-1);
            return Some(KeyAction::Consumed);
        }
        if vk == keymap::VK_NEXT && !typing {
            self.softkeyboard_cycle_page(1);
            return Some(KeyAction::Consumed);
        }

        // 带 Ctrl/Alt/Win 的组合一律不接管：那是宿主的快捷键，软键盘开着也不该抢。
        // Shift 不在此列——它是本面板的层选择器。
        if data.modifiers & MOD_SHORTCUT != 0 {
            return None;
        }

        // ── 键盘面：不接管，键归常规输入链路 ──
        //
        // 只把按下的样子画到面板上（跟手的高亮 + 切层），然后 `None` 放行。C++ 侧对称地
        // 不吃这些键（`STATUS_SOFT_KEYBOARD_KEYS`），于是这一面上打字与没开面板时完全
        // 一样：能组码、能出候选、能打中文。
        if send_keys {
            if let Some(slot) = vk_to_slot().get(&vk) {
                self.softkeyboard_press_feedback(slot, data.modifiers & MOD_SHIFT != 0);
            }
            return None;
        }

        // ── 符号键位 ──
        let slot = *vk_to_slot().get(&vk)?;
        let shift = data.modifiers & MOD_SHIFT != 0;
        self.softkeyboard_press_feedback(slot, shift);
        let page = self
            .softkeyboard
            .pages()
            .get(self.softkeyboard_page_idx())?;
        // ★ 取档与面板显示**同一份判据**（`slot_layer`）：面板画着 A，敲下去就得出 A。
        // 各写一份的症状是显示与输出分叉，比不支持 CapsLock 更糟。
        let layer = slot_layer(slot, shift, state.caps_lock);
        match page.output(slot, layer) {
            Some(text) => {
                let text = text.to_string();
                debug!(
                    "softkeyboard: {slot}{} -> {text:?}",
                    if layer { "+shift" } else { "" }
                );
                Some(self.softkeyboard_commit(state, &text))
            }
            None => {
                // 空键位：**吃掉并忽略**。透传是错的——面板上画着一个灰键位，敲下去却在
                // 宿主里出了个字母，比什么都不发生更让人困惑。
                debug!("softkeyboard: {slot} 是空键位，已忽略");
                Some(KeyAction::Consumed)
            }
        }
    }

    /// 面板跟随物理按键：切层与键帽高亮。
    ///
    /// 高亮不需要 keyup 配对——面板收到后自行短时清除，而物理长按会连发 keydown 不断
    /// 续期，视觉上就是持续按下。
    fn softkeyboard_press_feedback(&self, slot: &str, shift: bool) {
        let _ = self.ui_tx.send(UiCommand::SoftKeyboardLayer { shift });
        let _ = self.ui_tx.send(UiCommand::SoftKeyboardKeyState {
            slot: slot.to_string(),
            down: true,
        });
    }

    /// 把一个符号交给上屏出口。
    ///
    /// ⚠️ **必须过 `maybe_s2t`**，不能把字面量直接塞进 `InsertText`：简繁转换出口曾经
    /// 七处全漏、事后才收口，新增上屏路径正是那类缺陷的典型来源。
    fn softkeyboard_commit(&self, state: &State, text: &str) -> KeyAction {
        KeyAction::InsertText {
            text: self.maybe_s2t(state, text),
            new_composition: None,
            mode_changed: false,
            chinese_mode: state.chinese_mode,
            has_new_composition: false,
        }
    }
}

impl Coordinator {
    /// 把当前面下发给渲染端（开启 / 切面 / 内容变都走这里）。
    ///
    /// 只发当前面：切面时重发一次即可，而那一刻本来就要重排整块面板；一次性发全部
    /// 13 面要搬一千多个键位，其中 92% 当场用不上。
    pub(crate) fn push_softkeyboard_view(&self) {
        let idx = self.softkeyboard_page_idx();
        let pages: Vec<String> = self
            .softkeyboard
            .pages()
            .iter()
            .map(|p| p.name.clone())
            .collect();
        let page = self.softkeyboard.pages().get(idx);
        let keys: Vec<SoftKeyCap> = wind_softkeyboard::all_slots()
            .map(|slot| SoftKeyCap {
                slot: slot.to_string(),
                base: page
                    .and_then(|p| p.output(slot, false))
                    .unwrap_or_default()
                    .to_string(),
                shift: page
                    .and_then(|p| p.output(slot, true))
                    .unwrap_or_default()
                    .to_string(),
            })
            .collect();
        let _ = self.ui_tx.send(UiCommand::ShowSoftKeyboard {
            pages,
            current: idx,
            keys,
            send_keys: page.is_some_and(|p| p.send_keys),
        });
    }

    /// 面板上点了一个符号键帽。
    ///
    /// ⚠️ 走 `push_commit_text` 而不是返回 `KeyAction`：UI 事件不在按键路径上，没有那条
    /// 回程通道（同点击候选那条路）。文本仍要过 `maybe_s2t`——上屏出口只有一个。
    pub(crate) fn ui_softkeyboard_key(&self, slot: &str, shift: bool, ctrl: bool) {
        let Some(page) = self.softkeyboard.pages().get(self.softkeyboard_page_idx()) else {
            return;
        };
        // ── Ctrl 粘滞：一律合成组合键，**不看当前是哪种面** ──
        //
        // Ctrl+C 要的是复制，跟这一面在 `c` 那个位置画着什么符号没有关系。所以它排在
        // 取 `output` 之前——符号面上那些位置查表得到的是「©」之类，拿去当组合键毫无意义。
        if ctrl {
            let combo = if shift {
                format!("ctrl+shift+{slot}")
            } else {
                format!("ctrl+{slot}")
            };
            debug!("softkeyboard: 点击 {slot} -> 合成组合键 {combo:?}");
            self.softkeyboard_tap(&combo);
            return;
        }
        let Some(text) = page.output(slot, shift) else {
            return; // 空键位：面板上是灰的，点了不该有反应
        };
        // ── 键盘面：合成一次真实按键，交给输入法 ──
        //
        // ★ 发的是**键位**不是画布上那个字：键盘面的画布只管显示，点 `q` 就该发 q 键，
        // 于是它经宿主 → TSF → 常规输入链路，点 n-i-h-a-o 出的是「你好」。
        // 面板 `WS_EX_NOACTIVATE` 不抢焦点，合成的键落在真正的焦点窗口上。
        //
        // 这一轮不会被自己拦回来：C++ 侧在键盘面不启用软键盘总闸，Rust 侧
        // `handle_softkeyboard_key` 也对键盘面放行。
        if page.send_keys {
            let combo = if shift {
                format!("shift+{slot}")
            } else {
                slot.to_string()
            };
            debug!("softkeyboard: 点击 {slot} -> 合成按键 {combo:?}");
            self.softkeyboard_tap(&combo);
            return;
        }
        let out = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            self.maybe_s2t(&st, text)
        };
        debug!(
            "softkeyboard: 点击 {slot}{} -> {out:?}",
            if shift { "+shift" } else { "" }
        );
        self.push_commit_text(&out);
    }

    /// 面板上点了标签行 / 面名键。
    pub(crate) fn ui_softkeyboard_page(&self, idx: usize) {
        if idx >= self.softkeyboard.len() {
            warn!(
                "软键盘: 面下标 {idx} 越界（共 {} 面）",
                self.softkeyboard.len()
            );
            return;
        }
        self.softkeyboard_page.store(idx, Ordering::Relaxed);
        self.push_softkeyboard_view();
        // UI 事件不在按键路径上，没有那个 RAII guard 兜底，得自己推：切面可能改变
        // `STATUS_SOFT_KEYBOARD_KEYS`，C++ 的吃键判定要跟上。
        self.push_state_update();
    }

    /// 面板上点了关闭按钮。
    pub(crate) fn ui_softkeyboard_close(&self) {
        self.close_softkeyboard();
        self.push_state_update();
    }

    /// 面板上点了特殊键（退格 / Tab / 回车 / 空格 / Ins / Del）。
    ///
    /// 焦点在宿主、我们不能插文本，只能让那个键真的发生一次。本仓「勿用按键模拟实现
    /// `type()`」的禁令**不适用**——那条禁的是把待上屏文本降级成按键序列（会绕开 CR
    /// 规范化与跟打统计）；功能键点击走的正是 `key.tap` 那条正路。
    pub(crate) fn ui_softkeyboard_fn_key(&self, name: &str) {
        self.softkeyboard_tap(name);
    }

    /// 合成一次真实按键。功能键点击与键盘面的键帽点击共用。
    fn softkeyboard_tap(&self, combo: &str) {
        #[cfg(windows)]
        {
            use wind_cmdbar::KeyInjector;
            if let Err(e) = wind_keys::key_inject::SysKeys.tap(combo) {
                warn!("软键盘: 合成按键 {combo:?} 失败: {e}");
            }
        }
        #[cfg(not(windows))]
        {
            let _ = combo;
        }
    }
}

/// 按键返回时把软键盘状态推给 C++（`STATUS_SOFT_KEYBOARD` 位）。
///
/// ★ 用 RAII 而不是在各 return 点手写：`handle_key_event` 有几十个 return，漏一个的
/// 症状极隐蔽——Rust 这边认为软键盘开着并接管按键，C++ 那边仍按「没开」判定而不吃
/// 数字键，于是数字行整行失效、别的键都正常。
///
/// ⚠️ **必须声明在函数最开头**：局部变量按声明的逆序析构，声明得最早 ⇒ 析构得最晚
/// ⇒ 那时 `state` 的 `MutexGuard` 已经还了，推送里的 `state.lock()` 不会撞上自己。
pub(crate) struct SoftKeyboardPushOnDrop<'a>(pub(crate) &'a Coordinator);

impl Drop for SoftKeyboardPushOnDrop<'_> {
    fn drop(&mut self) {
        if self.0.softkeyboard_dirty.swap(false, Ordering::Relaxed) {
            self.0.push_state_update();
        }
    }
}

#[cfg(test)]
#[path = "handle_softkeyboard_tests.rs"]
mod tests;
