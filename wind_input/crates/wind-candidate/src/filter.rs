//! 候选词过滤逻辑
//!
//! 与 Go 版本 `wind_input/internal/candidate/filter.go` 对齐。

use crate::candidate::{Candidate, CandidateSource};

/// 过滤模式
///
/// `Default` 取 `Smart`，**与 [`from_config`](Self::from_config) 的未知值回退同源**
/// ——那里拼错的配置值也落到 Smart。两处若给出不同的「默认」，会出现
/// 「没配过滤模式」与「配错了过滤模式」表现不一致，而这种差异极难被想到。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilterMode {
    /// 不过滤
    Gb18030,
    /// 只保留常用词
    General,
    /// 智能过滤（同编码下有常用词则过滤非常用词）
    #[default]
    Smart,
}

impl FilterMode {
    /// 从配置值（`input.filter_mode`）解析。未知值一律回退 Smart——配置是用户可手改的
    /// 文本，拼错不该让输入法失去过滤能力。故不实现 `FromStr`（那要求返回 `Result`），
    /// 命名与 [`as_config`](Self::as_config) 对称。
    pub fn from_config(s: &str) -> Self {
        match s {
            "gb18030" => Self::Gb18030,
            "general" => Self::General,
            _ => Self::Smart,
        }
    }

    /// 配置值（`input.filter_mode`）。与 [`from_config`](Self::from_config) 成对：菜单切换要把
    /// 新模式写回配置（config 为单一源，见 `set_filter_mode`），故必须能反向取到配置字符串。
    pub fn as_config(&self) -> &'static str {
        match self {
            Self::Gb18030 => "gb18030",
            Self::General => "general",
            Self::Smart => "smart",
        }
    }
}

/// 过滤结果：保留集 + **被滤集**。
///
/// 被滤集不是调试信息，是「检索范围放宽」的数据来源（设计见
/// `docs/design/smart-filter-scope-relax.md`）：智能档下候选不足一页时从中回补，
/// 手动临时放宽时整体并回。**过滤器本就算出了这个集合，此前直接丢弃**——留下它使放宽
/// 无需重新查询词库、无需重新排序，被滤集天然保持原排序序。
pub struct FilterOutcome {
    /// 按当前模式保留、正常显示的候选。
    pub kept: Vec<Candidate>,
    /// 被本次过滤剔除的候选，保持原有相对顺序。`Gb18030`（不过滤）时恒空。
    pub filtered: Vec<Candidate>,
}

/// 「常用词类」判据：常用字 / 短语 / 命令 / 分组一律豁免过滤。
/// 两个模式共用同一判据，抽出以免两处分叉。
///
/// 对外公开的理由：emoji 扩展的「列表末尾」档要落在**常用候选之后、生僻候选之前**，
/// 判据必须与过滤器的「什么算常用」逐字一致，否则两边对同一条候选的归类会分叉。
pub fn is_common_like(c: &Candidate) -> bool {
    c.is_common || is_user_authored(c)
}

/// 用户**自己配出来的**候选：短语、命令、短语分组。
///
/// 抽成独立函数是因为它有两个语义不同的消费者，而两者只在这三项上重合：
///
/// - [`is_common_like`]（检索范围过滤）在此之上还要或上 `is_common`；
/// - [`word_scope_admits`]（字词范围）**恰恰不能或上** `is_common`——常用**词**
///   （「中国」「时候」）同样 `is_common = true`，带上它单字模式就整个失效了。
///
/// ⇒ ★ 直接把 `is_common_like` 当豁免判据用是错的。两个过滤器问的是不同的问题
/// （「这条算不算常用」 vs 「这条是不是用户自己配的」），只是答案在这三项上碰巧一致。
pub fn is_user_authored(c: &Candidate) -> bool {
    c.is_phrase || c.is_command || c.is_group
}

/// 字词范围（`word_scope`）：候选里出单字、出词组，还是两者都出。
///
/// 五笔系输入法的传统功能（极点 / 万能 / QQ 五笔的「字词 / 单字 / 词组」三态）。
/// [`Char`](Self::Char) 的用途不止「练习拆字」：五笔单字恒 ≤ 全码长，只出单字时
/// 「满码唯一即上屏」的成立率大幅提高，用户可以不看候选窗连续敲——这是它对码表方案的
/// 主要价值，也是拼音（没有定长）默认不开的原因。
///
/// `Default` 取 [`All`](Self::All)，**与 [`from_config`](Self::from_config) 的未知值
/// 回退同源**——理由同 [`FilterMode`]：两处若给出不同的「默认」，会出现「没配」与
/// 「配错了」表现不一致，而这种差异极难被想到。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WordScope {
    /// 字与词都出（出厂）。
    #[default]
    All,
    /// 只出单字。
    Char,
    /// 只出词组。
    Phrase,
}

impl WordScope {
    /// 从配置值（`word_scope`）解析。未知值一律回退 [`All`](Self::All)——配置是用户
    /// 可手改的文本，拼错不该让候选列表凭空少掉一半。故不实现 `FromStr`（那要求返回
    /// `Result`），命名与 [`as_config`](Self::as_config) 对称，同 [`FilterMode`]。
    pub fn from_config(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "char" => Self::Char,
            "phrase" => Self::Phrase,
            _ => Self::All,
        }
    }

    /// 配置值。与 [`from_config`](Self::from_config) 成对：运行时切换要能反向取到字符串
    /// （设置页回显、日志、状态泡文案）。
    pub fn as_config(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Char => "char",
            Self::Phrase => "phrase",
        }
    }

    /// 是否「不施加任何限制」。热路径上用它短路，省掉逐候选的字素簇切分。
    pub fn is_all(&self) -> bool {
        matches!(self, Self::All)
    }
}

/// 单条候选在给定字词范围下是否放行。
///
/// # 豁免：短语 / 命令 / 分组照常显示
///
/// 2026-09-07 用户拍板。字词范围管的是**词库**出什么，而这三类是用户自己配出来的
/// 快捷方式（`fj` → 一整段文本）——不该因为开了单字模式就整个消失。判据走
/// [`is_user_authored`]，⛔ **不是** [`is_common_like`]（那个会连常用词一起豁免，
/// 单字模式当场失效）。
///
/// # 「一个字」按字素簇算
///
/// 走 [`crate::single_markable_char`]（UAX #29），故 `⚽️`(2 码位)、`👨‍👩‍👧`(5 码位)
/// 都算一个字。⛔ 别退回 `chars().count() == 1` 去数码位——issue #83 的教训是逐条
/// 列举 Unicode 规则必随版本静默漏一类。
///
/// # 两态互补是刻意的
///
/// 豁免项之外，[`Char`](WordScope::Char) 放行的与 [`Phrase`](WordScope::Phrase) 放行的
/// **恰好互补**（判据是同一个 `is_some()` 的正反两面）。于是用户在两态间来回切换时，
/// 不会有候选两边都不出现——那种「切过去没有、切回来也没有」的洞最难被报告。
/// 空串与纯空白候选是唯一的例外：它们在 `Char` 下被 `is_markable` 拒绝、在 `Phrase`
/// 下作为「非单字」放行。正常列表里不存在这种候选，故不为它专门开一条判据。
pub fn word_scope_admits(c: &Candidate, scope: WordScope) -> bool {
    if scope.is_all() || is_user_authored(c) {
        return true;
    }
    let single = crate::single_markable_char(&c.text).is_some();
    match scope {
        WordScope::All => true,
        WordScope::Char => single,
        WordScope::Phrase => !single,
    }
}

/// 按模式过滤候选词
pub fn filter_candidates(candidates: Vec<Candidate>, mode: FilterMode) -> FilterOutcome {
    match mode {
        FilterMode::Gb18030 => FilterOutcome {
            kept: candidates,
            filtered: Vec::new(),
        },
        FilterMode::General => filter_common_only(candidates),
        FilterMode::Smart => filter_smart(candidates),
    }
}

/// 只保留常用词、短语、命令、分组
fn filter_common_only(candidates: Vec<Candidate>) -> FilterOutcome {
    let (kept, filtered) = candidates.into_iter().partition(is_common_like);
    FilterOutcome { kept, filtered }
}

/// 按 (来源, code) 分组统计「该组是否存在常用词」。
///
/// **必须带来源**：混输下码表（五笔码）与拼音候选常共用同一 code 字符串（原始输入，如
/// "wang"），但属不同编码体系；若仅按 code 分组，常用的拼音候选会误使同 code 的生僻码表字
/// （如 佢）被过滤，导致混输码表主方案与纯五笔表现不一致。按来源隔离后，码表候选只受同来源
/// 候选影响，混输码表与纯五笔一致。
fn build_has_common(
    candidates: &[Candidate],
) -> std::collections::HashMap<(CandidateSource, String), bool> {
    use std::collections::HashMap;
    let mut has_common: HashMap<(CandidateSource, String), bool> = HashMap::new();
    for c in candidates {
        // 先建组（哪怕非常用），使「该码位下无常用词」与「该码位没出现过」区分开。
        has_common
            .entry((c.source, c.code.clone()))
            .or_insert(false);
        if !is_common_like(c) {
            continue;
        }
        has_common.insert((c.source, c.code.clone()), true);
        // 去重吃掉的同文本码位一并遮蔽（见 `Candidate::merged_codes`）：「档」以简码 siv 命中时，
        // 它在 sivg 的那条已被去重丢弃，若不还原这层归属，sivg 组只剩生僻的「桜」而当孤儿码
        // 放行 —— 同一个字打 siv 出、打全 sivg 反而不出。非常用候选无需还原：它不遮蔽任何人。
        for code in &c.merged_codes {
            has_common.insert((c.source, code.clone()), true);
        }
    }
    has_common
}

/// 智能过滤：同一来源+编码下有常用词则过滤非常用词
fn filter_smart(candidates: Vec<Candidate>) -> FilterOutcome {
    let has_common = build_has_common(&candidates);
    let (kept, filtered) = candidates.into_iter().partition(|c| {
        // 用户**亲手**标成生僻的字：无条件滤掉，不吃下面那条「孤儿编码」保底。
        //
        // 那条保底本意是别让人打不出字，但它对用户显式降级的字会起反作用：把同码位唯一的
        // 常用字降级后，这一组就变成「一个常用字都没有」，保底于是把它原样放回、还留在
        // 第一位——用户看到的是「设了完全没反应」，而这正是智能档**独有**的现象
        // （常用字档直接滤掉、全部字符档本就不过滤）。
        //
        // 滤掉不等于打不出：它落进 `filtered`，末页再按一次翻页键就能放宽调出。
        if c.user_rare {
            return false;
        }
        let common_exists = has_common
            .get(&(c.source, c.code.clone()))
            .copied()
            .unwrap_or(false);
        // 同来源同编码下存在常用词则只保留常用词；否则保留全部（孤儿编码）
        !common_exists || is_common_like(c)
    });
    FilterOutcome { kept, filtered }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用户亲手标成生僻的字**不吃孤儿码位保底**：哪怕同码位一个常用字都没有，它也照滤。
    ///
    /// 这是智能档独有的现象：把同码位唯一的常用字降级后，这一组变成「没有常用字」，
    /// 保底会把它原样放回、还留在第一位——用户看到「设了完全没反应」。
    #[test]
    fn user_rare_ignores_orphan_code_fallback() {
        let mut demoted = cand("档", "sivg", CandidateSource::CodeTable, false);
        demoted.user_rare = true;
        let rare = cand("桜", "sivg", CandidateSource::CodeTable, false);

        // 前置：不带 user_rare 时，这一组无常用字 ⇒ 孤儿码位保底放行全部。
        let baseline = filter_candidates(
            vec![
                cand("档", "sivg", CandidateSource::CodeTable, false),
                rare.clone(),
            ],
            FilterMode::Smart,
        );
        assert_eq!(baseline.kept.len(), 2, "无 user_rare 时保底放行全部");

        // 带上 user_rare：那一条被滤掉，另一条仍靠保底留下（不能连累它）。
        let out = filter_candidates(vec![demoted, rare], FilterMode::Smart);
        assert_eq!(
            out.kept.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            vec!["桜"],
            "用户降级的字应被滤掉，同组其余候选照旧"
        );
        assert_eq!(
            out.filtered
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>(),
            vec!["档"],
            "被滤的要落进 filtered——末页翻页放宽靠它把字调回来，不是真的打不出"
        );
    }

    /// 组里**只剩**一个被降级的字时也照滤（候选可以为空）。
    ///
    /// 空了不等于打不出：`try_relax_scope_on_page_end` 的触发条件只要求「有输入」，
    /// 不要求「有候选」，故空列表按翻页键仍能放宽把它调出来。
    #[test]
    fn user_rare_may_empty_the_group() {
        let mut only = cand("档", "sivg", CandidateSource::CodeTable, false);
        only.user_rare = true;
        let out = filter_candidates(vec![only], FilterMode::Smart);
        assert!(out.kept.is_empty());
        assert_eq!(out.filtered.len(), 1);
    }

    /// 同码位**还有**常用字时，行为与降级前一致（这条路径本来就滤，不该因新判据变样）。
    #[test]
    fn user_rare_unchanged_when_group_still_has_common() {
        let mut demoted = cand("桜", "sivg", CandidateSource::CodeTable, false);
        demoted.user_rare = true;
        let common = cand("档", "sivg", CandidateSource::CodeTable, true);
        let out = filter_candidates(vec![common, demoted], FilterMode::Smart);
        assert_eq!(
            out.kept.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            vec!["档"]
        );
    }

    fn cand(text: &str, code: &str, source: CandidateSource, is_common: bool) -> Candidate {
        Candidate {
            text: text.into(),
            code: code.into(),
            source,
            is_common,
            ..Default::default()
        }
    }

    /// 只取保留集——多数用例关心的是「显示成什么样」。被滤集另有专门用例覆盖。
    fn kept_of(candidates: Vec<Candidate>, mode: FilterMode) -> Vec<Candidate> {
        filter_candidates(candidates, mode).kept
    }

    #[test]
    fn smart_keeps_uncommon_codetable_when_common_pinyin_shares_code() {
        // 混输：码表生僻字 佢 与常用拼音 往/王 共用 code "wang"（不同来源）。
        // 智能过滤按来源隔离 → 佢 不被拼音的常用性挤掉（与纯五笔一致）。
        let out = kept_of(
            vec![
                cand("佢", "wang", CandidateSource::CodeTable, false),
                cand("往", "wang", CandidateSource::Pinyin, true),
                cand("王", "wang", CandidateSource::Pinyin, true),
            ],
            FilterMode::Smart,
        );
        assert!(
            out.iter().any(|c| c.text == "佢"),
            "混输下生僻码表字应保留（与纯五笔一致）"
        );
        assert!(out.iter().any(|c| c.text == "往"));
    }

    #[test]
    fn smart_filters_uncommon_within_same_source_and_code() {
        // 同来源同 code：有常用则滤非常用（原语义不变）。
        let out = kept_of(
            vec![
                cand("王", "wang", CandidateSource::Pinyin, true),
                cand("尪", "wang", CandidateSource::Pinyin, false),
            ],
            FilterMode::Smart,
        );
        assert!(out.iter().any(|c| c.text == "王"));
        assert!(
            !out.iter().any(|c| c.text == "尪"),
            "同来源同码有常用则滤非常用"
        );
    }

    #[test]
    fn filter_mode_config_round_trip() {
        // 菜单切换靠 as_config 写回配置、启动/热重载靠 from_config 读回；两者必须互逆，
        // 否则会出现「菜单选了全部字符、重启回到智能」这类静默回退。
        for m in [FilterMode::Smart, FilterMode::General, FilterMode::Gb18030] {
            assert_eq!(FilterMode::from_config(m.as_config()), m);
        }
        // 配置值集须与 wind-setting 的 select options 一致（smart/general/gb18030）。
        assert_eq!(FilterMode::Smart.as_config(), "smart");
        assert_eq!(FilterMode::General.as_config(), "general");
        assert_eq!(FilterMode::Gb18030.as_config(), "gb18030");
    }

    /// 回归：同一个字「打前缀出得来、打全码反而没了」。
    ///
    /// 现场＝五笔 `桜`(sivg)。词库里 `sivg` 码位下还有常用字「档」，而「档」另有简码 `siv`：
    /// 打 `siv` 时「档」以 code="siv" 入列、它在 sivg 的那条被去重丢弃 → sivg 组只剩「桜」
    /// 成孤儿码而放行；打全 `sivg` 时两者同组 → 「桜」被滤。**过滤结果因此不单调**。
    /// 修法是让去重把被弃条目的码位并进幸存者（`merged_codes`），两种输入下遮蔽关系一致。
    #[test]
    fn smart_filters_uncommon_when_common_shares_code_via_merged() {
        // 打 siv：「档」主码 siv，被去重吃掉的 sivg 记在 merged_codes
        let mut dang = cand("档", "siv", CandidateSource::CodeTable, true);
        dang.merged_codes = vec!["sivg".into()];
        let out = kept_of(
            vec![dang, cand("桜", "sivg", CandidateSource::CodeTable, false)],
            FilterMode::Smart,
        );
        assert!(
            !out.iter().any(|c| c.text == "桜"),
            "sivg 码位有常用字「档」（经 merged_codes 还原），生僻的「桜」应与打全码时一样被滤"
        );
        assert!(out.iter().any(|c| c.text == "档"));
    }

    /// ★ 反向对照：`merged_codes` 只还原**真实存在**的同码位关系，不得无差别扩大过滤。
    ///
    /// 没有这条，上面那个测试同样能被「有常用字就滤掉所有生僻字」的错误实现通过 ——
    /// 那会让 `sivs` 的「樑」等孤儿码字跟着陪葬。
    #[test]
    fn smart_keeps_uncommon_when_common_does_not_share_its_code() {
        // 「档」只占 siv/sivg，与「樑」的 sivs 无关 → 樑 仍是孤儿码，须保留
        let mut dang = cand("档", "siv", CandidateSource::CodeTable, true);
        dang.merged_codes = vec!["sivg".into()];
        let out = kept_of(
            vec![dang, cand("樑", "sivs", CandidateSource::CodeTable, false)],
            FilterMode::Smart,
        );
        assert!(
            out.iter().any(|c| c.text == "樑"),
            "sivs 下无常用字，孤儿码生僻字不该被别的码位牵连滤掉"
        );
    }

    /// 归并的**传递性**：去重是链式的（跨层 → 引擎层 → 协调器），同一个字可被连续吃三次。
    /// 若 `absorb` 只取对方的 `code`、不取对方已归并的码位，中间那次的码位会静默丢失。
    #[test]
    fn absorb_codes_is_transitive() {
        let mut a = cand("档", "siv", CandidateSource::CodeTable, true);
        let mut b = cand("档", "sivg", CandidateSource::CodeTable, true);
        b.absorb_code("sivx"); // b 早先吃掉过 sivx
        a.absorb_codes_from(&b);
        assert!(a.merged_codes.contains(&"sivg".to_string()), "对方主码");
        assert!(
            a.merged_codes.contains(&"sivx".to_string()),
            "对方已归并的码位不能丢"
        );
        // 自身主码不重复记，重复归并幂等
        a.absorb_codes_from(&b);
        assert!(!a.merged_codes.contains(&"siv".to_string()));
        assert_eq!(a.merged_codes.len(), 2, "重复归并应幂等");
    }

    /// 被滤集是「检索范围放宽」的唯一数据来源（自动补充从中回补、手动放宽整体并回），
    /// 必须**完整且保持原序**——顺序即是放宽后的呈现顺序，乱序会让补充项的位置不可预期。
    #[test]
    fn filtered_set_captures_removed_candidates_in_order() {
        let mut dang = cand("档", "siv", CandidateSource::CodeTable, true);
        dang.merged_codes = vec!["sivg".into()];
        let out = filter_candidates(
            vec![
                dang,
                cand("桜", "sivg", CandidateSource::CodeTable, false),
                cand("樑", "sivs", CandidateSource::CodeTable, false),
                cand("醇", "sivw", CandidateSource::CodeTable, false),
            ],
            FilterMode::Smart,
        );
        let texts = |v: &[Candidate]| v.iter().map(|c| c.text.clone()).collect::<Vec<_>>();
        // 樑/醇 是孤儿码得以保留；桜 因同码位有常用「档」（经 merged_codes 还原）被滤
        assert_eq!(texts(&out.kept), vec!["档", "樑", "醇"]);
        assert_eq!(texts(&out.filtered), vec!["桜"]);
    }

    /// 保留集与被滤集**恰好构成原集合的划分**：不重不漏。
    /// 若哪天改成「过滤时顺手丢弃某类候选」，放宽就会补不回它，且没有任何现象提示。
    #[test]
    fn kept_and_filtered_partition_the_input() {
        let input = vec![
            cand("王", "wang", CandidateSource::Pinyin, true),
            cand("尪", "wang", CandidateSource::Pinyin, false),
            cand("往", "wang", CandidateSource::Pinyin, true),
        ];
        let n = input.len();
        for mode in [FilterMode::Smart, FilterMode::General, FilterMode::Gb18030] {
            let out = filter_candidates(input.clone(), mode);
            assert_eq!(
                out.kept.len() + out.filtered.len(),
                n,
                "{mode:?}: 保留集+被滤集须等于原集合，不重不漏"
            );
        }
    }

    /// `Gb18030`（不过滤）下被滤集恒空——该档位没有「放宽」可言，补充逻辑应自然退化为无操作。
    #[test]
    fn gb18030_yields_empty_filtered_set() {
        let out = filter_candidates(
            vec![cand("桜", "sivg", CandidateSource::CodeTable, false)],
            FilterMode::Gb18030,
        );
        assert_eq!(out.kept.len(), 1);
        assert!(out.filtered.is_empty(), "不过滤档不该产生被滤集");
    }

    /// ★ 跨来源**不得**归并码位：与 `smart_keeps_uncommon_codetable_when_common_pinyin_shares_code`
    /// 同一条约束的另一端。混输下 "wang" 既是五笔码又是拼音码，属两套编码体系；若去重时把
    /// 拼音码并进码表候选，`(CodeTable, "wang")` 会被凭空标记为「有常用字」，反过来误滤同码的
    /// 码表生僻字——恰好是 `merged_codes` 要修的 bug 的对称形态。
    #[test]
    fn absorb_ignores_cross_source_codes() {
        let mut ct = cand("档", "sivg", CandidateSource::CodeTable, true);
        let py = cand("档", "dang", CandidateSource::Pinyin, true);
        ct.absorb_codes_from(&py);
        assert!(
            ct.merged_codes.is_empty(),
            "拼音码不得并入码表候选，实际: {:?}",
            ct.merged_codes
        );
        // 同来源仍照常归并（守卫不能把主逻辑一起挡掉）
        let same = cand("档", "siv", CandidateSource::CodeTable, true);
        ct.absorb_codes_from(&same);
        assert_eq!(ct.merged_codes, vec!["siv".to_string()]);
    }

    #[test]
    fn smart_keeps_orphan_uncommon_code() {
        // 孤儿编码（无常用同码）：保留全部（纯五笔生僻字场景）。
        let out = kept_of(
            vec![cand("佢", "wtn", CandidateSource::CodeTable, false)],
            FilterMode::Smart,
        );
        assert!(out.iter().any(|c| c.text == "佢"));
    }

    // ── 字词范围（word_scope）─────────────────────────────────────────────
    mod word_scope {
        use super::*;

        fn admits(text: &str, scope: WordScope) -> bool {
            word_scope_admits(&cand(text, "x", CandidateSource::CodeTable, false), scope)
        }

        #[test]
        fn all_admits_everything() {
            for t in ["大", "大家", "", "⚽️"] {
                assert!(admits(t, WordScope::All), "All 档不该滤掉任何东西：{t:?}");
            }
        }

        #[test]
        fn char_keeps_single_and_drops_words() {
            assert!(admits("大", WordScope::Char));
            assert!(!admits("大家", WordScope::Char));
            assert!(!admits("大家好", WordScope::Char));
        }

        #[test]
        fn phrase_keeps_words_and_drops_single() {
            assert!(!admits("大", WordScope::Phrase));
            assert!(admits("大家", WordScope::Phrase));
        }

        /// 「一个字」按**字素簇**算，不按码位数。
        ///
        /// ⛔ 判据退回 `chars().count() == 1` 的话这一条会红：`⚽️` 是 2 码位、
        /// `👨‍👩‍👧` 是 5 码位，屏幕上却都是一个图形。同 `single_markable_char` 的文档。
        #[test]
        fn char_counts_grapheme_clusters_not_code_points() {
            for t in ["⚽️", "👍🏻", "👨‍👩‍👧", "🇨🇳", "1️⃣"] {
                assert!(
                    admits(t, WordScope::Char),
                    "{t:?} 在屏幕上是一个字，单字档应放行"
                );
            }
        }

        /// 短语 / 命令 / 分组豁免：用户自己配的快捷方式不受字词范围管辖。
        #[test]
        fn user_authored_candidates_are_exempt_in_both_scopes() {
            let mut phrase = cand("今天天气不错", "fj", CandidateSource::Phrase, false);
            phrase.is_phrase = true;
            let mut command = cand("重启服务", "cc", CandidateSource::None, false);
            command.is_command = true;
            let mut group = cand("符号", "fh", CandidateSource::Phrase, false);
            group.is_group = true;

            for scope in [WordScope::Char, WordScope::Phrase] {
                for c in [&phrase, &command, &group] {
                    assert!(
                        word_scope_admits(c, scope),
                        "{:?} 档不该滤掉用户自己配的 {:?}",
                        scope,
                        c.text
                    );
                }
            }
        }

        /// ★★★ 本轮最容易写错的一条：**常用词不豁免**。
        ///
        /// 豁免判据若图省事写成既有的 `is_common_like`（= `is_common ||` 那三项），
        /// 常用**词**（「中国」「时候」——`mark_common` 对逐字皆常用的词同样置
        /// `is_common = true`）会一并豁免，单字档就只剩生僻词被滤掉，**整个功能失效**
        /// 而候选窗看着还挺正常。
        ///
        /// 变异验证：把 `word_scope_admits` 的豁免换成 `is_common_like`，本条精确红，
        /// 上面那条 `user_authored_candidates_are_exempt_in_both_scopes` 照样绿。
        #[test]
        fn common_words_are_not_exempt() {
            let common_word = cand("中国", "x", CandidateSource::CodeTable, true);
            assert!(
                !word_scope_admits(&common_word, WordScope::Char),
                "常用词也是词，单字档必须滤掉——豁免判据不能带上 is_common"
            );

            let common_char = cand("大", "x", CandidateSource::CodeTable, true);
            assert!(
                word_scope_admits(&common_char, WordScope::Char),
                "常用字照留（反向对照，防止把上一条写成「凡 is_common 皆滤」）"
            );
        }

        /// 豁免项之外，两档**互补**：切过去没有、切回来也没有的候选不存在。
        #[test]
        fn char_and_phrase_partition_the_list() {
            for t in ["大", "大家", "⚽️", "a", "abc"] {
                let c = cand(t, "x", CandidateSource::CodeTable, false);
                assert_ne!(
                    word_scope_admits(&c, WordScope::Char),
                    word_scope_admits(&c, WordScope::Phrase),
                    "{t:?} 在两档里的归属应恰好互补"
                );
            }
        }

        /// 未知值回退 `All`，且与 `Default` 同源——两处给出不同的默认会让「没配」
        /// 与「配错了」表现不一致。同 `FilterMode::from_config`。
        #[test]
        fn from_config_falls_back_to_all() {
            assert_eq!(WordScope::from_config("char"), WordScope::Char);
            assert_eq!(WordScope::from_config(" PHRASE "), WordScope::Phrase);
            assert_eq!(WordScope::from_config("all"), WordScope::All);
            assert_eq!(WordScope::from_config("single"), WordScope::All);
            assert_eq!(WordScope::from_config(""), WordScope::All);
            assert_eq!(
                WordScope::default(),
                WordScope::from_config("no-such-value")
            );
        }

        /// `as_config` 与 `from_config` 成对：设置页回显与状态泡文案都要反向取字符串，
        /// 断链会让「切换了但界面没变」。
        #[test]
        fn as_config_round_trips() {
            for scope in [WordScope::All, WordScope::Char, WordScope::Phrase] {
                assert_eq!(WordScope::from_config(scope.as_config()), scope);
            }
        }
    }
}
