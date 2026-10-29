//! 码表通配输入（万能键）的按键裁决与冲突体检。
//! 设计见 `docs/design/codetable-wildcard.md` §3.3–§3.4、§5.3。
//!
//! # 为什么「哪几位是通配」由协调器定，而不是引擎按内容自判
//!
//! 缓冲本身不带标记：首位让位后进缓冲的字面 `z`（`zz*` 短语的活码）与作通配的 `z` 同形。
//! 故裁决按**同一套规则**从缓冲内容重算，不记标志位——理由同 `buffer_has_literal_symbol`：
//! 缓冲会被退格、光标中插改写，来源标志迟早与内容对不上，而字符与规则不会。规则必须
//! **与历史无关**（`z_key_repeat` 取配置开关而非「有没有上屏历史」），否则同一串缓冲
//! 前后两次重算会得出不同结论。

use crate::coordinator::{Coordinator, State};
use crate::key_convert::{punct_char, punct_source_vk};
use tracing::warn;
use wind_candidate::{Candidate, CandidateSource};
use wind_dict::WILDCARD_SLOT;
use wind_keys::keymap;

/// 通配组码的首批候选上限（spec §11）。不走 `initial_candidate_limit` 的码长分级
/// （码表 1/2/≥3 码 → 100/300/1000）：那套是给前缀补全配的，通配首位即退化成全表扫描；
/// 首批小、翻到边界再由 `expand_candidates` ×2 扩，`has_more = engine_count >= limit`
/// 才自然成立（旧实现首批 1000、引擎至多回 100 ⇒ `has_more` 恒假）。
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
/// ★ 与 `build_candidates` 选拼音子方案用的是同一个函数：两处各写一份，改一处另一处就会
/// 出现「续转走拼音、却又按通配查码表」。本判据看会话状态（已上屏段），退格回退段时随之
/// 失效，仍满足模块文档「同一状态前后两次重算结论相同」。
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
    /// ★ overlay（临拼 / 临英 / 快捷输入 / 特殊模式 / 辅助码…）激活时恒 `false`：通配键取自
    /// **活跃方案**的引擎，而 overlay 用的是别的方案或别的输入语义（临拼里 `z` 是拼音字母）。
    /// 引擎管理器没有 overlay 状态，这道门只能由协调器把。现有分派里 overlay 在两个接线点之前
    /// 就 return 了，这里是结构性兜底：日后哪条 overlay 路径复用了这两个接线点，不会静默带上通配。
    pub(crate) fn wildcard_enters(&self, state: &State, ch: char) -> bool {
        state.active.is_none()
            && !last_seg_is_pinyin(state)
            && self.engine_mgr.active_wildcard_key() == Some(ch)
            && self.wildcard_decision(&state.input_buffer, ch) == WildcardDecision::Enter
    }

    /// §3.3 按键裁决。`buffer` 是**按下前**的输入缓冲。
    ///
    /// ⚠️ 调用点必须晚于 `try_activate_mode` 与 `try_z_fallback`（顺序铁律，见
    /// `codetable-input-chars.md` §3.4）：首位让位本就是它们先赢；这里的首位判据只处理
    /// 「它们都没接手」之后的去向。
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
    /// ★ 按键裁决与 [`Self::wildcard_pattern`] 共用本判据：同一串缓冲前后两次重算须得出
    /// 同一结论（见模块文档）。通配码长（`wildcard_code_length`，混输取主码表的）为 0 时恒 `false`。
    fn wildcard_past_full(&self, pos: usize) -> bool {
        let max = self.engine_mgr.active_wildcard_code_length();
        max > 0 && pos >= max
    }

    /// 首位：通配键已绑定任何功能 / 模式即让位（spec §3.3 首位一行）。
    ///
    /// **符号键与字母分开判**（论坛 t302）：符号通配键首位**作通配**，只有方案级
    /// `[key_actions]` 显式绑了它才让位，见 [`Self::wildcard_symbol_takes_lead`]。
    fn wildcard_lead_yields(&self, key: char) -> bool {
        // 五笔拼音混输：首位一律字面（spec §10）。z 是拼音声母（`zhang` / `zai`），而混输下
        // `has_code_prefix("z")` 只在装了 `zz*` 短语时成立（单字母够不着 `min_pinyin_length`，
        // 五笔主库无 z 码），靠它让位的话没装短语的用户打 `zhang` 会变成 `?hang`。
        if self.engine_mgr.active_wildcard_mixes_pinyin() {
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
        // 取**配置开关**而不是 `z_key_repeat_text()`（有无上屏历史）：本判据要从缓冲反复
        // 重算，随历史变化的判据会让同一串缓冲前后解释不一。
        if key == 'z' && self.engine_mgr.codetable_settings().z_key_repeat {
            return true;
        }
        if self.is_any_mode_trigger(vk) {
            return true;
        }
        self.has_code_prefix(&key.to_string())
    }

    /// 符号通配键在空缓冲时是否**作首码**（论坛 t302）。
    ///
    /// 通配出厂关闭，开了并选了符号键就是方案作者 / 用户明说「这个键是码元」——码元理应
    /// 能出现在任何一位，包括第一位。此前符号键首位一律让位给标点 / 模式，于是反引号要先被
    /// 出厂的临拼触发抢走、再按一次退出临拼，才轮到通配（用户实测「要按两次」）。
    ///
    /// 让位只剩两种：
    /// - 混输（spec §10，首位恒字面，理由见 [`Self::wildcard_lead_yields`]）；
    /// - **方案级** `[key_actions]` 显式绑了这个键：同层冲突，显式绑定优先（与
    ///   [`Self::code_char_takes_lead`] 的跨层仲裁同一口径）。全局层的绑定则由
    ///   `code_char_takes_lead` 判让位——全局配置无从知道本方案把它当码元用了。
    ///
    /// 这个键原本的标点产物（反引号 → `·`）就此在首位拿不到，与把符号写进 `input_chars`
    /// 的代价相同；需要时用快捷输入 / 短语出。
    pub(crate) fn wildcard_symbol_takes_lead(&self, key: char) -> bool {
        if key.is_ascii_alphabetic()
            || self.engine_mgr.active_wildcard_key() != Some(key)
            || self.engine_mgr.active_wildcard_mixes_pinyin()
        {
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
    /// 判据反查现有函数，与 `code_char_conflicts` / `symbol_buffer_key_free` 同源——
    /// ⚠️ 那两处的 owners 链增减时这里要跟着改。
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
    /// ⚠️ 反查模式内**不能**用它：组码中选词 / 以词定字键会让通配让位，是冲突；反查模式里
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
            SessionAction::PagePrev | SessionAction::PageNext => Some("翻页键"),
            SessionAction::HighlightUp | SessionAction::HighlightDown => Some("高亮键"),
            SessionAction::Cancel => Some("取消键"),
            SessionAction::CommitHighlighted => Some("上屏键"),
            SessionAction::ToggleTip => Some("提示键"),
            SessionAction::CommitCode => Some("上屏编码键"),
            SessionAction::CaretLeft | SessionAction::CaretRight => Some("光标移动键"),
            SessionAction::Command(_) => Some("命令键"),
            SessionAction::SingleChar(_) => Some("单字键"),
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
    /// ★ 只接在主输入路的上屏记账点，`freq_code` 本体不动：mix / 临拼缓冲用的是别的方案，
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
        // 只报 `apply_session_action` **真正消费**的动作。选词 / 以词定字它刻意返回 None
        // （落到各自消费点，而反查的符号通配键臂排在选词臂之前）；独立辅助码的
        // `enter_aux_code` 在 overlay 模式里拒绝、同样返回 None——这三类键在模式内照常
        // 作通配进缓冲，报出来是误报。共键 `aux_code:page_next` 被拒后降级为翻页，照吃。
        let owner = match self.session_action_for(vk, shift, true) {
            Some(SessionAction::PagePrev | SessionAction::PageNext) => Some("翻页键"),
            Some(SessionAction::AuxCode(AuxCodeShare::PageNext)) => Some("辅助码/翻页键"),
            Some(SessionAction::HighlightUp | SessionAction::HighlightDown) => Some("高亮键"),
            Some(SessionAction::Cancel) => Some("取消键"),
            Some(SessionAction::CommitHighlighted) => Some("上屏键"),
            Some(SessionAction::ToggleTip) => Some("提示键"),
            Some(SessionAction::CommitCode) => Some("上屏编码键"),
            Some(SessionAction::CaretLeft | SessionAction::CaretRight) => Some("光标移动键"),
            Some(SessionAction::Command(_)) => Some("命令键"),
            Some(SessionAction::SingleChar(_)) => Some("单字键"),
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
mod tests {
    use super::WildcardDecision::{Enter, Yield};
    use crate::coordinator::Coordinator;
    use std::path::PathBuf;
    use std::sync::Arc;
    use wind_config::Config;

    fn data_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
    }

    /// 五笔 86 + 通配键 `z`；缺数据返回 `None`（同集成测试的静默跳过约定）。
    fn wubi_z(tweak: impl FnOnce(&mut Config)) -> Option<Arc<Coordinator>> {
        if !data_dir()
            .join("schemas/wubi86/wubi86_jidian.dict.yaml")
            .exists()
        {
            eprintln!("跳过：五笔词库不存在");
            return None;
        }
        let mut cfg = Config::default();
        cfg.schema.available = vec!["wubi86".into()];
        cfg.schema.active = "wubi86".into();
        cfg.input.default.chinese_mode = true;
        cfg.schema.codetable.wildcard = true;
        cfg.schema.codetable.wildcard_key = "z".into();
        tweak(&mut cfg);
        Some(Coordinator::new_headless(cfg, Some(&data_dir())))
    }

    /// 首位 z 是 `zz*` 活码 ⇒ 首位让位，且**整轮**字面（后续 z 也让位）；
    /// 非 z 起头的缓冲里 z 照常作通配。
    #[test]
    fn leading_literal_wildcard_makes_whole_buffer_literal() {
        let Some(c) = wubi_z(|_| {}) else { return };
        let seed = |code: &str, text: &str| wind_phrase::PhraseSeed {
            code: code.into(),
            text: text.into(),
            weight: 0,
            position: 0,
            is_system: true,
            category: String::new(),
        };
        c.debug_install_phrases(vec![seed("zzbd", "、")]);
        assert_eq!(c.wildcard_decision("", 'z'), Yield, "首位活码让位");
        assert_eq!(c.wildcard_decision("z", 'z'), Yield, "首位字面 ⇒ 整轮字面");
        assert_eq!(c.wildcard_decision("zzb", 'z'), Yield);
        assert_eq!(c.wildcard_decision("a", 'z'), Enter, "对照：a 起头照常通配");
    }

    /// overlay 激活时通配不生效（`wildcard_enters` 的 `state.active` 门）。
    /// 现有分派里 overlay 在两个接线点之前就 return，集成层测不到这道门，故在此直接钉。
    #[test]
    fn overlay_active_blocks_wildcard() {
        let Some(c) = wubi_z(|_| {}) else { return };
        let mut st = c.state.lock().unwrap_or_else(|e| e.into_inner());
        st.input_buffer = "a".into();
        st.active = None;
        assert!(
            c.wildcard_enters(&st, 'z'),
            "对照：无 overlay 时 a 后的 z 作通配"
        );
        st.active = Some(crate::pipeline::ModeKind::TempPinyin);
        assert!(!c.wildcard_enters(&st, 'z'), "overlay 激活时不作通配");
    }

    /// `z_key_repeat` 按**配置开关**让位，与有无上屏历史无关（新建协调器无历史）。
    /// 按 `z_key_repeat_text()` 判的话这里会得 Enter，同一串缓冲前后解释不一。
    #[test]
    fn z_key_repeat_yields_by_switch_not_history() {
        let Some(c) = wubi_z(|cfg| cfg.schema.codetable.z_key_repeat = true) else {
            return;
        };
        assert_eq!(c.wildcard_decision("", 'z'), Yield);
        assert_eq!(c.wildcard_decision("z", 'z'), Yield, "首位让位 ⇒ 整轮字面");
        assert_eq!(c.wildcard_decision("a", 'z'), Enter);
    }

    fn seed(code: &str, text: &str) -> wind_phrase::PhraseSeed {
        wind_phrase::PhraseSeed {
            code: code.into(),
            text: text.into(),
            weight: 0,
            position: 0,
            is_system: true,
            category: String::new(),
        }
    }

    const S: char = wind_dict::WILDCARD_SLOT;

    /// 作通配的位换成槽位；无通配键的缓冲不是通配组码。
    #[test]
    fn pattern_marks_every_wildcard_position() {
        let Some(c) = wubi_z(|_| {}) else { return };
        assert_eq!(c.wildcard_pattern("az"), Some(format!("a{S}")));
        assert_eq!(c.wildcard_pattern("azzd"), Some(format!("a{S}{S}d")));
        assert_eq!(
            c.wildcard_pattern("z"),
            Some(S.to_string()),
            "首位无绑定 ⇒ 首位即通配"
        );
        assert_eq!(c.wildcard_pattern("aaaa"), None);
        assert_eq!(c.wildcard_pattern(""), None);
    }

    /// 通配关闭 ⇒ 恒 `None`（缓冲里的 `z` 是字面码元）。
    #[test]
    fn pattern_is_none_when_wildcard_off() {
        let Some(c) = wubi_z(|cfg| cfg.schema.codetable.wildcard = false) else {
            return;
        };
        assert_eq!(c.wildcard_pattern("az"), None);
        assert_eq!(c.wildcard_pattern("z"), None);
    }

    /// 首位字面（`zz*` 活码让位进来的 `z`）⇒ 整轮字面，与 `wildcard_decision` 同一规则。
    #[test]
    fn pattern_is_none_when_lead_is_literal() {
        let Some(c) = wubi_z(|_| {}) else { return };
        c.debug_install_phrases(vec![seed("zzbd", "、")]);
        assert_eq!(c.wildcard_pattern("zzbd"), None);
        assert_eq!(c.wildcard_pattern("zz"), None);
        assert_eq!(
            c.wildcard_pattern("az"),
            Some(format!("a{S}")),
            "对照：a 起头照常"
        );
    }

    /// 非首位有冲突（通配键 `=` 是出厂翻页键）⇒ 非首位的通配键只可能是经让位进来的字面。
    /// 首位不受翻页键影响（空缓冲无页可翻）⇒ 照常作通配（论坛 t302）。
    #[test]
    fn pattern_keeps_mid_positions_literal_on_conflict() {
        let Some(c) = wubi_z(|cfg| cfg.schema.codetable.wildcard_key = "=".into()) else {
            return;
        };
        assert_eq!(c.wildcard_decision("", '='), Enter, "首位符号通配键作通配");
        assert_eq!(
            c.wildcard_decision("a", '='),
            Yield,
            "前置：非首位让位给翻页"
        );
        assert_eq!(c.wildcard_pattern("a="), None);
        assert_eq!(
            c.wildcard_pattern("=a="),
            Some(format!("{S}a=")),
            "首位通配、非首位字面"
        );
    }

    /// 符号通配键首位**作通配**（论坛 t302），与组码中一致；pattern 与按键裁决同一结论。
    /// 对照：字母通配键首位全无绑定时同样作通配。
    #[test]
    fn symbol_wildcard_enters_at_lead() {
        let Some(c) = wubi_z(|cfg| cfg.schema.codetable.wildcard_key = "?".into()) else {
            return;
        };
        assert_eq!(c.wildcard_decision("", '?'), Enter);
        assert_eq!(c.wildcard_decision("a", '?'), Enter, "组码中照常作通配");
        assert_eq!(c.wildcard_pattern("a?"), Some(format!("a{S}")));
        assert_eq!(
            c.wildcard_pattern("?a"),
            Some(format!("{S}a")),
            "首位也是通配"
        );

        let Some(c) = wubi_z(|_| {}) else { return };
        assert_eq!(
            c.wildcard_decision("", 'z'),
            Enter,
            "对照：字母键首位无绑定"
        );
    }

    /// 通配接管全局引导键时要能报出来（出厂反引号 = 临拼触发）；字母键与关闭通配时不报。
    #[test]
    fn wildcard_lead_override_of_global_binding_is_reported() {
        let Some(c) = wubi_z(|cfg| cfg.schema.codetable.wildcard_key = "`".into()) else {
            return;
        };
        assert!(c.wildcard_lead_overrides_global('`').is_some());
        let Some(c) = wubi_z(|_| {}) else { return };
        assert!(c.wildcard_lead_overrides_global('z').is_none());
    }

    /// 满码后通配键按字面：`aaaa` + `z` 让位，`aaaaz` 不是通配组码；按键裁决与 pattern
    /// 同一判据。对照：未满码时照常通配，满码前的通配位不受影响。
    #[test]
    fn wildcard_past_full_length_is_literal() {
        let Some(c) = wubi_z(|_| {}) else { return };
        assert_eq!(c.wildcard_decision("aaa", 'z'), Enter, "对照：未满码");
        assert_eq!(c.wildcard_decision("aaaa", 'z'), Yield);
        assert_eq!(c.wildcard_pattern("aaaaz"), None);
        assert_eq!(c.wildcard_pattern("azaaz"), Some(format!("a{S}aaz")));
    }

    /// ★ Review Focus 3：主输入路的上屏记账对通配组码下的码表候选记**候选全码**，不记
    /// `azzz` 这种通配串（读端永远查不中的孤儿键）。`freq_code` 本身不变（mix / 临拼缓冲
    /// 与读侧调频沿用它）。对照：非通配组码沿用缓冲；非码表来源不受影响。
    #[test]
    fn main_freq_code_under_wildcard_is_candidate_full_code() {
        use wind_candidate::{Candidate, CandidateSource};
        let Some(c) = wubi_z(|_| {}) else { return };
        let cand = Candidate {
            text: "工".into(),
            code: "aaaa".into(),
            source: CandidateSource::CodeTable,
            ..Default::default()
        };
        assert_eq!(c.main_freq_code("azzz", &cand), "aaaa");
        assert_eq!(
            c.main_freq_code("aaa", &cand),
            "aaa",
            "对照：非通配沿用缓冲"
        );
        assert_eq!(c.freq_code("azzz", &cand), "azzz", "freq_code 本体不变");
        let phrase = Candidate {
            text: "、".into(),
            source: CandidateSource::Phrase,
            ..Default::default()
        };
        assert_eq!(
            c.main_freq_code("azzz", &phrase),
            c.freq_code("azzz", &phrase)
        );
    }

    /// 五笔拼音混输 + 通配键 `z`；缺数据返回 `None`。
    fn wubi_pinyin_z(tweak: impl FnOnce(&mut Config)) -> Option<Arc<Coordinator>> {
        if !data_dir()
            .join("schemas/wubi86_pinyin.schema.toml")
            .exists()
        {
            eprintln!("跳过：混输方案不存在");
            return None;
        }
        wubi_z(|cfg| {
            cfg.schema.available = vec!["wubi86_pinyin".into(), "wubi86".into(), "pinyin".into()];
            cfg.schema.active = "wubi86_pinyin".into();
            tweak(cfg);
        })
    }

    /// ★ Review Focus 5：混输首位一律字面（z 是拼音声母），不靠 `zz*` 短语——这里**不装**
    /// 短语，混输下 `has_code_prefix("z")` 为假，旧规则会判 Enter。首位字面 ⇒ 整串字面。
    #[test]
    fn mixed_lead_is_always_literal() {
        let Some(c) = wubi_pinyin_z(|_| {}) else {
            return;
        };
        assert_eq!(c.wildcard_decision("", 'z'), Yield, "混输首位 z 是拼音声母");
        assert_eq!(c.wildcard_decision("z", 'z'), Yield, "首位字面 ⇒ 整轮字面");
        assert_eq!(c.wildcard_pattern("zhan"), None);
        assert_eq!(c.wildcard_pattern("zaz"), None);
        assert_eq!(
            c.wildcard_decision("a", 'z'),
            Enter,
            "对照：非首位码长内照常通配"
        );
    }

    /// 混输：整串 > 主码表码长 ⇒ 整串字面（连前段通配位一起）；码长取主码表的 4，
    /// 不是混输 `max_code_length` 的 0。对照纯五笔 `azaaz` → `a?aaz`（`wildcard_past_full_length_is_literal`）。
    #[test]
    fn mixed_overlength_buffer_is_literal() {
        let Some(c) = wubi_pinyin_z(|_| {}) else {
            return;
        };
        assert_eq!(c.wildcard_pattern("hanz"), Some(format!("han{S}")));
        assert_eq!(c.wildcard_pattern("gz"), Some(format!("g{S}")));
        assert_eq!(c.wildcard_pattern("hanzi"), None, "超码长整串字面");
        assert_eq!(c.wildcard_pattern("azaaz"), None, "前段通配位也随整串字面");
        assert_eq!(c.wildcard_decision("xian", 'z'), Yield, "码长取主码表的 4");
        assert_eq!(c.wildcard_decision("han", 'z'), Enter);
    }

    /// ★ Review Focus 4：拼音分段续转态（最后一段是拼音选词）剩余串一律字面，按键也不作通配。
    #[test]
    fn pinyin_continuation_is_literal() {
        let Some(c) = wubi_pinyin_z(|_| {}) else {
            return;
        };
        let mut st = crate::coordinator::State {
            input_buffer: "aizi".into(),
            ..Default::default()
        };
        assert_eq!(
            c.wildcard_pattern_of(&st),
            Some(format!("ai{S}i")),
            "前置：无分段时码长内照常通配"
        );
        st.committed_segs.push(crate::coordinator::CommittedSeg {
            raw_code: "wo".into(),
            code: "wo".into(),
            text: "我".into(),
            source: wind_candidate::CandidateSource::Pinyin,
            boundary: 0,
            learn: None,
        });
        assert_eq!(c.wildcard_pattern_of(&st), None, "拼音分段续转 ⇒ 字面");
        st.input_buffer = "ai".into();
        assert!(!c.wildcard_enters(&st, 'z'), "续转态按键也不作通配");
        st.committed_segs.clear();
        assert!(c.wildcard_enters(&st, 'z'), "对照：无分段时作通配");
    }
}
