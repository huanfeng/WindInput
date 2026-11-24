//! 码表通配输入（万能键）的按键裁决与冲突体检。
//! 设计见 `docs/design/codetable-wildcard.md` §3.3–§3.4、§5.3。
//!
//! # 为什么「哪几位是通配」由协调器定，而不是引擎按内容自判
//!
//! 缓冲不带标记：首位让位进来的字面 `z` 与作通配的 `z` 同形。缓冲会被退格、中插改写，
//! 来源标志迟早与内容对不上，故裁决按同一套规则从缓冲内容重算。规则必须与历史无关
//! （`z_key_repeat` 取配置开关而非「有没有上屏历史」），否则同一串缓冲两次重算结论不同。

use crate::coordinator::{Coordinator, State};
use crate::key_convert::{punct_char, punct_source_vk};
use tracing::warn;
use wind_candidate::{Candidate, CandidateSource};
use wind_dict::WILDCARD_SLOT;
use wind_keys::keymap;

/// 通配组码的首批候选上限（spec §11）。不走 `initial_candidate_limit` 的码长分级（那是给
/// 前缀补全配的）：首批小、翻到边界由 `expand_candidates` 加倍，`has_more` 判据才成立。
pub(crate) const WILDCARD_INITIAL_LIMIT: usize = 100;

/// 通配键此刻的去向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WildcardDecision {
    /// 让位：按它原本的身份走（码元 / 模式 / 选词 / 翻页 / 标点），与关闭通配时逐键相同。
    Yield,
    /// 作通配进缓冲。
    Enter,
}

/// 通配键 → (主键盘 VK, 是否需要 Shift)。字母恒无 Shift（缓冲恒存小写）。
fn wildcard_key_vk(key: char) -> Option<(u32, bool)> {
    if key.is_ascii_lowercase() {
        return Some((keymap::VK_A + (key as u32 - 'a' as u32), false));
    }
    let vk = punct_source_vk(key)?;
    Some((vk, punct_char(vk, false) != Some(key)))
}

/// 拼音分段续转态：最后一段是拼音选词，剩余编码按混输的拼音子方案转换（`build_candidates`）。
/// 该态下通配一律字面（spec §10）——剩余串是拼音的后半截，不是码表码。
///
/// 与 `build_candidates` 选拼音子方案共用本函数，否则会「续转走拼音、却按通配查码表」。
pub(crate) fn last_seg_is_pinyin(state: &State) -> bool {
    state
        .committed_segs
        .last()
        .is_some_and(|s| s.source == CandidateSource::Pinyin)
}

impl Coordinator {
    /// 按下的字符 `ch` 此刻是否作通配进缓冲——两个接线点（字母臂、`try_code_char_gate`）
    /// 的唯一入口。
    ///
    /// overlay 激活时恒 `false`：通配键取自活跃方案，overlay 用的是别的方案或输入语义，
    /// 而引擎管理器不知道 overlay 状态。现有分派里 overlay 已先 return，这里是结构性兜底。
    pub(crate) fn wildcard_enters(&self, state: &State, ch: char) -> bool {
        state.active.is_none()
            && !last_seg_is_pinyin(state)
            && self.engine_mgr.active_wildcard_key() == Some(ch)
            && self.wildcard_decision(&state.input_buffer, ch) == WildcardDecision::Enter
    }

    /// §3.3 按键裁决。`buffer` 是**按下前**的输入缓冲。
    ///
    /// 调用点必须晚于 `try_activate_mode` 与 `try_z_fallback`（见 `codetable-input-chars.md`
    /// §3.4）：首位让位本就是它们先赢，这里只处理它们都没接手之后的去向。
    pub(crate) fn wildcard_decision(&self, buffer: &str, key: char) -> WildcardDecision {
        let yield_ = if self.wildcard_past_full(buffer.chars().count()) {
            true
        } else if buffer.is_empty() {
            self.wildcard_lead_yields(key)
        } else {
            // 首字符是让位进来的字面通配键 ⇒ 整轮都是字面（否则 `zzbd` 变 `z?bd`，
            // 出厂 `zz*` 短语整批消失）。
            self.wildcard_lead_literal(buffer, key) || !self.wildcard_mid_owners(key).is_empty()
        };
        if yield_ {
            WildcardDecision::Yield
        } else {
            WildcardDecision::Enter
        }
    }

    /// 满码判据（spec §3.3「满码」一行）：落在第 `pos` 位（0 起）的通配键，若此时缓冲已达
    /// `max_code_length` ⇒ 按字面，与关闭通配时逐键相同（`aaaa` + `z` 照常顶字）。
    /// 作通配的话只会得到一条比任何码都长的死串：无候选、又因通配不顶字。
    ///
    /// 按键裁决与 [`Self::wildcard_pattern`] 共用本判据（见模块文档）。通配码长为 0 时恒 `false`。
    fn wildcard_past_full(&self, pos: usize) -> bool {
        let max = self.engine_mgr.active_wildcard_code_length();
        max > 0 && pos >= max
    }

    /// 首位：通配键已绑定任何功能 / 模式即让位（spec §3.3 首位一行）。
    ///
    /// 符号键与字母分开判：符号通配键首位作通配，见 [`Self::wildcard_symbol_takes_lead`]。
    fn wildcard_lead_yields(&self, key: char) -> bool {
        // 混输：字母通配键首位一律字面（spec §10）。靠 `has_code_prefix` 让位不可靠（没装
        // `zz*` 短语时不成立），`zhang` 会变成 `?hang`。符号不是拼音字母，走下面的符号键判据。
        if key.is_ascii_alphabetic() && self.engine_mgr.active_wildcard_mixes_pinyin() {
            return true;
        }
        if !key.is_ascii_lowercase() {
            return !self.wildcard_symbol_takes_lead(key);
        }
        let Some((vk, _)) = wildcard_key_vk(key) else {
            return true;
        };
        // `bound_action_for` 是三层链（方案 `[key_actions]` → 全局 → `z_key_action`）。
        if self.bound_action_for(vk).is_some_and(|a| a.is_enabled()) {
            return true;
        }
        // 取配置开关而非 `z_key_repeat_text()`：判据须与历史无关（见模块文档）。
        if key == 'z' && self.engine_mgr.codetable_settings().z_key_repeat {
            return true;
        }
        if self.is_any_mode_trigger(vk) {
            return true;
        }
        self.has_code_prefix(&key.to_string())
    }

    /// 符号通配键在空缓冲时是否作首码。
    ///
    /// 选了符号通配键即声明它是码元，码元理应能出现在第一位，故不再让位给全局的标点 / 模式
    /// 绑定（全局配置不知道本方案把它当码元）。只有方案级 `[key_actions]` 显式绑了它才让位，
    /// 与 [`Self::code_char_takes_lead`] 的跨层仲裁同一口径。
    ///
    /// 代价：这个键的标点产物（反引号 → `·`）在首位拿不到，同把符号写进 `input_chars`。
    pub(crate) fn wildcard_symbol_takes_lead(&self, key: char) -> bool {
        if key.is_ascii_alphabetic() || self.engine_mgr.active_wildcard_key() != Some(key) {
            return false;
        }
        let Some((vk, shift)) = wildcard_key_vk(key) else {
            return false;
        };
        // 按键绑定只认无 Shift 的那一下（空缓冲分派的守卫），`?` 这类 Shift 形态碰不到
        // `slash` 上的绑定 ⇒ 不拿它来挡。
        shift
            || !self
                .bound_action_with_source(vk)
                .is_some_and(|(a, from_schema)| from_schema && a.is_enabled())
    }

    /// 缓冲首字符是通配键，且首位按规则会让位 ⇒ 它是字面码元，整轮组码不作通配。
    fn wildcard_lead_literal(&self, buffer: &str, key: char) -> bool {
        buffer.starts_with(key) && self.wildcard_lead_yields(key)
    }

    /// 非首位：通配键若是组码中途的功能键，返回占用它的功能名（空 = 不冲突）。
    ///
    /// 判据与 `code_char_conflicts` / `symbol_buffer_key_free` 同源，那两处的 owners 链增减时
    /// 这里要跟着改。
    ///
    /// 会话表里一个键只有一个动作，故按动作**细分**成一条名字：`select_key_offset` /
    /// `select_char_index` 都是 `session_action_for` 的派生，再单独问一遍就会把同一个 `;`
    /// 报成「会话键」和「次选键」两条。
    fn wildcard_mid_owners(&self, key: char) -> Vec<&'static str> {
        let Some((vk, shift)) = wildcard_key_vk(key) else {
            return Vec::new();
        };
        let mut owners: Vec<&'static str> = Vec::new();
        owners.extend(self.wildcard_session_owner(vk, shift));
        if self.manual_separator_key(vk) {
            owners.push("音节分隔符");
        }
        owners
    }

    /// 通配键此刻在会话表里的身份名（`None` = 会话表没占它），组码中（[`Self::wildcard_mid_owners`]）用。
    ///
    /// 反查模式内不能用它：组码中选词 / 以词定字键会让通配让位，是冲突；反查模式里
    /// 它们不被导航吃掉，见 [`Self::reverse_wildcard_conflicts`]。
    fn wildcard_session_owner(&self, vk: u32, shift: bool) -> Option<&'static str> {
        use wind_config::{AuxCodeShare, SessionAction};
        match self.session_action_for(vk, shift, true)? {
            // 选词 / 以词定字的两个消费点自带 `!shift` 守卫：shift 形态本就不归它们。
            SessionAction::SelectCandidate(_) | SessionAction::SelectChar(_) if shift => None,
            SessionAction::SelectCandidate(2) => Some("次选键"),
            SessionAction::SelectCandidate(3) => Some("三选键"),
            SessionAction::SelectCandidate(_) => Some("选词键"),
            SessionAction::SelectChar(_) => Some("以词定字键"),
            SessionAction::AuxCode(AuxCodeShare::Solo) => Some("辅助码键"),
            SessionAction::AuxCode(AuxCodeShare::PageNext) => Some("辅助码/翻页键"),
            SessionAction::PagePrev(_)
            | SessionAction::PageNext(_)
            | SessionAction::PageScreenPrev
            | SessionAction::PageScreenNext => Some("翻页键"),
            SessionAction::HighlightUp | SessionAction::HighlightDown => Some("高亮键"),
            SessionAction::Cancel => Some("取消键"),
            SessionAction::CommitHighlighted => Some("上屏键"),
            SessionAction::ToggleTip => Some("提示键"),
            SessionAction::CommitCode => Some("上屏编码键"),
            SessionAction::CaretLeft
            | SessionAction::CaretRight(_)
            | SessionAction::SyllablePrev
            | SessionAction::SyllableNext => Some("光标移动键"),
            SessionAction::Command(_) => Some("命令键"),
            SessionAction::SingleChar(_) => Some("单字键"),
            SessionAction::CommentState(_) => Some("注释键"),
            SessionAction::None => None,
        }
    }

    /// 当前缓冲若是通配组码，返回交给引擎的 pattern（作通配的位替换成 `WILDCARD_SLOT`）。
    ///
    /// 与 [`Self::wildcard_decision`] 同一套规则重算（见模块文档）：首位字面 ⇒ 整轮字面；
    /// 非首位存在冲突 ⇒ 非首位的通配键一律字面（冲突时它们只可能经让位进来，spec §3.3
    /// 非首位一行）；落在满码之后的通配键一律字面（[`Self::wildcard_past_full`]）。
    /// 混输下整串超过主码表码长 ⇒ 整串字面（[`Self::wildcard_mixed_overflow`]）；拼音分段
    /// 续转态见 [`Self::wildcard_pattern_of`]。
    ///
    /// 通配关闭时 `active_wildcard_key()` 为 `None`，在任何缓冲扫描与分配之前返回。
    pub(crate) fn wildcard_pattern(&self, buffer: &str) -> Option<String> {
        let key = self.engine_mgr.active_wildcard_key()?;
        if !buffer.contains(key)
            || self.wildcard_lead_literal(buffer, key)
            || self.wildcard_mixed_overflow(buffer)
        {
            return None;
        }
        let mid_ok = self.wildcard_mid_owners(key).is_empty();
        let mut any = false;
        let pattern: String = buffer
            .chars()
            .enumerate()
            .map(|(i, c)| {
                if c == key && (i == 0 || mid_ok) && !self.wildcard_past_full(i) {
                    any = true;
                    WILDCARD_SLOT
                } else {
                    c
                }
            })
            .collect();
        any.then_some(pattern)
    }

    /// 混输：整串长度超过主码表码长 ⇒ 码表不可能配上，整串归拼音、按字面（spec §10）。
    /// 纯码表不走这条：那里按位判满码（`azaaz` 前段仍是通配，见 [`Self::wildcard_past_full`]）。
    fn wildcard_mixed_overflow(&self, buffer: &str) -> bool {
        let max = self.engine_mgr.active_wildcard_code_length();
        self.engine_mgr.active_wildcard_mixes_pinyin() && max > 0 && buffer.chars().count() > max
    }

    /// [`Self::wildcard_pattern`] 再加一条**看会话状态**的判据：拼音分段续转态一律字面
    /// （spec §10，[`last_seg_is_pinyin`]）。手里有 `State` 的调用点一律走这里。
    ///
    /// `main_freq_code` 仍按缓冲调 `wildcard_pattern`：续转态的转换走纯拼音子方案，不产出
    /// 码表候选，而 `main_freq_code` 只对码表候选改记全码，两者结论一致。
    pub(crate) fn wildcard_pattern_of(&self, state: &State) -> Option<String> {
        if last_seg_is_pinyin(state) {
            return None;
        }
        self.wildcard_pattern(&state.input_buffer)
    }

    /// 主输入路**上屏记账点**用的记账码：通配组码下的码表候选记候选**全码**，其余同
    /// [`Self::freq_code`]。
    ///
    /// 通配组码的缓冲（`azzd`）是查询串，不是任何词条的码位——按它记账会写出读端永远查不中
    /// 的孤儿键。改记候选自己的全码，即该字在正常输入时所在的码位。
    ///
    /// 只接在主输入路的上屏记账点，`freq_code` 本体不动：mix / 临拼缓冲用的是别的方案，
    /// 通配键不适用于它们（同 `wildcard_enters` 的 overlay 门）；读侧调频
    /// （`apply_freq_rerank`）按通配串查，查无记录 ⇒ 通配结果不参与调频，是有意的。
    ///
    /// 先判来源与码再问通配：非码表候选（拼音 / 短语…）不碰引擎管理器。
    pub(crate) fn main_freq_code(&self, buf: &str, cand: &Candidate) -> String {
        if cand.source == CandidateSource::CodeTable
            && !cand.code.is_empty()
            && self.wildcard_pattern(buf).is_some()
        {
            return cand.code.clone();
        }
        self.freq_code(buf, cand)
    }

    /// 通配键与既有按键功能的冲突清单（只告警）。空 = 无冲突或通配关闭。
    ///
    /// 首位让位**不报**（那是设计内行为）；只报非首位让位（意味着通配在组码中实际不可用）
    /// 与「通配键是显式配置的方案码元」（该码元将被通配吞掉）。
    ///
    /// 码元一项只在 `input_chars` **显式配置**时报：五笔 86 未配 `input_chars`，默认 `a-z`
    /// 含 `z`，但词库里 `z` 开头 / 含 `z` 的码为 0——按默认集报会让出厂主用例每次启动都告警。
    pub fn wildcard_conflicts(&self) -> Vec<&'static str> {
        let Some(key) = self.engine_mgr.active_wildcard_key() else {
            return Vec::new();
        };
        let mut owners = self.wildcard_mid_owners(key);
        if self.wildcard_is_configured_code_char(key) {
            owners.push("方案码元（组码中将被通配吞掉）");
        }
        owners
    }

    /// 反查模式内的通配键冲突（只告警）。空 = 无冲突、反查模式不可用或没绑键。
    ///
    /// 模式内的按键处理（`handle_special_key`）里导航 `handle_candidate_nav` 排在符号通配键
    /// 进缓冲之前：通配键若同时是翻页 / 高亮 / 取消等会被导航消费的会话键，按下去先被吃掉，
    /// 模式里就再也打不出通配。按键顺序不改（改了就是另一侧的键失灵），由体检报出，让用户换键。
    ///
    /// 与 [`Self::wildcard_conflicts`] 分开：那边只在通配主开关开时有意义，本模式不看主开关；
    /// 且组码中的冲突会让位（通配不可用），这边是导航先吃（同样不可用，但成因不同）。
    pub fn reverse_wildcard_conflicts(&self) -> Vec<&'static str> {
        if !(self.reverse_mode_available() && self.reverse_bound_anywhere()) {
            return Vec::new();
        }
        let Some((vk, shift)) = self
            .engine_mgr
            .active_reverse_key()
            .and_then(wildcard_key_vk)
        else {
            return Vec::new();
        };
        use wind_config::{AuxCodeShare, SessionAction};
        // 只报 `apply_session_action` 真正消费的动作。选词 / 以词定字它刻意返回 None
        // （落到各自消费点，而反查的符号通配键臂排在选词臂之前）；独立辅助码的
        // `enter_aux_code` 在 overlay 模式里拒绝、同样返回 None——这三类键在模式内照常
        // 作通配进缓冲，报出来是误报。共键 `aux_code:page_next` 被拒后降级为翻页，照吃。
        let owner = match self.session_action_for(vk, shift, true) {
            Some(
                SessionAction::PagePrev(_)
                | SessionAction::PageNext(_)
                | SessionAction::PageScreenPrev
                | SessionAction::PageScreenNext,
            ) => Some("翻页键"),
            Some(SessionAction::AuxCode(AuxCodeShare::PageNext)) => Some("辅助码/翻页键"),
            Some(SessionAction::HighlightUp | SessionAction::HighlightDown) => Some("高亮键"),
            Some(SessionAction::Cancel) => Some("取消键"),
            Some(SessionAction::CommitHighlighted) => Some("上屏键"),
            Some(SessionAction::ToggleTip) => Some("提示键"),
            Some(SessionAction::CommitCode) => Some("上屏编码键"),
            Some(
                SessionAction::CaretLeft
                | SessionAction::CaretRight(_)
                | SessionAction::SyllablePrev
                | SessionAction::SyllableNext,
            ) => Some("光标移动键"),
            Some(SessionAction::Command(_)) => Some("命令键"),
            Some(SessionAction::SingleChar(_)) => Some("单字键"),
            Some(SessionAction::CommentState(_)) => Some("注释键"),
            Some(
                SessionAction::SelectCandidate(_)
                | SessionAction::SelectChar(_)
                | SessionAction::AuxCode(AuxCodeShare::Solo)
                | SessionAction::None,
            )
            | None => None,
        };
        owner.into_iter().collect()
    }

    fn wildcard_is_configured_code_char(&self, key: char) -> bool {
        let charset = self.engine_mgr.active_input_chars();
        !charset.is_default_alpha() && charset.contains(key)
    }

    /// 启动时把 [`Self::wildcard_conflicts`] 写进日志。只告警，不改行为。
    ///
    /// 两类后果相反，故分两条：功能键冲突是「通配让位、组码中用不了」；码元冲突是「通配
    /// 照样生效、该码元被吞掉」。混在一句里用户读不出该改哪边。
    pub(crate) fn warn_wildcard_conflicts(&self) {
        let reverse_owners = self.reverse_wildcard_conflicts();
        if !reverse_owners.is_empty() {
            warn!(
                "反查模式通配键 {:?} 同时配作 {}；反查模式内它先按原功能处理，模式里打不出通配——请在方案设置里换一个通配键",
                self.engine_mgr.active_reverse_key(),
                reverse_owners.join(" / ")
            );
        }
        let Some(key) = self.engine_mgr.active_wildcard_key() else {
            return;
        };
        let owners = self.wildcard_mid_owners(key);
        if !owners.is_empty() {
            warn!(
                "通配键 {:?} 同时配作 {}；组码中它按原功能处理，通配在该方案下不可用——请在方案设置里换一个通配键",
                key,
                owners.join(" / ")
            );
        }
        if self.wildcard_is_configured_code_char(key) {
            warn!(
                "通配键 {:?} 是本方案显式配置的码元（input_chars）；组码中该码元将被通配吞掉，不再按字面参与编码——如非有意，请换一个通配键",
                key
            );
        }
        if let Some(name) = self.wildcard_lead_overrides_global(key) {
            warn!(
                "通配键 {:?} 同时是全局「{}」的按键；本方案首位它作通配，该功能在本方案下按这个键进不去——如需保留，请换通配键或给该功能另配键",
                key, name
            );
        }
    }

    /// 符号通配键在首位接管了哪一项**全局**绑定（`None` = 没有）。码元撞引导键时
    /// `code_char_conflicts` 会报，通配接管同样要报，否则用户只看到「反引号进不了临拼了」。
    fn wildcard_lead_overrides_global(&self, key: char) -> Option<String> {
        if !self.wildcard_symbol_takes_lead(key) {
            return None;
        }
        let (vk, _) = wildcard_key_vk(key)?;
        match self.bound_action_with_source(vk)? {
            (action, false) if action.is_enabled() => Some(format!("{action:?}")),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "wildcard_tests.rs"]
mod tests;
