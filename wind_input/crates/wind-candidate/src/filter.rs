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
/// - [`single_char_admits`]（单字输入）**恰恰不能或上** `is_common`——常用**词**
///   （「中国」「时候」）同样 `is_common = true`，带上它单字模式就整个失效了。
///
/// ⇒ ★ 直接把 `is_common_like` 当豁免判据用是错的。两个过滤器问的是不同的问题
/// （「这条算不算常用」 vs 「这条是不是用户自己配的」），只是答案在这三项上碰巧一致。
pub fn is_user_authored(c: &Candidate) -> bool {
    c.is_phrase || c.is_command || c.is_group
}

/// 含生僻字的**多字候选**（词组）在检索范围过滤下的处置。
///
/// # 要解决什么
///
/// [`crate::CommonChars::is_string_common`] 是**全称**判据：整串每字都常用才算常用。
/// 于是「苯磺酸」（磺）、「馎饦」、五笔的「磳碟」这类**词库正常收录的词**，只因某一个字
/// 不在通用规范汉字表（8105 字）里，整条就被判非常用 —— 常用字档直接滤掉、智能档在同码
/// 有常用候选时滤掉（五笔 4 码词位几乎必然同码有常用字，故默认档同样命中）。论坛 t103。
///
/// ⚠️ 举例别想当然：「饕餮」「耄耋」「旮旯」「貔貅」**全在表内**（三级字表收了它们），
/// 拿它们当素材两档结果相同。真正落在表外的是异体字、日韩汉字与冷僻化学 / 人名用字。
///
/// # 一般输入法怎么做
///
/// RIME 的 `charset_filter` 同样是「有一即滤」的全称判据（`FilterText` 逐字扫描整条候选），
/// 社区的 gbk/gb2312 lua 过滤器亦然。但它们的判据是**编码字符集**（GBK 21003 字），词库里
/// 的正常词几乎都落在范围内；而 gb2312 档（6763 字）在社区公认「过紧、很多词打不出」，
/// 通行解法正是换用更宽的集合。搜狗 / 微软拼音一类则根本不拿常用字表去裁词库词 —— 字符集
/// 设置影响的是**单字**可用性，生僻词靠词频沉底而非硬过滤。
///
/// ⇒ 行业共识是「过滤作用在单字层，多字词由词库收录背书」，出厂档 [`Keep`](Self::Keep)
/// 取的就是这个语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RarePhrasePolicy {
    /// 整词放行：多字候选不受检索范围裁剪（出厂）。
    #[default]
    Keep,
    /// 与单字同判：整串每字都常用才留（0.121 及以前的行为）。
    Filter,
}

impl RarePhrasePolicy {
    /// 从配置值（`input.rare_phrase`）解析。未知值回退 [`Keep`](Self::Keep)，
    /// **与 `Default` 同源**——理由同 [`FilterMode::from_config`]：「没配」与「配错」
    /// 若落到不同档，差异极难被想到。
    pub fn from_config(s: &str) -> Self {
        match s {
            "filter" => Self::Filter,
            _ => Self::Keep,
        }
    }

    /// 配置值（`input.rare_phrase`）。与 [`from_config`](Self::from_config) 成对。
    pub fn as_config(&self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Filter => "filter",
        }
    }
}

/// 这条候选是否吃「整词放行」豁免：[`Keep`](RarePhrasePolicy::Keep) 档下的**多字**候选。
///
/// # 「多字」按字素簇算
///
/// 与 [`single_char_admits`] 同一把尺子（UAX #29），故 `⚽️`(2 码位)、`👨‍👩‍👧`(5 码位)
/// 都只算一个字、不吃豁免。⛔ 别退回 `chars().count() > 1` 去数码位 —— 那会把单个 emoji
/// 当成「词」放行，而 issue #83 的教训就是自己列举 Unicode 规则必随版本静默漏一类。
///
/// # 热路径：两个调用点的位置**不同**，都不能随手挪
///
/// `nth(1)` 最多推进两簇就停，不是整串切分，故代价本就不大。
///
/// - `filter_common_only` 排在 `is_common_like` **之后**：常用候选本就该留，不必问豁免。
/// - `filter_smart` **必须排在最前**（`user_rare` 判据之前）：词里含用户亲手标成生僻的字
///   也要放行（2026-09-17 拍板）。挪到 `is_common_like` 之后，「常用多字词 + `user_rare`」
///   那一格就从放行翻成滤掉，与那条拍板相反。
///
/// ⇒ 出厂档下智能档**每条候选**都会跑一次 `nth(1)`，这是明知的代价，不是漏写了短路。
pub fn rare_phrase_admits(c: &Candidate, policy: RarePhrasePolicy) -> bool {
    use unicode_segmentation::UnicodeSegmentation;
    policy == RarePhrasePolicy::Keep && c.text.graphemes(true).nth(1).is_some()
}

/// 单条候选在**单字输入模式**（`single_char`）下是否放行。
///
/// 五笔系输入法的传统功能。用途不止「练习拆字」：五笔单字恒 ≤ 全码长，只出单字时
/// 「满码唯一即上屏」的成立率大幅提高，用户可以不看候选窗连续敲——这是它对码表方案的
/// 主要价值，也是拼音（没有定长）默认不开的原因。
///
/// # 只有「开 / 关」一个开关
///
/// 曾做成 `all / char / phrase` 三档（对齐极点 / 万能 / QQ 五笔的「字词 / 单字 / 词组」）。
/// **「只出词组」档 2026-09-09 已删**：用户实测其表现「非常奇怪」——单字被整批滤掉后，
/// 大量码位只剩零星几条甚至空列表，而五笔用户按码取字的肌肉记忆全部落空。它与「只出
/// 单字」看似对称，实际价值完全不对等，留着只是徒增一个没人用却处处要考虑的状态。
///
/// ⇒ 关时不调用本函数（调用方短路），故这里**没有**「全出」那一档要表达。
///
/// # 豁免：短语 / 命令 / 分组照常显示
///
/// 2026-09-07 用户拍板。本项管的是**词库**出什么，而这三类是用户自己配出来的
/// 快捷方式（`fj` → 一整段文本）——不该因为开了单字模式就整个消失。判据走
/// [`is_user_authored`]，⛔ **不是** [`is_common_like`]（那个会连常用词一起豁免，
/// 单字模式当场失效）。
///
/// # 「一个字」按字素簇算
///
/// 走 [`crate::single_markable_char`]（UAX #29），故 `⚽️`(2 码位)、`👨‍👩‍👧`(5 码位)
/// 都算一个字。⛔ 别退回 `chars().count() == 1` 去数码位——issue #83 的教训是逐条
/// 列举 Unicode 规则必随版本静默漏一类。
pub fn single_char_admits(c: &Candidate) -> bool {
    is_user_authored(c) || crate::single_markable_char(&c.text).is_some()
}

/// 按模式过滤候选词。`phrase` 是含生僻字的**词**怎么处置，见 [`RarePhrasePolicy`]——
/// 它与 `mode` 正交：`mode` 决定「什么算该滤」，`phrase` 决定「词要不要吃这一刀」。
pub fn filter_candidates(
    candidates: Vec<Candidate>,
    mode: FilterMode,
    phrase: RarePhrasePolicy,
) -> FilterOutcome {
    match mode {
        FilterMode::Gb18030 => FilterOutcome {
            kept: candidates,
            filtered: Vec::new(),
        },
        FilterMode::General => filter_common_only(candidates, phrase),
        FilterMode::Smart => filter_smart(candidates, phrase),
    }
}

/// 只保留常用词、短语、命令、分组（外加 [`Keep`](RarePhrasePolicy::Keep) 档下的多字候选）
fn filter_common_only(candidates: Vec<Candidate>, phrase: RarePhrasePolicy) -> FilterOutcome {
    let (kept, filtered) = candidates
        .into_iter()
        .partition(|c| is_common_like(c) || rare_phrase_admits(c, phrase));
    FilterOutcome { kept, filtered }
}

/// 智能档的分组键：按 `(来源, 码)`，通配结果除外。
///
/// **必须带来源**：混输下码表（五笔码）与拼音候选常共用同一 code 字符串（原始输入，如
/// "wang"），但属不同编码体系；若仅按 code 分组，常用的拼音候选会误使同 code 的生僻码表字
/// （如 佢）被过滤，导致混输码表主方案与纯五笔表现不一致。
///
/// **通配结果（[`Candidate::is_wildcard`]）整份一组**（spec `codetable-wildcard.md` §11）：
/// 用户没有打出那些码，按码分组时「某码下只有生僻字」会被当孤儿码位放行，首页于是全是
/// 生僻字。一次构建只有一个 pattern，故「按 pattern 分组」＝「通配结果一组」，不必把
/// pattern 传进来。`is_wildcard` 只由码表通配入口置位，非通配候选的键与原 `(source, code)`
/// 一一对应——非通配路径的分组逐字节不变。
#[derive(Clone, PartialEq, Eq, Hash)]
enum SmartGroup {
    Code(CandidateSource, String),
    Wildcard(CandidateSource),
}

impl SmartGroup {
    fn of(c: &Candidate) -> Self {
        if c.is_wildcard {
            Self::Wildcard(c.source)
        } else {
            Self::Code(c.source, c.code.clone())
        }
    }
}

/// 统计每个 [`SmartGroup`]「是否存在常用词」。
fn build_has_common(candidates: &[Candidate]) -> std::collections::HashMap<SmartGroup, bool> {
    use std::collections::HashMap;
    let mut has_common: HashMap<SmartGroup, bool> = HashMap::new();
    for c in candidates {
        let group = SmartGroup::of(c);
        // 先建组（哪怕非常用），使「该码位下无常用词」与「该码位没出现过」区分开。
        has_common.entry(group.clone()).or_insert(false);
        if !is_common_like(c) {
            continue;
        }
        has_common.insert(group, true);
        // ⚠️ 通配候选只写自己那一组：它的码位（含 merged_codes）若写进按码分组，同码位的
        // 非通配生僻字会被一条通配常用字遮蔽，「非通配分组不变」就破了。
        if c.is_wildcard {
            continue;
        }
        // 去重吃掉的同文本码位一并遮蔽（见 `Candidate::merged_codes`）：「档」以简码 siv 命中时，
        // 它在 sivg 的那条已被去重丢弃，若不还原这层归属，sivg 组只剩生僻的「桜」而当孤儿码
        // 放行 —— 同一个字打 siv 出、打全 sivg 反而不出。非常用候选无需还原：它不遮蔽任何人。
        for code in &c.merged_codes {
            has_common.insert(SmartGroup::Code(c.source, code.clone()), true);
        }
    }
    has_common
}

/// 智能过滤：同一组（来源+编码；通配结果整份一组，见 [`SmartGroup`]）下有常用词则过滤非常用词
fn filter_smart(candidates: Vec<Candidate>, phrase: RarePhrasePolicy) -> FilterOutcome {
    // ⚠️ 「该组有没有常用词」的统计**不吃词豁免**（`build_has_common` 只问 `is_common_like`）。
    // 若把放行的词也算作「常用」，它就会遮蔽同码位的生僻**单字**，把孤儿码位保底顶掉——
    // 打某个码原本字、词都在，修完反而只剩词。本次改的是「词要不要被滤」，不该顺手改单字。
    let has_common = build_has_common(&candidates);
    let (kept, filtered) = candidates.into_iter().partition(|c| {
        // 整词放行（`input.rare_phrase = "keep"`，出厂）：多字候选直接留下。
        //
        // ★ 位置在 `user_rare` **之前**是用户 2026-09-17 拍板的：词里含一个被亲手标成
        // 生僻的字，整条词照样放行。下面那条 `user_rare` 因此只管**单字**——它要挡的现象
        // （把同码位唯一的常用字降级后，保底又把它原样放回第一位）本就只发生在单字上。
        if rare_phrase_admits(c, phrase) {
            return true;
        }
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
        let common_exists = has_common.get(&SmartGroup::of(c)).copied().unwrap_or(false);
        // 同组存在常用词则只保留常用词；否则保留全部（孤儿编码 / 全无常用字的通配结果）
        !common_exists || is_common_like(c)
    });
    FilterOutcome { kept, filtered }
}

#[cfg(test)]
#[path = "filter_tests.rs"]
mod tests;
