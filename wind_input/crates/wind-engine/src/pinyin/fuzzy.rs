//! 模糊音配置与匹配
//!
//! 与 Go 版本 `wind_input/internal/engine/pinyin/fuzzy.go` 对齐。
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
    /// **不是用户配置**：由 `PinyinEngine::with_shuangpin` 从布局推出
    /// （`ShuangpinConverter::shared_key_syllables`），全拼方案恒空。双拼转换器对一个键对
    /// 只交出一条全拼，其余读音靠这里在拼音引擎里展开回来。
    ///
    /// 展开时**零罚分**（处数计 0）：用户自己把两个韵母配在同一个键上，两种读音按词频平等
    /// 竞争，不是「打错了」。与用户开的模糊组叠加时，后者照旧计处数。
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
    /// 同键音节必须算在内：否则用户没开任何模糊音时，各调用点的早退会把它整条跳过。
    /// 只改声母首字母的路径（简拼键）对它是恒等变换，多算进来只多一次空展开。
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
            || self.un_ong
            || self.eng_ong
            || !self.shared_key.is_empty()
            || !self.correction.is_empty()
    }
}

/// 模糊音组（**无序对**：两个方向都成立，故只登记一次）。
///
/// 此前写成 `from`/`to` 的单向规则表、正反两条各列一行，两个方向都用子串试探
/// （声母 `starts_with`、韵母 `find`）匹配 —— 于是同一个音节会被表里**多条**规则重复命中：
/// `sheng` 既被 `sh→s` 命中得 `seng`、又被 `s→sh` 命中得 `shheng`；`en→eng` 还能在
/// `sheng` 中间 `find` 到 `en` 得 `shengg`。这些非法码查不到词、只白白撑大多音节展开的
/// 组合预算。改为「按组取对端」后天然只匹配一次。
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
/// [`INITIAL_GROUPS`] 的条目数。**导出仅供测试**：那边按 `2^N` 枚举全部标志位组合，
/// 组数一旦变化而测试没跟上，新组就是零覆盖且全绿。断言它，改了就红。
pub const INITIAL_GROUP_COUNT: usize = 6;

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
/// ## 为什么单独有这个函数
///
/// 简拼索引的键是各音节**首字母**拼成的串，所以只有**改首字母**的声母组会影响召回：
/// `n↔l`、`f↔h`、`r↔l`。而 `sh↔s` / `zh↔z` / `ch↔c` 模糊前后首字母相同（s/z/c），
/// 对本函数是恒等变换 —— **它们之所以一直走得通，正是因为这一点**，不是因为简拼路径
/// 支持模糊音。用 [`FuzzyMatcher::fuzzy_variants_scored`] 代替不了它：那个要完整音节，
/// 而这里只有一个首字母，`split_initial_final("n")` 得到的是裸声母、韵母为空。
///
/// ⚠️ `l` **同时属于** `n↔l` 与 `r↔l` 两组，两组都开时等价集是 `{l, n, r}`（3 个）。
/// 这是唯一一个等价集大于 2 的字母，键变体数的上界由它决定。
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
/// 段校验跑在 `keys × abbr_code × hit` 的三重循环里，每条候选的每个声母段都要问一次；
/// 走 `initial_alternatives(..).contains(..)` 等于在最内层为最多 3 个 char 反复分配 Vec。
/// 判据与它**逐位一致**，改一处必须改另一处 —— 两者的一致性由
/// `initial_alternatives_matches_pairwise_equal` 守住。
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
/// ⚠️ 不能改用 [`initials_fuzzy_equal`]：那个比的是**首字母**，而 `zh` 与 `z` 的首字母
/// 相同 ⇒ 它会走 `a == b` 的短路恒返回 true，于是 `zh_z` 关着也放行 `ze`，
/// 双字母声母就白设了。`head` 传的是 z/c/s 那一位。
///
/// ⚠️ 判据**两端对称**，不依赖组表把长的那端写在 `a` 还是 `b`（本组是无序对，见
/// [`FuzzyGroup`]）。只问 `b` 的写法能work是因为今天三组都写成 `a="zh", b="z"`；
/// 哪天有人写反，该函数会静默返回 false、开关对本段悄悄失效，而
/// [`initials_fuzzy_equal`] 与 [`initial_alternatives`] 都是对称写的、不会跟着红。
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
/// **处数随键一起返回**而不是让调用方事后逐位比较：它在笛卡尔积构造期是免费的
/// （每选一个非原字母就 +1），五个召回点的罚分口径因此由构造侧统一保证、不会各自漂移。
/// 与本模块 [`FuzzyMatcher::fuzzy_variants_scored`] 同族形态。
///
/// 处数口径 = **逐位计**，与校验侧 `AbbrevSeg::Initial` 段计 1 处一致：简拼键的每一位
/// 就是一个 `Initial` 段。`l` 的等价集 `{l,n,r}` 那种多来源不影响计量 —— 逐位比较不问
/// 「经由哪一组」，一位改了就是一处。
///
/// 键是各音节首字母的拼接，故变体 = 各位 [`initial_alternatives`] 的笛卡尔积。
/// 变体数是各位等价集大小之积，最坏 `3^len`（全 `l` 且 n_l + r_l 都开）。
///
/// ⚠️ **超过 [`MAX_ABBREV_KEY_VARIANTS`] 时只返回原键**，不做部分展开 —— 截一半等于
/// 按字母表顺序随机挑几个变体放行，用户看到的是「有时出得来有时出不来」，比一致地
/// 出不来更难排查。
///
/// ⚠️ **规模检查必须边乘边判、用 `checked_mul`**，不能先 `product()` 再比：
/// `product()` 在 debug 下会 panic（实测 41 个 `l` + n_l/r_l ⇒ `3^41` 溢出 usize），
/// 而 **release 下更糟** —— 乘法 wrapping 后 `total` 可能绕回一个小值、通过上限检查，
/// 于是真的去铺 `3^41` 个变体。溢出与超限在这里是同一种处置（只查原键），
/// 所以一个 `match` 就够，不必分开。
pub fn fuzzy_abbrev_keys(key: &str, config: &FuzzyConfig) -> Vec<(String, usize)> {
    // `any_enabled()` 这道是**早退优化、不是语义保证**：全关时 `initial_alternatives`
    // 每位也只返回原字母 ⇒ `total == 1` ⇒ 下面那条同样返回原键。删掉它行为不变，
    // 省下的是 `per_pos` 的分配。改这里时别把它当成正确性依赖。
    if key.is_empty() || !config.any_enabled() {
        return vec![(key.to_string(), 0)];
    }
    let per_pos: Vec<Vec<char>> = key
        .chars()
        .map(|c| initial_alternatives(c, config))
        .collect();
    // 边乘边判：溢出与超限同样处置（见上方 ⚠️），且超限时不必把剩下的位乘完。
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
/// 真机简拼键长度就是音节数，常见 2~4。**各位的等价集大小不一样**，所以位数不能直接
/// 换算成变体数 —— `n`/`f`/`h`/`r` 是 2 选，只有 `l` 是 3 选（同属 `n↔l` 与 `r↔l`）：
///
/// | 键 | 开启的组 | 变体数 | 64 之内？ |
/// |---|---|---|---|
/// | `nqc`  | n_l | 2 | ✔ |
/// | `nnnn` | n_l | `2^4` = 16 | ✔ |
/// | `lllh` | n_l + r_l + f_h | `3³ × 2` = 54 | ✔ |
/// | `llll` | n_l + r_l | `3^4` = 81 | ✘ 降级为只查原键 |
///
/// ⚠️ 本 doc 此前写的是「4 位全落在 n/l/f/h/r 上且都开组才 16 个变体，仍在限内」——
/// 那个算例**只对 2 选位成立**，`llll` 实际是 81。按错的上界做判断会以为全 `l` 的键
/// 也在覆盖内，而它恰恰是唯一落在覆盖外的形状。
///
/// ## 超限的处置：整体放弃，且这与同模块的音节变体**不一致**
///
/// 超限时只返回原键，不做部分展开 —— 截一半等于按字母表顺序随机挑几个变体放行，
/// 用户看到的是「有时出得来有时出不来」，比一致地出不来更难排查。
///
/// 但多音节的全拼展开（`LatticeBuilder::fuzzy_spans`）面对同类问题用的是**按改动处数截断**
/// （每层只留改动最少的那批）而非整体放弃。类似的办法这里也适用：按「改动了几位」
/// 分层递增枚举，某层放不下就**整层丢弃并停止**，规则变成一句用户能理解的话
/// 「改一个音能出、改两个音出不来」，而层内截断又会退回不可预期。
/// 未做 —— 它是召回面的扩张，不该与罚分口径的修复混在一笔里。
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
    // `ong` 同属下面两组（与声母 `l` 同属 n↔l、r↔l 同形），两组都开时有两个对端。
    // **不传递**：[`part_options`] 只取与原韵母直接成对的对端，`dun` 不会经 `ong` 走到
    // `deng`。**按拼写字面配对**：`jun`/`qun`/`xun` 的韵母按字面切成 `un`，对端 `jong` 等非音节由
    // `VALID_SYLLABLES` 过滤，不映射到 `jiong` 系（iong 韵）；`yun↔yong` 能配上只是 `y` 按声母
    // 切后字面恰好对上——从发音看它与 jun↔jiong 同类，这个不对称是刻意接受的（与手机输入法
    // 「un=ong」的字面语义一致）。反向注册的 `jong`/`qong`/`xong` 拼写因此落到军/群/寻。
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

/// 枚举「用户会打、但本身不是合法音节」的模糊拼写全集，供切分层注册。
///
/// ## 为什么需要它
///
/// 模糊音是在**切分之后**逐音节展开的（`LatticeBuilder::fuzzy_spans` 沿音节图的边逐条
/// 展开）。而切分器 [`SyllableTrie`](super::syllable::SyllableTrie) 只认
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
/// ## 三个刻意的边界
///
/// - **只收非法拼写**：产物本身已是合法音节的（`jin`→`jing`、`si`→`shi`）不注册，
///   它们本来就切得出来，重复注册只会让 `is_end` 多一份无谓开销。
/// - **不收零声母拼写**（`eng`→`ong`）：以元音开头的边能接在任何音节后面，会改动
///   合法输入的切分（`xiongdi` 多出 `xi|ong|di`）。
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
                // 零声母的非法拼写不注册：它以元音开头，能接在任何以辅音收尾的前一音节
                // 后面，`eng`→`ong` 会在 `xi|ong`、`ji|ong` 处凭空多出切分（而没有人用
                // `ong` 去打「鞥」）。今天只有这一条落在这里。
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
    /// 声母取值集与韵母取值集做**笛卡尔积**，而非各改一处后并列列出 —— 后者是
    /// 「`senxiao` 打不出生肖」的根因：`sen` 只产出 `shen`（改声母）与 `seng`（改韵母），
    /// 独独缺同时改两处的 `sheng`。凡「声母组 + 韵母组同时开启」的交叉场景全部受影响：
    /// `zen`→`zheng`、`cen`→`cheng`、`san`→`shang`…
    ///
    /// 返回值里的处数是**惩罚计量**：`sen`→`sheng` 两处都不同，置信度本就该低于只错一处的
    /// `sen`→`shen`，对齐 librime `kFuzzySpellingPenalty` / libime `fuzzyCost` 的逐处累加。
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
