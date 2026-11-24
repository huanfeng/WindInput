//! 标点编排 + 智能符号状态机
//!
//! 纯转换逻辑在 wind-punct crate；此处是 coordinator 包装（锁转换器/读状态）+ 智能符号
//! 连按替换的状态机（武装/触发/解除）。

use crate::config_bundle::ConfigBundle;
use crate::coordinator::{Coordinator, State, full_width_source_char, numpad_char, punct_char};
use tracing::debug;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::config::SmartMethod;
use wind_ipc::protocol::{MOD_SHIFT, MOD_SHORTCUT};
use wind_keys::keymap;

/// 恰好单字符则返回之，否则 None（自定义映射可为多字符串，不能充当配对符）。
fn single_char(s: &str) -> Option<char> {
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

impl Coordinator {
    /// 当前输入语境下生效的标点配置，`wind_punct::*` 读 `punct.*` 都从这里取。
    ///
    /// 归属方案取 `effective_data_schema` 而非活跃方案：自定义映射是数据，与 `[phrases]` 同源，
    /// 按活跃方案取会让快符/临英方案的符号表永不生效。同段的 `[punct] mode` 刻意走活跃方案，
    /// 理由见 [`wind_config::PunctSpec`]，别统一成一个判据。
    ///
    /// 方案声明了表就整表替换，连 `custom_enabled` 一起换（`Some({})` = 本方案不用自定义映射）；
    /// `smart_after_digit` / `smart_list` 不下放，仍取全局值。无方案表时借用全局那份，不 clone。
    pub(crate) fn effective_punct<'a>(
        &self,
        bundle: &'a ConfigBundle,
        state: &State,
    ) -> std::borrow::Cow<'a, wind_config::config::PunctConfig> {
        let global = &bundle.config.input.punct;
        let behavior = match self.effective_data_schema(state) {
            Some(id) => self.engine_mgr.behavior_for(&id),
            None => self.engine_mgr.active_behavior(),
        };
        match &behavior.punct_custom_mappings {
            None => std::borrow::Cow::Borrowed(global),
            Some(table) => {
                let mut p = global.clone();
                p.custom_enabled = !table.is_empty();
                p.custom_mappings = table.clone();
                std::borrow::Cow::Owned(p)
            }
        }
    }

    /// 数字后智能标点：在中文标点模式下，若 ch 在智能标点列表且光标前一字符为数字，
    /// 则该标点应按英文（半角）输出（如 "3." 不转成 "3。"）。
    ///
    /// `smart_after_digit` / `smart_list` 不在方案级下放范围，直接取全局那份。
    pub(crate) fn is_smart_punct_after_digit(&self, ch: char, prev_char: u16) -> bool {
        wind_punct::is_smart_punct_after_digit(&self.rt().config.input.punct, ch, prev_char)
    }

    /// 「标点前一字符是拉丁字母」的默认口径：锁存的 `state.spacing_prev`（见 `crate::auto_spacing`）。
    ///
    /// 只在无输入会话（或联想态）时成立；组合中它停在组合开始前，顶屏出口要用被顶出文本的
    /// 末字符，走 [`Self::convert_punct_after`]。
    fn latin_before_idle(state: &State) -> bool {
        (!Self::has_input_session(state) || state.assoc_active())
            && state.spacing_prev == wind_punct::spacing::CharClass::Latin
    }

    /// 英文后智能标点（反向判定用）：同 [`Self::is_smart_punct_after_digit`]，判据见
    /// [`Self::latin_before_idle`]。
    pub(crate) fn is_smart_punct_after_latin_idle(&self, state: &State, ch: char) -> bool {
        wind_punct::is_smart_punct_after_latin(
            &self.rt().config.input.punct,
            ch,
            Self::latin_before_idle(state),
        )
    }

    /// 小键盘恒半角（`input.numpad_half_width`）对本次按键是否生效。
    ///
    /// 为真时调用方必须原样输出 ASCII，跳过整条标点流水线（不只是全半角一步），
    /// `direct` 与 `follow_main` 两档在此统一。凡是小键盘字符直接上屏的路径都必须问它，漏接一处
    /// 就是某模式下开关静默无效。现有调用点：`convert_punct`、`commit_highlight_then_char`、
    /// 临拼 numpad 臂、数字键越界 `commit_and_input`。缓冲类模式（临英 / URL / mix）刻意不接，
    /// 见 `InputConfig::numpad_half_width`。
    pub(crate) fn numpad_raw_output(&self, state: &State) -> bool {
        state.numpad_origin && self.rt().config.input.numpad_half_width
    }

    /// 按当前中英标点/全半角配置转换一个标点字符为上屏文本（无 prev_char 上下文）。
    /// 用于独占模式（快捷输入/临时英文）等不涉及数字后智能的场景。
    pub(crate) fn convert_punct_char(&self, state: &State, ch: char) -> String {
        self.convert_punct(state, ch, 0)
    }

    /// 空缓冲的空格 DLL 吃没吃：全角态恒吃；中文模式下还看空格行当前列配没配映射
    /// （`CONFIG_KEY_SPACE_CUSTOM_COLS`，与 DLL 同一份掩码）。
    pub(crate) fn idle_space_eaten(&self, state: &State) -> bool {
        let col = wind_punct::punct_col_idx(state.chinese_punct, state.full_width);
        state.full_width || (state.chinese_mode && self.rt().space_custom_cols & (1 << col) != 0)
    }

    /// 标点转换单点流水线（对齐 Go `convertPunct`，固定优先级）：
    ///   1. 自定义映射（四状态：中半 0 / 英全 1 / 中全 2 / 英半 3，按当前中英标点+全半角选列）
    ///   2. 数字后智能转换（命中则该标点按英文输出，不转中文）
    ///   3. 中文标点转换（引号左右交替状态机）
    ///   4. 全半角转换
    ///
    /// `prev_char` 为光标前一字符的 UTF-16 单元（0=不可用），用于数字后智能判定。
    pub(crate) fn convert_punct(&self, state: &State, ch: char, prev_char: u16) -> String {
        self.convert_punct_latin(state, ch, prev_char, Self::latin_before_idle(state))
    }

    /// 顶屏出口的标点转换：标点紧跟在 `committed`（被顶出的候选 / 原码）后面上屏，「英文后」
    /// 看它的末字符；`committed` 为空时退回 [`Self::convert_punct`] 的默认口径。
    pub(crate) fn convert_punct_after(
        &self,
        state: &State,
        ch: char,
        prev_char: u16,
        committed: &str,
    ) -> String {
        let latin = match committed.chars().last() {
            Some(c) => wind_punct::spacing::classify(c) == wind_punct::spacing::CharClass::Latin,
            None => Self::latin_before_idle(state),
        };
        self.convert_punct_latin(state, ch, prev_char, latin)
    }

    fn convert_punct_latin(&self, state: &State, ch: char, prev_char: u16, latin: bool) -> String {
        // 小键盘恒半角的主收口：整条流水线跳过；`ch` 进来时已是 ASCII 原形，原样返回。
        if self.numpad_raw_output(state) {
            return ch.to_string();
        }
        let bundle = self.rt();
        let punct = self.effective_punct(&bundle, state);
        let mut conv = self.punct.lock().unwrap_or_else(|e| e.into_inner());
        wind_punct::convert_punct_ctx(
            &mut conv,
            &punct,
            state.chinese_punct,
            state.full_width,
            ch,
            prev_char,
            latin,
        )
    }

    /// 引号 × 自动配对：若 `ch` 是引号、且其当前模式下的实际左右形在生效配对表中构成一对，
    /// 则把交替态钉回「左」并返回 true。
    ///
    /// 不钉会「一次出对、一次出单」：配对一次吐出左右两个引号，交替开关却只前进一格，
    /// 下次按键给出右引号走右符号分支。钉左即把左右判定单一收口到配对栈。
    ///
    /// 左右形必须取 [`wind_punct::quote_forms`]（含自定义的 `"1`/`"2`），不可用内置 `quote_pair`，
    /// 否则引号自定义成 `「」` 后判定不命中却照样配对插入。多字符自定义值不能充当配对符，返回 false。
    /// 须在标点流水线之前调用：钉的是本次转换的输入态。
    pub(crate) fn pin_quote_left_if_paired(&self, state: &State, ch: char) -> bool {
        let bundle = self.rt();
        let Some((l, r)) = wind_punct::quote_forms(
            &self.effective_punct(&bundle, state),
            state.chinese_punct,
            state.full_width,
            ch,
        ) else {
            return false;
        };
        let (Some(l), Some(r)) = (single_char(&l), single_char(&r)) else {
            return false;
        };
        let Some(pairs) = self.active_pairs(state.chinese_punct) else {
            return false;
        };
        if !pairs.contains(&(l, r)) {
            return false;
        }
        self.punct
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pin_quote_left(ch);
        true
    }

    /// 智能符号模式判定时限（非法值回退 500ms）。
    pub(crate) fn smart_symbol_timeout(&self) -> std::time::Duration {
        let ms = self.rt().config.input.symbol.smart_timeout_ms;
        let ms = if ms <= 0 { 500 } else { ms };
        std::time::Duration::from_millis(ms as u64)
    }

    /// 无副作用地计算 `ch` 在当前模式下的标点产物，**镜像** `convert_punct` 优先级
    /// （自定义列 > 中/英转换 > 全半角）。对齐 Go `computePunctStrPure`。
    ///   - `chinese=true`：算中文标点产物（武装/匹配用，引号经 peek 预测不改状态）。
    ///   - `chinese=false`：算英文标点产物（替换用，即该键英文模式下输出）。
    ///
    /// 引号有状态、键名特殊，此处保守跳过自定义、走标准引号/英文产物。
    pub(crate) fn compute_punct_str_pure(
        &self,
        state: &State,
        ch: char,
        chinese: bool,
    ) -> Option<String> {
        let bundle = self.rt();
        let punct = self.effective_punct(&bundle, state);
        let conv = self.punct.lock().unwrap_or_else(|e| e.into_inner());
        wind_punct::compute_punct_str_pure(&conv, &punct, state.full_width, ch, chinese)
    }

    /// 判断中文标点串 `cn` 是否在用户配置的参与集合内（子串包含匹配，支持多字符/引号）。
    pub(crate) fn smart_symbol_participates(&self, cn: &str) -> bool {
        wind_punct::participates(&self.rt().config.input, cn)
    }

    /// press1 实际会上屏的串（无副作用镜像 `convert_punct`）。
    ///
    /// 与 [`Self::compute_punct_str_pure`] 只差英文半角这一格：pure 刻意不查自定义（它是 press2 的
    /// 替换目标），而武装串必须等于真正插进文档的东西，否则 press2 的 `prev_char` 比对永远失配。
    /// 三条反向通路（数字后 / 英文标点状态 / 英文输入模式）的 press1 都落英文列，必须走这里。
    fn press1_committed_str(&self, state: &State, ch: char, chinese: bool) -> Option<String> {
        if !chinese && !state.full_width {
            let bundle = self.rt();
            let punct = self.effective_punct(&bundle, state);
            let conv = self.punct.lock().unwrap_or_else(|e| e.into_inner());
            // 英半列（col 3）：先查自定义，无值回落原样 ASCII——与 `convert_punct` 同一条路。
            if let Some(v) = wind_punct::custom_lookup(&conv, &punct, ch, 3) {
                return Some(v);
            }
            return Some(ch.to_string());
        }
        self.compute_punct_str_pure(state, ch, chinese)
    }

    /// 智能符号三个总开关是否有任一开启（入口短路用；具体该走哪个由上下文判定各自再查）。
    fn any_smart_symbol_enabled(&self) -> bool {
        let s = &self.rt().config.input.symbol;
        s.smart_mode || s.english_punct_mode || s.english_mode
    }

    /// 计算 `ch` 本次按下会进入的武装态：`Some((press1 实际上屏串, reverse))`；不参与返回 None。
    ///
    /// 判据是实际会上屏的产物，含自定义映射的产物（不给自定义键开后门，不想要就从
    /// `smart_chars` 移除）。数字后 / 英文后智能标点走反向：press1 仍出英文，时限内再按换回中文。
    /// 参与集合恒按中文产物查 `smart_chars`（反向时也一样，避免两方向答案不一致）；英文标点状态
    /// 则按源字符查 `english_chars`，见 [`wind_punct::english_participates`]。
    ///
    /// 优先级：智能符号（短路）> 自定义映射（定产物）> 自动配对。武装 `HoldComposition` 会直接
    /// return，配对当次不执行；排查「配对忽然不生效」先看日志有无 `SmartSymbol(HoldComposition): press1`。
    ///
    /// 只管中文输入模式；英文输入模式走 [`Self::english_mode_smart_symbol`]。
    pub(crate) fn smart_symbol_arm_str(
        &self,
        state: &State,
        ch: char,
        prev_char: u16,
    ) -> Option<(String, bool)> {
        let bundle = self.rt();
        let sym = &bundle.config.input.symbol;
        // 上下文一：英文标点状态（中文输入 + 工具栏标点切英文）。恒反向，独立开关、独立集合。
        if !state.chinese_punct {
            if !sym.english_punct_mode
                || !wind_punct::english_participates(&bundle.config.input, ch)
            {
                return None;
            }
            let out = self.press1_committed_str(state, ch, false)?;
            return self.reject_if_auto_paired(state, out).map(|o| (o, true));
        }
        // 上下文二：中文标点状态。正向；其中数字后智能标点反向（press1 仍出英文）。
        if !sym.smart_mode {
            return None;
        }
        // 英文后智能标点同样反向，但只认空闲态（见 `latin_before_idle`）：顶屏时 press1 武装在
        // 顶屏之前，拿不到被顶出候选的末字符，故不反向。
        let reverse = self.is_smart_punct_after_digit(ch, prev_char)
            || self.is_smart_punct_after_latin_idle(state, ch);
        let cn = self.compute_punct_str_pure(state, ch, true)?;
        if !self.smart_symbol_participates(&cn) {
            return None;
        }
        // 反向取英文产物，须与 `convert_punct` 产出同一个串，否则 press2 的删除数对不上。
        let out = if reverse {
            self.press1_committed_str(state, ch, false)?
        } else {
            cn
        };
        self.reject_if_auto_paired(state, out).map(|o| (o, reverse))
    }

    /// 与自动配对互斥：被配对的符号（单字符且在生效配对表）不武装智能符号，返回 None。
    ///
    /// 否则 press2 时 `prev_char` 恰为配对左符号，误删左符号、留下孤零零的右符号。
    /// 判据用实际上屏串（反向时是英文形）。
    fn reject_if_auto_paired(&self, state: &State, out: String) -> Option<String> {
        if out.chars().count() == 1 {
            let c0 = out.chars().next().unwrap();
            if self.is_auto_pair_char(state, c0) {
                return None;
            }
        }
        Some(out)
    }

    /// 智能符号替换判定（在标点分支入口调用）。对齐 Go `trySmartSymbolReplace`：
    ///   - 返回 Some：本次为 press2 触发，调用方应直接返回该替换响应（短路）。
    ///   - 返回 None：未触发；已按需更新武装态，调用方继续普通标点流程。
    pub(crate) fn try_smart_symbol_replace(
        &self,
        state: &State,
        ch: char,
        prev_char: u16,
    ) -> Option<KeyAction> {
        if !self.any_smart_symbol_enabled() {
            return None;
        }
        let method = self.effective_smart_method();
        let timeout_ms = self.smart_symbol_timeout().as_millis() as u32;
        let mut arm = self.smart_symbol.lock().unwrap_or_else(|e| e.into_inner());

        if let Some(act) = self.smart_symbol_press2(state, ch, prev_char, &method, &mut arm) {
            return Some(act);
        }

        // ── press1：尝试武装 ─────────────────────────────────────────────────────
        match self.smart_symbol_arm_str(state, ch, prev_char) {
            Some((out, reverse)) => {
                arm.key = ch;
                arm.str = out.clone();
                arm.at = Some(std::time::Instant::now());
                arm.reverse = reverse;
                arm.mode_snapshot = (state.chinese_mode, state.chinese_punct);

                match method {
                    SmartMethod::HoldComposition => {
                        let has_input =
                            !state.input_buffer.is_empty() || !state.committed_text.is_empty();
                        arm.armed = true;
                        if has_input {
                            // 有活跃编码时，不短路进入 hold composition——让调用方的标点分支
                            // 检测 hold_pending_commit 并生成 CommitAndHoldComposition：先顶屏
                            // 上屏候选，再开 HoldComposition 放入标点。
                            arm.held_text = None;
                            arm.hold_pending_commit = true;
                        } else {
                            arm.held_text = Some(out.clone());
                            debug!(
                                "SmartSymbol(HoldComposition): press1, hold composition: {}, reverse={}, timeout={}ms",
                                out, reverse, timeout_ms
                            );
                            // 短路返回：由 C++ 端负责开启组合态和计时，不走普通标点流程
                            return Some(KeyAction::HoldComposition {
                                text: out,
                                timeout_ms,
                            });
                        }
                    }
                    SmartMethod::DeleteReplace => {
                        arm.armed = true;
                        arm.held_text = None;
                        // 返回 None：调用方继续普通标点流程（CommitText "，"）
                    }
                }
            }
            None => {
                arm.armed = false;
                arm.held_text = None;
            }
        }
        None
    }

    /// press2 判定（同键、时限内）：命中则解除武装并返回替换响应，未命中返回 None 不动武装态。
    ///
    /// 替换目标由 `arm.reverse` 定：正向换英文产物、反向换中文产物，两方向共用同一套删改/组合覆盖机制。
    fn smart_symbol_press2(
        &self,
        state: &State,
        ch: char,
        prev_char: u16,
        method: &SmartMethod,
        arm: &mut crate::coordinator::SmartSymbolArm,
    ) -> Option<KeyAction> {
        if !arm.armed
            || ch != arm.key
            // 上下文必须与 press1 时一致，切了中英/标点模式就当全新 press1，不按旧方向删字。
            || arm.mode_snapshot != (state.chinese_mode, state.chinese_punct)
            // press1 之后缓冲必为空，此刻非空说明中间又敲了编码，替换会削掉候选。`prev_char`
            // 对照兜不住；只问缓冲不问 `candidates`（联想态须照常 press2）。这不是
            // `handle_key_event_policed` 解除武装的冗余副本：移动端只有这一道拦得住。
            // 见 docs/architecture/smart-symbol-compat-notes.md 的「press1 与 press2 之间又有输入」。
            || !state.input_buffer.is_empty()
            || !state.committed_text.is_empty()
            || !arm
                .at
                .map(|t| t.elapsed() < self.smart_symbol_timeout())
                .unwrap_or(false)
        {
            return None;
        }
        // 替换产物取武装方向的另一侧。
        let target_chinese = arm.reverse;
        // 引号交替态修正：正向时 press1 已由 `convert_punct` 推进过一格，换英文后须退回；
        // 反向时 press1 出的是英文（未推进），而 press2 真吐了一个中文引号，须补进一格。
        // 引号只有左/右两态，退回与补进是同一个翻转。
        let fix_quote = |this: &Self| {
            if ch == '\'' || ch == '"' {
                this.punct
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .revert_last_quote(ch);
            }
        };
        let dir = if target_chinese { "en->cn" } else { "cn->en" };

        match method {
            SmartMethod::HoldComposition if arm.held_text.is_some() => {
                // 正常 hold 路径：press1 时无活跃编码，组合态内无需 prev_char 验证，直接提交。
                let rep = self.compute_punct_str_pure(state, ch, target_chinese)?;
                arm.armed = false;
                arm.held_text = None;
                fix_quote(self);
                debug!(
                    "SmartSymbol(HoldComposition): press2 {}, commit: {}",
                    dir, rep
                );
                // 必须是 CommitReplacingHeld：InsertText 在 hold 活跃时是追加语义，会打出「。.」。
                Some(KeyAction::CommitReplacingHeld {
                    text: rep,
                    chinese_mode: state.chinese_mode,
                })
            }
            // 删改路径：DeleteReplace 方案，以及 HoldComposition 下 `held_text.is_none()` 的降级
            // （press1 时有活跃编码而顶屏提交，或模式进入键直接上屏）。
            //
            // 光标前字符须与武装串末位匹配；`prev_char == 0` 视为宿主读不回文档（微信/终端一族），
            // 而非不匹配，此时只信服务端武装态，否则这些宿主上永远判不成 press2。
            _ => {
                let armed_runes: Vec<char> = arm.str.chars().collect();
                let &last = armed_runes.last()?;
                if prev_char != 0 && last as u32 != prev_char as u32 {
                    return None;
                }
                let rep = self.compute_punct_str_pure(state, ch, target_chinese)?;
                arm.armed = false;
                fix_quote(self);
                debug!(
                    "SmartSymbol(replace): press2 {}, count={}, text={}",
                    dir,
                    armed_runes.len(),
                    rep
                );
                Some(KeyAction::ReplaceBackward {
                    count: armed_runes.len() as u32,
                    text: rep,
                })
            }
        }
    }

    /// 符号已经 `CommitText` 上屏后的智能符号武装，模式进入键二次按下专用。
    ///
    /// `;` / `` ` `` / `\` 这类被模式占用的键在模式内空缓冲时再按会上屏中文标点并退出模式
    /// （`handle_mode.rs` / `handle_temp.rs` / `handle_special.rs`），此处顺带武装。恒按删改语义
    /// （`held_text=None`），即使选了 `HoldComposition`。press2 的拦截点在 `try_activate_mode`
    /// 开头，必须早于模式激活链，否则空闲态再按会被模式进入抢走。
    pub(crate) fn arm_smart_symbol_after_commit(&self, state: &State, ch: char, out: &str) {
        if !self.rt().config.input.symbol.smart_mode || !state.chinese_punct {
            return;
        }
        // 参与集合按实际上屏串判定，与 `smart_symbol_arm_str` 同一判据。
        if !self.smart_symbol_participates(out) {
            return;
        }
        let mut arm = self.smart_symbol.lock().unwrap_or_else(|e| e.into_inner());
        arm.armed = true;
        arm.reverse = false;
        arm.key = ch;
        arm.str = out.to_string();
        arm.at = Some(std::time::Instant::now());
        arm.mode_snapshot = (state.chinese_mode, state.chinese_punct);
        arm.held_text = None;
        arm.hold_pending_commit = false;
        debug!("SmartSymbol(mode trigger): armed after commit: {}", out);
    }

    /// 英文输入模式（`chinese_mode=false`）的智能符号：press2 判定 + press1 武装，一次收口。
    ///
    /// 与中文输入模式分开：英文半角下标点键要先经 `CONFIG_KEY_CUSTOM_EN_PUNCT` 从 DLL 要回来，
    /// 故调用点只能在 [`Self::handle_english_custom_punct`] / [`Self::handle_english_full_width`]；
    /// 这条路上 `state.chinese_punct` 不代表产物列，上下文只看 `chinese_mode`；开关也独立。
    ///
    /// `committed` 是本次 press1 会上屏的串。返回 `Some` 表示这次是 press2，调用方应短路返回。
    ///
    /// 已知限制：英文半角下 press1 与 press2 之间打的字母服务端看不见，`prev_char` 恒 0 的宿主上
    /// 会误替换。见 docs/architecture/smart-symbol-compat-notes.md 的「press1 与 press2 之间又有输入」。
    pub(crate) fn english_mode_smart_symbol(
        &self,
        state: &State,
        ch: char,
        prev_char: u16,
        committed: &str,
    ) -> Option<KeyAction> {
        let bundle = self.rt();
        if !bundle.config.input.symbol.english_mode {
            return None;
        }
        let method = self.effective_smart_method();
        let mut arm = self.smart_symbol.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(act) = self.smart_symbol_press2(state, ch, prev_char, &method, &mut arm) {
            return Some(act);
        }
        // press1：仅参与集合内的键武装。调用点在配对分支之后，插对/跳出的键走不到这里
        // （理由同 `reject_if_auto_paired`）。
        if !wind_punct::english_participates(&bundle.config.input, ch) {
            arm.armed = false;
            arm.held_text = None;
            return None;
        }
        arm.armed = true;
        // 恒反向：英文模式 press1 出英文形，press2 换中文形。
        arm.reverse = true;
        arm.key = ch;
        arm.str = committed.to_string();
        arm.at = Some(std::time::Instant::now());
        arm.mode_snapshot = (state.chinese_mode, state.chinese_punct);
        // 恒按删改语义武装：press1 经 `CommitText` 真上屏，无组合态可覆盖。
        arm.held_text = None;
        arm.hold_pending_commit = false;
        debug!("SmartSymbol(english mode): press1 armed: {}", committed);
        None
    }

    /// 只做 press2 判定、不做 press1 武装，供 `try_activate_mode` 抢在模式激活链之前调用。
    /// 武装仍收口在 `try_smart_symbol_replace` 与 `arm_smart_symbol_after_commit`。
    pub(crate) fn try_smart_symbol_press2_only(
        &self,
        state: &State,
        ch: char,
        prev_char: u16,
    ) -> Option<KeyAction> {
        if !self.any_smart_symbol_enabled() {
            return None;
        }
        let method = self.effective_smart_method();
        let mut arm = self.smart_symbol.lock().unwrap_or_else(|e| e.into_inner());
        self.smart_symbol_press2(state, ch, prev_char, &method, &mut arm)
    }

    /// 解除智能符号待命态（焦点变化/模式切换等的防御性复位）。
    pub(crate) fn disarm_smart_symbol(&self) {
        let mut arm = self.smart_symbol.lock().unwrap_or_else(|e| e.into_inner());
        arm.armed = false;
        arm.reverse = false;
        arm.held_text = None;
        arm.hold_pending_commit = false;
        // HoldComposition 下未提交的组合由 C++ 端计时器到期提交，或焦点切换时自然结束。
    }

    /// 英文模式 + 全角：按键经完整标点流水线转全角后上屏（含自动配对）。
    ///
    /// 必须出字：这些键已被 TSF `OnTestKeyDown` 的 `english_fullwidth` 分支吃下，返回 None 会变成
    /// 「吃了再吐」，Chrome/Electron 等严格宿主上键直接丢失。接住的键集必须覆盖 C++ 吃键集，同增同减。
    pub(crate) fn handle_english_full_width(
        &self,
        state: &mut State,
        data: &KeyEventData,
    ) -> Option<KeyAction> {
        let shift = data.modifiers & MOD_SHIFT != 0;
        let is_letter = (keymap::VK_A..=keymap::VK_Z).contains(&data.key_code);
        // 键被吃下后系统不再代劳大小写，由 CapsLock 镜像 XOR Shift 定（镜像每键按事件快照校准）。
        let effective_shift = if is_letter {
            shift != state.caps_lock
        } else {
            shift
        };
        // 覆盖面须 ⊇ C++ 全角吃键集（含空格/小键盘），由 full_width_source_char 统一收口。
        let ch = full_width_source_char(data.key_code, effective_shift)?;

        // 「英全」列（col 1）。走完整流水线而非裸 to_full_width，自定义映射才生效。
        let saved_punct = state.chinese_punct;
        state.chinese_punct = false;
        let piece = self.convert_punct_char(state, ch);
        let pairs = self.english_pairs_via_pipeline(state);
        state.chinese_punct = saved_punct;

        // C++ `OnKeyTraceDown` 在全角态跳过计数，英文统计由本路径记，不记则恒为 0。
        self.record_english_key_stat(data.key_code, shift);

        debug!("English full-width: {:?} -> {:?}", ch, piece);

        if let Some(pairs) = pairs {
            let pch = piece.chars().last().unwrap_or(' ');
            // 输右符号且栈顶正是它 → 光标右移越过。对称配对（`＂＂`）除外：分不清跳出还是嵌套，
            // 一律开新的一对，跳出交给 `auto_pair.jump_out_keys`。
            if self.rt().jump_out_on_right_symbol
                && pairs.iter().any(|(l, r)| *r == pch && *l != *r)
            {
                let mut tr = self.pair_tracker.lock().unwrap_or_else(|e| e.into_inner());
                // 只认单字符右段；`ime.pair` 压入的多字符右段只能靠 Tab/Enter 跳出。
                if tr.peek().is_some_and(|e| e.right_is_char(pch)) {
                    tr.pop();
                    return Some(KeyAction::MoveCursorRight { count: 1 });
                }
                tr.clear();
            }
            // 插入配对：左符号 → 补右符号，光标置于其间。
            if let Some((_, right)) = pairs.iter().find(|(l, _)| *l == pch).copied() {
                self.push_pair(pch, right);
                // 回退格数 = 右符号格数，恒为 1；别取 `piece` 长度，左段自定义成多字符时会落错。
                let cursor_offset = 1;
                return Some(KeyAction::InsertTextWithCursor {
                    text: format!("{}{}", piece, right),
                    cursor_offset,
                });
            }
        }
        // 英文智能符号置于配对分支之后：插对/跳出的键不参与武装，否则 press2 会误删配对左符号。
        if let Some(act) = self.english_mode_smart_symbol(state, ch, data.prev_char, &piece) {
            return Some(act);
        }
        Some(Self::commit_action(piece, false))
    }

    /// 当前焦点应用生效的智能符号替换方案：per-app 规则（`compat.toml` 的 `smart_method`）优先，
    /// 未配则跟随全局。所有读取点都须经此入口。典型用途是给 Tabby 一类删改出错的终端单配
    /// `HoldComposition`。
    pub(crate) fn effective_smart_method(&self) -> SmartMethod {
        if let Some(m) = self
            .active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .smart_method
        {
            return m;
        }
        self.rt().config.input.symbol.smart_method
    }

    /// 英文态下的配对表：`english_pairs` 左右符号各过一遍同一条标点转换（「打出什么就配对什么」），
    /// 列随 `state.full_width` 定（英全 1 / 英半 3）。
    ///
    /// 不可复用 `cn_pairs`：中文标点与 ASCII 的全角形不是同一字符（`［` U+FF3B ≠ `【` U+3010）。
    /// 必须用 `peek_custom` 而非 `convert_punct_char`：后者会推进引号交替态，每键过一遍整表就推 N 格。
    /// 调用前须已置 `state.chinese_punct = false`。
    fn english_pairs_via_pipeline(&self, state: &State) -> Option<Vec<(char, char)>> {
        // per-app 关闭：与 `active_pairs()` 同一道闸门。
        if !self.auto_pair_allowed_here() {
            return None;
        }
        let rt = self.rt();
        if !rt.config.input.auto_pair.english {
            return None;
        }
        let col = wind_punct::punct_col_idx(false, state.full_width);
        // 这里直调 `peek_custom` 绕过了 wind-punct 层，签名变更不会让它编译失败，生效表必须显式取。
        // 见 `english_pairs_pipeline_uses_effective_punct`。
        let punct = self.effective_punct(&rt, state);
        let conv = self.punct.lock().unwrap_or_else(|e| e.into_inner());
        let form = |c: char| -> Option<char> {
            let s = conv.peek_custom(&punct, c, col).unwrap_or_else(|| {
                let raw = c.to_string();
                if state.full_width {
                    wind_transform::fullwidth::to_full_width(&raw)
                } else {
                    raw
                }
            });
            single_char(&s)
        };
        let pairs: Vec<(char, char)> = rt
            .en_pairs
            .iter()
            .filter_map(|(l, r)| Some((form(*l)?, form(*r)?)))
            .collect();
        (!pairs.is_empty()).then_some(pairs)
    }

    /// 英文模式 + 半角：只接手「英半列有自定义映射覆盖」的标点键，按英半列（col 3）出字。
    ///
    /// 英文半角下 TSF 默认透传标点键，英半列本是死格；core 把配了英半列的源字符推给 DLL，
    /// DLL 吃下转发到这里出字。必须出字（同 `handle_english_full_width`）：接手判据与推给 DLL
    /// 的集合同源，返回 None 只发生在 DLL 本就没吃的键上。
    ///
    /// 引号照常左右交替，交替态与中文侧共用转换器，中英切换时由 `punct.reset()` 归零。
    /// 本路径同时接手英文半角的普通配对键：四条配对建立路径都入 `pair_tracker`，
    /// 中英之间才能互相跳出，DLL 只留吃键判据。
    pub(crate) fn handle_english_custom_punct(
        &self,
        state: &mut State,
        data: &KeyEventData,
    ) -> Option<KeyAction> {
        let shift = data.modifiers & MOD_SHIFT != 0;
        let ch = punct_char(data.key_code, shift)?;
        // 判据必须与 DLL 的吃键判据逐条同源，漂移即「吃了再吐」丢键。DLL 吃两类键：
        //   ① 英半列配了自定义映射的源字符（`push_custom_en_punct_config` 推送的集合）
        //   ② 英文配对表内的字符（`push_english_pair_config` 推送 `en_pairs` + `enabled`；
        //      DLL 侧判据是 `IsEnabled() && (IsLeft(c) || IsRight(c))`）
        // 两类都必须出字：①必有英半列值，②无自定义时 `convert_punct_char` 回落原样 ASCII，
        // 与透传等价，故并入安全。
        let rt = self.rt();
        let eaten_as_custom = rt.custom_en_punct_chars.contains(&ch);
        let eaten_as_pair = rt.config.input.auto_pair.english
            && rt.en_pairs.iter().any(|(l, r)| *l == ch || *r == ch);
        if !eaten_as_custom && !eaten_as_pair {
            return None; // DLL 未吃此键，透传给宿主
        }

        // 「英半」列（col 3）。英文模式恒按英文标点列输出，与工具栏中/英标点开关无关。
        let saved_punct = state.chinese_punct;
        state.chinese_punct = false;
        let piece = self.convert_punct_char(state, ch);
        let pairs = self.english_pairs_via_pipeline(state);
        state.chinese_punct = saved_punct;

        // 统计：与全角路径同理，键被吃下后 C++ 的英文计数不再经过原路径，此处补记。
        self.record_english_key_stat(data.key_code, shift);
        debug!("English custom punct: {:?} -> {:?}", ch, piece);

        // 配对：DLL 对被吃下的键会跳过本地英文配对（让位 core），故此处须自行处理，
        // 否则「自定义产物恰是配对左符」时配对能力凭空消失。逻辑与全角路径同构。
        if let Some(pairs) = pairs {
            let pch = piece.chars().last().unwrap_or(' ');
            if self.rt().jump_out_on_right_symbol
                && pairs.iter().any(|(l, r)| *r == pch && *l != *r)
            {
                let mut tr = self.pair_tracker.lock().unwrap_or_else(|e| e.into_inner());
                // 只认单字符右段；`ime.pair` 压入的多字符右段只能靠 Tab/Enter 跳出。
                if tr.peek().is_some_and(|e| e.right_is_char(pch)) {
                    tr.pop();
                    return Some(KeyAction::MoveCursorRight { count: 1 });
                }
                tr.clear();
            }
            if let Some((_, right)) = pairs.iter().find(|(l, _)| *l == pch).copied() {
                self.push_pair(pch, right);
                // 回退格数 = 右符号格数，恒为 1；别取 `piece` 长度，左段自定义成多字符时会落错。
                let cursor_offset = 1;
                return Some(KeyAction::InsertTextWithCursor {
                    text: format!("{}{}", piece, right),
                    cursor_offset,
                });
            }
        }
        // 英文智能符号置于配对分支之后：插对/跳出的键不参与武装，否则 press2 会误删配对左符号。
        // `custom_en_punct_chars` 已并入 `english_chars`，开了 `english_mode` 的键才能走到这里。
        if let Some(act) = self.english_mode_smart_symbol(state, ch, data.prev_char, &piece) {
            return Some(act);
        }
        Some(Self::commit_action(piece, false))
    }

    /// 英文模式按键的统计归类：镜像 C++ `_RecordEnglishKeyTrace` 的分桶（Shift+数字算标点、
    /// 小键盘数字算数字…），保证全角/半角两条路径统计口径一致。
    fn record_english_key_stat(&self, key_code: u32, shift: bool) {
        let (chars, digits, puncts, spaces) = if (keymap::VK_A..=keymap::VK_Z).contains(&key_code) {
            (1, 0, 0, 0)
        } else if (keymap::VK_0..=keymap::VK_9).contains(&key_code) {
            if shift { (0, 0, 1, 0) } else { (0, 1, 0, 0) }
        } else if key_code == keymap::VK_SPACE {
            (0, 0, 0, 1)
        } else if numpad_char(key_code).is_some_and(|c| c.is_ascii_digit()) {
            (0, 1, 0, 0)
        } else {
            (0, 0, 1, 0)
        };
        self.handle_english_stats(chars, digits, puncts, spaces);
    }

    /// 清空配对跟踪栈（失焦 / 组合被意外终止时）：光标已离开原位，残留栈会让跳出误判。
    ///
    /// 中英模式切换不调它：切模式不移动光标，清掉只会让切走再切回后跳不出去。
    /// C++ 侧的 `_pairPendingDepth` 同样保留。
    pub(crate) fn clear_pair_tracker(&self) {
        self.pair_tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// 聚焦到与配对状态归属不同的客户端时清栈。配对栈是全局单栈、不分宿主，
    /// 换了宿主之后栈顶可能是别人压的那层。`token == 0`（未归属/未知）不做判断。
    ///
    /// 真实失焦已在 `handle_focus_lost` 清过栈，这里是防御性保险：成本为零，且栈仍是全局单栈，
    /// 失焦判据一旦放宽它就是最后一道防串台的闸。
    pub(crate) fn clear_pair_tracker_if_foreign(&self, client_token: u64) {
        if client_token == 0 {
            return;
        }
        let mut tr = self.pair_tracker.lock().unwrap_or_else(|e| e.into_inner());
        let owner = tr.owner_token();
        if owner != 0 && owner != client_token {
            debug!("配对状态归属不符（owner={owner:#x} → {client_token:#x}），清栈");
            tr.clear();
        }
    }

    /// 压入一层配对，并认领归属（首层才有意义，非首层 set 同一 token 无副作用）。
    pub(crate) fn push_pair(&self, left: char, right: char) {
        let token = self.push_server.active_token();
        let mut tr = self.pair_tracker.lock().unwrap_or_else(|e| e.into_inner());
        if tr.is_empty() {
            tr.set_owner_token(token);
        }
        tr.push(left, right);
    }

    /// 配对状态是否已陈旧（距最后一次按键超过 `state_ttl_secs`）。
    ///
    /// 吃键闸门在 DLL，用同一阈值本地判 `_pairPendingDepth`。英文模式下协调器收不到普通字母键，
    /// 时间戳偏旧；这个不对称的失效方向是安全的：宁可不跳出，不要误跳出。
    pub(crate) fn pair_state_stale(&self) -> bool {
        let ttl = self.rt().config.input.auto_pair.state_ttl_secs;
        self.pair_tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_stale(ttl)
    }

    /// 配对跳出键的统一前置判定（全模式生效）。命中则光标越过右符号并弹栈。
    ///
    /// 必须置于英文模式分支之前：英文模式下普通键直接 `PassThrough`，判定留在中文分派路径里就是死代码。
    ///
    /// 守卫（任一不满足即不拦截，绝不吞键）：
    /// - 不在独占模式（`state.active`）：那些模式里的 Tab/Enter 有自己的语义
    /// - 无编码、无候选：避免吞掉正常的选词/编辑按键
    /// - 无 Ctrl/Alt/Shift：Shift+Tab 是反向切焦点，不能当跳出
    /// - 命中 `jump_out_keys` 且配对栈非空、未陈旧
    pub(crate) fn try_jump_out(&self, state: &State, data: &KeyEventData) -> Option<KeyAction> {
        if state.active.is_some()
            || !state.input_buffer.is_empty()
            || !state.candidates.is_empty()
            || data.modifiers & (MOD_SHORTCUT | MOD_SHIFT) != 0
            || !self.rt().jump_out_keys.contains(&data.key_code)
        {
            return None;
        }
        if self.pair_state_stale() {
            // 陈旧即就地清掉：否则每次按跳出键都要重算一遍，且状态会一直挂到下次失焦。
            debug!("配对状态已陈旧（超过 state_ttl_secs），清栈并放行跳出键");
            self.clear_pair_tracker();
            return None;
        }
        let mut tr = self.pair_tracker.lock().unwrap_or_else(|e| e.into_inner());
        tr.peek()?;
        // 右移格数取自弹出的那层（标点配对恒 1，`ime.pair` 按声明值）；`max(1)` 防吞键不动。
        let steps = tr.pop().map(|e| e.jump_steps).unwrap_or(1).max(1);
        Some(KeyAction::MoveCursorRight { count: steps })
    }

    /// 压入一层文本配对并认领归属（直通 `ime.pair` 用；单字符路径仍走 [`Self::push_pair`]）。
    pub(crate) fn push_pair_text(&self, left: String, right: String, jump_steps: u32) {
        let token = self.push_server.active_token();
        let mut tr = self.pair_tracker.lock().unwrap_or_else(|e| e.into_inner());
        if tr.is_empty() {
            tr.set_owner_token(token);
        }
        tr.push_text(left, right, jump_steps);
    }

    /// `ime.pair(left, right, jump=N)`：上屏 `left+right`、光标落两段之间，并压入配对栈。
    ///
    /// 走 push 通道而非 KeyAction：命令在独立线程执行，按键响应早已返回。DLL 须在 push 分支里
    /// 递增 `_pairPendingDepth`（中文模式下 Enter 能否转发的闸门），漏了就「Tab 跳得出、Enter 无反应」。
    ///
    /// `input.auto_pair` 关时退化为纯上屏（光标落末尾、不压栈）：该开关管住所有配对行为，
    /// 显式命令也不开后门（并非压了栈跳不出去，[`Self::try_jump_out`] 不看这个开关）。代价是出厂
    /// 关闭时 `ime.pair` 词条看似没生效。带光标偏移的命令只在真压栈时发出，保证 DLL 计数与本侧栈同步。
    /// 开关按当前输入模式取（中文看 `chinese`、英文看 `english`）。
    pub(crate) fn cmd_pair_commit(&self, left: &str, right: &str, jump_steps: u32) {
        if left.is_empty() && right.is_empty() {
            return;
        }
        let text = format!("{left}{right}");
        let pair_on = {
            let ap = &self.rt().config.input.auto_pair;
            if self.is_chinese_mode() {
                ap.chinese
            } else {
                ap.english
            }
        };
        // 右段为空 = 没有要越过的内容，压栈只会让下一次跳出键空跑一格。
        if !pair_on || right.is_empty() {
            debug!(
                "ime.pair: 退化为纯上屏（配对开关={}, 右段空={}）",
                pair_on,
                right.is_empty()
            );
            self.push_commit_text_from(crate::session_exit::ActionOrigin::Command, &text);
            return;
        }
        // `cursor_offset` 是「插入后向左移几格」，不是从开头的偏移；退回右段之前与跳出越过右段
        // 是同一段距离，故两端共用 `jump_steps`，进出自动对称。换行改写在收尾里做，不影响
        // `jump_steps`（它是用户配的格数）。`Command` 收尾不补空格；撤销计数只计左段。
        let act = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            self.finish_session_action(
                st,
                crate::session_exit::ActionOrigin::Command,
                KeyAction::InsertTextWithCursor {
                    text,
                    cursor_offset: jump_steps,
                },
            )
        };
        let KeyAction::InsertTextWithCursor { text, .. } = act else {
            debug_assert!(false, "收尾不应改变配对上屏的动作变体");
            tracing::warn!("ime.pair: 收尾改变了动作变体，放弃上屏");
            return;
        };
        // 换行已在出口改写（`Command` 的 `commit_newline`）。
        let encoded = wind_ipc::codec::encode_commit_text_with_cursor(&text, jump_steps);
        self.push_server.push_commit_to_active(&encoded);
        self.hook_commit(&text, "punct");
        self.push_pair_text(left.to_string(), right.to_string(), jump_steps);
    }

    /// 刷新配对状态活动时间（每次按键调用；栈空时是空操作）。
    pub(crate) fn touch_pair_state(&self) {
        self.pair_tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .touch();
    }
}

#[cfg(test)]
#[path = "handle_punct_pair_commit_tests.rs"]
mod pair_commit_tests;

/// 方案级自定义标点表（`[punct.custom_mappings]`）：整表替换 + 按数据归属方案取。
#[cfg(test)]
#[path = "handle_punct_schema_scoped_punct_tests.rs"]
mod schema_scoped_punct_tests;
