//! wind-assoc: 联想候选的生成与合并（纯逻辑，可原生测试）。
//!
//! 与 `wind-phrase` / `wind-punct` / `wind-quick-input` 同构：输入 → 候选的自包含单元，
//! 不持有宿主状态、不做 IO 决策，词典与用户数据以 trait 形式注入。
//!
//! 联想的输入是上文文本而不是编码缓冲，所以它不走 `EngineManager::convert_with(schema, code)`。
//!
//! ## 四个数据源与它们的排序
//!
//! | 源 | 数据来源 | 说明 |
//! |---|---|---|
//! | [`AssocSource::History`] | 用户上屏历史（redb） | 个人化，冷启动为空 |
//! | [`AssocSource::Bigram`] | 词→后继表（离线蒸馏） | 覆盖面最广 |
//! | [`AssocSource::Prefix`] | 码表词的文本前缀索引 | 长词补全，如「北京」→「大学」 |
//! | [`AssocSource::Punct`] | 静态规则表 + 学习覆盖 | 移动端刚需 |
//!
//! 跨源不比较分数，只按固定优先级取配额：四个源的分值量纲不同（计数 / log 概率 / 词频 / 无分），
//! 归一化混排解决不了系统性偏高。

use std::collections::HashSet;

pub mod bigram;
pub mod punct;

/// 联想的上文。
#[derive(Debug, Clone, Copy, Default)]
pub struct AssocContext<'a> {
    /// 刚上屏的文本（简体域的整词）。前缀词、联想历史、标点都按它查。
    pub text: &'a str,
    /// 这次上屏**之前**光标前的正文（宿主报来的，见 `Coordinator::set_preceding_text`），
    /// 末尾紧接着 `text`。只有「下一词」源用它：逐字上屏时把被拆开的词凑回来。
    /// 宿主不报（桌面）就是空串，一切照旧。
    pub preceding: &'a str,
    /// 用户逐字上屏（形码方案）：单字上屏后「下一词」源也给词内接续（「我」→「们」），
    /// 见 [`bigram::AssocBigram::lookup_after`]。
    pub char_wise: bool,
    /// 连续性是否已断（失焦 / 切宿主 / 鼠标点击 / 方向键 / 超时）。
    ///
    /// 断链时不做联想：`recent_commits` 是跨应用、跨焦点的全局队列，历史上相邻的两次上屏
    /// 不等于屏幕上相邻的两个词。
    pub boundary_broken: bool,
}

/// 智能联想里前缀词最多占几个名额，见 [`AssocKind::source_cap`]。
pub const SMART_PREFIX_CAP: usize = 4;

/// 智能联想里「下一词」最多占几个名额：留几个位置给前缀词与标点（表里每个上文有十来个后继，
/// 不设限会把名额全占掉）。
pub const SMART_BIGRAM_CAP: usize = 6;

/// 智能联想里前缀词补出部分的最大字数，见 [`AssocKind::keeps`]。
pub const SMART_PREFIX_TAIL_MAX: usize = 2;

/// 候选来自哪个源。**顺序即优先级**（靠前的先取配额）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AssocSource {
    /// 用户上屏历史学到的个人搭配。
    History,
    /// 词→后继表（离线从 n-gram 模型蒸馏）。
    Bigram,
    /// 码表词的文本前缀延伸（「北京」→「大学」）。
    Prefix,
    /// 标点与符号（静态规则表打底）。
    Punct,
}

impl AssocSource {
    /// 全部源，按优先级升序。[`associate`] 只遍历这个数组。
    ///
    /// 新增变体时必须登记进来，否则那个源被静默忽略。[`AssocSource::priority`] 的穷尽 `match`
    /// 会让新增变体编译失败，强制作者路过此处。
    pub const ALL: [AssocSource; 4] = [
        AssocSource::History,
        AssocSource::Bigram,
        AssocSource::Prefix,
        AssocSource::Punct,
    ];

    /// 优先级序号（越小越靠前）。存在意义是穷尽 `match` 让新增变体编译失败，提醒登记进
    /// [`Self::ALL`]（[`associate`] 按 `ALL` 的顺序遍历）。
    pub fn priority(self) -> usize {
        match self {
            AssocSource::History => 0,
            AssocSource::Bigram => 1,
            AssocSource::Prefix => 2,
            AssocSource::Punct => 3,
        }
    }
}

/// 一条联想候选。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssocHit {
    /// 展示文本。词语联想里这是整词（「中国」），不是要上屏的那一半。
    pub text: String,
    /// 上屏文本；`None` = 就用 [`Self::text`]。词语联想里显示「中国」、只补「国」。
    ///
    /// 存成字段而不让下游按前缀现算：Smart 档候选不以上文开头，统一 `strip_prefix` 会混淆两种档位。
    pub commit: Option<String>,
    /// 来源，决定它排在哪一档。
    pub source: AssocSource,
    /// 源内排序用的分数，跨源之间没有可比性（量纲不同）。
    pub score: i64,
}

impl AssocHit {
    /// 上屏该写出的文本。
    pub fn commit_text(&self) -> &str {
        self.commit.as_deref().unwrap_or(&self.text)
    }
}

/// 联想是哪一种。两种「联想」回答的是完全不同的问题：
///
/// | | 上文怎么用 | 打完「中」之后给什么 | 典型平台 |
/// |---|---|---|---|
/// | [`Word`](AssocKind::Word) | 当**前缀** | 「中国」「中间」「中心」——以「中」开头的词 | PC |
/// | [`Smart`](AssocKind::Smart) | 当**上下文** | 「国」「，」「的」——「中」后面常跟什么 | 移动端 |
///
/// 两者不是弱版/强版的关系，不能用一个开关的强弱表达：混在一起出，用户会看到「中国」和「国」
/// 并排，不知道选中哪个会得到什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AssocKind {
    /// 关闭。PC 默认：候选窗是浮层，联想常驻会挡正文，且数字键会被占用。
    #[default]
    Off,
    /// 词语联想：上文当前缀，出词库里以它开头的更长的词。
    ///
    /// 不含标点：「，」不是以上文开头的词。
    Word,
    /// 智能联想：上文当上下文，出下一个可能的词与标点。移动端默认。
    Smart,
}

impl AssocKind {
    /// 解析配置里的 `kind`。未知值一律回退 [`Off`](AssocKind::Off)：拼错了值就不启用，
    /// 好过静默启用一个用户没要求的档。
    pub fn parse(s: &str) -> Self {
        match s {
            "word" => AssocKind::Word,
            "smart" => AssocKind::Smart,
            _ => AssocKind::Off,
        }
    }

    /// 本档是否启用该数据源。
    ///
    /// 词语联想只认前缀延伸（[`AssocSource::Prefix`] 与 History 里的前缀延伸行）；其余源问的是
    /// 「后面接什么」，属上下文语义。
    pub fn allows(self, src: AssocSource) -> bool {
        match self {
            AssocKind::Off => false,
            // History 里的前缀延伸行（整词以上文开头）与词语联想「只出上文的延长」同形
            // ⇒ 词语联想档也放行它；同表的「下一词」行由读端按档位挡掉（`HistoryWords`）。
            AssocKind::Word => matches!(src, AssocSource::Prefix | AssocSource::History),
            AssocKind::Smart => true,
        }
    }

    /// 本档下，候选文本是否以上文为**前缀**（⇒ 上屏时只补剩余部分）。
    pub fn extends_context(self) -> bool {
        matches!(self, AssocKind::Word)
    }

    /// 本档下，前缀延伸类候选（「中」→「中国」）是否只显示补出的那一截（「国」）。
    ///
    /// 智能联想同栏还有下一词与标点，显示整词会让「中国」与「国」并排；词语联想整栏都是整词。
    /// 只改显示与去重的口径：[`AssocHit::text`] 仍是整词、`commit` 仍是那一截。
    pub fn shows_continuation(self) -> bool {
        matches!(self, AssocKind::Smart)
    }

    /// 本档下某个源最多占几个名额（`remaining` 是此刻还剩的总名额）。
    ///
    /// 智能联想里前缀词与下一词各设上限（[`SMART_PREFIX_CAP`] / [`SMART_BIGRAM_CAP`]），否则大词库下
    /// 它们占满名额、同栏标点出不来。词语联想整栏就是前缀词，不设限。
    pub fn source_cap(self, src: AssocSource, remaining: usize) -> usize {
        match (self, src) {
            (AssocKind::Smart, AssocSource::Prefix) => remaining.min(SMART_PREFIX_CAP),
            (AssocKind::Smart, AssocSource::Bigram) => remaining.min(SMART_BIGRAM_CAP),
            _ => remaining,
        }
    }

    /// 本档下这条候选能不能出（源内过滤，先于去重）。
    ///
    /// 智能联想只显示补出的那一截，补出过长的前缀词（「中」→「华人民共和国」）读起来是半截词，
    /// 丢掉；短的（「北京」→「大学」）正是下一词。
    pub fn keeps(self, hit: &AssocHit) -> bool {
        match (self, hit.source) {
            (AssocKind::Smart, AssocSource::Prefix) => {
                hit.commit_text().chars().count() <= SMART_PREFIX_TAIL_MAX
            }
            _ => true,
        }
    }
}

/// 联想何时展示、何时退出。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AssocMode {
    /// 仅上屏后出一次，任何非选词动作即退出。桌面默认。
    #[default]
    OneShot,
    /// 只要编码为空就持续展示。移动端默认（软键盘常驻，无额外遮挡成本）。
    Continuous,
}

impl AssocMode {
    /// 解析配置里的 `mode` 字符串。未知值回退 [`OneShot`](AssocMode::OneShot)。
    ///
    /// 移动端的 `Continuous` 来自 `[mobile.association]`（见 [`AssocConfig::from_config`]）。
    /// 别给本函数加「平台默认」参数：平台知识会顺着解析链一路渗透。
    pub fn parse(s: &str) -> Self {
        match s {
            "continuous" => AssocMode::Continuous,
            _ => AssocMode::OneShot,
        }
    }
}

/// `[input.association]`（叠加 `[mobile.association]` 覆盖后）的运行时视图。
///
/// 本 crate 只认已解析的具体值，不知道自己跑在哪个平台——平台差异在
/// [`AssocConfig::from_config`] 的调用方就已经落定。
#[derive(Debug, Clone, Copy)]
pub struct AssocConfig {
    /// 哪一种联想（含「关」）。开关与类型合并成一个字段，免得出现「开着但类型没配」的歧义状态。
    pub kind: AssocKind,
    /// [`associate`] 不读这个字段：它由宿主侧状态机消费，决定何时调用、何时退出联想态。
    pub mode: AssocMode,
    /// 候选总数上限。各源配额由它按优先级分配，配额本身是调参项、不暴露给用户。
    pub max_count: usize,
    /// 宿主侧消费：空格是否上屏当前高亮联想候选。
    pub space_commits: bool,
    /// 宿主侧消费：联想窗自动隐藏的毫秒数，`0` = 不自动隐藏。
    pub hide_after_ms: u64,
    pub history: bool,
    pub bigram: bool,
    pub prefix: bool,
    pub punct: bool,
}

impl Default for AssocConfig {
    fn default() -> Self {
        Self {
            // 桌面基线的镜像，必须逐字段等于 `wind_config::AssociationConfig::default()`
            // （见 `config_defaults_agree_with_runtime_defaults`）；移动端差异在 `[mobile.association]`。
            kind: AssocKind::Off,
            mode: AssocMode::OneShot,
            max_count: 9,
            space_commits: true,
            hide_after_ms: 5000,
            history: true,
            bigram: true,
            prefix: true,
            // 桌面关：实体键盘上标点一键可达，打完一个字就弹一串标点干扰大于收益。
            punct: false,
        }
    }
}

impl AssocConfig {
    /// 从配置段构造运行时视图。`cfg` 是桌面基线 `[input.association]`；`mobile` 为 `Some` 时
    /// `kind`/`mode`/`punct`/`space_commits` 改从 `[mobile.association]` 取，其余走基线。
    ///
    /// 要不要取 mobile 段由调用方决定，不在这里 `cfg!(target_os)`：跑在哪是宿主的知识。
    /// 往 `MobileAssociationConfig` 加字段时必须在这里放行（见 `mobile_section_does_not_touch_other_fields`）。
    pub fn from_config(
        cfg: &wind_config::AssociationConfig,
        mobile: Option<&wind_config::MobileAssociationConfig>,
    ) -> Self {
        let (kind, mode, punct, space_commits) = match mobile {
            Some(m) => (m.kind.as_str(), m.mode.as_str(), m.punct, m.space_commits),
            None => (
                cfg.kind.as_str(),
                cfg.mode.as_str(),
                cfg.punct,
                cfg.space_commits,
            ),
        };
        Self {
            kind: AssocKind::parse(kind),
            mode: AssocMode::parse(mode),
            max_count: cfg.max_count,
            space_commits,
            hide_after_ms: cfg.hide_after_ms,
            history: cfg.history,
            bigram: cfg.bigram,
            prefix: cfg.prefix,
            punct,
        }
    }

    /// 是否学用户的「下一词」（写入端据此决定记不记）。与读端同口径：下一词只在智能联想档出
    /// （词语联想只出上文的延长），且要历史源开着——用不上的用户不默默攒数据。
    pub fn learns_next_words(&self) -> bool {
        self.kind == AssocKind::Smart && self.source_enabled(AssocSource::History)
    }

    /// 该源是否启用：先过档位、再过开关。档位是硬性的，否则「词语联想 + punct=true」会出标点。
    pub fn source_enabled(&self, s: AssocSource) -> bool {
        if !self.kind.allows(s) {
            return false;
        }
        match s {
            AssocSource::History => self.history,
            AssocSource::Bigram => self.bigram,
            AssocSource::Prefix => self.prefix,
            AssocSource::Punct => self.punct,
        }
    }
}

/// 一个数据源的查询面。
///
/// 由调用方（协调器）为每个源注入实现，本 crate 不碰 IO。实现契约：
/// - `limit` 可以少给，不得多给；
/// - 必须返回本源内分数最高的那 `limit` 条：[`associate`] 只在收到结果后排序，救不回已被截断的候选；
/// - `limit == 0` 时直接返回空，不做查询。
pub trait AssocProvider {
    fn suggest(&self, ctx: &AssocContext<'_>, limit: usize) -> Vec<AssocHit>;
}

/// 按固定优先级 + 每源配额汇聚四个源。
///
/// 靠前的源先取，取不满的名额顺延给后面的源。按显示口径去重（[`AssocKind::shows_continuation`]
/// 档按上屏的那一截，否则按 `text`），先到先得，故同一个词保留优先级更高那条的来源标记。
pub fn associate(
    ctx: &AssocContext<'_>,
    cfg: &AssocConfig,
    providers: &[(AssocSource, &dyn AssocProvider)],
) -> Vec<AssocHit> {
    if cfg.kind == AssocKind::Off || cfg.max_count == 0 {
        return Vec::new();
    }
    // 断链或空上文 ⇒ 没有可依据的上下文，直接不出。
    if ctx.boundary_broken || ctx.text.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<AssocHit> = Vec::with_capacity(cfg.max_count);
    let mut seen: HashSet<String> = HashSet::new();

    for src in AssocSource::ALL {
        if out.len() >= cfg.max_count {
            break;
        }
        if !cfg.source_enabled(src) {
            continue;
        }
        // 智能联想、上文以语气词结尾（「好吗」）：后面几乎总是断句，只出个人学习与标点。
        // 下一词表与前缀词在这里只有噪声（网络语料的「吗的」、前缀词「吗啡」）。
        if cfg.kind == AssocKind::Smart
            && matches!(src, AssocSource::Bigram | AssocSource::Prefix)
            && punct::sentence_particle_punct(ctx.text).is_some()
        {
            continue;
        }
        let Some((_, p)) = providers.iter().find(|(s, _)| *s == src) else {
            continue;
        };
        let cap = cfg.kind.source_cap(src, cfg.max_count - out.len());
        // 有源内过滤时多取几条，免得滤完凑不满本源的名额
        let mut hits = p.suggest(ctx, cap.saturating_mul(4));
        // 源内按分数降序；跨源不比较（量纲不同）。
        hits.sort_by_key(|h| std::cmp::Reverse(h.score));
        let mut taken = 0;
        for h in hits {
            if taken >= cap {
                break;
            }
            if !cfg.kind.keeps(&h) {
                continue;
            }
            let shown = if cfg.kind.shows_continuation() {
                h.commit_text()
            } else {
                &h.text
            };
            if shown.is_empty() || seen.contains(shown) {
                continue;
            }
            // 只显示补出的那截时，前缀词常是前面某条的延长：「我」后已有「们」（下一词表），
            // 前缀词「我们的」再补「们的」就是重复的一格
            if cfg.kind.shows_continuation()
                && src == AssocSource::Prefix
                && seen.iter().any(|s| shown.starts_with(s.as_str()))
            {
                continue;
            }
            seen.insert(shown.to_string());
            out.push(h);
            taken += 1;
        }
    }
    out
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
