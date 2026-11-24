//! 字符类的**判定结构**：把配置编译成可放上按键热路径的查表。
//!
//! 设计与全部取舍见 `docs/design/charset-classification.md`。本模块只做判定，
//! 不认识 TOML——配置的读取与三层合并在 `wind-config` 的 `charset_def`，
//! 由 coordinator 把解析结果转成 [`ClassSpec`] 喂进来。
//!
//! 与 `wind-config` 的 `code_charset::CodeCharSet` 无关：那个是码元字符集（值域 ASCII）。
//!
//! # 两种性质，两种合成规则
//!
//! | 性质 | 规则 | 为什么 |
//! |---|---|---|
//! | 常用性 [`verdict_of`](CharsetRegistry::verdict_of) | **仲裁**：`order` 最小且表态的类赢 | 二值判定，只能有一个答案 |
//! | [`no_freq`](CharsetRegistry::no_freq) / [`in_rare`](CharsetRegistry::in_rare) | **并集**：任一命中的类为真即真 | 布尔能力，多给一个是安全方向 |
//!
//! 不得把并集那两项改成仲裁：`▶` 同时命中 emoji 类与「符号」类，`no_freq` 相反时按仲裁
//! 会取决于 `order`，本该免词频的字符可能静默开始记词频。

use std::collections::HashSet;

/// 类的**作用域**：本类「管得着」谁。值域是闭集，见设计文档 §2.4。
///
/// 不接受用户自定义码位段：作用域背后是**判定域**，漏一段的后果是那批字恒判常用、
/// 过滤静默失效（issue #83 就是差一个码位）。判定域的完整性必须由代码保证。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// 汉字 ∪ 私用区——默认字表的管辖域，即 [`crate::is_common_scope`]。
    Han,
    /// 私用区。
    Pua,
}

impl Scope {
    fn contains(self, ch: char) -> bool {
        match self {
            Self::Han => crate::is_common_scope(ch),
            Self::Pua => crate::is_pua(ch),
        }
    }
}

/// 编译 registry 的输入：一个类的规格，**已解析**、与配置格式无关。
///
/// 常用性 `None` = 不表态（不参与仲裁），`Some(true)` = 常用。配置层的 serde 枚举由
/// coordinator 转换过来，逻辑层不引 serde。
#[derive(Debug, Clone, Default)]
pub struct ClassSpec {
    /// 稳定标识，配置里的键名，也是 `exclude_blocks` 等外部引用的取值。
    pub key: String,
    /// 显示名。空则回落 `key`。
    pub name: String,
    /// 码位段（闭区间），可空。
    pub ranges: Vec<(u32, u32)>,
    /// 离散成员（字表文件 + 内嵌列表），键是**字素簇**。判定查它，O(1)。
    pub members: HashSet<String>,
    /// 成员的**配置顺序**，与 [`Self::members`] 同集合。设置页列全表按它排。
    ///
    /// 单独留一份 Vec 而不是从 `members` 迭代：`HashSet` 的迭代序是随机的，
    /// 拿它列表会得到一份每次启动都不一样的清单，分页更是直接失效。
    pub member_order: Vec<String>,
    /// 排除的字素簇（`remove`），**优先于 `ranges` 与 `members`**。
    pub excluded: HashSet<String>,
    /// 作用域。配了它 `outside_common` 才有意义。
    pub scope: Option<Scope>,
    /// 成员的常用性；`None` = 不表态。
    pub default_common: Option<bool>,
    /// 作用域内、成员外的常用性；`None` = 不表态。
    pub outside_common: Option<bool>,
    /// 仲裁顺序，小的优先。
    pub order: i32,
    /// 免词频（并集语义）。
    pub no_freq: bool,
    /// 纳入生僻字模式（并集语义）。
    pub in_rare: bool,
}

/// 位集宽度上限。超出的类会被丢弃并告警——**不是**静默截断：位集塞不下时高位类
/// 恒不命中，配置写了也没反应，是最难查的一类故障。
pub const MAX_CLASSES: usize = 128;

/// 区间分段表的一段：`[start, end]` 内命中的类完全一致。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Segment {
    start: u32,
    end: u32,
    /// 哪些类的 `ranges` 覆盖本段，按 [`CharsetRegistry::classes`] 的下标建位。
    hits: u128,
}

/// 编译后的判定结构。
///
/// 区间走分段表、离散走 HashSet。离散型不要切进分段表：`common_han` 会把 Han 域打成
/// 上万段，反而比「区间比较 + 一次 HashSet 查询」慢。
#[derive(Debug, Clone, Default)]
pub struct CharsetRegistry {
    /// 按 `order` 升序（同 order 按 `key` 定序，保证编译结果可复现）。
    classes: Vec<ClassSpec>,
    /// 区间型的不相交段，按 `start` 升序。
    segments: Vec<Segment>,
    /// 开了 `no_freq` 的类的位集，**编译期算好**。
    ///
    /// 存成掩码而不现算：查询在按键热路径上，现算要遍历类列表并分配 `Vec`。
    /// 掩码为 0 时判定直接恒假，未配置的用户零成本。
    no_freq_mask: u128,
    /// 开了 `in_rare` 的类的位集，同上。
    in_rare_mask: u128,
}

impl CharsetRegistry {
    /// 编译。超出 [`MAX_CLASSES`] 的类被丢弃（返回它们的 key 供调用方告警）。
    pub fn compile(mut specs: Vec<ClassSpec>) -> (Self, Vec<String>) {
        // 同 order 按 key 定序：HashMap 迭代序随机，不定序会让同一份配置每次启动
        // 编译出不同的仲裁结果，而这种不确定性极难复现。
        specs.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.key.cmp(&b.key)));

        let dropped = specs
            .split_off(specs.len().min(MAX_CLASSES))
            .into_iter()
            .map(|s| s.key)
            .collect();

        let segments = Self::build_segments(&specs);
        let bits_of = |want: fn(&ClassSpec) -> bool| {
            specs
                .iter()
                .enumerate()
                .filter(|(_, c)| want(c))
                .fold(0u128, |m, (i, _)| m | (1u128 << i))
        };
        let no_freq_mask = bits_of(|c| c.no_freq);
        let in_rare_mask = bits_of(|c| c.in_rare);
        (
            Self {
                classes: specs,
                segments,
                no_freq_mask,
                in_rare_mask,
            },
            dropped,
        )
    }

    /// 把所有类的 `ranges` 切成不相交段。
    ///
    /// 取所有区间的 `start` 与 `end + 1` 作为切点，相邻切点之间的码位命中的类必然一致
    /// ——这是「区间集合的边界即等价类边界」，故每段只需算一次命中位集。
    fn build_segments(specs: &[ClassSpec]) -> Vec<Segment> {
        let mut cuts: Vec<u32> = Vec::new();
        for s in specs {
            for &(lo, hi) in &s.ranges {
                cuts.push(lo);
                // `hi + 1` 可能溢出到 0x110000，那正是「Unicode 上界之后」这个哨兵，
                // 用 saturating 保证不 panic；u32 装得下，不会真的回绕。
                cuts.push(hi.saturating_add(1));
            }
        }
        cuts.sort_unstable();
        cuts.dedup();

        let mut out = Vec::with_capacity(cuts.len());
        for w in cuts.windows(2) {
            let (start, end) = (w[0], w[1] - 1);
            let mut hits = 0u128;
            for (i, s) in specs.iter().enumerate() {
                if s.ranges.iter().any(|&(lo, hi)| lo <= start && end <= hi) {
                    hits |= 1u128 << i;
                }
            }
            if hits != 0 {
                out.push(Segment { start, end, hits });
            }
        }
        out
    }

    /// 某个码位落在哪些区间类里。表外返回 0。
    fn range_hits(&self, ch: char) -> u128 {
        let c = ch as u32;
        let i = self.segments.partition_point(|s| s.start <= c);
        match i.checked_sub(1) {
            Some(i) if c <= self.segments[i].end => self.segments[i].hits,
            _ => 0,
        }
    }

    /// 这个字素簇命中第 `i` 个类吗。`excluded` 优先于一切。
    fn hits_class(&self, cluster: &str, i: usize, range_hits: u128) -> bool {
        let cl = &self.classes[i];
        if cl.excluded.contains(cluster) {
            return false;
        }
        cl.members.contains(cluster) || range_hits & (1u128 << i) != 0
    }

    /// **常用性仲裁**：`order` 最小且表态的类说了算。`None` = 无人表态，由调用方兜底。
    ///
    /// 「命中但不表态」要继续往下让，不能就此返回 `None`——那 50 个内置区块类存在的
    /// 理由只是给类型列一个标签，它们命中不该挡住后面真正表态的类。
    pub fn verdict_of(&self, cluster: &str) -> Option<bool> {
        let ch = cluster.chars().next()?;
        let hits = self.range_hits(ch);
        for i in 0..self.classes.len() {
            let cl = &self.classes[i];
            if self.hits_class(cluster, i, hits) {
                if let Some(v) = cl.default_common {
                    return Some(v);
                }
                continue;
            }
            // 作用域内、成员外——「是汉字却不在字表里 ⇒ 生僻」那一半。
            if let Some(sc) = cl.scope
                && sc.contains(ch)
                && let Some(v) = cl.outside_common
            {
                return Some(v);
            }
        }
        None
    }

    /// 单个 `char` 的常用性仲裁。
    ///
    /// 存在是为了避免热路径上 `ch.to_string()` 的堆分配，用栈上 4 字节缓冲。
    pub fn verdict_of_char(&self, ch: char) -> Option<bool> {
        let mut buf = [0u8; 4];
        self.verdict_of(ch.encode_utf8(&mut buf))
    }

    /// 这个簇属于哪个类（类型列显示用）：优先给**仲裁赢家**，无人表态则给首个命中的类。
    ///
    /// 给仲裁赢家而不是首个命中，是因为用户看这一列是想知道「谁决定了它的常用性」。
    pub fn class_of(&self, cluster: &str) -> Option<&ClassSpec> {
        let ch = cluster.chars().next()?;
        let hits = self.range_hits(ch);
        let mut first = None;
        for i in 0..self.classes.len() {
            if self.hits_class(cluster, i, hits) {
                if self.classes[i].default_common.is_some() {
                    return Some(&self.classes[i]);
                }
                first.get_or_insert(&self.classes[i]);
            }
        }
        first
    }

    /// 免词频（**并集**，存在性：文本里任一字符命中任一 `no_freq` 类）。
    pub fn no_freq(&self, text: &str) -> bool {
        self.any_char_in(text, self.no_freq_mask)
    }

    /// 纳入生僻字模式（**并集**，存在性）。
    pub fn in_rare(&self, text: &str) -> bool {
        self.any_char_in(text, self.in_rare_mask)
    }

    /// 没有任何类开 `no_freq` ⇒ 免词频判定恒假，调用方可整条绕开。
    ///
    /// 与 `BlockMask::is_empty` 同一用途：绝大多数用户没配过这个功能，此刻的成本应当是
    /// **零**而不是「很小」。
    pub fn no_freq_is_empty(&self) -> bool {
        self.no_freq_mask == 0
    }

    /// 没有任何类开 `in_rare` ⇒ 生僻准入判定恒假。
    pub fn in_rare_is_empty(&self) -> bool {
        self.in_rare_mask == 0
    }

    /// 并集类查询的共同实现：逐字素簇看有没有命中 `wanted` 位集里的某个类。
    ///
    /// 按簇遍历：多码位 emoji 序列在字表里是一整条。`wanted` 由调用方传编译期算好的掩码，
    /// 不在热路径上现算。
    fn any_char_in(&self, text: &str, wanted: u128) -> bool {
        if wanted == 0 {
            return false;
        }
        crate::split_markable_clusters(text).any(|cluster| self.cluster_hits(cluster, wanted))
    }

    /// 一个字素簇命中 `wanted` 里的某个类吗：先查整簇，不中再逐 `char` 回落。
    ///
    /// 两步都不能省：字表不列变体序列与 ZWJ 组合（设计文档 §5.3），只查整簇会漏掉上游词库
    /// 里常见的「裸基字符 + 变体选择符」写法。见设计文档 §6.6。
    ///
    /// `excluded` 仍然优先：用户删掉 `⚽` 之后 `⚽️` 也不再命中（两步都会被它挡住）。
    fn cluster_hits(&self, cluster: &str, wanted: u128) -> bool {
        let Some(first) = cluster.chars().next() else {
            return false;
        };
        if self.probe(cluster, first, wanted) {
            return true;
        }
        // 单 `char` 的簇：逐 char 与整簇是同一次查询，不必再走一遍。
        if cluster.chars().nth(1).is_none() {
            return false;
        }
        cluster.chars().any(|ch| {
            let mut buf = [0u8; 4];
            self.probe(ch.encode_utf8(&mut buf), ch, wanted)
        })
    }

    /// 只遍历 `wanted` 里真正置位的那几个类（通常一两个），而不是全部类。
    fn probe(&self, cluster: &str, first: char, wanted: u128) -> bool {
        let hits = self.range_hits(first);
        let mut w = wanted;
        while w != 0 {
            let i = w.trailing_zeros() as usize;
            w &= w - 1;
            if self.hits_class(cluster, i, hits) {
                return true;
            }
        }
        false
    }

    /// 按 key 取类（外部引用 `exclude_blocks = ["emoji"]` 的解析用）。
    pub fn class_by_key(&self, key: &str) -> Option<&ClassSpec> {
        self.classes.iter().find(|c| c.key == key)
    }

    /// 全部类，按 `order` 升序。设置页列表用。
    pub fn classes(&self) -> &[ClassSpec] {
        &self.classes
    }

    /// **表态类**的成员，按类的 `order`、类内按配置顺序。
    ///
    /// 「列全表」在新模型下就是「列出那些有人给出判定的字」——不表态的类（50 个内置
    /// 区块、出厂的 emoji）只是给字符一个归属，它们的成员没有「常用还是生僻」可显示。
    pub fn ordered_members(&self) -> impl Iterator<Item = &str> {
        self.classes
            .iter()
            .filter(|c| c.default_common.is_some())
            .flat_map(|c| c.member_order.iter().map(String::as_str))
    }

    /// 完全被更靠前的类遮住、永远轮不到的类——「配了没反应」的经典来源，装载期告警。
    ///
    /// 只查区间型：离散成员要逐条比对才能判「被遮住」，而那正是设置页该做的事
    /// （它有具体的字可显示），装载期给不出有用的信息。
    pub fn shadowed_keys(&self) -> Vec<&str> {
        let mut out = Vec::new();
        for (i, cl) in self.classes.iter().enumerate() {
            if cl.ranges.is_empty() || !cl.members.is_empty() || cl.default_common.is_none() {
                continue;
            }
            let bit = 1u128 << i;
            // order 更靠前（下标更小）且**表态**的类构成的掩码。不表态的类遮不住谁
            // ——它命中后会继续往下让（见 `verdict_of`）。
            let earlier: u128 = self
                .classes
                .iter()
                .take(i)
                .enumerate()
                .filter(|(_, c)| c.default_common.is_some())
                .fold(0, |m, (j, _)| m | (1u128 << j));
            if earlier == 0 {
                continue;
            }
            // 本类覆盖的每一段都被某个更靠前的表态类盖住 ⇒ 它永远轮不到。
            let mut covered_any = false;
            let mut all_shadowed = true;
            for seg in self.segments.iter().filter(|s| s.hits & bit != 0) {
                covered_any = true;
                if seg.hits & earlier == 0 {
                    all_shadowed = false;
                    break;
                }
            }
            if covered_any && all_shadowed {
                out.push(cl.key.as_str());
            }
        }
        out
    }
}

#[cfg(test)]
#[path = "charset_registry_tests.rs"]
mod tests;
