//! 通用规范汉字表（常用字判定）
//!
//! 与 Go 版本 `wind_input/internal/dict/common_chars.go` 对齐。
//! 用于"检索范围"过滤：判定候选是否为常用字/词。

use std::collections::{HashMap, HashSet};
use std::path::Path;

/// 用户覆盖层，叠加在字符类 registry 的默认判定之上。
///
/// 默认判定归 [`crate::CharsetRegistry`]（`charsets/*.yaml`），用户覆盖（候选右键 / 词库管理）
/// 存在本结构。两者只在查询时合成（覆盖优先），不预先 merge：界面要显示「出厂：生僻 → 现在：常用」，
/// 「恢复出厂」也要知道回退到哪边。
///
/// registry 按参数传入而不持有：它由配置派生，持有拷贝就多一处要记得同步的地方
/// （同 [`crate::rare_admits`]）。
pub struct CommonChars {
    /// 出厂字表的成员集。不参与判定（那归 registry），只服务 [`Self::is_empty`] 与
    /// [`Self::list_all`] 里「覆盖键在不在出厂表内」的分类。
    base: HashSet<char>,
    /// 出厂基表的原始顺序（按级别拼接）。设置页按原序分页列举，`HashSet` 迭代序不稳定。
    base_order: Vec<char>,
    /// 用户覆盖：`true` = 强制判为常用，`false` = 强制判为生僻。只含被碰过的字。
    ///
    /// 键是一个字素簇而不是一个 `char`：屏幕上的一个图形可能由多个码位拼成（`⚽️`、`👨‍👩‍👧`、`🇨🇳`）。
    overrides: HashMap<String, bool>,
    /// 覆盖里存在多码位键。热路径闸门：为假时 [`Self::is_string_common`] 按 `char` 切，
    /// 免去字素簇分割的开销。
    has_multi_char_keys: bool,
}

impl CommonChars {
    /// 从字符类 registry 建：全表 = 那些表态类的成员，顺序取自配置。出厂走这条路，判定与列表同源。
    ///
    /// 多码位簇会被跳过：`base` 是 `char` 集合，而表态类的成员本就全是单字符。
    pub fn from_registry(cs: &crate::CharsetRegistry) -> Self {
        Self::from_base(cs.ordered_members().filter_map(|m| {
            let mut it = m.chars();
            let c = it.next()?;
            it.next().is_none().then_some(c)
        }))
    }

    /// 从文件加载（一字一行，`#` 注释行跳过；空白与控制字符外全收，见 [`is_markable`]）。
    ///
    /// 入口过一道 [`wind_utils::text::normalize_input`]：剥 BOM、抹平行尾。
    ///
    /// 出厂不走这条路（常用字表在 `charsets/common_han.yaml`），保留给用户类引用外部字表与测试。
    /// 失败（文件缺失）返回空集；上层应在空集时退化为"不过滤"。
    pub fn load(path: &Path) -> Self {
        let mut base = HashSet::new();
        let mut base_order = Vec::new();
        if let Ok(raw) = std::fs::read_to_string(path) {
            // 行尾规整：孤立 \r 会让整份文件算成一行，带 `#` 注释头的表被整个跳过成空表
            let content = wind_utils::text::normalize_input(&raw);
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                for ch in line.chars() {
                    // 收录判据与写端一致用 `is_markable` 而非 `is_han`，否则用户手写的符号会静默消失。
                    // 重复字不再追加进顺序表，免得列表出现两行同样的字。
                    if is_markable(ch) && base.insert(ch) {
                        base_order.push(ch);
                    }
                }
            }
        }
        Self {
            base,
            base_order,
            overrides: HashMap::new(),
            has_multi_char_keys: false,
        }
    }

    /// 由一批字直接构造基表（测试与内存态装配用）。
    pub fn from_base(chars: impl IntoIterator<Item = char>) -> Self {
        let mut base = HashSet::new();
        let mut base_order = Vec::new();
        for ch in chars {
            if base.insert(ch) {
                base_order.push(ch);
            }
        }
        Self {
            base,
            base_order,
            overrides: HashMap::new(),
            has_multi_char_keys: false,
        }
    }

    /// 全表列举：出厂字（按字表原序）+ 用户加的、不在出厂表里的字（追加在后）。
    ///
    /// 返回 `(字素簇, 出厂是否常用, 现在是否常用)`，设置页靠它列全表并搜索。追加那批的默认判定
    /// 走真实判据而非写死：默认字表管不着的字符（`、` `ㄅ` `⿰`）事实上被放行，默认就是「常用」，
    /// 否则界面与写端「与默认同向就删覆盖」的判断会对同一条记录说法相反。
    pub fn list_all(&self, cs: &crate::CharsetRegistry) -> Vec<(String, bool, bool)> {
        let mut out: Vec<(String, bool, bool)> = self
            .base_order
            .iter()
            .map(|&c| {
                (
                    c.to_string(),
                    self.is_base_common(c, cs),
                    self.is_char_common(c, cs),
                )
            })
            .collect();
        // 覆盖里那些不在出厂表的：排序后追加，保证顺序稳定。
        let mut extra: Vec<&String> = self
            .overrides
            .keys()
            .filter(|k| match single_char_of(k) {
                Some(c) => !self.base.contains(&c),
                None => true, // 多码位簇必不在默认字表里
            })
            .collect();
        extra.sort_unstable();
        // 多码位簇也不能写死 `true`：`鬱`+VS16 按默认判据是生僻。
        out.extend(extra.into_iter().map(|k| {
            (
                k.clone(),
                self.is_cluster_common_by_default(k, cs),
                self.is_cluster_common(k, cs),
            )
        }));
        out
    }

    /// 整体替换用户覆盖（调用方从 store 全量读出后灌进来）。
    ///
    /// 刻意不做增量合并：增量语义下「撤销某字的覆盖」没有落点。
    pub fn set_overrides(&mut self, it: impl IntoIterator<Item = (String, bool)>) {
        // 装载时滤掉不是一个字素簇的键（备份包可能被手工改过）：`我们` 这样的键永远查不中，
        // 还会把 `has_multi_char_keys` 顶成真、让热路径改走字素簇分割。滤在这里而非 store 层，
        // 判据只此一份，且 wind-store 不该反过来依赖本 crate。
        self.overrides = it
            .into_iter()
            .filter(|(k, _)| single_markable_char(k).is_some())
            .collect();
        // 热路径闸门只在装载时算一次，查询时零成本（见字段注释）。
        self.has_multi_char_keys = self.overrides.keys().any(|k| k.chars().count() > 1);
    }

    /// 是否未加载到任何出厂字（数据缺失），上层据此退化为不过滤（同 registry 的
    /// `disarm_empty_scoped_classes`）。
    ///
    /// 只看 `base`、不看覆盖：否则出厂表缺失而用户设过一两个字时会照常过滤，几乎全部候选被滤掉。
    pub fn is_empty(&self) -> bool {
        self.base.is_empty()
    }

    /// 单字是否常用：用户覆盖优先，其次落到 [`Self::is_base_common`]。
    ///
    /// 与 [`Self::is_string_common`] 对单字符串必须给出同一个答案（见 `char_and_string_judgements_agree`）。
    pub fn is_char_common(&self, ch: char, cs: &crate::CharsetRegistry) -> bool {
        self.is_cluster_common(ch.encode_utf8(&mut [0u8; 4]), cs)
    }

    /// 一个字素簇是否常用：整簇覆盖优先，否则逐 `char` 回落（见 [`Self::fallback_verdict_of`]）。
    pub fn is_cluster_common(&self, cluster: &str, cs: &crate::CharsetRegistry) -> bool {
        match self.overrides.get(cluster) {
            Some(&v) => v,
            None => self.fallback_verdict_of(cluster, cs),
        }
    }

    /// 簇本身没有覆盖时的回落判定：逐 `char` 走一遍完整判据（先查覆盖、再查默认字表）。
    ///
    /// 两处都不能省，否则多码位闸门开关两侧判定不一致（见 `gate_consistency`）：不能整簇忽略
    /// （`鬱`+VS16 应判非常用）；逐 `char` 时也必须查覆盖（用户对 `鬱` 的覆盖键不是 `鬱\u{FE0F}`）。
    fn fallback_verdict_of(&self, cluster: &str, cs: &crate::CharsetRegistry) -> bool {
        // 别加「整簇先问一次 registry」：本方法是全称语义（每个 char 都常用才算），整簇查询在这里
        // 会变成短路，用户对 `鬱` 的覆盖就走不到了。`no_freq` / `in_rare` 是存在性语义，那边可以。
        cluster.chars().all(|ch| self.char_verdict(ch, cs))
    }

    /// 单个 `char` 的判定：覆盖优先，其次受管辖的查默认字表，域外一律放行。
    /// 这就是闸门关着时逐 `char` 走的那条路径，抽出来供簇路径回落，两边因此不会分叉。
    fn char_verdict(&self, ch: char, cs: &crate::CharsetRegistry) -> bool {
        match self.overrides.get(ch.encode_utf8(&mut [0u8; 4])) {
            Some(&v) => v,
            // 无类表态即域外，一律放行（出厂 `common_han` 类的 scope 就是 `is_common_scope`）。
            None => cs.verdict_of_char(ch).unwrap_or(true),
        }
    }

    /// 簇的**默认**判定（忽略全部用户覆盖），供「与默认同向就删覆盖」那条判据用。
    /// 与 [`Self::fallback_verdict_of`] 的差别只在要不要看 `overrides`。
    fn default_verdict_of(&self, cluster: &str, cs: &crate::CharsetRegistry) -> bool {
        // 同 `fallback_verdict_of`：全称语义，逐 char，不加整簇短路。
        cluster
            .chars()
            .all(|ch| cs.verdict_of_char(ch).unwrap_or(true))
    }

    /// 默认判定（忽略用户覆盖）。供界面显示「默认：常用 → 现在：生僻」这类对照，
    /// 以及判断某条覆盖是否与默认同向（同向即冗余，写端据此删覆盖而不是存记录）。
    ///
    /// 默认字表管不着的字符默认判「常用」（读端事实上放行它们）。若返回 `false`，用户把 `、`
    /// 设成生僻会被 `apply_common_target` 判成与默认同向而删掉覆盖，设了没反应。
    pub fn is_base_common(&self, ch: char, cs: &crate::CharsetRegistry) -> bool {
        cs.verdict_of_char(ch).unwrap_or(true)
    }

    /// 一个字素簇的默认判定（忽略用户覆盖）：逐 `char` 查默认字表，每个都常用才算常用。
    pub fn is_cluster_common_by_default(&self, cluster: &str, cs: &crate::CharsetRegistry) -> bool {
        self.default_verdict_of(cluster, cs)
    }

    /// 某字的用户覆盖方向；`None` = 未覆盖。
    pub fn override_of(&self, ch: char) -> Option<bool> {
        self.override_of_cluster(ch.encode_utf8(&mut [0u8; 4]))
    }

    /// 某个字素簇的用户覆盖方向；`None` = 未覆盖。
    pub fn override_of_cluster(&self, cluster: &str) -> Option<bool> {
        self.overrides.get(cluster).copied()
    }

    /// 这串文本里有没有用户亲手标成生僻的字（见 [`crate::Candidate::user_rare`]）。
    ///
    /// 存在性语义：词组里有一个字被降级就算，与 `is_string_common` 的全称语义相反。出厂
    /// `input.rare_phrase = "keep"` 下多字候选先被放行，实际只管得着单字。
    pub fn has_user_rare(&self, text: &str) -> bool {
        self.units_of(text)
            .any(|u| match self.override_of_cluster(u) {
                Some(v) => !v,
                // 簇本身没覆盖时必须逐 `char` 再问一遍（覆盖键是裸 `⚽`、候选是 `⚽️`），
                // 否则多码位闸门一开 `user_rare` 就悄悄变 false。
                None => u.chars().any(|c| self.override_of(c) == Some(false)),
            })
    }

    /// 按什么粒度切这串文本去查覆盖表：没有多码位覆盖时按 `char` 切，有才按字素簇切
    /// （热路径闸门，见 `has_multi_char_keys`）。
    fn units_of<'a>(&self, text: &'a str) -> Box<dyn Iterator<Item = &'a str> + 'a> {
        if self.has_multi_char_keys {
            use unicode_segmentation::UnicodeSegmentation;
            Box::new(text.graphemes(true))
        } else {
            // `split("")` 会带空串，用 char_indices 手工切才干净。
            Box::new(
                text.char_indices()
                    .map(move |(i, c)| &text[i..i + c.len_utf8()]),
            )
        }
    }

    /// 字符串是否常用：其中所有「汉字」都在表内，非汉字辅助字符（标点/字母/数字/
    /// emoji/符号）忽略。空串视为非常用。
    ///
    /// 「汉字」的作用域 = [`is_han`] ∪ [`is_pua`]：码表拿它当汉字用（含冒充生僻字的 PUA）才查表，
    /// 只是符号（`、。《》` 等 CJK 标点）就忽略，与 Unicode 块的相邻关系无关。
    ///
    /// 用户覆盖优先于整套作用域判断，且不限字符种类：这是写端全字符放开的读端一半，否则
    /// `ㄅ` `⿰` `あ` 等作用域外字符的覆盖会存进库却永不被查询。
    pub fn is_string_common(&self, text: &str, cs: &crate::CharsetRegistry) -> bool {
        if text.is_empty() {
            return false;
        }
        for unit in self.units_of(text) {
            match self.overrides.get(unit) {
                // 用户显式表过态：照办，与它是不是汉字无关。
                Some(&common) => {
                    if !common {
                        return false;
                    }
                }
                // 未表态：汉字（含 PUA）查默认字表，其余辅助字符忽略。
                // 多码位簇逐 char 回落，与闸门关着时给出同一答案。
                None => {
                    if !self.fallback_verdict_of(unit, cs) {
                        return false;
                    }
                }
            }
        }
        true
    }
}

/// 默认字表管辖哪些字符 = [`is_han`] ∪ [`is_pua`]：用户没表过态时，域内查不到判非常用，
/// 域外一律忽略。
///
/// 这不是写端准入（那是更宽的 [`is_markable`]），不得据此拒绝写入；写端放行的字符设成生僻后
/// 读端一定判非常用，由 `markable_char_takes_effect` 保证。
pub fn is_common_scope(ch: char) -> bool {
    is_han(ch) || is_pua(ch)
}

/// 能不能给这个字符登记常用/生僻覆盖——**除空白与控制字符外一律可以**。
///
/// 刻意放到最宽：用户也要能把字根、间架结构符（`⿰⿱`）、注音、假名这类 [`is_common_scope`]
/// 之外的符号设成生僻。排除空白与控制字符只是数据卫生：登记进去在列表里是一行看不见的空白。
pub fn is_markable(ch: char) -> bool {
    !ch.is_whitespace() && !ch.is_control()
}

/// 把一串文本切成一个个「用户眼里的字符」（字素簇）。
///
/// 导入旧版 `rare = "⿰ㄅ"` 整串格式时用它：按 `char` 切会把 `👨‍👩‍👧` 拆成 5 条垃圾记录。
/// 字素簇算法收在本 crate 一处，别的 crate 调它而不是各自依赖 `unicode-segmentation`。
pub fn split_markable_clusters(text: &str) -> impl Iterator<Item = &str> {
    use unicode_segmentation::UnicodeSegmentation;
    text.graphemes(true)
}

/// 恰好由一个 `char` 组成时返回它，否则 `None`（多码位簇没有单个 char 可查默认表）。
fn single_char_of(s: &str) -> Option<char> {
    let mut it = s.chars();
    let ch = it.next()?;
    it.next().is_none().then_some(ch)
}

/// 这串文本是不是**用户眼里的一个字符**；是则原样返回，用作覆盖表的键。
///
/// 「一个」按 **Unicode 字素簇**（UAX #29）算，不是按 `char` 数：屏幕上是一个图形的东西，
/// 码位数可以差很多——
///
/// | 形态 | 例 | 码位数 |
/// |---|---|---|
/// | 变体选择符 | `⚽️` | 2 |
/// | 肤色修饰符 | `👍🏻` | 2 |
/// | ZWJ 序列 | `👨‍👩‍👧` | 5 |
/// | 区域指示符对（国旗） | `🇨🇳` | 2 |
/// | Keycap | `1️⃣` | 3 |
///
/// 用户分辨不出的差别不该决定能不能设置。别退回自己列举修饰码位的规则，逐条列举必随
/// Unicode 版本漏掉一类。
///
/// 多个字素簇一律拒绝（`我们`、`abc`）：常用性是单字属性，给词组存覆盖读端永远查不到。
pub fn single_markable_char(text: &str) -> Option<&str> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut it = text.graphemes(true);
    let g = it.next()?;
    if it.next().is_some() {
        return None;
    }
    // 簇内任一 char 不可登记（空白、控制字符）即整簇拒绝。
    g.chars().all(is_markable).then_some(g)
}

/// 是否「须按通用规范汉字表判定常用性」的汉字。
///
/// 判定域是真汉字块，外加在码表里占汉字编码出现的部首、笔画。
///
/// 刻意排除紧邻汉字块的辅助符号（规范汉字表对其无从判断，查表必失败）：`U+2FF0..=U+2FFF`
/// 结构描述符、`U+3000..=U+303F` CJK 符号和标点、`U+3040..=U+319F` 假名/注音/谚文/汉文标注、
/// `U+3200..=U+33FF` 带圈与兼容符号。别为了「不想看到它们」补进本域：隐藏整类符号是与常用性
/// 正交的另一根轴（按 Unicode 类别过滤）。
///
/// 公开是因为展开态的分类（单字 / 词组 / 生僻）要按「是不是汉字」分，与常用性判定必须同一个域。
pub fn is_han(ch: char) -> bool {
    let c = ch as u32;
    // BMP：汉字块之间夹着假名、注音、标点，只能逐块列举；且 BMP 已排满，不会再新增汉字块。
    (0x2E80..=0x2EFF).contains(&c)        // CJK 部首补充
        || (0x2F00..=0x2FDF).contains(&c) // 康熙部首
        || (0x31C0..=0x31EF).contains(&c) // CJK 笔画
        || (0x3400..=0x4DBF).contains(&c) // 扩展 A
        || (0x4E00..=0x9FFF).contains(&c) // 基本汉字
        || (0xF900..=0xFAFF).contains(&c) // 兼容汉字
        // 平面 2、3 整体纳入：专用于表意文字，扩展 B–J 与兼容汉字补充全在其中，将来的扩展亦然。
        // 别改回逐块列举：每升一版 Unicode 新扩展区就会落到域外、在任何档下都被放行。
        || (0x20000..=0x3FFFF).contains(&c)
}

/// 候选文本的**语义单元数**：汉字逐字计，连续的西文/数字段整体计 1。
///
/// ```text
/// 「东西」    → 2      hello        → 1
/// 「的样子」  → 3      thank you    → 2
/// 「iPhone」  → 1      「新iPhone」 → 2
/// ```
///
/// 用途：词频位置提升的准入判据（`schema.*.frequency.promote_prefix = "single"`）。
///
/// 不能用 `chars().count()`：英文候选全是前缀匹配，按字符数会被「只提升单字」挡死，英文调频全灭。
/// 一个汉字、一个西文词，都是用户心智里的「一个东西」。
///
/// PUA 按汉字计——本码表把私用区码位当生僻字使用，见 [`is_pua`]。
pub fn semantic_units(text: &str) -> usize {
    let mut units = 0usize;
    let mut in_latin_word = false;
    for ch in text.chars() {
        if is_han(ch) || is_pua(ch) {
            units += 1;
            in_latin_word = false;
        } else if ch.is_whitespace() {
            in_latin_word = false;
        } else if !in_latin_word {
            units += 1;
            in_latin_word = true;
        }
    }
    units
}

/// 是否 Unicode 私用区（PUA）。本码表把 PUA 码位当汉字使用（占汉字编码、冒充生僻字），
/// 故常用性判定须把 PUA 视作「必须查表的汉字」，不在规范字表内即判非常用。
///
/// 公开是因为 [`crate::charset_registry::Scope`] 要拿它当作用域判据，只能引用这份定义、不能各写一份。
pub fn is_pua(ch: char) -> bool {
    let c = ch as u32;
    (0xE000..=0xF8FF).contains(&c)          // BMP 私用区
        || (0xF0000..=0xFFFFD).contains(&c) // 补充私用区 A
        || (0x100000..=0x10FFFD).contains(&c) // 补充私用区 B
}

#[cfg(test)]
mod semantic_units_tests {
    use super::semantic_units;

    /// 中英混排的计数口径——词频 `promote_prefix = "single"` 全靠它区分「单字/单词」与「词组」。
    #[test]
    fn counts_han_per_char_and_latin_per_word() {
        // 中文逐字
        assert_eq!(semantic_units("东"), 1);
        assert_eq!(semantic_units("东西"), 2);
        assert_eq!(semantic_units("的样子"), 3);
        // 西文整词计 1 —— 这是英文调频能工作的前提
        assert_eq!(semantic_units("hello"), 1);
        assert_eq!(semantic_units("a"), 1);
        assert_eq!(semantic_units("thank you"), 2);
        assert_eq!(semantic_units("e-mail"), 1, "连字符不断词");
        // 混排
        assert_eq!(semantic_units("新iPhone"), 2);
        assert_eq!(semantic_units("iPhone手机"), 3);
        // 边界
        assert_eq!(semantic_units(""), 0);
        assert_eq!(semantic_units("   "), 0);
        // 数字按一段计
        assert_eq!(semantic_units("2026"), 1);
    }

    /// 扩展区汉字与 PUA 均按汉字逐字计——码表把私用区码位当生僻字使用。
    #[test]
    fn counts_ext_han_and_pua_as_chars() {
        assert_eq!(semantic_units("\u{20000}"), 1, "扩展 B");
        assert_eq!(semantic_units("\u{E000}\u{E001}"), 2, "PUA 逐字计");
    }
}

#[cfg(test)]
#[path = "common_tests.rs"]
mod tests;
