//! 网址输入模式（劫持缓冲 + 回退），**兼所有前缀夺取式模式的共享骨架**。
//!
//! 从 coordinator.rs 拆出（同 crate 内 `impl Coordinator` 块，组织性重构，无逻辑变更）。
//!
//! # 本文件里哪些是共享的
//!
//! [`Coordinator::try_prefix_hijack`]（入口闸门）、[`Coordinator::active_hijack_buffer`]、
//! [`Coordinator::can_rewind`]、[`Coordinator::rewind_hijack`] 四个不属于 url，是
//! url / unicode / z 夺取共用的骨架。新增一个前缀夺取式模式要碰哪些地方，见
//! `docs/design/prefix-hijack-modes.md` 的清单——那份清单是照着本文件与
//! `handle_unicode.rs` 的实际接线点列的。

use crate::coordinator::{Coordinator, State, numpad_char, printable_char};
use crate::pipeline::{ModeKind, Rewind, RewindOrigin};
use crate::preedit_cursor;
use tracing::debug;
use wind_bridge::handler::{KeyAction, KeyEventData};
use wind_ipc::protocol::MOD_SHIFT;
use wind_keys::keymap;

impl Coordinator {
    /// 前缀夺取闸门：正常输入累积到某个已注册前缀时，夺取进入对应模式。
    ///
    /// # ★ 未开启时的代价必须是常数
    ///
    /// 本函数在**逐键热路径**上，而出厂态两个模式都关着——绝大多数用户永远不该为它们
    /// 付出任何代价。故先一次性读完所有总开关，全关就立刻返回：不做 `printable_char`
    /// 键码转换、不构造探针 `String`、不遍历前缀表。
    ///
    /// 两个开关刻意在**同一次** `rt()` 借用里读完，而不是每个模式各借一次——那样加一个
    /// 模式就多一次运行时借用，正是「未开启也变慢」的来源。
    ///
    /// # 扩展
    ///
    /// 新增一个前缀夺取式模式 = 开关元组加一位 + 下方链里加一条分支。刻意保留**显式
    /// 分支**而不做注册表：优先级由代码顺序表达，一眼可读可调。这与 `try_activate_mode`
    /// 的既有取舍一致（见 `handle_mode.rs` 「不强塞统一表，避免死抽象」）。
    pub(crate) fn try_prefix_hijack(
        &self,
        state: &mut State,
        data: &KeyEventData,
    ) -> Option<KeyAction> {
        let (url_on, unicode_on) = {
            let rt = self.rt();
            (rt.config.input.url.enabled, rt.config.input.unicode.enabled)
        };
        if !url_on && !unicode_on {
            return None;
        }
        let shift = data.modifiers & MOD_SHIFT != 0;
        let ch = printable_char(data.key_code, shift)?;
        // 网址：前缀表按惯例小写，故探针把字母归一到小写再比。
        if url_on {
            let probe = format!("{}{}", state.input_buffer, ch.to_ascii_lowercase());
            if self.is_url_prefix(&probe) {
                return Some(self.enter_url_mode(state, probe));
            }
        }
        // Unicode：**字面**比较，不归一大小写（理由见 `UnicodeConfig::prefixes`）。
        if unicode_on {
            let probe = format!("{}{}", state.input_buffer, ch);
            if self.is_unicode_prefix(&probe) {
                return Some(self.enter_unicode_mode(state, probe, RewindOrigin::Normal));
            }
        }
        None
    }

    /// 探针是否恰好等于某个网址前缀（精确匹配，对齐 Go urlActivationResidual 的全匹配语义）。
    pub(crate) fn is_url_prefix(&self, probe: &str) -> bool {
        self.rt()
            .config
            .input
            .url
            .prefixes
            .iter()
            .any(|p| !p.is_empty() && p == probe)
    }

    /// 进入网址模式：以补全前缀的完整文本作初始缓冲，清空普通输入/候选。
    /// 网址模式无候选，但保留候选窗显示「网址输入」模式徽标 + 累积文本（对齐 Go showUrlUI）。
    /// 同时登记夺取回退：snapshot=夺取前的正常输入（=前缀去掉补全键），host_text=完整前缀。
    pub(crate) fn enter_url_mode(&self, state: &mut State, buffer: String) -> KeyAction {
        // 夺取前的正常 input_buffer 即回退快照（前缀的最后一字符是刚补全的那一键）。
        let snapshot = state.input_buffer.clone();
        state.input_buffer.clear();
        state.candidates.clear();
        state.active = Some(ModeKind::Url);
        state.url_buffer = buffer.clone();
        state.url_cursor = state.url_buffer.len(); // 夺取进入时缓冲已有内容，光标落末尾
        state.preedit = buffer.clone();
        state.rewind = Some(Rewind {
            snapshot,
            host_text: buffer,
            origin: RewindOrigin::Normal, // 前缀夺取抢的是正常码表输入流
        });
        // 显示候选窗（空候选 + 模式徽标）而非隐藏，给出「正在输入网址」提示。
        self.notify_ui_update(state);
        let disp = state.url_buffer.clone();
        debug!("Entered URL mode (buffer={})", disp);
        KeyAction::UpdateComposition {
            text: disp.clone(),
            caret_pos: disp.chars().count() as u32,
        }
    }

    /// 退出网址模式并清空相关状态（含作废回退登记）。
    pub(crate) fn exit_url_mode(&self, state: &mut State) {
        state.active = None;
        state.url_buffer.clear();
        state.url_cursor = 0;
        state.preedit.clear();
        state.rewind = None;
    }

    /// 当前夺取式模式的 buffer（用于回退边界判定）。非夺取式模式返回 None。
    pub(crate) fn active_hijack_buffer<'a>(&self, state: &'a State) -> Option<&'a str> {
        match state.active {
            Some(ModeKind::Url) => Some(&state.url_buffer),
            Some(ModeKind::Unicode) => Some(&state.unicode_buffer),
            // z 夺取：仅 try_z_fallback 会同时武装 state.rewind，故 can_rewind 只对夺取式进入
            // 成立（符号/字母首键进入的这些模式 rewind=None，不会误回退）。
            Some(ModeKind::TempPinyin) => Some(&state.temp_pinyin_buffer),
            Some(ModeKind::TempEnglish) => Some(&state.temp_english_buffer),
            Some(ModeKind::Mix(_)) => Some(&state.mix_buffer),
            _ => None,
        }
    }

    /// 是否可回退：已登记 + 当前模式 buffer 已退回到夺取边界（== 登记时的 host_text）。
    pub(crate) fn can_rewind(&self, state: &State) -> bool {
        match (&state.rewind, self.active_hijack_buffer(state)) {
            (Some(rw), Some(buf)) => buf == rw.host_text,
            _ => false,
        }
    }

    /// 执行夺取回退：撤销夺取，把快照回放到正常码表输入流并重算候选。
    pub(crate) fn rewind_hijack(&self, state: &mut State) -> KeyAction {
        let rw = state.rewind.take();
        let origin = rw.as_ref().map(|r| r.origin).unwrap_or_default();
        let snapshot = rw.map(|r| r.snapshot).unwrap_or_default();
        // 退出当前夺取式模式：URL / z-fallback 的临拼、临英、mix。
        // ⚠️ 必须与 `active_hijack_buffer` 枚举的模式**一一对应**：那边认得、这边漏了，
        // 就会走 `reset_exclusive_modes` 兜底——状态清得掉，但各模式自己的收尾
        // （committed_segs、cursor、mix 的透镜态）不会跑，回退后留下半清理的残局。
        match state.active {
            Some(ModeKind::Url) => self.exit_url_mode(state),
            Some(ModeKind::Unicode) => self.exit_unicode_mode(state),
            Some(ModeKind::TempPinyin) => self.exit_temp_pinyin(state),
            Some(ModeKind::TempEnglish) => self.exit_temp_english(state),
            Some(ModeKind::Mix(_)) => self.exit_mix_mode(state),
            _ => self.reset_exclusive_modes(state),
        }
        // 回放目标由**来源**决定，不是恒定的 `input_buffer`（见 `RewindOrigin`）。
        match origin {
            RewindOrigin::Normal => {
                state.input_buffer = snapshot;
                state.input_cursor_pos = state.input_buffer.len(); // 光标落到恢复码末尾
                self.update_candidates(state);
                self.notify_ui_update(state);
                let display = state.preedit.clone();
                debug!(
                    "rewind_hijack: restored normal input '{}'",
                    state.input_buffer
                );
                KeyAction::UpdateComposition {
                    text: display,
                    caret_pos: state.input_buffer.chars().count() as u32,
                }
            }
            RewindOrigin::TempEnglish => {
                // 重进临英而不是把快照塞进码表缓冲：来源是 Shift+U 那条路，用户的上下文
                // 是「我在打英文」，退格该退回英文缓冲。`temp_english_prefix` 清空——
                // Shift+字母进入的临英本就没有前缀字符（触发键进入的才有）。
                state.active = Some(ModeKind::TempEnglish);
                state.temp_english_buffer = snapshot;
                state.temp_english_cursor = state.temp_english_buffer.len();
                state.temp_english_prefix = String::new();
                self.update_temp_english_candidates(state);
                self.notify_ui_update(state);
                let display = state.preedit.clone();
                debug!(
                    "rewind_hijack: restored temp English '{}'",
                    state.temp_english_buffer
                );
                KeyAction::UpdateComposition {
                    text: display.clone(),
                    caret_pos: display.chars().count() as u32,
                }
            }
        }
    }

    /// 网址模式按键处理：可见 ASCII 原样累积；空格/回车上屏原文；退格删空退出；Esc 放弃。
    pub(crate) fn handle_url_key(&self, state: &mut State, data: &KeyEventData) -> KeyAction {
        // 缓冲变化后：同步 preedit + 刷新候选窗（保留「网址输入」徽标），再返回组合区动作。
        let refresh = |this: &Self, state: &mut State| -> KeyAction {
            state.preedit = state.url_buffer.clone();
            this.notify_ui_update(state);
            KeyAction::UpdateComposition {
                text: state.url_buffer.clone(),
                caret_pos: this.overlay_caret(state),
            }
        };
        // Ctrl/Alt 组合守卫（见 `overlay_ctrl_alt_guard`）：必须最先，否则组合键会落到
        // 下方 `printable_char` 臂被当成网址字符（`Ctrl+V` 粘贴时凭空多一个 v）。
        if let Some(act) =
            self.overlay_ctrl_alt_guard(state, data, !state.url_buffer.is_empty(), |s, st| {
                s.exit_url_mode(st)
            })
        {
            return act;
        }
        // 会话态按键绑定（`keys.session_actions`）。⚠️ 网址模式此前是**唯一**没接这条的
        // overlay——另外四个都经 `handle_candidate_nav` 接了。一期没暴露是因为那时的动词
        // 全是导航类，而网址模式原样累积文本、从不产候选，导航在这里本就无事可做；二期
        // 的 `cancel` 一加进来，缺口立刻变成「Tab 在网址模式里按了没反应」。
        //
        // ★ 判据：**新增一类动词时，要重查每条通路是不是都接了这个消费点**，不能因为
        // 「现有动词在那条路上没意义」就默认它不需要接。这与本仓「一个能力多条通路、
        // 闸门必须每条都接」是同一条，那个已经栽过四次。
        if let Some(act) = self.handle_candidate_nav(state, data) {
            return act;
        }
        // 编码区光标移动（左右 / Home / End）
        if let Some(act) = self.overlay_cursor_key(state, data) {
            return act;
        }
        match data.key_code {
            // Esc：放弃退出（无上屏），实现收口在 `cancel_session`。
            keymap::VK_ESCAPE => self.cancel_session(state),
            keymap::VK_BACK | keymap::VK_DELETE => {
                // 退格删光标前 / Delete 删光标后。缓冲被删空 → 退出模式（无论前删后删，否则会
                // 留下空组合区）；本就空缓冲时只有退格退出（保持原语义），Delete 只吃键。
                let backward = data.key_code == keymap::VK_BACK;
                let removed = {
                    let mut ed =
                        preedit_cursor::BufEdit::new(&mut state.url_buffer, &mut state.url_cursor);
                    if backward {
                        ed.backspace()
                    } else {
                        ed.delete()
                    }
                };
                if state.url_buffer.is_empty() && (removed || backward) {
                    self.exit_url_mode(state);
                    self.notify_ui_hide();
                    KeyAction::ClearComposition
                } else if removed {
                    refresh(self, state)
                } else {
                    // 退格时光标已在最左 / Delete 时已在末尾：吃掉不透传。
                    KeyAction::Consumed
                }
            }
            keymap::VK_SPACE | keymap::VK_RETURN => {
                // 空格/回车：上屏当前缓冲原文（不做全半角/标点转换）
                let text = state.url_buffer.clone();
                self.record_commit(&text, 0, -1, wind_store::stats::CommitSource::Url);
                self.exit_url_mode(state);
                self.notify_ui_hide();
                if text.is_empty() {
                    KeyAction::ClearComposition
                } else {
                    Self::commit_action(text, true)
                }
            }
            _ => {
                let shift = data.modifiers & MOD_SHIFT != 0;
                // 小键盘键（direct 语义）回退 numpad_char：网址缓冲是文本，数字/`.`/`-`/`/`
                // 都是合法网址内容 → 与主键盘同样入缓冲（follow_main 时键已在入口归一化）。
                if let Some(ch) =
                    printable_char(data.key_code, shift).or_else(|| numpad_char(data.key_code))
                {
                    preedit_cursor::BufEdit::new(&mut state.url_buffer, &mut state.url_cursor)
                        .insert(ch);
                    refresh(self, state)
                } else {
                    // 其它非可打印键：消费但不改缓冲
                    KeyAction::Consumed
                }
            }
        }
    }
}
