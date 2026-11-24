//! 模糊音配置与匹配
//!
//! 允许用户输入时忽略常见发音混淆（如 z/zh, c/ch, s/sh, n/l）。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

use super::correction::{CORRECTION_EDITS, CorrectionRules};
use super::misread::MisreadTable;

/// 「算不算正确拼音提示」的运行时开关：克隆共享同一份值，`EngineManager` 持有一份并注入各引擎，
/// 翻转即时生效、无需重建引擎。提示只是 `convert` 收尾的后处理，不影响候选集合与排序。
#[derive(Debug, Clone, Default)]
pub struct HintSwitch(Arc<AtomicBool>);

impl HintSwitch {
    pub fn new(on: bool) -> Self {
        Self(Arc::new(AtomicBool::new(on)))
    }

    pub fn get(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub fn set(&self, on: bool) {
        self.0.store(on, Ordering::Relaxed);
    }
}

/// 模糊音配置
#[derive(Debug, Clone, Default)]
pub struct FuzzyConfig {
    pub zh_z: bool,
    pub ch_c: bool,
    pub sh_s: bool,
    pub n_l: bool,
    pub f_h: bool,
    pub r_l: bool,
    pub an_ang: bool,
    pub en_eng: bool,
    pub in_ing: bool,
    pub ian_iang: bool,
    pub uan_uang: bool,
    /// `un ↔ ong`（`dun`↔`dong`、`yun`↔`yong`）。
    pub un_ong: bool,
    /// `eng ↔ ong`（`deng`↔`dong`、`zheng`↔`zhong`）。
    pub eng_ong: bool,
    /// 算不算正确拼音提示（见 [`HintSwitch`]；管理器按 `pinyin_hint_on` 置值）。
    /// 不影响展开/匹配，仅供上层消费。
    pub hint: HintSwitch,
    /// 双拼**同键韵母**推出的等价音节：音节 → 同一键对下其余的合法读音
    /// （自定义布局 `q = ["iu", "ui"]` 时 `diu` → `[dui]`、`dui` → `[diu]`）。
    ///
    /// 不是用户配置：由 `PinyinEngine::with_shuangpin` 从布局推出，全拼方案恒空。双拼转换器
    /// 对一个键对只交出一条全拼，其余读音靠这里展开回来。
    ///
    /// 展开时零罚分：同键两种读音按词频平等竞争，不是「打错了」。叠加的模糊组照旧计处数。
    pub shared_key: HashMap<String, Vec<String>>,
    /// 字母换位纠错规则（[`CorrectionRules`]）。错误拼写并入 [`fuzzy_spellings`]，
    /// [`FuzzyMatcher::fuzzy_variants_scored`] 对其追加正确音节、计 [`CORRECTION_EDITS`] 处。
    /// 双拼方案恒空。
    pub correction: Arc<CorrectionRules>,
    /// 误读表（[`MisreadTable`]）：`None` = 「误读」来源未勾选或表空，不召回不标。
    /// 不影响展开/匹配，由 `PinyinEngine::convert` 收尾召回与标提示。
    pub misread: Option<Arc<MisreadTable>>,
}

impl FuzzyConfig {
    /// 只留用户开的模糊组、去掉双拼同键音节的副本（双拼方案下的全拼降级支路用）。
    pub fn without_shared_key(&self) -> FuzzyConfig {
        FuzzyConfig {
            shared_key: HashMap::new(),
            ..self.clone()
        }
    }

    /// 是否有任一展开来源：用户开的模糊组，或双拼同键音节（[`Self::shared_key`]）。
    /// 供调用方在全关时跳过整段展开逻辑（含其前置的切分求解）。
    ///
    /// 同键音节必须算在内，否则没开模糊音时各调用点的早退会把它整条跳过。本判断只看配置，
    /// 可在任何前置计算之前短路。
    pub fn any_enabled(&self) -> bool {
        self.zh_z
            || self.ch_c
            || self.sh_s
            || self.n_l
            || self.f_h
            || self.r_l
            || self.an_ang
            || self.en_eng
            || self.in_ing
            || self.ian_iang
            || self.uan_uang
            || self.un_ong
            || self.eng_ong
            || !self.shared_key.is_empty()
            || !self.correction.is_empty()
    }
}

/// 模糊音组（无序对：两个方向都成立，故只登记一次）。按整体相等取对端，不做子串试探，
/// 否则同一音节会被多条规则重复命中、产出撑大组合预算的非法码（`sheng` → `shheng`）。
struct FuzzyGroup {
    a: &'static str,
    b: &'static str,
    flag: fn(&FuzzyConfig) -> bool,
}

impl FuzzyGroup {
    /// 若 `part` 是本组一端且本组已开启，返回另一端。
    fn counterpart(&self, part: &str, config: &FuzzyConfig) -> Option<&'static str> {
        if !(self.flag)(config) {
            return None;
        }
        match part {
            p if p == self.a => Some(self.b),
            p if p == self.b => Some(self.a),
            _ => None,
        }
    }
}

/// [`INITIAL_GROUPS`] 的条目数。导出仅供测试：那边按 `2^N` 枚举全部标志位组合，
/// 组数变了而测试没跟上，新组就是零覆盖且全绿。
pub const INITIAL_GROUP_COUNT: usize = 6;

/// 声母模糊组
const INITIAL_GROUPS: &[FuzzyGroup] = &[
    FuzzyGroup {
        a: "zh",
        b: "z",
        flag: |c| c.zh_z,
    },
    FuzzyGroup {
        a: "ch",
        b: "c",
        flag: |c| c.ch_c,
    },
    FuzzyGroup {
        a: "sh",
        b: "s",
        flag: |c| c.sh_s,
    },
    FuzzyGroup {
        a: "n",
        b: "l",
        flag: |c| c.n_l,
    },
    FuzzyGroup {
        a: "f",
        b: "h",
        flag: |c| c.f_h,
    },
    FuzzyGroup {
        a: "r",
        b: "l",
        flag: |c| c.r_l,
    },
];

/// 声母**首字母**的模糊等价集（含 `c` 自身，只含开启的组）。
///
/// 简拼索引的键是各音节首字母，只有改首字母的组（`n↔l`、`f↔h`、`r↔l`）影响召回；
/// `zh↔z` 等对本函数是恒等变换。[`FuzzyMatcher::fuzzy_variants_scored`] 要完整音节，代替不了它。
///
/// `l` 同属 `n↔l` 与 `r↔l`，两组都开时等价集是 `{l, n, r}`，是唯一大于 2 的，决定键变体数上界。
pub fn initial_alternatives(c: char, config: &FuzzyConfig) -> Vec<char> {
    let mut out = vec![c];
    for g in INITIAL_GROUPS {
        if !(g.flag)(config) {
            continue;
        }
        let (Some(a), Some(b)) = (g.a.chars().next(), g.b.chars().next()) else {
            continue;
        };
        // 首字母相同的组（zh/z、ch/c、sh/s）对本函数是恒等变换，跳过。
        if a == b {
            continue;
        }
        let other = if a == c {
            b
        } else if b == c {
            a
        } else {
            continue;
        };
        if !out.contains(&other) {
            out.push(other);
        }
    }
    out
}

/// 两个**首字母**是否模糊等价（含相等）。[`initial_alternatives`] 的无分配版本。
///
/// 段校验在三重循环最内层调用，不能每次分配 Vec。判据与 [`initial_alternatives`] 逐位一致，
/// 改一处必须改另一处（`initial_alternatives_matches_pairwise_equal` 守住）。
pub fn initials_fuzzy_equal(a: char, b: char, config: &FuzzyConfig) -> bool {
    if a == b {
        return true;
    }
    INITIAL_GROUPS.iter().any(|g| {
        if !(g.flag)(config) {
            return false;
        }
        let (Some(x), Some(y)) = (g.a.chars().next(), g.b.chars().next()) else {
            return false;
        };
        // 首字母相同的组（zh/z、ch/c、sh/s）不产生跨字母等价。
        x != y && ((x == a && y == b) || (y == a && x == b))
    })
}

/// 双字母声母（`zh`/`ch`/`sh`）能否放宽成它不带 `h` 的那一支。
///
/// 供混合简拼的 [`AbbrevSeg::Retroflex`](super::mixed_abbrev::AbbrevSeg) 段做模糊放宽：
/// 该段的精确语义是「音节以 `zh` 开头」，放宽成「以 `z` 开头」**只有对应组开着时才成立**。
///
/// 不能改用 [`initials_fuzzy_equal`]：`zh` 与 `z` 首字母相同，它恒返回 true，`zh_z` 关着
/// 也会放行。`head` 传的是 z/c/s 那一位。判据两端对称，不依赖组表把长的那端写在哪边。
pub fn retroflex_relaxed(head: char, config: &FuzzyConfig) -> bool {
    INITIAL_GROUPS.iter().any(|g| {
        (g.flag)(config)
            // 一端是双字母、另一端是单字母，且两端都以 head 开头 ⇒ 正是 zh/z 这类组。
            && g.a.starts_with(head)
            && g.b.starts_with(head)
            && g.a.len() != g.b.len()
    })
}

/// 简拼键的模糊变体，附带该变体改动了**几位**（**含原键自身，处数 0，且恒在首位**）。
///
/// 处数随键一起返回，由构造侧统一保证各召回点的罚分口径。口径逐位计，与校验侧
/// `AbbrevSeg::Initial` 段计 1 处一致。变体 = 各位 [`initial_alternatives`] 的笛卡尔积。
///
/// 超过 [`MAX_ABBREV_KEY_VARIANTS`] 时只返回原键，不做部分展开（截一半会让召回时有时无）。
/// 规模检查必须边乘边判、用 `checked_mul`：先 `product()` 在 release 下会 wrapping 绕回小值、
/// 通过上限检查后真去铺指数个变体。
pub fn fuzzy_abbrev_keys(key: &str, config: &FuzzyConfig) -> Vec<(String, usize)> {
    // `any_enabled()` 只是早退优化：全关时下面同样返回原键。
    if key.is_empty() || !config.any_enabled() {
        return vec![(key.to_string(), 0)];
    }
    let per_pos: Vec<Vec<char>> = key
        .chars()
        .map(|c| initial_alternatives(c, config))
        .collect();
    // 边乘边判：溢出与超限同样处置（只查原键）。
    let mut total: usize = 1;
    for opts in &per_pos {
        match total.checked_mul(opts.len()) {
            Some(t) if t <= MAX_ABBREV_KEY_VARIANTS => total = t,
            _ => return vec![(key.to_string(), 0)],
        }
    }
    if total <= 1 {
        return vec![(key.to_string(), 0)];
    }
    // 原键恒在首位，处数 0：各位的 alternatives[0] 就是原字母，笛卡尔积的第一项即原键。
    let mut out: Vec<(String, usize)> = vec![(String::new(), 0)];
    for opts in &per_pos {
        let mut next = Vec::with_capacity(out.len() * opts.len());
        for (prefix, edits) in &out {
            for (j, c) in opts.iter().enumerate() {
                let mut s = prefix.clone();
                s.push(*c);
                // `opts[0]` 恒是原字母（见 `initial_alternatives`），故 j>0 即这一位改了一处。
                next.push((s, edits + usize::from(j > 0)));
            }
        }
        out = next;
    }
    out
}

/// [`fuzzy_abbrev_keys`] 的变体数上限。
///
/// 4 位键里只有全 `l`（n_l + r_l 都开，`3^4` = 81）超限，降级为只查原键。算例与未做的
/// 「按改动位数分层」改进见 candidate-sorting-rules.md「拼音常量的取值依据」。
pub const MAX_ABBREV_KEY_VARIANTS: usize = 64;

/// 韵母模糊组。**整体相等**比较，不是子串查找：`jiang` 的韵母是 `iang`，它不该因为
/// 「串里含 `ang`」就被 `an_ang` 组改成 `jian`（那是 `ian_iang` 组的事，用户可能根本没开）。
const FINAL_GROUPS: &[FuzzyGroup] = &[
    FuzzyGroup {
        a: "an",
        b: "ang",
        flag: |c| c.an_ang,
    },
    FuzzyGroup {
        a: "en",
        b: "eng",
        flag: |c| c.en_eng,
    },
    FuzzyGroup {
        a: "in",
        b: "ing",
        flag: |c| c.in_ing,
    },
    FuzzyGroup {
        a: "ian",
        b: "iang",
        flag: |c| c.ian_iang,
    },
    FuzzyGroup {
        a: "uan",
        b: "uang",
        flag: |c| c.uan_uang,
    },
    // `ong` 同属下面两组，两组都开时有两个对端，但不传递（`dun` 不会经 `ong` 走到 `deng`）。
    // 按拼写字面配对：`jun` 的对端 `jong` 是非音节、被过滤，不映射到 `jiong`；`yun↔yong`
    // 能配上只因 `y` 按声母切后字面对上。这个不对称是刻意接受的（同手机输入法「un=ong」）。
    FuzzyGroup {
        a: "un",
        b: "ong",
        flag: |c| c.un_ong,
    },
    FuzzyGroup {
        a: "eng",
        b: "ong",
        flag: |c| c.eng_ong,
    },
];

/// 声母表，**按长度降序**（`zh`/`ch`/`sh` 必须先于 `z`/`c`/`s`，否则 `sheng` 会被切成
/// `s` + `heng`）。含 `y`/`w`：不把它们当声母，`yin` 的韵母就成了整串 `yin`，与 `in` 不等，
/// `yin`→`ying` 会失效。
const INITIALS: &[&str] = &[
    "zh", "ch", "sh", "b", "p", "m", "f", "d", "t", "n", "l", "g", "k", "h", "j", "q", "x", "r",
    "z", "c", "s", "y", "w",
];

/// 合法音节集合，用于剔除模糊组合产出的非法码（`juan`+uan_uang→`juang`、`yuan`→`yuang`）。
/// 只在**输入本身是合法音节**时启用过滤，见 [`FuzzyMatcher::fuzzy_variants_scored`]。
pub(crate) static VALID_SYLLABLES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    super::syllable::STANDARD_SYLLABLES
        .iter()
        .copied()
        .collect()
});

/// 拆成（声母, 韵母）。零声母音节（`an`/`en`/`er`…）返回 `("", 整串)`；
/// 裸声母（`s`/`sh`/`zh` 这类半成品码）返回 `(整串, "")` —— 后者必须支持，
/// 单音节查询路径（`mod.rs` 的 `lookup_with_fuzzy`）会拿未成音节的 code 直接进来。
fn split_initial_final(syllable: &str) -> (&str, &str) {
    for init in INITIALS {
        if let Some(rest) = syllable.strip_prefix(init) {
            return (&syllable[..init.len()], rest);
        }
    }
    ("", syllable)
}

/// 取某一部分（声母或韵母）的全部取值：原值恒在 `[0]`，其后是各组给出的对端。
/// `l` 可同时属于 `n_l` 与 `r_l`，故可能有多个对端。
fn part_options<'a>(part: &'a str, groups: &[FuzzyGroup], config: &FuzzyConfig) -> Vec<&'a str> {
    let mut opts = vec![part];
    for group in groups {
        if let Some(other) = group.counterpart(part, config)
            && !opts.contains(&other)
        {
            opts.push(other);
        }
    }
    opts
}

/// 枚举「用户会打、但本身不是合法音节」的模糊拼写全集（`tin`→ting），注册进切分层
/// （[`SyllableTrie::load_fuzzy_spellings`](super::syllable::SyllableTrie::load_fuzzy_spellings)）。
/// 模糊音在切分之后才逐音节展开，切不出 `tin` 就等于整条模糊链路没执行。相当于 librime
/// 拼写代数的最小等价物，见 engine-candidate-pipeline.md §4.1。
///
/// 边界：只收非法拼写（合法的本来就切得出）；不收零声母拼写（元音开头的边会改动合法输入的
/// 切分，`xiongdi` 多出 `xi|ong`）；只进切分层、不进 `is_syllable`/`is_prefix` 这类真值判据。
///
/// 集合闭合、至多数十条，在 `with_fuzzy` 时一次性构建，不进按键热路径。
pub fn fuzzy_spellings(config: &FuzzyConfig) -> Vec<String> {
    if !config.any_enabled() {
        return Vec::new();
    }
    let mut out = BTreeSet::new();
    for syllable in super::syllable::STANDARD_SYLLABLES {
        let (initial, final_) = split_initial_final(syllable);
        let initials = part_options(initial, INITIAL_GROUPS, config);
        let finals = part_options(final_, FINAL_GROUPS, config);
        for (i, init) in initials.iter().enumerate() {
            for (j, fin) in finals.iter().enumerate() {
                if i == 0 && j == 0 {
                    continue; // 音节自身
                }
                // 零声母的非法拼写不注册（见函数文档）。
                if init.is_empty() {
                    continue;
                }
                let variant = format!("{init}{fin}");
                if !VALID_SYLLABLES.contains(variant.as_str()) {
                    out.insert(variant);
                }
            }
        }
    }
    out.extend(config.correction.spellings().map(str::to_string));
    out.into_iter().collect()
}

/// 模糊拼音匹配器
pub struct FuzzyMatcher;

impl FuzzyMatcher {
    /// 生成**单个音节**的模糊变体，附带该变体改动了几处（声母 1 处 + 韵母 1 处 = 2）。
    ///
    /// 声母取值集与韵母取值集做笛卡尔积，否则缺同时改两处的变体（`sen` 出不来 `sheng`）。
    /// 处数是惩罚计量，逐处累加（对齐 librime `kFuzzySpellingPenalty` / libime `fuzzyCost`）。
    ///
    /// 双拼同键音节（[`FuzzyConfig::shared_key`]）并在这里：对端计 **0 处**，对端再经用户
    /// 模糊组得到的变体照常计处数；同一变体多条来源取处数最小者。
    ///
    /// 换位纠错（[`FuzzyConfig::correction`]）也并在这里：`input` 是错误拼写时追加其正确音节，
    /// 计 [`CORRECTION_EDITS`] 处；纠错得到的音节**不再**做模糊组展开（一个音节最多一处编辑来源）。
    pub fn fuzzy_variants_scored(input: &str, config: &FuzzyConfig) -> Vec<(String, usize)> {
        let mut variants = Self::group_variants_scored(input, config);
        let mut add = |variant: String, edits: usize| {
            if variant == input {
                return;
            }
            match variants.iter_mut().find(|(v, _)| *v == variant) {
                Some(slot) => slot.1 = slot.1.min(edits),
                None => variants.push((variant, edits)),
            }
        };
        if let Some(partners) = config.shared_key.get(input) {
            for partner in partners {
                add(partner.clone(), 0);
                for (v, e) in Self::group_variants_scored(partner, config) {
                    add(v, e);
                }
            }
        }
        for correct in config.correction.corrections_of(input) {
            add(correct.clone(), CORRECTION_EDITS);
        }
        variants
    }

    /// 只看用户模糊组的单音节变体（[`Self::fuzzy_variants_scored`] 的本体）。
    fn group_variants_scored(input: &str, config: &FuzzyConfig) -> Vec<(String, usize)> {
        let (initial, final_) = split_initial_final(input);
        let initials = part_options(initial, INITIAL_GROUPS, config);
        let finals = part_options(final_, FINAL_GROUPS, config);

        // 只有输入本身是合法音节时才校验产物合法性；裸声母/半成品码（`s`）保持宽松。
        let check_valid = VALID_SYLLABLES.contains(input);

        let mut variants = Vec::new();
        for (i, init) in initials.iter().enumerate() {
            for (j, fin) in finals.iter().enumerate() {
                if i == 0 && j == 0 {
                    continue; // 原音节自身
                }
                let variant = format!("{init}{fin}");
                if variant == input {
                    continue;
                }
                if check_valid && !VALID_SYLLABLES.contains(variant.as_str()) {
                    continue;
                }
                variants.push((variant, usize::from(i > 0) + usize::from(j > 0)));
            }
        }
        variants
    }

    /// 单音节的**拼写级**模糊变体（只看用户模糊组，不含双拼同键）：合法变体之外，再收
    /// 非法错音串（`ding`→`din`），但不收零声母的非法串（与 [`fuzzy_spellings`] 同口径）。
    ///
    /// 供「模糊音提示」判定用户敲的段是不是这个音节的模糊拼写；[`Self::fuzzy_variants`]
    /// 对合法音节会滤掉非法产物，`din` 这类真实误打因此判不出来。
    ///
    /// 不读 [`FuzzyConfig::shared_key`]：调用方传整份配置即可，无需先 `without_shared_key`。
    pub(crate) fn fuzzy_spelling_variants(input: &str, config: &FuzzyConfig) -> Vec<String> {
        let (initial, final_) = split_initial_final(input);
        let initials = part_options(initial, INITIAL_GROUPS, config);
        let finals = part_options(final_, FINAL_GROUPS, config);
        let mut out = Vec::new();
        for (i, init) in initials.iter().enumerate() {
            for (j, fin) in finals.iter().enumerate() {
                if i == 0 && j == 0 {
                    continue;
                }
                let variant = format!("{init}{fin}");
                if variant == input
                    || (init.is_empty() && !VALID_SYLLABLES.contains(variant.as_str()))
                {
                    continue;
                }
                out.push(variant);
            }
        }
        out
    }

    /// [`Self::fuzzy_variants_scored`] 的丢弃计数版本。
    pub fn fuzzy_variants(input: &str, config: &FuzzyConfig) -> Vec<String> {
        Self::fuzzy_variants_scored(input, config)
            .into_iter()
            .map(|(v, _)| v)
            .collect()
    }

    /// 检查两个拼音是否模糊等价
    pub fn is_fuzzy_equal(a: &str, b: &str, config: &FuzzyConfig) -> bool {
        if a == b {
            return true;
        }

        let variants = Self::fuzzy_variants(a, config);
        variants.contains(&b.to_string())
    }
}

#[cfg(test)]
#[path = "fuzzy_tests.rs"]
mod tests;
