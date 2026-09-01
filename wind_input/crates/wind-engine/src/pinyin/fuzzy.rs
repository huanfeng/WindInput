//! 模糊音配置与匹配
//!
//! 与 Go 版本 `wind_input/internal/engine/pinyin/fuzzy.go` 对齐。
//! 允许用户输入时忽略常见发音混淆（如 z/zh, c/ch, s/sh, n/l）。

use std::collections::{BTreeSet, HashSet};
use std::sync::LazyLock;

/// 笛卡尔积展开的组合数上限（超出即降级，见 [`FuzzyMatcher::expand_syllables`]）。
pub const MAX_FUZZY_COMBOS: usize = 64;

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
}

impl FuzzyConfig {
    /// 是否有任一模糊组开启。供调用方在全关时跳过整段展开逻辑（含其前置的切分求解）。
    ///
    /// 注意**不能**用「对整串求 `fuzzy_variants` 得空」来代替这个判断：那既漏掉非首音节的
    /// 变体，又要求先拿到 code；本判断只看配置，可在任何前置计算之前短路。
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
    }
}

/// 模糊音组（**无序对**：两个方向都成立，故只登记一次）。
///
/// 此前写成 `from`/`to` 的单向规则表、正反两条各列一行，两个方向都用子串试探
/// （声母 `starts_with`、韵母 `find`）匹配 —— 于是同一个音节会被表里**多条**规则重复命中：
/// `sheng` 既被 `sh→s` 命中得 `seng`、又被 `s→sh` 命中得 `shheng`；`en→eng` 还能在
/// `sheng` 中间 `find` 到 `en` 得 `shengg`。这些非法码查不到词、只白白撑大
/// [`MAX_FUZZY_COMBOS`] 的组合预算。改为「按组取对端」后天然只匹配一次。
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
static VALID_SYLLABLES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
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

/// 数「最多 `limit` 个音节被模糊」时的组合总数，用于 [`FuzzyMatcher::expand_syllables`]
/// 选降级档位。`dp[j]` = 已处理音节中恰有 `j` 个被模糊的组合数。
fn combo_count(per_syllable: &[Vec<(String, usize)>], limit: usize) -> usize {
    let mut dp = vec![0usize; limit + 1];
    dp[0] = 1;
    for opts in per_syllable {
        let alts = opts.len() - 1; // 除原音节外的变体数
        let mut next = vec![0usize; limit + 1];
        for j in 0..=limit {
            if dp[j] == 0 {
                continue;
            }
            next[j] = next[j].saturating_add(dp[j]); // 该音节取原值
            if j < limit {
                next[j + 1] = next[j + 1].saturating_add(dp[j].saturating_mul(alts));
            }
        }
        dp = next;
    }
    dp.iter().fold(0usize, |acc, n| acc.saturating_add(*n))
}

/// 枚举「用户会打、但本身不是合法音节」的模糊拼写全集，供切分层注册。
///
/// ## 为什么需要它
///
/// 模糊音是在**切分之后**逐音节展开的（[`FuzzyMatcher::expand_syllables`] 的入参就是
/// 已切好的音节数组）。而切分器 [`SyllableTrie`](super::syllable::SyllableTrie) 只认
/// 标准音节表，于是 `tin`（想打 `ting`）这类**本身不成音节**的串在图上根本没有边：
/// `tinzhi` 的 DAG 最远只走到 `ti`，`nzhi` 全成残码，模糊展开一次都轮不到执行。
/// 现象是「明明开了 in-ing，`tinzhi` 却打不出「停止」」——不是排序靠后，是候选里压根没有。
///
/// 只在**打的那端不成音节**时失效，故 `jinzhi`→`jingzhi`（`jin` 合法）一直正常，
/// `an_ang` 组更是一条都不缺（an/ang 系列两侧齐全）——这正是它长期没被发现的原因。
/// 真实高频误打里受影响的有 `zuang`→zhuang、`suang`→shuang、`cuang`→chuang、
/// `fui`→hui、`fuan`→huan、`tin`→ting、`din`→ding 等。
///
/// ## 做法
///
/// 把这些拼写注册进切分层（[`SyllableTrie::load_fuzzy_spellings`](
/// super::syllable::SyllableTrie::load_fuzzy_spellings)），让错音串成为一条**可切的边**。
/// 这是 librime `speller/algebra` 拼写代数的最小等价物：rime 把 `tin` 直接注册成 `ting`
/// 的一种合法拼写，切分器天然认识它。我们只补「可切」这一步，其余（变体展开、`0.5^k`
/// 惩罚、排序）沿用既有链路 —— 切出 `tin` 后 [`FuzzyMatcher::fuzzy_variants_scored`]
/// 因 `check_valid` 为假而宽松放行，照常产出 `ting` 并计 1 处改动，下游无需任何改动。
///
/// ## 两个刻意的边界
///
/// - **只收非法拼写**：产物本身已是合法音节的（`jin`→`jing`、`si`→`shi`）不注册，
///   它们本来就切得出来，重复注册只会让 `is_end` 多一份无谓开销。
/// - **只进切分层，不进 `is_syllable`/`is_prefix`**：后两者是**真值判据**，被双拼真值
///   校验（[`shuangpin`](super::shuangpin)）、造词边界推导（[`generate`](super::generate)）
///   等复用，让 `tin` 变成「合法音节」会污染它们。
///
/// 集合**闭合可枚举**（标准音节表固定、模糊组固定），按开启的组过滤后至多数十条，
/// 故在 `with_fuzzy` 时一次性构建即可，不进按键热路径。
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
                let variant = format!("{init}{fin}");
                if !VALID_SYLLABLES.contains(variant.as_str()) {
                    out.insert(variant);
                }
            }
        }
    }
    out.into_iter().collect()
}

/// 模糊拼音匹配器
pub struct FuzzyMatcher;

impl FuzzyMatcher {
    /// 生成**单个音节**的模糊变体，附带该变体改动了几处（声母 1 处 + 韵母 1 处 = 2）。
    ///
    /// 声母取值集与韵母取值集做**笛卡尔积**，而非各改一处后并列列出 —— 后者是
    /// 「`senxiao` 打不出生肖」的根因：`sen` 只产出 `shen`（改声母）与 `seng`（改韵母），
    /// 独独缺同时改两处的 `sheng`。凡「声母组 + 韵母组同时开启」的交叉场景全部受影响：
    /// `zen`→`zheng`、`cen`→`cheng`、`san`→`shang`…
    ///
    /// 返回值里的处数是**惩罚计量**：`sen`→`sheng` 两处都不同，置信度本就该低于只错一处的
    /// `sen`→`shen`，对齐 librime `kFuzzySpellingPenalty` / libime `fuzzyCost` 的逐处累加。
    pub fn fuzzy_variants_scored(input: &str, config: &FuzzyConfig) -> Vec<(String, usize)> {
        let (initial, final_) = split_initial_final(input);
        let initials = part_options(initial, INITIAL_GROUPS, config);
        let finals = part_options(final_, FINAL_GROUPS, config);

        // 只有输入本身是合法音节时才校验产物合法性。裸声母/半成品码（`s`、`sh`）进来时
        // 无从谈论「合法音节」，此时保持宽松，否则 `s`→`sh` 这类既有召回会被误杀。
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

    /// [`Self::fuzzy_variants_scored`] 的丢弃计数版本。
    pub fn fuzzy_variants(input: &str, config: &FuzzyConfig) -> Vec<String> {
        Self::fuzzy_variants_scored(input, config)
            .into_iter()
            .map(|(v, _)| v)
            .collect()
    }

    /// 逐音节展开模糊变体的笛卡尔积，拼成完整 code 列表（**含全原音节的原码本身**）。
    ///
    /// **必须逐音节调用 [`Self::fuzzy_variants`]，不可对多音节拼接串整体调用**：声母规则用
    /// `input.starts_with(rule.from)`、韵母规则用 `input.find(rule.from)`，对整串只能改到
    /// **第一个音节的声母**与**第一处**韵母匹配。`zhongzou`→`zhongzhou`（中州）这类
    /// 非首音节模糊会整片丢失。切分信息在两个调用点都是现成的——`mod.rs` 有 DAG 的
    /// `syllables`，`lattice.rs` 有 `graph.any_path` 的 `offsets`（还紧接着用
    /// `slice_syllables` 切过一次）——本函数即为收口这两处而抽出。
    ///
    /// 组合数超 [`MAX_FUZZY_COMBOS`] 时**逐级降低「允许同时被模糊的音节数」**，而非整体
    /// 放弃。此前是超限即 `return Vec::new()`，于是长句里模糊音会**静默全失效**——用户感受
    /// 是「短词模糊音好使、长句就不灵」。降级后至少保留「只错一两个音节」的召回，那也正是
    /// 模糊音的主场景（真按错五六个音节的输入，本就该由整句纠错而非模糊音兜）。
    ///
    /// 返回 `(变体码, 模糊改动处数)`。**第二项是惩罚的计量单位**：librime 的
    /// `kFuzzySpellingPenalty` 与 libime 的 `fuzzyCost` 都是「每个模糊拼写 log(0.5)」并
    /// 逐个累加，即概率域按改动处数**累乘 0.5**。我们此前两处惩罚（词图 −0.5、候选层
    /// ×0.01）都是**一次性固定值**，`beijinsi`（2 处模糊）与 `si`（1 处）同等对待。
    pub fn expand_syllables(syllables: &[String], config: &FuzzyConfig) -> Vec<(String, usize)> {
        let per_syllable: Vec<Vec<(String, usize)>> = syllables
            .iter()
            .map(|s| {
                // opts[0] 恒为原音节（0 处改动），故「下标 > 0」即「该音节被模糊了」。
                let mut opts = vec![(s.clone(), 0usize)];
                opts.extend(Self::fuzzy_variants_scored(s, config));
                opts
            })
            .collect();

        // 允许同时被模糊的音节数上限：从「全部可模糊」往下降，取第一个不超预算的。
        let mut limit = per_syllable.len();
        while limit > 0 && combo_count(&per_syllable, limit) > MAX_FUZZY_COMBOS {
            limit -= 1;
        }

        // 元素为 (码, 累计改动处数, 已模糊的音节数)——后者仅用于按 `limit` 剪枝。
        let mut codes: Vec<(String, usize, usize)> = vec![(String::new(), 0, 0)];
        for opts in &per_syllable {
            let mut next: Vec<(String, usize, usize)> = Vec::with_capacity(codes.len());
            for (prefix, edits, fuzzy_syls) in &codes {
                for (i, (opt, opt_edits)) in opts.iter().enumerate() {
                    let fuzzy_syls = fuzzy_syls + usize::from(i > 0);
                    if fuzzy_syls > limit {
                        continue;
                    }
                    next.push((format!("{prefix}{opt}"), edits + opt_edits, fuzzy_syls));
                }
            }
            codes = next;
        }
        codes.into_iter().map(|(c, edits, _)| (c, edits)).collect()
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
mod tests {
    use super::*;

    fn cfg(f: impl FnOnce(&mut FuzzyConfig)) -> FuzzyConfig {
        let mut c = FuzzyConfig::default();
        f(&mut c);
        c
    }

    fn syls(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // ---------------------------------------------------------------- any_enabled

    #[test]
    fn any_enabled_reflects_each_group() {
        assert!(!FuzzyConfig::default().any_enabled(), "默认全关");
        assert!(cfg(|c| c.zh_z = true).any_enabled());
        assert!(cfg(|c| c.uan_uang = true).any_enabled(), "末位组也须被算上");
    }

    // ---------------------------------------------------------------- fuzzy_spellings

    #[test]
    fn fuzzy_spellings_empty_when_all_disabled() {
        assert!(fuzzy_spellings(&FuzzyConfig::default()).is_empty());
    }

    /// 只收**不是合法音节**的拼写：`jin`→`jing` 那侧本就切得出来，不必注册。
    #[test]
    fn fuzzy_spellings_only_collects_invalid_ones() {
        let out = fuzzy_spellings(&cfg(|c| c.in_ing = true));
        assert!(
            out.contains(&"tin".to_string()),
            "tin 须注册，实际: {out:?}"
        );
        assert!(
            out.contains(&"din".to_string()),
            "din 须注册，实际: {out:?}"
        );
        assert!(
            !out.iter().any(|s| VALID_SYLLABLES.contains(s.as_str())),
            "合法音节不该出现在模糊拼写表里，实际: {out:?}"
        );
        // in_ing 组在标准音节表上的缺口恰好只有 t/d 两个声母。
        assert_eq!(out, vec!["din".to_string(), "tin".to_string()]);
    }

    /// 声母组同样有缺口，且比韵母组更常见：平翘舌不分的人打「装置」正是 `zuangzhi`。
    #[test]
    fn fuzzy_spellings_covers_initial_groups() {
        let out = fuzzy_spellings(&cfg(|c| c.zh_z = true));
        assert_eq!(
            out,
            vec!["zua".to_string(), "zuai".to_string(), "zuang".to_string()],
            "zh_z 组的缺口"
        );
    }

    /// 只登记开启了的组 —— 关掉的组一条都不该混进来。
    #[test]
    fn fuzzy_spellings_respects_enabled_groups() {
        let out = fuzzy_spellings(&cfg(|c| c.in_ing = true));
        assert!(
            !out.contains(&"zuang".to_string()),
            "没开 zh_z 就不该有 zuang，实际: {out:?}"
        );
    }

    // ---------------------------------------------------------------- expand_syllables

    /// 测试辅助：只取变体码，丢掉模糊音节数。
    fn codes(out: &[(String, usize)]) -> Vec<String> {
        out.iter().map(|(c, _)| c.clone()).collect()
    }

    /// 测试辅助：查某个变体码对应的模糊音节数。
    fn fuzzy_count_of(out: &[(String, usize)], code: &str) -> Option<usize> {
        out.iter().find(|(c, _)| c == code).map(|(_, k)| *k)
    }

    /// 全原音节组合恒排第一、且模糊音节数为 0（调用方据 `variant == code` 跳过精确命中，
    /// 依赖此性质）。
    #[test]
    fn expand_first_combo_is_original_code() {
        let c = cfg(|c| {
            c.zh_z = true;
            c.sh_s = true;
        });
        let out = FuzzyMatcher::expand_syllables(&syls(&["zhong", "guo"]), &c);
        assert_eq!(out[0].0, "zhongguo", "首个组合须是原码，实际: {out:?}");
        assert_eq!(out[0].1, 0, "原码的模糊音节数须为 0");
    }

    #[test]
    fn expand_disabled_yields_only_original() {
        let out = FuzzyMatcher::expand_syllables(&syls(&["si", "jin"]), &FuzzyConfig::default());
        assert_eq!(
            out,
            vec![("sijin".to_string(), 0)],
            "全关时只应有原码，且计数为 0"
        );
    }

    #[test]
    fn expand_single_syllable_degrades_to_variants() {
        let out = FuzzyMatcher::expand_syllables(&syls(&["si"]), &cfg(|c| c.sh_s = true));
        assert!(codes(&out).contains(&"si".to_string()));
        assert_eq!(
            fuzzy_count_of(&out, "shi"),
            Some(1),
            "单音节 s→sh 须计 1 个模糊音节，实际: {out:?}"
        );
    }

    /// **本次修复的核心**：非首音节的**声母**变体必须能生成。
    /// `zhongzou` → 「中州」需要第 2 音节 zou→zhou。
    #[test]
    fn expand_covers_non_initial_syllable_initial() {
        let out = FuzzyMatcher::expand_syllables(&syls(&["zhong", "zou"]), &cfg(|c| c.zh_z = true));
        assert_eq!(
            fuzzy_count_of(&out, "zhongzhou"),
            Some(1),
            "第 2 音节 zou→zhou 须被展开并计 1，实际: {out:?}"
        );
    }

    /// 非首音节的**韵母**变体：`beijin` → `beijing`（第 2 音节 in→ing）。
    #[test]
    fn expand_covers_non_initial_syllable_final() {
        let out = FuzzyMatcher::expand_syllables(&syls(&["bei", "jin"]), &cfg(|c| c.in_ing = true));
        assert_eq!(
            fuzzy_count_of(&out, "beijing"),
            Some(1),
            "第 2 音节 in→ing 须被展开并计 1，实际: {out:?}"
        );
    }

    /// **多处音节同时模糊**（笛卡尔积的意义）：`beijinsi` → `beijingshi`（北京市）
    /// 需要第 2 音节 in→ing **且** 第 3 音节 s→sh。
    ///
    /// ★ 计数须为 **2** —— 惩罚按模糊音节数累乘（`0.5^2`），对齐 librime
    /// `kFuzzySpellingPenalty` 与 libime `fuzzyCost` 的逐个累加。写成一次性固定折扣时，
    /// 本串与单音节模糊同等对待，置信度差异被抹平。
    #[test]
    fn expand_covers_multiple_syllables_at_once() {
        let c = cfg(|c| {
            c.in_ing = true;
            c.sh_s = true;
        });
        let out = FuzzyMatcher::expand_syllables(&syls(&["bei", "jin", "si"]), &c);
        assert_eq!(
            fuzzy_count_of(&out, "beijingshi"),
            Some(2),
            "第 2、3 音节同时变体须计 2，实际: {out:?}"
        );
        // 同一次展开里，只改一个音节的组合计 1 —— 与上面合看才说明计数真在数音节，
        // 而非「只要有模糊就置 1」或「恒等于音节总数」。
        assert_eq!(fuzzy_count_of(&out, "beijingsi"), Some(1));
        assert_eq!(fuzzy_count_of(&out, "beijinshi"), Some(1));
    }

    /// **回归守卫（钉死旧 bug）**：对多音节**拼接串**整体调 `fuzzy_variants` 拿不到
    /// 非首音节的声母变体——声母规则是 `starts_with`。谁把 `expand_syllables` 改回
    /// 整串调用，这条就会挂。
    #[test]
    fn whole_string_variants_miss_non_initial_initials() {
        let c = cfg(|c| {
            c.in_ing = true;
            c.sh_s = true;
        });
        let whole = FuzzyMatcher::fuzzy_variants("beijinsi", &c);
        assert!(
            !whole.contains(&"beijingshi".to_string()),
            "整串调用本就拿不到非首音节声母变体（已知限制），实际: {whole:?}"
        );

        // 而逐音节展开可以——两者的差集正是本次修复的价值。
        let per_syllable = FuzzyMatcher::expand_syllables(&syls(&["bei", "jin", "si"]), &c);
        assert!(codes(&per_syllable).contains(&"beijingshi".to_string()));
    }

    /// 组合数超上限时**降级**（限制同时被模糊的音节数），而非整体放弃。
    ///
    /// 旧行为是超限即返回空 —— 长句里模糊音会静默全失效，用户感受是「短词好使、长句不灵」。
    #[test]
    fn expand_degrades_instead_of_giving_up_beyond_combo_limit() {
        let c = cfg(|c| c.in_ing = true);
        // 每个 "jin" 有 2 个选项（jin / jing）→ 全展开 2^7 = 128 > MAX_FUZZY_COMBOS(64)
        let out = FuzzyMatcher::expand_syllables(&syls(&["jin"; 7]), &c);
        assert!(
            out.len() <= MAX_FUZZY_COMBOS,
            "降级后仍须守住预算: {}",
            out.len()
        );
        assert_eq!(out[0].0, "jinjinjinjinjinjinjin", "原码恒在首位");
        assert!(
            out.iter().any(|(_, k)| *k == 1),
            "至少保留「只错一个音节」的召回，而非整体放弃: {out:?}"
        );

        // 未超限时不降级：6 个 jin 全展开恰好 64 组合，7 个音节全模糊的组合此时应存在。
        let full = FuzzyMatcher::expand_syllables(&syls(&["jin"; 6]), &c);
        assert_eq!(full.len(), 64);
        assert!(
            full.iter().any(|(_, k)| *k == 6),
            "未超限时须保留全模糊组合"
        );
    }

    // ------------------------------------------------- 音节内声母 × 韵母的交叉组合

    /// **本次修复的核心**：同一音节里声母与韵母**同时**模糊。
    /// `senxiao` → 「生肖」需要 `sen` 一次性走完 s→sh **且** en→eng。
    ///
    /// 旧实现把声母规则与韵母规则**并列** push 进同一个 Vec，各自作用于原始输入，
    /// 只能产出「改一处」的结果：`sen` → [`shen`, `seng`]，独缺 `sheng`。
    #[test]
    fn variants_cross_initial_and_final_within_one_syllable() {
        let c = cfg(|c| {
            c.sh_s = true;
            c.en_eng = true;
        });
        let out = FuzzyMatcher::fuzzy_variants_scored("sen", &c);
        assert_eq!(
            out.iter().find(|(v, _)| v == "sheng").map(|(_, k)| *k),
            Some(2),
            "sen→sheng 须产出且计 2 处改动，实际: {out:?}"
        );
        // 单改一处的两个变体同时保留，且各计 1。
        assert_eq!(
            out.iter().find(|(v, _)| v == "shen").map(|(_, k)| *k),
            Some(1)
        );
        assert_eq!(
            out.iter().find(|(v, _)| v == "seng").map(|(_, k)| *k),
            Some(1)
        );

        // 端到端：整码 senxiao 须能展开出 shengxiao。
        let expanded = FuzzyMatcher::expand_syllables(&syls(&["sen", "xiao"]), &c);
        assert_eq!(
            expanded
                .iter()
                .find(|(v, _)| v == "shengxiao")
                .map(|(_, k)| *k),
            Some(2),
            "senxiao 须展开出 shengxiao，实际: {expanded:?}"
        );
    }

    /// 反向同样成立（词库侧/用户侧对称）：`sheng` → `sen`。
    #[test]
    fn variants_cross_is_symmetric() {
        let c = cfg(|c| {
            c.sh_s = true;
            c.en_eng = true;
        });
        let v = FuzzyMatcher::fuzzy_variants("sheng", &c);
        assert!(v.contains(&"sen".to_string()), "实际: {v:?}");
    }

    /// 声母表须按长度降序匹配：`sheng` 的声母是 `sh` 而非 `s`。
    /// 旧实现里 `sh→s` 与 `s→sh` 两条规则都对 `sheng` 的 `starts_with` 成立，
    /// 于是凭空造出 `shheng`；韵母侧 `find("en")` 在 `sheng` 中间命中，又造出 `shengg`。
    /// 这些非法码查不到词，只白白吃掉组合预算。
    #[test]
    fn variants_produce_no_garbage_codes() {
        let c = cfg(|c| {
            c.sh_s = true;
            c.en_eng = true;
        });
        let v = FuzzyMatcher::fuzzy_variants("sheng", &c);
        assert!(
            !v.contains(&"shheng".to_string()),
            "s→sh 不该再命中 sh 开头: {v:?}"
        );
        assert!(
            !v.contains(&"shengg".to_string()),
            "韵母须整体比较而非 find: {v:?}"
        );
    }

    /// 韵母整体相等比较，不做子串查找：只开 `an_ang` 时，`jiang`（韵母 iang）不该变成
    /// `jian` —— 那是 `ian_iang` 组的事，用户根本没开。`shuang`→`shuan` 同理。
    #[test]
    fn final_rules_do_not_leak_across_groups() {
        let c = cfg(|c| c.an_ang = true);
        assert!(
            FuzzyMatcher::fuzzy_variants("jiang", &c).is_empty(),
            "只开 an_ang 时 jiang 不该有变体"
        );
        assert!(
            FuzzyMatcher::fuzzy_variants("shuang", &c).is_empty(),
            "只开 an_ang 时 shuang 不该有变体"
        );
        // 而真正属于 an_ang 组的音节照常工作。
        assert_eq!(
            FuzzyMatcher::fuzzy_variants("san", &c),
            vec!["sang".to_string()]
        );
    }

    /// `y`/`w` 必须计入声母表：否则 `yin` 的韵母成了整串 `yin`、与 `in` 不等，
    /// `yin`→`ying` 会从既有行为退化（旧实现靠 `find("in")` 蒙对）。
    #[test]
    fn zero_consonant_initials_still_match_finals() {
        let c = cfg(|c| c.in_ing = true);
        assert!(FuzzyMatcher::fuzzy_variants("yin", &c).contains(&"ying".to_string()));
        assert!(FuzzyMatcher::fuzzy_variants("ying", &c).contains(&"yin".to_string()));
        // 零声母音节（韵母即整串）
        let c2 = cfg(|c| c.en_eng = true);
        assert!(FuzzyMatcher::fuzzy_variants("en", &c2).contains(&"eng".to_string()));
    }

    /// 组合产物须落在合法音节集内：`juan` + uan_uang 会算出 `juang`，不是音节，须剔除。
    #[test]
    fn variants_reject_invalid_syllables() {
        let c = cfg(|c| c.uan_uang = true);
        assert!(
            FuzzyMatcher::fuzzy_variants("juan", &c).is_empty(),
            "juang 不是合法音节，须被剔除"
        );
        // 合法的照常保留。
        assert!(FuzzyMatcher::fuzzy_variants("zhuan", &c).contains(&"zhuang".to_string()));
    }

    /// 裸声母/半成品码（`s`、`sh`）不是合法音节，此时**不**做合法性过滤 ——
    /// 单音节查询路径会拿未成音节的 code 直接进来，过滤会误杀既有召回。
    #[test]
    fn bare_initial_keeps_loose_matching() {
        let c = cfg(|c| c.sh_s = true);
        assert!(FuzzyMatcher::fuzzy_variants("s", &c).contains(&"sh".to_string()));
        assert!(FuzzyMatcher::fuzzy_variants("sh", &c).contains(&"s".to_string()));
    }

    #[test]
    fn expand_empty_input_is_safe() {
        let out = FuzzyMatcher::expand_syllables(&[], &cfg(|c| c.zh_z = true));
        assert_eq!(
            out,
            vec![(String::new(), 0)],
            "空音节列表只产出空串，交调用方跳过"
        );
    }
}
