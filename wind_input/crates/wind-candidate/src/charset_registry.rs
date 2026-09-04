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
        (
            Self {
                classes: specs,
                segments,
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
        self.any_char_in(text, |c| c.no_freq)
    }

    /// 纳入生僻字模式（**并集**，存在性）。
    pub fn in_rare(&self, text: &str) -> bool {
        self.any_char_in(text, |c| c.in_rare)
    }

    /// 并集类查询的共同实现：逐字素簇看有没有命中某个满足 `want` 的类。
    ///
    /// 按**簇**而不是按 char 遍历，簇本身也要查一次：多码位的 emoji 序列
    /// （`👨‍👩‍👧`、`1️⃣`）在字表里是一整条，按 char 拆开就查不到了。
    fn any_char_in(&self, text: &str, want: impl Fn(&ClassSpec) -> bool) -> bool {
        let wanted: Vec<usize> = (0..self.classes.len())
            .filter(|&i| want(&self.classes[i]))
            .collect();
        if wanted.is_empty() {
            return false;
        }
        crate::split_markable_clusters(text).any(|cluster| {
            let Some(ch) = cluster.chars().next() else {
                return false;
            };
            let hits = self.range_hits(ch);
            wanted.iter().any(|&i| self.hits_class(cluster, i, hits))
        })
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
}
