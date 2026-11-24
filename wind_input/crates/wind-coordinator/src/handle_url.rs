//! 网址输入模式（劫持缓冲 + 回退），兼所有前缀夺取式模式的共享骨架。
//!
//! [`Coordinator::try_prefix_hijack`]（入口闸门）、[`Coordinator::active_hijack_buffer`]、
//! [`Coordinator::can_rewind`]、[`Coordinator::rewind_hijack`] 四个不属于 url，是
//! url / unicode / email 等夺取模式共用的骨架。新增前缀夺取式模式要碰哪些地方，见
//! `docs/design/prefix-hijack-modes.md` 的清单。

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
    /// 在逐键热路径上且出厂全关，未开启时代价必须是常数：先在同一次 `rt()` 借用里读完所有
    /// 总开关，全关立刻返回，不做键码转换、不构造 `String`、不遍历前缀表。
    ///
    /// 新增模式 = 开关元组加一位 + 下方链里加一条分支。刻意用显式分支而非注册表：优先级由
    /// 代码顺序表达（同 `try_activate_mode` 的取舍）。
    pub(crate) fn try_prefix_hijack(
        &self,
        state: &mut State,
        data: &KeyEventData,
    ) -> Option<KeyAction> {
        let (url_on, unicode_on, email_on) = {
            let rt = self.rt();
            (
                rt.config.input.url.enabled,
                rt.config.input.unicode.enabled,
                rt.config.input.email.enabled,
            )
        };
        if !url_on && !unicode_on && !email_on {
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
        // Unicode：字面比较，不归一大小写（理由见 `UnicodeConfig::prefixes`）。
        if unicode_on {
            let probe = format!("{}{}", state.input_buffer, ch);
            if self.is_unicode_prefix(&probe) {
                return Some(self.enter_unicode_mode(state, probe, RewindOrigin::Normal));
            }
        }
        // 邮箱：后缀触发——「本键是 `@` 且缓冲非空」，缓冲整体作为用户名带进模式（见
        // `handle_email.rs` 文件头与 `docs/design/prefix-hijack-modes.md` §5.2）。
        //
        // 排在最后：用户可能把 `@` 配进 `input.url.prefixes`，显式配置的前缀优先。
        // 空缓冲不触发：否则单独打 `@` 都会掉进邮箱模式。
        //
        // 分段上屏中途不夺取（`committed_text` 非空）：夺取不处置 `committed_text`，已选的半句
        // 会成孤儿、随后被拼回 preedit 造成乱序。url / unicode 结构上有同一个洞，但触发面窄
        // （缓冲须恰等于前缀去末键），另案处理，不在这里改动既有行为。
        if email_on
            && ch == crate::handle_email::EMAIL_AT
            && !state.input_buffer.is_empty()
            && state.committed_text.is_empty()
        {
            let buffer = format!("{}{}", state.input_buffer, crate::handle_email::EMAIL_AT);
            return Some(self.enter_email_mode(state, buffer, RewindOrigin::Normal));
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
        // 历史补全候选（`input.url.history_enabled` 关着时恒空）。候选窗照常显示——
        // 空候选时它给的是「正在输入网址」的模式徽标提示。
        self.update_url_candidates(state);
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
        // 候选与翻页视图必须一起清：否则退出后 `state.candidates` 仍非空而候选窗已隐藏，下一次
        // 空格 / 数字键 / 翻页会命中这批幽灵候选、凭空上屏一条历史网址，`has_input_session` 也被
        // 骗过。四条退出路径（上屏 / Esc / Ctrl-Alt 守卫 / 退格删空）都经这里。
        state.candidates.clear();
        self.reset_candidate_view(state);
        state.preedit.clear();
        state.rewind = None;
    }

    /// 当前夺取式模式的 buffer（用于回退边界判定）。非夺取式模式返回 None。
    pub(crate) fn active_hijack_buffer<'a>(&self, state: &'a State) -> Option<&'a str> {
        match state.active {
            Some(ModeKind::Url) => Some(&state.url_buffer),
            Some(ModeKind::Email) => Some(&state.email_buffer),
            Some(ModeKind::Unicode) => Some(&state.unicode_buffer),
            // z 夺取：仅 try_z_fallback 会同时武装 state.rewind，故 can_rewind 只对夺取式进入
            // 成立（符号/字母首键进入的这些模式 rewind=None，不会误回退）。
            Some(ModeKind::TempPinyin) => Some(&state.temp_pinyin_buffer),
            Some(ModeKind::TempEnglish) => Some(&state.temp_english_buffer),
            Some(ModeKind::Mix(_)) => Some(&state.mix_buffer),
            Some(ModeKind::RareChar) => Some(&state.special_buffer),
            Some(ModeKind::Reverse) => Some(&state.special_buffer),
            None | Some(ModeKind::Special(_) | ModeKind::AuxCode) => None,
        }
    }

    /// 是否可回退：已登记 + 当前模式 buffer 已退回到夺取边界（== 登记时的 host_text）
    /// + 没有分步上屏留下的已转换前缀。
    ///
    /// `committed_text` 非空时缓冲等于边界只是巧合（`lalal` 选「拉拉」后剩 `l`），此刻回退会把
    /// 「拉拉」一并清掉。判据收在这里而不是分步上屏时清 `rewind`：分步出口有多条，这里是唯一
    /// 汇合点；且段全部退回后那一帧正是夺取边界，回退仍应可用。
    pub(crate) fn can_rewind(&self, state: &State) -> bool {
        match (&state.rewind, self.active_hijack_buffer(state)) {
            (Some(rw), Some(buf)) => buf == rw.host_text && state.committed_text.is_empty(),
            _ => false,
        }
    }

    /// 执行夺取回退：撤销夺取，把快照回放到正常码表输入流并重算候选。
    pub(crate) fn rewind_hijack(&self, state: &mut State) -> KeyAction {
        let (snapshot, origin) = state
            .rewind
            .take()
            .map(|r| (r.snapshot, r.origin))
            .unwrap_or_default();
        // 退出当前夺取式模式：URL / z-fallback 的临拼、临英、mix、生僻字。
        // 必须与 `active_hijack_buffer` 枚举的模式一一对应：那边认得、这边漏了，
        // 就会走 `reset_exclusive_modes` 兜底——状态清得掉，但各模式自己的收尾
        // （committed_segs、cursor、mix 的透镜态）不会跑，回退后留下半清理的残局。
        match state.active {
            Some(ModeKind::Url) => self.exit_url_mode(state),
            Some(ModeKind::Email) => self.exit_email_mode(state),
            Some(ModeKind::Unicode) => self.exit_unicode_mode(state),
            Some(ModeKind::TempPinyin) => self.exit_temp_pinyin(state),
            Some(ModeKind::TempEnglish) => self.exit_temp_english(state),
            Some(ModeKind::Mix(_)) => self.exit_mix_mode(state),
            Some(ModeKind::RareChar) => self.exit_special_mode(state),
            Some(ModeKind::Reverse) => self.exit_special_mode(state),
            None | Some(ModeKind::Special(_) | ModeKind::AuxCode) => {
                self.reset_exclusive_modes(state)
            }
        }
        // 回放目标由来源决定，不是恒定的 `input_buffer`（见 `RewindOrigin`）。
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
            RewindOrigin::Mix { idx, prefix } => {
                // 重进同一个快捷输入实例，前缀原样放回（理由见 `RewindOrigin::Mix`）。
                // 不走 `enter_mix_mode`：它按进入键重算前缀、并把缓冲清空。
                //
                // 若该快捷输入当初是 z 夺取进来的，那份回退登记已在转交邮箱时被覆盖，
                // 回到这里后再退到边界不会继续退回码表缓冲——`Rewind` 只有一层，临英转交
                // 同此。
                state.active = Some(ModeKind::Mix(idx));
                state.mix_id = idx;
                state.mix_buffer = snapshot;
                state.mix_cursor = state.mix_buffer.len();
                state.mix_prefix = prefix;
                self.update_mix_candidates(state);
                let caret_pos = self.overlay_caret(state);
                self.notify_ui_update(state);
                debug!("rewind_hijack: restored quick input '{}'", state.mix_buffer);
                KeyAction::UpdateComposition {
                    text: state.preedit.clone(),
                    caret_pos,
                }
            }
        }
    }

    /// 网址模式按键处理：可见 ASCII 原样累积；空格/回车上屏原文；退格删空退出；Esc 放弃。
    pub(crate) fn handle_url_key(&self, state: &mut State, data: &KeyEventData) -> KeyAction {
        // 缓冲变化后：重算历史补全候选 + 同步 preedit + 刷新候选窗（无候选时保留
        // 「网址输入」徽标），再返回组合区动作。
        let refresh = |this: &Self, state: &mut State| -> KeyAction {
            this.update_url_candidates(state);
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
        // 会话态按键绑定（`keys.session_actions`），与其余 overlay 一样经 `handle_candidate_nav`。
        // 新增一类动词时要重查每条通路都接了这个消费点，别因「现有动词在这条路上没意义」就漏接。
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
            // 空格选候选、回车上屏原文（分工见 `mode_completion.rs` 文件头）。都不做
            // 全半角/标点转换。回车这一支与加补全之前逐字相同——那时恒无候选，回车本
            // 就是上屏原文。收口在 `commit_url`，与邮箱模式共用同一段语义。
            // 有候选那一支与 `commit_highlighted` 动词同一出口；无候选上屏原文。
            keymap::VK_SPACE => self
                .commit_highlighted(state)
                .unwrap_or_else(|| self.commit_url(state, true)),
            keymap::VK_RETURN => self.commit_url(state, false),
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
