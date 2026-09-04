//! 字符类的**判定结构**：把配置编译成可放上按键热路径的查表。
//!
//! 设计与全部取舍见 `docs/design/charset-classification.md`。本模块只做判定，
//! 不认识 TOML——配置的读取与三层合并在 `wind-config` 的 `charset_def`，
//! 由 coordinator 把解析结果转成 [`ClassSpec`] 喂进来。
//!
//! ⛔ 与 `wind-config` 的 `code_charset::CodeCharSet` 无关：那个是**码元**字符集
//! （哪些按键算输入码，值域是 ASCII）。本模块的值域是全 Unicode。
//!
//! # ★★★ 两种性质，两种合成规则
//!
//! | 性质 | 规则 | 为什么 |
//! |---|---|---|
//! | 常用性 [`verdict_of`](CharsetRegistry::verdict_of) | **仲裁**：`order` 最小且表态的类赢 | 二值判定，只能有一个答案 |
//! | [`no_freq`](CharsetRegistry::no_freq) / [`in_rare`](CharsetRegistry::in_rare) | **并集**：任一命中的类为真即真 | 布尔能力，多给一个是安全方向 |
//!
//! ⚠️ 不得把并集那两项也改成仲裁：`▶` 同时命中 emoji 类与「符号」类，若两者的
//! `no_freq` 相反，按并集它免词频（与既有 `exclude_blocks` 一致），按仲裁则取决于
//! `order`——那是**静默的行为变更**，且方向不安全（一个本该免词频的字符会开始记词频）。

use std::collections::HashSet;

/// 类的**作用域**：本类「管得着」谁。值域是闭集，见设计文档 §2.4。
///
/// ⛔ 不接受用户自定义码位段：作用域背后是**判定域**，漏一段的后果是那批字恒判常用、
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
/// 常用性用 `Option<bool>` 而不是三态枚举：`None` = 不表态（不参与仲裁），
/// `Some(true)` = 常用。配置层那个带 serde 的枚举由 coordinator 转换过来，
/// 逻辑层不引 serde（与 headless 解耦那条纪律同源）。
#[derive(Debug, Clone, Default)]
pub struct ClassSpec {
    /// 稳定标识，配置里的键名，也是 `exclude_blocks` 等外部引用的取值。
    pub key: String,
    /// 显示名。空则回落 `key`。
    pub name: String,
    /// 码位段（闭区间），可空。
    pub ranges: Vec<(u32, u32)>,
    /// 离散成员（字表文件 + `add`），键是**字素簇**。
    pub members: HashSet<String>,
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
/// # 为什么区间走分段表、离散走 HashSet
///
/// ⛔ **离散型不要切进分段表**。`common_han` 的 8104 字是离散点集，切进去会把 Han 域
/// 打成约 16000 段、二分要 14 次比较——而现状是「几次区间比较 + 一次 HashSet 查询」，
/// 改完**反而更慢**。区间型的段边界只有几百个，二分一次即可。
#[derive(Debug, Clone, Default)]
pub struct CharsetRegistry {
    /// 按 `order` 升序（同 order 按 `key` 定序，保证编译结果可复现）。
    classes: Vec<ClassSpec>,
    /// 区间型的不相交段，按 `start` 升序。
    segments: Vec<Segment>,
    /// 开了 `no_freq` 的类的位集，**编译期算好**。
    ///
    /// ★ 存成掩码而不是查询时现算，是因为这两个查询在按键热路径上（每次按键 × 每个
    /// 候选）。现算意味着每次调用都要遍历类列表并**分配一个 `Vec`**——那正是
    /// `BlockMask` 用 `is_empty()` 短路刻意避开的成本。掩码为 0 时判定直接恒假，
    /// 与「没配过这个功能的用户零成本」那条路径对齐。
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
    /// ★ 「命中但不表态」要继续往下让，不能就此返回 `None`——那 50 个内置区块类存在的
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

    /// 这个簇属于哪个类（类型列显示用）：优先给**仲裁赢家**，无人表态则给首个命中的类。
    ///
    /// ★ 给仲裁赢家而不是首个命中，是因为用户看这一列是想知道「谁决定了它的常用性」。
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
    /// 按**簇**而不是按 char 遍历，簇本身也要查一次：多码位的 emoji 序列
    /// （`👨‍👩‍👧`、`1️⃣`）在字表里是一整条，按 char 拆开就查不到了。
    ///
    /// ⚠️ `wanted` 由调用方传编译期算好的掩码，**不在这里现算**——现算要遍历类列表并
    /// 分配一个 `Vec`，而本函数在按键热路径上。
    fn any_char_in(&self, text: &str, wanted: u128) -> bool {
        if wanted == 0 {
            return false;
        }
        crate::split_markable_clusters(text).any(|cluster| self.cluster_hits(cluster, wanted))
    }

    /// ★★ 一个字素簇命中 `wanted` 里的某个类吗：**先查整簇，不中再逐 `char` 回落**。
    ///
    /// # 两步都不能省
    ///
    /// 字表**不列**变体序列与 ZWJ 组合（生成 `emoji.yaml` 时的决定，设计文档 §5.3），
    /// 理由是「逐 `char` 查表时首码位已命中」——那条推理的前提是逐 `char`。
    ///
    /// | 输入 | 只查整簇 | 只逐 `char` |
    /// |---|---|---|
    /// | `👨‍👩‍👧`（字表里是一整条） | ✅ | ✅ 首码位也在表里 |
    /// | `⚽️` = `⚽` + `U+FE0F` | ⛔ **漏**，整簇不在表里 | ✅ 基字符在表里 |
    /// | `1️⃣` = `1` + `FE0F` + `20E3` | ⛔ 漏 | ✅ `U+20E3` 在表里 |
    ///
    /// ⇒ 省掉逐 `char` 那一步，`⚽️` 这类**裸基字符 + 变体选择符**的写法全都漏判，而
    /// 上游词库存的恰恰多是这种形态。这是接口两侧各自合理的假设失配出来的洞：
    /// 生成器按「消费方逐 char」省略了组合序列，判定按簇遍历。
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

/// 内置区块类的 `order`。
///
/// ★ 区块类**一个判定字段都不表态**（`default_common` / `outside_common` / `no_freq` /
/// `in_rare` 全空），所以本值对仲裁毫无影响。它唯一的作用是让
/// [`CharsetRegistry::class_of`] 在无人表态时返回的「首个命中」是**区块**而不是
/// 「符号」那种粗粒度组名——类型列要显示的是块名，与现状 [`crate::block_of_cluster`]
/// 一致。故本值必须**小于** [`PRESET_ORDER`]。
const BLOCK_ORDER: i32 = 900;

/// 预设组「符号」的 `order`。必须**大于** [`BLOCK_ORDER`]，理由见那里。
const PRESET_ORDER: i32 = 1000;

/// Unicode 码位上界（含）。「其它」档的补集算到这里为止。
const MAX_CODEPOINT: u32 = 0x10FFFF;

/// 内置类：50 个 Unicode 区块 + 「其它」兜底档 + 预设组「符号」。
///
/// 这些类**不落配置文件**，由代码提供。三条理由：
///
/// 1. 区块表是**显示域**的划分（[`crate::charblock`]），用户改它只改标签，却会让类型列
///    与 Unicode 官方块名对不上——收益为零、代价是困惑；
/// 2. 出厂 50 个 `.yaml` 会淹掉 `charsets/` 目录，用户看不见真正该配的那两个；
/// 3. 块表随 Unicode 升版增长，是代码维护物。用户要自定义范围，路径是**新建自己的类**
///    （设计文档 §4.3），而不是改内置块。
///
/// # ⛔ 刻意不造 `emoji` 内置类
///
/// [`crate::charclass::PRESET_EMOJI`] 把 `emoji` 展开成五个块，那个口径**两个方向同时
/// 不准**：漏掉 `⬅ ⭐ 🀄 🅰 🈚 ▶ ↔ ™ ©` 等 182 条（它们的块根本不在块表里，或整块搬进来
/// 会连 `← → ▲ ◆` 一起搬），又多收「杂项符号」块里的 `☰` 与「杂项技术符号」块里的
/// `⌘ ⌥`。
///
/// ⇒ `emoji` 这个 key 由 `data/charsets/emoji.yaml` 那份按 UTS #51 `Emoji` 属性生成的
/// 精确字表提供，本函数**不得**再造一个同名类顶掉它。`emoji_is_not_a_builtin_class`
/// 钉住这条。
///
/// ⚠️ 这意味着 `exclude_blocks = ["emoji"]` 切到本 registry 后**判据会变准**——这是
/// 预期的修正，不是回归。出厂 `exclude_blocks` 为空，没配过的用户零影响。
pub fn builtin_block_specs() -> Vec<ClassSpec> {
    let mut out: Vec<ClassSpec> = crate::charblock::BLOCKS
        .iter()
        .map(|b| ClassSpec {
            key: b.name.to_string(),
            name: b.name.to_string(),
            ranges: vec![(b.start, b.end)],
            order: BLOCK_ORDER,
            ..Default::default()
        })
        .collect();

    // 「其它」= 块表**之外**的一切。它在 `BlockMask` 里是单独一位（没有下标可用），
    // 这里则化成**补集区间**——纯数据变换，不必给 `Scope` 加一个「全域」变体去承载
    // 「补集」这个概念（`scope` 的值域是闭集，见设计文档 §2.4）。
    out.push(ClassSpec {
        key: crate::charblock::OTHER.name.to_string(),
        name: crate::charblock::OTHER.name.to_string(),
        ranges: blocks_complement(),
        order: BLOCK_ORDER,
        ..Default::default()
    });

    out.push(ClassSpec {
        key: crate::charclass::PRESET_SYMBOLS_NAME.to_string(),
        name: crate::charclass::PRESET_SYMBOLS_NAME.to_string(),
        ranges: ranges_of_named_blocks(crate::charclass::PRESET_SYMBOLS),
        order: PRESET_ORDER,
        ..Default::default()
    });

    out
}

/// 块表的补集（「其它」档的区间形态）。
///
/// 依赖块表**按 `start` 升序且互不重叠**——`blocks_are_sorted_and_disjoint` 钉着这条，
/// 它同时也是 [`crate::block_index_of`] 二分查找的正确性前提。
fn blocks_complement() -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut next = 0u32;
    for b in crate::charblock::BLOCKS {
        if b.start > next {
            out.push((next, b.start - 1));
        }
        // 块表最后一段的 `end` 可能贴近 `MAX_CODEPOINT`，`+1` 用 saturating 防溢出。
        next = b.end.saturating_add(1);
    }
    if next <= MAX_CODEPOINT {
        out.push((next, MAX_CODEPOINT));
    }
    out
}

/// 按块名取区间。名字不存在就跳过——预设组的成员是代码里的常量，拼错会被
/// `preset_members_all_resolve` 当场抓住，运行期不必再有别的表现。
fn ranges_of_named_blocks(names: &[&str]) -> Vec<(u32, u32)> {
    names
        .iter()
        .filter_map(|n| {
            crate::charblock::BLOCKS
                .iter()
                .find(|b| b.name == *n)
                .map(|b| (b.start, b.end))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(key: &str, order: i32) -> ClassSpec {
        ClassSpec {
            key: key.into(),
            name: key.into(),
            order,
            ..Default::default()
        }
    }

    fn with_ranges(key: &str, order: i32, r: &[(u32, u32)], common: Option<bool>) -> ClassSpec {
        ClassSpec {
            ranges: r.to_vec(),
            default_common: common,
            ..spec(key, order)
        }
    }

    /// 朴素线性实现，作为分段表的对照物。与 `block_of` 换二分时用的同一种守门。
    fn linear_verdict(specs: &[ClassSpec], cluster: &str) -> Option<bool> {
        let ch = cluster.chars().next()?;
        let c = ch as u32;
        let mut ordered: Vec<&ClassSpec> = specs.iter().collect();
        ordered.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.key.cmp(&b.key)));
        for cl in ordered {
            let hit = !cl.excluded.contains(cluster)
                && (cl.members.contains(cluster)
                    || cl.ranges.iter().any(|&(lo, hi)| (lo..=hi).contains(&c)));
            if hit {
                if let Some(v) = cl.default_common {
                    return Some(v);
                }
                continue;
            }
            if let Some(sc) = cl.scope
                && sc.contains(ch)
                && let Some(v) = cl.outside_common
            {
                return Some(v);
            }
        }
        None
    }

    #[test]
    fn segments_are_sorted_and_disjoint() {
        let (reg, _) = CharsetRegistry::compile(vec![
            with_ranges("a", 1, &[(0x20, 0x7F), (0x2600, 0x26FF)], Some(false)),
            with_ranges("b", 2, &[(0x50, 0x2000)], Some(true)),
        ]);
        for w in reg.segments.windows(2) {
            assert!(w[0].end < w[1].start, "段必须有序且不相交: {w:?}");
        }
        assert!(reg.segments.iter().all(|s| s.start <= s.end));
    }

    /// ★ 分段表与朴素扫描必须逐码位等价——这条把「切段」这个优化钉成正确性前提。
    #[test]
    fn segment_table_matches_linear_scan() {
        let specs = vec![
            with_ranges("x", 1, &[(0x20, 0x7F), (0x2600, 0x26FF)], Some(false)),
            with_ranges("y", 2, &[(0x50, 0x2700)], Some(true)),
            with_ranges("z", 0, &[(0x2650, 0x2660)], None), // 命中但不表态
        ];
        let (reg, _) = CharsetRegistry::compile(specs.clone());
        for c in (0x00u32..0x3000).filter_map(char::from_u32) {
            let s = c.to_string();
            assert_eq!(
                reg.verdict_of(&s),
                linear_verdict(&specs, &s),
                "码位 U+{:04X} 两种实现不一致",
                c as u32
            );
        }
    }

    /// ★★ 「命中但不表态」必须继续往下让：内置区块类只为类型列存在，
    /// 它们挡住后面真正表态的类就是一次静默的判定丢失。
    #[test]
    fn a_hit_without_opinion_defers_to_the_next_class() {
        let (reg, _) = CharsetRegistry::compile(vec![
            with_ranges("块", 1, &[(0x2600, 0x26FF)], None),
            with_ranges("emoji", 2, &[(0x2600, 0x26FF)], Some(false)),
        ]);
        assert_eq!(reg.verdict_of("\u{2600}"), Some(false));
        assert_eq!(
            reg.class_of("\u{2600}").unwrap().key,
            "emoji",
            "类型列给仲裁赢家"
        );
    }

    #[test]
    fn smaller_order_wins() {
        let (reg, _) = CharsetRegistry::compile(vec![
            with_ranges("late", 9, &[(0x100, 0x200)], Some(true)),
            with_ranges("early", 1, &[(0x100, 0x200)], Some(false)),
        ]);
        assert_eq!(reg.verdict_of("\u{150}"), Some(false));
    }

    /// `remove` 要盖过 `ranges` 的命中，否则「把这个符号从类里去掉」做不到。
    #[test]
    fn excluded_beats_ranges_and_members() {
        let mut s = with_ranges("t", 1, &[(0x2600, 0x26FF)], Some(false));
        s.members.insert("★".into());
        s.excluded.insert("\u{2601}".into());
        s.excluded.insert("★".into());
        let (reg, _) = CharsetRegistry::compile(vec![s]);
        assert_eq!(reg.verdict_of("\u{2600}"), Some(false), "未排除的照常命中");
        assert_eq!(reg.verdict_of("\u{2601}"), None, "排除的不命中");
        assert_eq!(reg.verdict_of("★"), None, "排除优先于 members");
    }

    /// ★★ 作用域：「是汉字却不在字表里 ⇒ 生僻」——成员关系表达不了的那一半。
    #[test]
    fn scope_covers_the_complement_of_the_member_list() {
        let mut s = spec("common_han", 1);
        s.scope = Some(Scope::Han);
        s.members.insert("我".into());
        s.default_common = Some(true);
        s.outside_common = Some(false);
        let (reg, _) = CharsetRegistry::compile(vec![s]);
        assert_eq!(reg.verdict_of("我"), Some(true), "在名单里");
        assert_eq!(reg.verdict_of("龘"), Some(false), "是汉字、不在名单 ⇒ 生僻");
        assert_eq!(reg.verdict_of("A"), None, "域外不表态，留给兜底");
    }

    /// ★★★ 并集不是仲裁：两个类对 no_freq 意见相反时，命中任一为真即为真。
    #[test]
    fn no_freq_is_a_union_not_an_arbitration() {
        let mut emoji = with_ranges("emoji", 9, &[(0x25B6, 0x25B6)], Some(false));
        emoji.no_freq = true;
        let symbols = with_ranges("符号", 1, &[(0x2190, 0x2BFF)], Some(true));
        let (reg, _) = CharsetRegistry::compile(vec![emoji, symbols]);

        assert_eq!(
            reg.verdict_of("\u{25B6}"),
            Some(true),
            "常用性按 order 仲裁"
        );
        assert!(
            reg.no_freq("\u{25B6}"),
            "★ 免词频按并集——order 更靠前的「符号」没有 no_freq 也不该压掉 emoji 的"
        );
    }

    /// 存在性：一串文本里只要有一个字符命中就算。
    #[test]
    fn union_queries_are_existential_over_the_text() {
        let mut s = with_ranges("e", 1, &[(0x1F300, 0x1FAFF)], Some(false));
        s.no_freq = true;
        let (reg, _) = CharsetRegistry::compile(vec![s]);
        assert!(reg.no_freq("我\u{1F34E}你"));
        assert!(!reg.no_freq("我你"));
    }

    /// 多码位簇在字表里是一整条，按 char 拆开就查不到——按簇遍历钉住这一点。
    #[test]
    fn multi_codepoint_clusters_are_matched_whole() {
        let mut s = spec("e", 1);
        s.members.insert("1\u{FE0F}\u{20E3}".into());
        s.default_common = Some(false);
        s.no_freq = true;
        let (reg, _) = CharsetRegistry::compile(vec![s]);
        assert_eq!(reg.verdict_of("1\u{FE0F}\u{20E3}"), Some(false));
        assert!(reg.no_freq("按1\u{FE0F}\u{20E3}键"));
        assert_eq!(reg.verdict_of("1"), None, "裸数字不该命中");
    }

    /// 同 order 时按 key 定序：否则同一份配置每次启动可能编译出不同的仲裁结果。
    #[test]
    fn equal_order_is_broken_deterministically_by_key() {
        // 同一组 (key, default)，只改**输入顺序**——若把 key 与 default 的配对一起对调，
        // 测的就成了「换个类赢」而不是「换个输入顺序」，那两者本就该给出不同答案。
        let build = |reversed: bool| {
            let mut specs = vec![
                with_ranges("a", 5, &[(0x100, 0x200)], Some(true)),
                with_ranges("b", 5, &[(0x100, 0x200)], Some(false)),
            ];
            if reversed {
                specs.reverse();
            }
            let (r, _) = CharsetRegistry::compile(specs);
            r.verdict_of("\u{150}")
        };
        assert_eq!(build(false), build(true), "编译结果必须与输入顺序无关");
        assert_eq!(build(false), Some(true), "同 order 时 key 小的赢");
    }

    /// 位集塞不下时必须把类**丢弃并报出来**，不能静默截断——高位类恒不命中、
    /// 配了没反应，是最难查的一类故障。
    #[test]
    fn classes_beyond_the_bitset_width_are_reported() {
        let specs: Vec<ClassSpec> = (0..MAX_CLASSES + 3)
            .map(|i| with_ranges(&format!("c{i:03}"), i as i32, &[(0x100, 0x200)], Some(true)))
            .collect();
        let (reg, dropped) = CharsetRegistry::compile(specs);
        assert_eq!(reg.classes().len(), MAX_CLASSES);
        assert_eq!(dropped.len(), 3, "超出的必须被报出来");
    }

    #[test]
    fn detects_a_fully_shadowed_class() {
        let (reg, _) = CharsetRegistry::compile(vec![
            with_ranges("wide", 1, &[(0x100, 0x900)], Some(true)),
            with_ranges("inner", 2, &[(0x200, 0x300)], Some(false)),
            with_ranges("free", 3, &[(0x1000, 0x1100)], Some(false)),
        ]);
        assert_eq!(reg.shadowed_keys(), vec!["inner"]);
    }

    #[test]
    fn empty_registry_is_inert() {
        let (reg, dropped) = CharsetRegistry::compile(Vec::new());
        assert!(dropped.is_empty());
        assert_eq!(reg.verdict_of("我"), None);
        assert!(!reg.no_freq("我"));
        assert!(reg.class_of("我").is_none());
    }

    /// ★ `no_freq` / `in_rare` 的掩码必须在**排序之后**算——位是按 `classes` 的下标建的，
    /// 而排序会改变下标。算早了就位错人，表现为「开了免词频的是 A，实际免的是 B」。
    ///
    /// 构造：`late` 开着 no_freq 但 order 靠后，`early` 没开却排在前面。排序前 `late`
    /// 在下标 0，排序后在下标 1；掩码算早了会置位 0，于是 `early` 的字符被误判免词频。
    #[test]
    fn union_masks_are_computed_after_sorting() {
        let late = ClassSpec {
            key: "late".into(),
            ranges: vec![(0x41, 0x41)], // 'A'
            order: 100,
            no_freq: true,
            ..Default::default()
        };
        let early = ClassSpec {
            key: "early".into(),
            ranges: vec![(0x42, 0x42)], // 'B'
            order: 1,
            ..Default::default()
        };
        let (reg, _) = CharsetRegistry::compile(vec![late, early]);
        assert!(reg.no_freq("A"), "开了 no_freq 的那个类没生效");
        assert!(!reg.no_freq("B"), "没开 no_freq 的类被误判——掩码位错人了");
    }

    /// 没有任何类开并集属性时，两个查询恒假且可被调用方整条绕开。
    #[test]
    fn empty_union_masks_report_themselves() {
        let (reg, _) = CharsetRegistry::compile(builtin_block_specs());
        assert!(reg.no_freq_is_empty());
        assert!(reg.in_rare_is_empty());
        assert!(!reg.no_freq("A"));
        assert!(!reg.in_rare("A"));
    }

    /// ★★ 裸基字符 + 变体选择符（`⚽️` = `⚽` + `U+FE0F`）必须命中。
    ///
    /// 字表里只有基字符——生成 `emoji.yaml` 时刻意不列变体序列与 ZWJ 组合（否则文件
    /// 大 6 倍），前提是消费方会逐 `char` 回落。上游词库存的恰恰多是这种裸码位 +
    /// 变体符的形态，漏了就是「配了免词频，可有些 emoji 还在记词频」。
    #[test]
    fn a_variation_sequence_falls_back_to_its_base_character() {
        let (reg, _) = CharsetRegistry::compile(vec![ClassSpec {
            key: "e".into(),
            members: ["⚽".to_string()].into_iter().collect(),
            no_freq: true,
            ..Default::default()
        }]);
        assert!(reg.no_freq("⚽"), "基字符本身该命中");
        assert!(
            reg.no_freq("⚽\u{FE0F}"),
            "基字符 + 变体选择符——字表不列这种组合，靠逐 char 回落"
        );
    }

    /// 整簇优先于回落：字表里有整条 ZWJ 序列时按整簇命中，不必拆。
    #[test]
    fn a_whole_cluster_in_the_table_matches_as_one() {
        let (reg, _) = CharsetRegistry::compile(vec![ClassSpec {
            key: "e".into(),
            members: ["👨\u{200D}👩\u{200D}👧".to_string()].into_iter().collect(),
            no_freq: true,
            ..Default::default()
        }]);
        assert!(reg.no_freq("👨\u{200D}👩\u{200D}👧"));
        // 首码位单独出现时不在表里 ⇒ 不命中（这一条同时证明上一条不是靠回落蒙对的）。
        assert!(!reg.no_freq("👨"));
    }

    /// `excluded` 对回落路径同样优先：删掉基字符后，变体序列也不再命中。
    #[test]
    fn removing_the_base_also_kills_the_variation_sequence() {
        let (reg, _) = CharsetRegistry::compile(vec![ClassSpec {
            key: "e".into(),
            members: ["⚽".to_string()].into_iter().collect(),
            excluded: ["⚽".to_string()].into_iter().collect(),
            no_freq: true,
            ..Default::default()
        }]);
        assert!(!reg.no_freq("⚽"));
        assert!(!reg.no_freq("⚽\u{FE0F}"), "删除必须挡住回落那一步");
    }

    // ── 内置区块类（`builtin_block_specs`）──────────────────────────────────

    /// 把某个内置类单独挑出来、给它 `no_freq`，用 `no_freq()` 当「这个码位命中本类吗」
    /// 的探针——`hits_class` 是私有的，而这是唯一一条不为测试开后门的问法。
    fn probe_of(key: &str) -> CharsetRegistry {
        let spec = builtin_block_specs()
            .into_iter()
            .find(|c| c.key == key)
            .unwrap_or_else(|| panic!("内置类里没有 {key}"));
        let (reg, dropped) = CharsetRegistry::compile(vec![ClassSpec {
            no_freq: true,
            ..spec
        }]);
        assert!(dropped.is_empty());
        reg
    }

    /// 逐码位扫描（跳过代理区——它构不成 `char`）。
    fn for_each_codepoint(mut f: impl FnMut(char)) {
        for c in 0u32..=0x10FFFF {
            if let Some(ch) = char::from_u32(c) {
                f(ch);
            }
        }
    }

    /// ★ `exclude_blocks` / `include_blocks` 里能写的每个名字，registry 都得认——
    /// **除了 `emoji`**，那个 key 由 `data/charsets/emoji.yaml` 提供（见
    /// `builtin_block_specs` 的文档）。
    ///
    /// 少一个名字的后果是静默的：用户配的那一行变成「未识别」被跳过，功能不生效而无报错。
    #[test]
    fn builtin_keys_cover_every_name_block_mask_accepts() {
        let (reg, dropped) = CharsetRegistry::compile(builtin_block_specs());
        assert!(dropped.is_empty(), "内置类不该撞上 MAX_CLASSES");

        for b in crate::charblock::BLOCKS {
            assert!(reg.class_by_key(b.name).is_some(), "缺区块类 {}", b.name);
        }
        assert!(
            reg.class_by_key(crate::charblock::OTHER.name).is_some(),
            "缺「其它」兜底档"
        );
        assert!(
            reg.class_by_key(crate::charclass::PRESET_SYMBOLS_NAME)
                .is_some(),
            "缺预设组「符号」"
        );
    }

    /// ⛔ `emoji` **不**是内置类。它由出厂字表提供，内置一个同名类会按 key 顶掉它，
    /// 而那正是本次改造要修掉的粗判据。
    #[test]
    fn emoji_is_not_a_builtin_class() {
        assert!(
            !builtin_block_specs()
                .iter()
                .any(|c| c.key == crate::charclass::PRESET_EMOJI_NAME),
            "emoji 的成员必须来自 charsets/emoji.yaml，不得由内置块类顶掉"
        );
    }

    /// ★★ 内置类**一个判定字段都不表态**。
    ///
    /// 它们存在的理由只是给 `exclude_blocks` 那套名字一个落点、给类型列一个标签。
    /// 任何一个表了态，都会在用户什么都没配的情况下改变候选的常用性判定。
    #[test]
    fn every_builtin_class_stays_silent() {
        for c in builtin_block_specs() {
            assert!(c.default_common.is_none(), "{} 表了 default", c.key);
            assert!(c.outside_common.is_none(), "{} 表了 outside", c.key);
            assert!(c.scope.is_none(), "{} 配了 scope", c.key);
            assert!(!c.no_freq, "{} 配了 no_freq", c.key);
            assert!(!c.in_rare, "{} 配了 in_rare", c.key);
        }
    }

    /// 「其它」恰好是块表的补集：逐码位与 `block_index_of` 对答案。
    #[test]
    fn other_class_is_exactly_the_complement_of_the_block_table() {
        let reg = probe_of(crate::charblock::OTHER.name);
        for_each_codepoint(|ch| {
            let in_other = reg.no_freq(&ch.to_string());
            let in_table = crate::block_index_of(ch).is_some();
            assert_eq!(
                in_other, !in_table,
                "U+{:04X} 归属对不上：其它={in_other} 块表内={in_table}",
                ch as u32
            );
        });
    }

    /// 预设组「符号」与 `BlockMask` 的展开逐码位一致。
    ///
    /// 这条是 P3 接线的等价性凭据：切到 registry 之后，配了 `"符号"` 的用户
    /// **判定一个码位都不许变**。
    #[test]
    fn symbols_class_agrees_with_block_mask() {
        let reg = probe_of(crate::charclass::PRESET_SYMBOLS_NAME);
        let (mask, unknown) =
            crate::BlockMask::from_config(&[crate::charclass::PRESET_SYMBOLS_NAME]);
        assert!(unknown.is_empty());
        for_each_codepoint(|ch| {
            assert_eq!(
                reg.no_freq(&ch.to_string()),
                mask.contains_char(ch),
                "U+{:04X} 在「符号」组里的判定变了",
                ch as u32
            );
        });
    }

    /// 只装内置类时，`class_of` 给出的就是**块名**——与现状 `block_of_cluster` 一致。
    ///
    /// ★ 这条钉的是 `BLOCK_ORDER < PRESET_ORDER`：反过来的话，「符号」组会抢在具体块
    /// 前面命中，类型列上半个 BMP 都显示成「符号」，而没有任何报错。
    #[test]
    fn class_of_agrees_with_block_of() {
        let (reg, _) = CharsetRegistry::compile(builtin_block_specs());
        for_each_codepoint(|ch| {
            let got = reg.class_of(&ch.to_string()).map(|c| c.key.as_str());
            assert_eq!(
                got,
                Some(crate::block_of(ch).name),
                "U+{:04X} 的类型列标签变了",
                ch as u32
            );
        });
    }

    /// 预设组的成员块名全都能在块表里解析出来。拼错一个的后果是那一块**静默消失**：
    /// 用户勾了「符号」，某一片字符不生效，而配置校验一声不吭。
    #[test]
    fn preset_members_all_resolve() {
        let n = ranges_of_named_blocks(crate::charclass::PRESET_SYMBOLS).len();
        assert_eq!(
            n,
            crate::charclass::PRESET_SYMBOLS.len(),
            "「符号」组有块名在块表里找不到"
        );
    }
}
