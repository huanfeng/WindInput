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
mod tests {
    use super::*;

    struct Fixed(AssocSource, Vec<(&'static str, i64)>);

    impl AssocProvider for Fixed {
        fn suggest(&self, _ctx: &AssocContext<'_>, limit: usize) -> Vec<AssocHit> {
            self.1
                .iter()
                .take(limit)
                .map(|(t, s)| AssocHit {
                    text: (*t).to_string(),
                    commit: None,
                    source: self.0,
                    score: *s,
                })
                .collect()
        }
    }

    fn ctx() -> AssocContext<'static> {
        AssocContext {
            text: "你好",
            preceding: "",
            char_wise: false,
            boundary_broken: false,
        }
    }

    #[test]
    fn disabled_yields_nothing() {
        let p = Fixed(AssocSource::Punct, vec![("，", 1)]);
        let cfg = AssocConfig {
            kind: AssocKind::Off,
            ..Default::default()
        };
        assert!(associate(&ctx(), &cfg, &[(AssocSource::Punct, &p)]).is_empty());
    }

    /// ★ 断链必须挡住联想：跨应用/跨焦点的「历史相邻」不是「屏幕相邻」。
    #[test]
    fn broken_boundary_yields_nothing() {
        let p = Fixed(AssocSource::Punct, vec![("，", 1)]);
        let cfg = AssocConfig::default().with_enabled();
        let c = AssocContext {
            text: "你好",
            preceding: "",
            char_wise: false,
            boundary_broken: true,
        };
        assert!(associate(&c, &cfg, &[(AssocSource::Punct, &p)]).is_empty());
    }

    #[test]
    fn empty_context_yields_nothing() {
        let p = Fixed(AssocSource::Punct, vec![("，", 1)]);
        let cfg = AssocConfig::default().with_enabled();
        let c = AssocContext {
            text: "",
            preceding: "",
            char_wise: false,
            boundary_broken: false,
        };
        assert!(associate(&c, &cfg, &[(AssocSource::Punct, &p)]).is_empty());
    }

    /// 优先级：History 在 Punct 之前，**与分数无关**。
    ///
    /// 这里刻意让 Punct 的分数远高于 History——若哪天有人改成跨源比分数，本测试立刻变红。
    #[test]
    fn priority_beats_score_across_sources() {
        let hist = Fixed(AssocSource::History, vec![("世界", 1)]);
        let punct = Fixed(AssocSource::Punct, vec![("，", 9999)]);
        let cfg = AssocConfig::default().with_enabled();
        let out = associate(
            &ctx(),
            &cfg,
            &[(AssocSource::Punct, &punct), (AssocSource::History, &hist)],
        );
        assert_eq!(out[0].text, "世界", "高优先级源在前，即便分数低得多");
        assert_eq!(out[1].text, "，");
    }

    /// 靠前的源取不满时，名额要**顺延**给后面的源——否则新用户（历史为空）
    /// 会白白浪费掉最靠前的几个位置。
    #[test]
    fn unused_quota_rolls_down() {
        let hist = Fixed(AssocSource::History, vec![]);
        let punct = Fixed(AssocSource::Punct, vec![("，", 3), ("。", 2), ("？", 1)]);
        let cfg = AssocConfig {
            max_count: 3,
            ..AssocConfig::default().with_enabled()
        };
        let out = associate(
            &ctx(),
            &cfg,
            &[(AssocSource::History, &hist), (AssocSource::Punct, &punct)],
        );
        assert_eq!(out.len(), 3, "历史为空时标点应吃满全部名额");
    }

    /// 同文本跨源重复时先到先得，且**来源标记跟着保留的那条**。
    #[test]
    fn dedup_keeps_higher_priority_source() {
        let hist = Fixed(AssocSource::History, vec![("世界", 1)]);
        let bigram = Fixed(AssocSource::Bigram, vec![("世界", 9999)]);
        let cfg = AssocConfig::default().with_enabled();
        let out = associate(
            &ctx(),
            &cfg,
            &[
                (AssocSource::History, &hist),
                (AssocSource::Bigram, &bigram),
            ],
        );
        assert_eq!(out.len(), 1, "同文本只留一条");
        assert_eq!(out[0].source, AssocSource::History, "保留高优先级源的标记");
    }

    /// ★ 被去重吃掉的名额同样要顺延给后面的源。
    ///
    /// 若实现里用「已请求数」而不是「已产出数」算剩余配额，这里就会只剩 1 条——
    /// 因为 Bigram 那条与 History 重复，白白吃掉一个名额却没贡献候选。
    #[test]
    fn quota_rolls_down_after_dedup_absorbs_hits() {
        let hist = Fixed(AssocSource::History, vec![("世界", 5)]);
        let bigram = Fixed(AssocSource::Bigram, vec![("世界", 9)]); // 与上面重复
        let punct = Fixed(AssocSource::Punct, vec![("，", 3)]);
        let cfg = AssocConfig {
            max_count: 2,
            ..AssocConfig::default().with_enabled()
        };
        let out = associate(
            &ctx(),
            &cfg,
            &[
                (AssocSource::History, &hist),
                (AssocSource::Bigram, &bigram),
                (AssocSource::Punct, &punct),
            ],
        );
        let texts: Vec<_> = out.iter().map(|h| h.text.as_str()).collect();
        assert_eq!(texts, ["世界", "，"], "重复项不该占用名额");
    }

    #[test]
    fn per_source_switch_disables_it() {
        let punct = Fixed(AssocSource::Punct, vec![("，", 1)]);
        let cfg = AssocConfig {
            punct: false,
            ..AssocConfig::default().with_enabled()
        };
        assert!(associate(&ctx(), &cfg, &[(AssocSource::Punct, &punct)]).is_empty());
    }

    /// ★ `ALL` 必须覆盖全部变体，且顺序与 `priority()` 一致。
    ///
    /// 守的是「加了新源却忘了登记进 ALL」——那种情况下新源不报错、不 panic，
    /// 只是永远不出候选。这里从 `priority()` 反查：它的 match 是穷尽的（编译器保证），
    /// 于是只要新变体的 priority 落在 0..ALL.len() 之外、或与既有项重号，本测试即红。
    #[test]
    fn all_is_complete_and_ordered() {
        let prios: Vec<usize> = AssocSource::ALL.iter().map(|s| s.priority()).collect();
        assert_eq!(
            prios,
            (0..AssocSource::ALL.len()).collect::<Vec<_>>(),
            "ALL 的顺序必须是 priority 的 0..n 且无缺号/重号——新增源忘了登记进 ALL 时这里会红"
        );
    }

    #[test]
    fn source_internal_order_is_by_score() {
        let punct = Fixed(AssocSource::Punct, vec![("。", 1), ("，", 5), ("？", 3)]);
        let cfg = AssocConfig::default().with_enabled();
        let out = associate(&ctx(), &cfg, &[(AssocSource::Punct, &punct)]);
        let texts: Vec<_> = out.iter().map(|h| h.text.as_str()).collect();
        assert_eq!(texts, ["，", "？", "。"], "源内按分数降序");
    }

    impl AssocConfig {
        /// 测试夹具：开到「四个源全放行的智能联想」。
        ///
        /// ⚠️ **必须显式开 `punct`**：`Default` 是桌面基线的镜像，那里 punct 是关的
        /// （实体键盘上标点一键可达）。下面一批测的是**合并算法**——配额顺延、跨源
        /// 优先级、去重、源内排序——与「桌面该不该默认出标点」这个产品决策无关，
        /// 借 `Default` 当夹具会让它们被那个决策连累（2026-08-16 实测红 5 条）。
        ///
        /// 真正钉产品决策的是 `desktop_smart_yields_no_punct_by_default`，那条**不用**
        /// 本夹具、直接吃出厂配置。
        fn with_enabled(mut self) -> Self {
            self.kind = AssocKind::Smart;
            self.punct = true;
            self
        }
    }

    /// ★★★ **词语联想不出标点。** 这是用户报的问题，也是两档分家的直接理由。
    ///
    /// 「中」后面接「，」在词语联想的语义下根本不成立——它不是以「中」开头的词。
    /// 档位过滤必须**先于**用户开关：`punct = true` 在 `word` 档下也不该放行。
    #[test]
    fn word_kind_never_yields_punctuation() {
        let punct = Fixed(AssocSource::Punct, vec![("，", 9999)]);
        let prefix = Fixed(AssocSource::Prefix, vec![("中国", 5)]);
        let cfg = AssocConfig {
            kind: AssocKind::Word,
            punct: true, // 开关开着也不行
            ..Default::default()
        };
        let out = associate(
            &ctx(),
            &cfg,
            &[(AssocSource::Punct, &punct), (AssocSource::Prefix, &prefix)],
        );
        let texts: Vec<_> = out.iter().map(|h| h.text.as_str()).collect();
        assert_eq!(texts, ["中国"], "词语联想只出前缀延伸词");
    }

    /// **受控对照**：同样的两个源、同样的开关，换成智能联想档就该两个都出。
    /// 少了这条，「词语联想没标点」可能只是因为标点源压根没接上。
    #[test]
    fn smart_kind_does_yield_punctuation() {
        let punct = Fixed(AssocSource::Punct, vec![("，", 9999)]);
        let prefix = Fixed(AssocSource::Prefix, vec![("中国", 5)]);
        // 显式开 punct：本条测的是**档位放行语义**（smart 档不像 word 档那样把标点源
        // 挡在门外），而不是「桌面出厂要不要出标点」——后者由
        // `desktop_smart_yields_no_punct_by_default` 钉。
        let cfg = AssocConfig {
            kind: AssocKind::Smart,
            punct: true,
            ..Default::default()
        };
        let out = associate(
            &ctx(),
            &cfg,
            &[(AssocSource::Punct, &punct), (AssocSource::Prefix, &prefix)],
        );
        let texts: Vec<_> = out.iter().map(|h| h.text.as_str()).collect();
        assert_eq!(
            texts,
            ["中国", "，"],
            "智能档两个源都出，且 Prefix 优先级更高"
        );
    }

    /// 下一词只在智能联想档读出（词语联想只出上文的延长），写入端须同口径：
    /// 词语联想档开着历史源也不学，否则只写不读、白攒数据。
    #[test]
    fn learns_next_words_only_in_smart_with_history() {
        let with = |kind, history| AssocConfig {
            kind,
            history,
            ..Default::default()
        };
        assert!(with(AssocKind::Smart, true).learns_next_words());
        assert!(!with(AssocKind::Smart, false).learns_next_words());
        assert!(!with(AssocKind::Word, true).learns_next_words());
        assert!(!with(AssocKind::Off, true).learns_next_words());
    }

    #[test]
    fn off_kind_yields_nothing() {
        let p = Fixed(AssocSource::Prefix, vec![("中国", 5)]);
        let cfg = AssocConfig {
            kind: AssocKind::Off,
            ..Default::default()
        };
        assert!(associate(&ctx(), &cfg, &[(AssocSource::Prefix, &p)]).is_empty());
    }

    #[test]
    fn kind_parses_explicit_values() {
        assert_eq!(AssocKind::parse("off"), AssocKind::Off);
        assert_eq!(AssocKind::parse("word"), AssocKind::Word);
        assert_eq!(AssocKind::parse("smart"), AssocKind::Smart);
    }

    /// 未知值一律回退「关」——`"auto"` 这类平台哨兵**不是**合法值，平台差异走
    /// `[mobile.association]`，不进值域。
    #[test]
    fn unknown_kind_falls_back_to_off() {
        for s in ["auto", "", "Word", "词语", "smrt"] {
            assert_eq!(AssocKind::parse(s), AssocKind::Off, "{s:?} 应回退 Off");
        }
    }

    /// `commit_text()` 是上屏那一半的唯一取值口。
    #[test]
    fn commit_text_falls_back_to_display_text() {
        let plain = AssocHit {
            text: "，".into(),
            commit: None,
            source: AssocSource::Punct,
            score: 1,
        };
        assert_eq!(plain.commit_text(), "，", "无 commit 时就用 text");
        let word = AssocHit {
            text: "中国".into(),
            commit: Some("国".into()),
            source: AssocSource::Prefix,
            score: 1,
        };
        assert_eq!(word.commit_text(), "国", "词语联想只补剩余部分");
    }

    #[test]
    fn mode_parses_explicit_values() {
        assert_eq!(AssocMode::parse("one_shot"), AssocMode::OneShot);
        assert_eq!(AssocMode::parse("continuous"), AssocMode::Continuous);
    }

    #[test]
    fn unknown_mode_falls_back_to_one_shot() {
        for s in ["auto", "", "One_Shot", "持续", "continous"] {
            assert_eq!(
                AssocMode::parse(s),
                AssocMode::OneShot,
                "{s:?} 应回退 OneShot"
            );
        }
    }

    /// ★ 同一份配置，取不取用 `[mobile.association]` 得到两种行为——**这就是平台差异
    /// 的全部落点**，此外没有任何 `cfg!(target_os)` 参与联想的取值。
    #[test]
    fn mobile_section_switches_kind_mode_and_punct() {
        let base = wind_config::AssociationConfig {
            kind: "off".into(),
            mode: "one_shot".into(),
            punct: false,
            ..Default::default()
        };
        let m = wind_config::MobileAssociationConfig::default();

        let desktop = AssocConfig::from_config(&base, None);
        assert_eq!(desktop.kind, AssocKind::Off, "桌面基线：默认关");
        assert_eq!(desktop.mode, AssocMode::OneShot);
        assert!(!desktop.punct, "桌面基线：标点联想默认关");

        let mobile = AssocConfig::from_config(&base, Some(&m));
        assert_eq!(mobile.kind, AssocKind::Smart, "移动端出厂：智能联想");
        assert_eq!(mobile.mode, AssocMode::Continuous);
        assert!(mobile.punct, "移动端出厂：标点联想开");
    }

    /// 空格选不选联想：移动端段说了算（出厂关），桌面基线（出厂开）在手机上不起作用；
    /// 用户在手机上打开后确实生效。
    #[test]
    fn mobile_section_switches_space_commits() {
        let base = wind_config::AssociationConfig::default();
        assert!(base.space_commits, "前提：桌面基线空格选联想");
        assert!(AssocConfig::from_config(&base, None).space_commits);

        let mut m = wind_config::MobileAssociationConfig::default();
        assert!(
            !AssocConfig::from_config(&base, Some(&m)).space_commits,
            "移动端出厂：空格出空格、不选联想"
        );
        m.space_commits = true;
        assert!(
            AssocConfig::from_config(&base, Some(&m)).space_commits,
            "手机上打开后生效"
        );
    }

    /// ★ 智能联想里前缀词最多占 [`SMART_PREFIX_CAP`] 个名额，余下顺延给标点；补出部分超过
    /// [`SMART_PREFIX_TAIL_MAX`] 字的丢掉、由后面更短的补上。词语联想两条都不管。
    #[test]
    fn smart_caps_prefix_and_leaves_room_for_punct() {
        struct Words(Vec<&'static str>);
        impl AssocProvider for Words {
            fn suggest(&self, ctx: &AssocContext<'_>, limit: usize) -> Vec<AssocHit> {
                let n = self.0.len() as i64;
                self.0
                    .iter()
                    .take(limit)
                    .enumerate()
                    .map(|(i, w)| AssocHit {
                        text: (*w).to_string(),
                        commit: w.strip_prefix(ctx.text).map(str::to_string),
                        source: AssocSource::Prefix,
                        score: n - i as i64,
                    })
                    .collect()
            }
        }
        let prefix = Words(vec![
            "中国",
            "中华人民共和国",
            "中间",
            "中心",
            "中文",
            "中午",
            "中医",
            "中学",
        ]);
        let punct = crate::punct::PunctRules;
        let providers: [(AssocSource, &dyn AssocProvider); 2] =
            [(AssocSource::Prefix, &prefix), (AssocSource::Punct, &punct)];
        let run = |kind| {
            let cfg = AssocConfig {
                kind,
                max_count: 9,
                punct: true,
                ..Default::default()
            };
            associate(
                &AssocContext {
                    text: "中",
                    preceding: "",
                    char_wise: false,
                    boundary_broken: false,
                },
                &cfg,
                &providers,
            )
        };
        let smart = run(AssocKind::Smart);
        let prefix_hits: Vec<_> = smart
            .iter()
            .filter(|h| h.source == AssocSource::Prefix)
            .map(|h| h.commit_text())
            .collect();
        assert_eq!(
            prefix_hits,
            ["国", "间", "心", "文"],
            "前 4 条短的，长尾那条跳过"
        );
        assert!(
            smart.iter().any(|h| h.text == "，"),
            "标点要排得进来，实得 {smart:?}"
        );

        let word = run(AssocKind::Word);
        assert_eq!(word.len(), 8, "词语联想不设前缀上限，也不丢长词");
        assert!(word.iter().any(|h| h.text == "中华人民共和国"));
    }

    /// ★ 语气词结尾（「好吗」）：智能联想只出个人学习与标点，句末标点排第一；
    /// 词语联想不受影响（它本就只有前缀词）。
    #[test]
    fn smart_particle_tail_yields_punct_first() {
        let bigram = Fixed(AssocSource::Bigram, vec![("的", 9)]);
        let prefix = Fixed(AssocSource::Prefix, vec![("吗啡", 9)]);
        let punct = crate::punct::PunctRules;
        let providers: [(AssocSource, &dyn AssocProvider); 3] = [
            (AssocSource::Bigram, &bigram),
            (AssocSource::Prefix, &prefix),
            (AssocSource::Punct, &punct),
        ];
        let cfg = AssocConfig {
            kind: AssocKind::Smart,
            max_count: 9,
            punct: true,
            ..Default::default()
        };
        let run = |text| {
            associate(
                &AssocContext {
                    text,
                    preceding: "",
                    char_wise: false,
                    boundary_broken: false,
                },
                &cfg,
                &providers,
            )
            .into_iter()
            .map(|h| h.text)
            .collect::<Vec<_>>()
        };
        let got = run("好吗");
        assert_eq!(got[0], "？", "{got:?}");
        assert!(!got.iter().any(|t| t == "的" || t == "吗啡"), "{got:?}");
        assert_eq!(run("你好")[0], "的", "非语气词照常出下一词");
    }

    /// ★ 智能联想按上屏的那一截去重：前缀延伸「中国」（补「国」）与下一词「国」只留先到的，
    /// 名额顺延给后面的；补出的那截延长了已有一格的前缀词也不出。词语联想仍按整词去重（都留）。
    #[test]
    fn smart_dedups_by_continuation() {
        struct WithCommit(AssocSource, Vec<(&'static str, Option<&'static str>)>);
        impl AssocProvider for WithCommit {
            fn suggest(&self, _ctx: &AssocContext<'_>, limit: usize) -> Vec<AssocHit> {
                self.1
                    .iter()
                    .take(limit)
                    .map(|(t, c)| AssocHit {
                        text: (*t).to_string(),
                        commit: c.map(str::to_string),
                        source: self.0,
                        score: 0,
                    })
                    .collect()
            }
        }
        let history = WithCommit(AssocSource::History, vec![("国", None)]);
        let prefix = WithCommit(
            AssocSource::Prefix,
            vec![
                ("中国", Some("国")),
                ("中国人", Some("国人")),
                ("中间", Some("间")),
            ],
        );
        let providers: [(AssocSource, &dyn AssocProvider); 2] = [
            (AssocSource::History, &history),
            (AssocSource::Prefix, &prefix),
        ];
        let run = |kind| {
            let cfg = AssocConfig {
                kind,
                max_count: 9,
                ..Default::default()
            };
            let hits = associate(
                &AssocContext {
                    text: "中",
                    preceding: "",
                    char_wise: false,
                    boundary_broken: false,
                },
                &cfg,
                &providers,
            );
            hits.into_iter().map(|h| h.text).collect::<Vec<_>>()
        };
        assert_eq!(
            run(AssocKind::Smart),
            ["国", "中间"],
            "「中国」显示同为「国」，去掉；「中国人」补「国人」是「国」的延长，也去掉"
        );
        assert_eq!(
            run(AssocKind::Word),
            ["国", "中国", "中国人", "中间"],
            "词语联想按整词，都留"
        );
    }

    /// 移动端段只该动 kind / mode / punct / space_commits，其余五项一律走基线——它们当前没有平台差异。
    ///
    /// ⚠️ 这条不是形式主义，它守的是**两个方向**：
    /// - 把某个键加进 `[mobile.association]` 却忘了在 `from_config` 里放行 ⇒
    ///   「手机上改了那一项没反应」，而配置文件看着完全正常；
    /// - 反过来，误把不该分平台的字段接上移动端段 ⇒ 这里立刻红。
    #[test]
    fn mobile_section_does_not_touch_other_fields() {
        let base = wind_config::AssociationConfig {
            max_count: 5,
            hide_after_ms: 1234,
            history: false,
            bigram: false,
            prefix: false,
            ..Default::default()
        };
        let m = wind_config::MobileAssociationConfig::default();
        let got = AssocConfig::from_config(&base, Some(&m));
        assert_eq!(got.max_count, 5);
        assert_eq!(got.hide_after_ms, 1234);
        assert!(!got.history);
        assert!(!got.bigram);
        assert!(!got.prefix);
    }

    /// ★ 桌面的智能联想档**不出标点**（`punct` 桌面默认关）。
    ///
    /// 这条钉的是用户可感的行为，而不是某个字段的值：走完整的 `associate` 链路，
    /// 用出厂基线配置喂一条汉字上文，断言标点源一条也没出来。
    #[test]
    fn desktop_smart_yields_no_punct_by_default() {
        // 用户显式开了智能联想，但没动 punct
        let base = wind_config::AssociationConfig {
            kind: "smart".into(),
            ..Default::default()
        };
        let cfg = AssocConfig::from_config(&base, None);
        assert!(!cfg.punct, "前提：桌面基线 punct 关");

        let p = wind_assoc_test_punct();
        let hits = associate(
            &AssocContext {
                text: "你好",
                preceding: "",
                char_wise: false,
                boundary_broken: false,
            },
            &cfg,
            &[(AssocSource::Punct, &p)],
        );
        assert!(hits.is_empty(), "桌面智能联想不该出标点，实得 {hits:?}");

        // 反向对照：移动端段接上后就出得来，否则本测试可能只是因为标点源坏了。
        let m = wind_config::MobileAssociationConfig::default();
        let mcfg = AssocConfig::from_config(&base, Some(&m));
        let mhits = associate(
            &AssocContext {
                text: "你好",
                preceding: "",
                char_wise: false,
                boundary_broken: false,
            },
            &mcfg,
            &[(AssocSource::Punct, &p)],
        );
        assert!(!mhits.is_empty(), "移动端该出标点");
    }

    fn wind_assoc_test_punct() -> crate::punct::PunctRules {
        crate::punct::PunctRules
    }

    /// ★ 两处默认值必须一致：`wind_config::AssociationConfig::default()`（配置文件的默认）
    /// 与 `AssocConfig::default()`（无配置时的运行时默认）。
    ///
    /// 它们是**独立维护**的两份字面量。漂移的后果极隐蔽：用户没写配置时走前者、
    /// 某些内部调用点走后者，于是「同一个开关在两条路径上取值不同」——本仓已在
    /// 别处栽过这个形状（见 `project_z_key_action_live_code_gate`）。
    #[test]
    fn config_defaults_agree_with_runtime_defaults() {
        let from_cfg = AssocConfig::from_config(&wind_config::AssociationConfig::default(), None);
        let rt = AssocConfig::default();
        assert_eq!(from_cfg.kind, rt.kind, "默认关这一条尤其不能漂");
        assert_eq!(from_cfg.mode, rt.mode);
        assert_eq!(from_cfg.max_count, rt.max_count);
        assert_eq!(from_cfg.space_commits, rt.space_commits);
        assert_eq!(from_cfg.hide_after_ms, rt.hide_after_ms);
        assert_eq!(from_cfg.history, rt.history);
        assert_eq!(from_cfg.bigram, rt.bigram);
        assert_eq!(from_cfg.prefix, rt.prefix);
        assert_eq!(from_cfg.punct, rt.punct);
    }

    /// 配置默认必须是关的——联想会占用数字键，对既有用户是突发的行为变化。
    ///
    /// ★ 断言的是**字面量**而不只是解析结果：值域里一旦再混进哨兵（`"auto"` 之类），
    /// 解析结果可能照样是 Off，但设置界面又会被迫列一个语义空洞的选项。
    #[test]
    fn association_is_off_by_default() {
        let d = wind_config::AssociationConfig::default();
        assert_eq!(d.kind, "off");
        assert_eq!(d.mode, "one_shot");
        assert_eq!(AssocKind::parse(&d.kind), AssocKind::Off);
        assert_eq!(AssocMode::parse(&d.mode), AssocMode::OneShot);
    }

    /// 移动端段的代码默认值，必须与预置 `data/config.toml` 的 `[mobile.association]` 一致
    /// ——那一致性本身由 `wind-config` 的 `data_config_toml_covers_registry` 一族守护，
    /// 这里钉的是**值本身**：移动端出厂就该是智能联想 + 持续。
    #[test]
    fn mobile_association_defaults_are_the_mobile_values() {
        let d = wind_config::MobileAssociationConfig::default();
        assert_eq!(d.kind, "smart");
        assert_eq!(d.mode, "continuous");
        assert_eq!(AssocKind::parse(&d.kind), AssocKind::Smart);
        assert_eq!(AssocMode::parse(&d.mode), AssocMode::Continuous);
        // 手机上选联想只靠点候选；退格、回车、二三候选键都不被联想截走
        assert!(!d.space_commits);
        assert!(!d.select_keys_commit);
        assert!(!d.enter_cancels_only);
        assert!(!d.backspace_cancels_only);
    }
}
