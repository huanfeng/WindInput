//! wind-assoc: 联想候选的生成与合并（纯逻辑，可原生测试）。
//!
//! 与 `wind-phrase` / `wind-punct` / `wind-quick-input` 同构：**输入 → 候选**的
//! 自包含单元，不持有宿主状态、不做 IO 决策，词典与用户数据以 trait 形式注入。
//!
//! ## 联想与普通输入的根本差别
//!
//! 普通候选的输入是**编码缓冲**（`nihao` → 你好），联想候选的输入是**上文文本**
//! （刚上屏的「你好」→ 「，」「世界」）。所以它接不进 `EngineManager::convert_with(schema, code)`
//! ——那个签名的第二个参数是编码。硬塞进去会重蹈 `project_mixed_pinyin_exact_tier`
//! 的覆辙（判据在引擎侧恒为假，误用后静默落错档）。
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
//! ★ **跨源不比较分数，只按固定优先级取配额**。四个源的分值天然不同量纲
//! （计数 / log 概率 / 词频 i32 / 规则无分），归一化混排是条老路——
//! `project_freq_weight_model` 记着整句权重那次的教训：**同量纲解决不了系统性偏高**，
//! 最后还是拆成了独立档位。这里直接不引入那个问题。

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
    /// ★ 断链时**不做联想**。`recent_commits` 是一个跨应用、跨焦点的全局队列，
    /// 「历史上相邻的两次上屏」不等于「屏幕上相邻的两个词」——用户在 A 窗口打完
    /// 「你好」、切到 B 窗口打「世界」，朴素实现就会拿「你好」当上文。
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
    /// 全部源，**按优先级升序**。[`associate`] 只遍历这个数组。
    ///
    /// ⚠️ **它与枚举变体是两处独立维护的**：加了新变体却忘了登记进来，那个源会被
    /// 静默忽略——不报错、不 panic、只是永远不出候选，属本仓最难自查的一类故障。
    /// 守门在 [`AssocSource::priority`]：那里的 `match` 是穷尽的，新增变体会**编译失败**，
    /// 强制作者路过此处。
    pub const ALL: [AssocSource; 4] = [
        AssocSource::History,
        AssocSource::Bigram,
        AssocSource::Prefix,
        AssocSource::Punct,
    ];

    /// 优先级序号（越小越靠前），**唯一的事实源**。
    ///
    /// 存在的意义不是被调用——[`associate`] 直接按 [`Self::ALL`] 的顺序遍历——
    /// 而是让编译器盯着：新增枚举变体时这里的 `match` 不穷尽即编译失败，
    /// 于是作者必然会看到上面那条「记得登记进 ALL」的告诫。
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
    /// **展示**文本。词语联想里这是整词（「中国」），不是要上屏的那一半。
    pub text: String,
    /// **上屏**文本；`None` = 就用 [`Self::text`]。
    ///
    /// 词语联想里两者必然不同：候选栏显示整词「中国」（用户才看得懂自己在选什么），
    /// 而「中」已经在屏幕上了，真正要补出去的只有「国」。
    ///
    /// ★ 存成一个字段而不是让下游按前缀现算：Smart 档的候选**不以上文开头**
    /// （「你好」→「，」），下游若统一 `strip_prefix` 会把两种档位的语义搅在一起，
    /// 而 strip 失败时的兜底又恰好看起来是对的——这类缺陷不会报错，只会偶尔少个字。
    pub commit: Option<String>,
    /// 来源，决定它排在哪一档。
    pub source: AssocSource,
    /// **源内**排序用的分数，跨源之间没有可比性（量纲不同）。
    pub score: i64,
}

impl AssocHit {
    /// 上屏该写出的文本。
    pub fn commit_text(&self) -> &str {
        self.commit.as_deref().unwrap_or(&self.text)
    }
}

/// **联想是哪一种**。这是本模块最重要的一个区分——两种「联想」回答的是完全不同的问题。
///
/// | | 上文怎么用 | 打完「中」之后给什么 | 典型平台 |
/// |---|---|---|---|
/// | [`Word`](AssocKind::Word) | 当**前缀** | 「中国」「中间」「中心」——以「中」开头的词 | PC |
/// | [`Smart`](AssocKind::Smart) | 当**上下文** | 「国」「，」「的」——「中」后面常跟什么 | 移动端 |
///
/// PC 输入法里说的「联想」几乎总是前者：词库里以刚上屏内容开头的更长的词，选中后补出
/// 剩余部分。它不需要任何上下文模型，只需要一份带权重的词库。
///
/// 后者才是移动端刚需——软键盘上每多打一个字都很贵，所以要连标点带下一个词一起猜。
///
/// ⚠️ **两者不是「弱版/强版」的关系，不能用一个开关的强弱表达。** 同样打完「中」，
/// 一个该给「中国」，另一个该给「国」；把它们混在一起出，用户会看到「中国」和「国」
/// 并排，完全不知道选中哪个会得到什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AssocKind {
    /// 关闭。**PC 默认**——PC 上候选窗是浮层，联想常驻会挡正文，且数字键会被占用。
    #[default]
    Off,
    /// 词语联想：上文当前缀，出词库里以它开头的更长的词。
    ///
    /// ★ **不含标点**。「中」后面接「，」在这个语义下没有意义——它不是以「中」开头的词。
    Word,
    /// 智能联想：上文当上下文，出下一个可能的词与标点。**移动端默认**。
    Smart,
}

impl AssocKind {
    /// 解析配置里的 `kind`。**未知值一律回退 [`Off`](AssocKind::Off)**。
    ///
    /// ★ 回退到「关」而不是某个开着的档：拼错了值就不启用，比静默启用一个用户没要求的
    /// 档要好——后者会让人以为「配置生效了」，实际生效的是另一件事。
    pub fn parse(s: &str) -> Self {
        match s {
            "word" => AssocKind::Word,
            "smart" => AssocKind::Smart,
            _ => AssocKind::Off,
        }
    }

    /// 本档是否启用该数据源。
    ///
    /// **词语联想只认 [`AssocSource::Prefix`]**——那正是「以上文为前缀的更长的词」这件事。
    /// 其余三源（个人搭配、词→后继、标点）问的都是「后面接什么」，属上下文语义，
    /// 混进词语联想会让候选变成两种不同东西的拼盘。
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

    /// 本档下，前缀延伸类候选（「中」→「中国」）是否**只显示补出的那一截**（「国」）。
    ///
    /// 智能联想问的是「后面接什么」，同一栏里还有下一词（「的」）与标点（「，」）——
    /// 前缀词若显示整词，「中国」与「国」会并排出现，用户分不清选哪个得到什么（见上表）。
    /// 词语联想整栏都是「以上文开头的词」，显示整词用户才看得懂自己在选什么。
    ///
    /// 只改**显示与去重的口径**：[`AssocHit::text`] 仍是整词、`commit` 仍是那一截，
    /// 联想历史（前缀延伸行按「整词 − 补出的一截」还原上文）照旧记。
    pub fn shows_continuation(self) -> bool {
        matches!(self, AssocKind::Smart)
    }

    /// 本档下某个源最多占几个名额（`remaining` 是此刻还剩的总名额）。
    ///
    /// 智能联想里前缀词只是「把上文补长」，不是「猜下一个词」：拼音大词库里以「好的」开头的
    /// 词成百上千，不设上限时它一口气占满全部名额，同栏的标点就永远出不来（实测「你好」后
    /// 没有「，」）。压到 [`SMART_PREFIX_CAP`] 条，余下的顺延给标点。词语联想整栏就是前缀词，
    /// 不设限。
    pub fn source_cap(self, src: AssocSource, remaining: usize) -> usize {
        match (self, src) {
            (AssocKind::Smart, AssocSource::Prefix) => remaining.min(SMART_PREFIX_CAP),
            (AssocKind::Smart, AssocSource::Bigram) => remaining.min(SMART_BIGRAM_CAP),
            _ => remaining,
        }
    }

    /// 本档下这条候选能不能出（源内过滤，先于去重）。
    ///
    /// 智能联想只显示补出的那一截（[`Self::shows_continuation`]），补出部分过长的前缀词
    /// （「中」→「华人民共和国」）在候选条上读起来是半截词、也不像「下一个词」，丢掉；
    /// 短的（「谢谢」→「大家」、「北京」→「大学」）正是下一词。
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
    /// ⚠️ 移动端**不靠这里**拿到 `Continuous`——那是 `[mobile.association]` 的职责
    /// （见 [`AssocConfig::from_config`]）。
    ///
    /// ⛔ 别让本函数收一个「平台默认」参数去兜底手机上写错值的情形：解析函数一旦要知道
    /// 自己跑在哪，值域就不再是纯粹的值域，平台知识会顺着解析链一路渗透。
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
    /// 哪一种联想（含「关」）。**开关与类型合并成一个字段**——分成 `enabled` + `kind`
    /// 两个会立刻产生「开着但类型没配」的歧义状态，而那种状态没有正确答案。
    pub kind: AssocKind,
    /// ⚠️ **[`associate`] 不读这个字段**，它由宿主侧的状态机消费——决定的是
    /// 「什么时候调用 associate」「什么时候退出联想态」，而不是「给出哪些候选」。
    /// 放在同一个 config 里只是为了整体传递，不是遗漏。
    pub mode: AssocMode,
    /// 候选总数上限。各源配额由它按优先级分配，**配额本身不暴露给用户**
    /// ——那是调参项，而用户没有评测手段。
    pub max_count: usize,
    /// ⚠️ 与 [`Self::mode`] 同属**宿主侧消费**：空格是否上屏当前高亮联想候选。
    pub space_commits: bool,
    /// ⚠️ 宿主侧消费：联想窗自动隐藏的毫秒数，`0` = 不自动隐藏。
    pub hide_after_ms: u64,
    pub history: bool,
    pub bigram: bool,
    pub prefix: bool,
    pub punct: bool,
}

impl Default for AssocConfig {
    fn default() -> Self {
        Self {
            // ★ 本 Default 是**桌面基线**的镜像，必须逐字段等于
            // `wind_config::AssociationConfig::default()`（由
            // `config_defaults_agree_with_runtime_defaults` 守着）。
            // 移动端的差异不在这里，而在 `[mobile.association]`。
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
    /// 从配置段构造运行时视图。`cfg` 是桌面基线 `[input.association]`；`mobile` 是
    /// `[mobile.association]`，**`Some` 时 `kind`/`mode`/`punct`/`space_commits` 改从它取**，其余字段
    /// 一律走基线（那些键当前没有平台差异，故根本不在移动端段里）。
    ///
    /// ⚠️ 「要不要取用 mobile 段」由**调用方**决定，而不是这里 `cfg!(target_os)` 判断：
    /// `wind-assoc` 会被桌面与安卓两条构建链同时编译，而「跑在哪」是宿主的知识，
    /// 不是本 crate 的。
    ///
    /// ⚠️ **往 `MobileAssociationConfig` 加字段时必须在这里放行**，否则症状是「手机上
    /// 改了那一项没反应」，而配置文件看着完全正常。守门在
    /// `mobile_section_does_not_touch_other_fields`。
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

    /// 该源是否启用：**先过档位、再过开关**。
    ///
    /// 档位是硬性的（词语联想里标点源根本没有语义），用户开关只在档位放行的源上生效。
    /// 两者顺序反过来会让「词语联想 + punct=true」出标点——那正是用户报的问题。
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
/// 由调用方（协调器）为每个源注入实现——词典句柄、redb、规则表各自封在实现里，
/// 本 crate 不碰 IO。
///
/// ## 实现契约
///
/// - `limit` 是该源分到的配额，**可以少给，不得多给**（多给的部分会被丢弃）。
/// - ★ **必须返回本源内分数最高的那 `limit` 条**，而不是任意 `limit` 条。
///   [`associate`] 只在收到结果后做源内排序，**它救不回已经被截断掉的候选**——
///   若实现按「先查到的先给」截断，好候选会在进入排序前就丢了。
/// - `limit == 0` 时应直接返回空，不要做任何查询工作。
pub trait AssocProvider {
    fn suggest(&self, ctx: &AssocContext<'_>, limit: usize) -> Vec<AssocHit>;
}

/// 按固定优先级 + 每源配额汇聚四个源。
///
/// ## 配额怎么分
///
/// 不是平均分。靠前的源先取，取不满的名额**顺延给后面的源**——否则个人历史为空的
/// 新用户会白白浪费掉最靠前的几个位置。
///
/// ## 去重
///
/// 按**显示口径**去重（[`AssocKind::shows_continuation`] 档按上屏的那一截，否则按 `text`），
/// **先到先得**。智能联想里「中国」（前缀延伸，补「国」）与「国」（下一词）显示同为「国」，
/// 只留一条，名额顺延。于是同一个词若同时来自历史与 bigram，保留历史那条
/// （优先级更高），其来源标记也是历史——这对下游按来源做统计是正确的。
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
