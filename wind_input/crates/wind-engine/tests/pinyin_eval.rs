//! 拼音整句质量批量评测 harness（Phase 0）
//!
//! 这不是回归门禁，是测量工具：不做 `assert_eq!`，只产出可比对的数字。单一 ground truth
//! 带同音词噪声（`shiyan` 可能是 实验/试验/誓言），有用的是改动前后的 delta。
//!
//! 运行（默认被 `#[ignore]` 排除在 `cargo test` 之外，因为它慢且依赖 build_dev 真实数据）：
//!
//! ```text
//! cargo test -p wind-engine --test pinyin_eval -- --ignored --nocapture
//! ```
//!
//! 指定输出路径（默认 `target/pinyin_eval/latest.json`）：
//!
//! ```text
//! WIND_PINYIN_EVAL_OUT=target/pinyin_eval/baseline.json \
//!   cargo test -p wind-engine --test pinyin_eval -- --ignored --nocapture
//! ```
//!
//! 可调环境变量：
//! - `WIND_PINYIN_EVAL_OUT`   结果 JSON 路径
//! - `WIND_PINYIN_EVAL_N`     每类抽样数（默认 600）
//! - `WIND_PINYIN_EVAL_SEED`  随机种子（默认 20260721）
//! - `WIND_PINYIN_EVAL_DUMP`  每类导出的未命中明细条数（默认 40）
//! - `WIND_PINYIN_EVAL_FUZZY` 开模糊音：逗号分隔的组名（`in_ing,an_ang`）或 `all`
//! - `WIND_PINYIN_EVAL_CORRECTION` 换位纠错：不设 = 出厂；`off` = 关；否则为逗号分隔的规则 id
//!   （`gn_ng,on_ong`，`all` = 八条全开）
//! - `WIND_PINYIN_EVAL_PER_RULE` F/G 类每条规则抽样数（默认 300）
//! - `WIND_PINYIN_EVAL_LATENCY=1` 额外跑一遍逐键耗时（每条样本按前缀逐键 convert）
//! - `WIND_PINYIN_EVAL_MISREAD=1` 开正文提示与误读来源，另统计界面首页上的误读提示（打对的输入上
//!   出现的都算误标候选，逐条列出供人工判）；`=0` 关掉「误读」来源（召回与提示都没有），与 `=1` 对比
//!   即误读表对排序的影响
//! - `WIND_PINYIN_EVAL_SHUANGPIN=1` 只对 `pinyin_eval_shuangpin` 生效：A/B/C 抽样按真值音节转成
//!   小鹤击键，比较双拼击键坐标图开关关 / 开，写 `<out>.sp.json` 与 `<out>.sp.ranks.tsv`；
//!   不设时该测试直接跳过，`pinyin_eval_report` 不受影响
//!
//! 除 JSON 外还写 `<out>.ranks.tsv`：每条样本的引擎序 / 界面序位次与界面首页。两档之间逐位比对
//! 用它；与协调器对拍见 `wind-coordinator/tests/pinyin_eval_ui_parity.rs`（读同一份 TSV）。
//!
//! ## 样本分类判据
//!
//! 设某词条为 `词 \t 拼音(空格分隔) \t 权重`：
//! - `true_syls` = 拼音按空格切分 —— 音节边界真值（来自 rime 源数据）
//! - `input`     = 拼音去空格 —— 用户实际敲入的串
//! - `mm`        = `Dag::maximum_match(input)` —— 当前生产切分
//!
//! 入池前置条件（不满足者整条丢弃，不计入任何类别）：
//! 汉字数 == `true_syls.len()`、`mm` 完整覆盖 `input`、纯 CJK、2~8 字。
//! 于是「汉字数 > mm 音节数」等价于 `true_syls.len() > mm.len()`。
//!
//! | 类 | 判据 | 含义 |
//! |---|---|---|
//! | **A** 普通词 | `mm == true_syls` | 切分与真值一致，边界校验天然通过 |
//! | **B** 缩合音短词 | `mm != true_syls` 且 `mm.len() == 1` | 整词塌缩进单个音节边（李安/西安/企鹅/余额），在单音节边上与高频单字竞争 |
//! | **C** 多音节含缩合音 | `mm != true_syls` 且 `mm.len() >= 2` | 词跨多个 mm 音节，但内部某处边界与真值不符（西安交通大学：真值 6 音节 vs mm 5 音节） |
//!
//! B / C 互斥且穷尽。按 `mm.len()` 而非「是否含零声母字」二分：「占据单音节边」才是短词被误提升
//! 的机制本身，零声母只是常见成因。
//!
//! D / E 两类是用 A 池常用词构造的击键串：D = 简拼词 + 全拼词的长串（`bzd`+`haobuhao`），
//! E = 单个词的短简拼形态（`zhge` / `zhy` / `baichx`），见各自的构造函数。
//!
//! F / G 两类为换位纠错而设，用 A 池常用词按规则分组构造（与本轮开哪几条规则无关，各档样本集
//! 相同）：F = 打对了的击键串里恰含某条规则的错误拼写（`xign` 是「习惯」类简拼，也是 `gn_ng`
//! 的错拼），门禁要求它与关纠错时逐位相同；G = 换位错拼（`gognping` → 公平），看纠错命中率。
//!
//! ## 两套位次：引擎序与界面序
//!
//! 引擎序（主表）= `convert` 直接给出的顺序，limit 取 [`TOP_N`]。它不是用户看到的顺序：
//! 协调器还要以 `wind_candidate::candidate_display_order` 整体重排（消费长度为首键），
//! 只吃掉前缀的候选在引擎序里可以压住整串匹配，界面上恰好相反。
//!
//! 界面序（第二张表）= 以协调器的 limit（[`UI_LIMIT`]）取候选 → 同文纠错让位 → 同一个
//! `candidate_display_order` 排序 → 按文本去重（保留第一条）→ 纠错不占首选（D6）。协调器此后的
//! 英文定位、整句块等后处理对纯拼音出厂配置不改变顺序，与协调器对拍逐条一致。
//! 判断一个排序改动对用户的影响，看界面序。

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use wind_config::Config;
use wind_engine::pinyin::dag::Dag;
use wind_engine::pinyin::shuangpin::{Layout, ShuangpinConverter};
use wind_engine::pinyin::syllable::SyllableTrie;
use wind_engine::{ConvertOptions, EngineManager};

// ---------------------------------------------------------------- 基础设施

fn data_dir() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("build_dev")
        .join("data");
    p.join("schemas/pinyin/cn_dicts/base.dict.yaml")
        .exists()
        .then_some(p)
}

fn manager(dir: &Path) -> EngineManager {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".to_string()];
    cfg.schema.active = "pinyin".to_string();
    // 语法模型权重（标定用）。默认 0 = 不加载模型，各项指标应与「没有这功能」时逐位相同。
    // 标定：`WIND_PINYIN_EVAL_GRAM_WEIGHT=0.2 cargo test ... -- --ignored --nocapture`
    // 需要 build_dev/data/schemas/pinyin/grammar/ 下有 .gram，否则引擎降级为关闭、只在日志
    // 里 warn：指标没变时先确认模型真的加载了。
    if let Some(w) = std::env::var("WIND_PINYIN_EVAL_GRAM_WEIGHT")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
    {
        cfg.schema.pinyin.grammar.weight = w;
    }
    // 换模型：bgc（10MB 字级）/ bgw（41MB 词级）/ wanxiang-lts（420MB）的
    // 质量与开销差别都很大，标定时要能一键切换。
    if let Ok(m) = std::env::var("WIND_GRAM_MODEL") {
        cfg.schema.pinyin.grammar.model = m;
    }
    // 模糊音：逗号分隔的组名（`in_ing,an_ang`），`all` = 13 组全开。不设 = 出厂（全关）。
    if let Ok(groups) = std::env::var("WIND_PINYIN_EVAL_FUZZY") {
        let f = &mut cfg.schema.pinyin.fuzzy;
        f.enabled = true;
        for g in groups.split(',').map(str::trim) {
            let all = g == "all";
            let mut known = all;
            for (name, slot) in [
                ("zh_z", &mut f.zh_z),
                ("ch_c", &mut f.ch_c),
                ("sh_s", &mut f.sh_s),
                ("n_l", &mut f.n_l),
                ("f_h", &mut f.f_h),
                ("r_l", &mut f.r_l),
                ("an_ang", &mut f.an_ang),
                ("en_eng", &mut f.en_eng),
                ("in_ing", &mut f.in_ing),
                ("ian_iang", &mut f.ian_iang),
                ("uan_uang", &mut f.uan_uang),
                ("un_ong", &mut f.un_ong),
                ("eng_ong", &mut f.eng_ong),
            ] {
                if all || g == name {
                    *slot = true;
                    known = true;
                }
            }
            assert!(known, "WIND_PINYIN_EVAL_FUZZY 里有不认识的组名「{g}」");
        }
    }
    apply_correction_env(&mut cfg.input.correction);
    match std::env::var("WIND_PINYIN_EVAL_MISREAD").as_deref() {
        Ok("1") => {
            cfg.input.correction.hint.enabled = true;
            cfg.input.correction.hint.misread = true;
        }
        Ok("0") => cfg.input.correction.hint.misread = false,
        _ => {}
    }
    EngineManager::new(&cfg, Some(dir))
}

fn misread_eval() -> bool {
    std::env::var("WIND_PINYIN_EVAL_MISREAD").is_ok_and(|v| v == "1")
}

fn misread_styled(c: &wind_candidate::Candidate) -> Option<String> {
    let hints: Vec<_> = c
        .meta
        .pinyin_hints
        .iter()
        .filter(|h| h.kind == wind_candidate::HintKind::Misread)
        .collect();
    if hints.is_empty() {
        return None;
    }
    let mut out = String::new();
    for (i, ch) in c.text.chars().enumerate() {
        out.push(ch);
        if let Some(h) = hints.iter().find(|h| h.char_index as usize == i) {
            let _ = write!(out, "({})", h.correct);
        }
    }
    Some(out)
}

/// 纠错规则 id 与配置开关（设计 §5.2）。
fn correction_rule_slots(
    r: &mut wind_config::CorrectionRuleToggles,
) -> [(&'static str, &mut bool); 8] {
    [
        ("gn_ng", &mut r.gn_ng),
        ("on_ong", &mut r.on_ong),
        ("vowel_swap", &mut r.vowel_swap),
        ("legacy_spelling", &mut r.legacy_spelling),
        ("h_misplaced", &mut r.h_misplaced),
        ("mg_ng", &mut r.mg_ng),
        ("n_before_g", &mut r.n_before_g),
        ("ai_ia", &mut r.ai_ia),
    ]
}

const CORRECTION_RULE_IDS: [&str; 8] = [
    "gn_ng",
    "on_ong",
    "vowel_swap",
    "legacy_spelling",
    "h_misplaced",
    "mg_ng",
    "n_before_g",
    "ai_ia",
];

/// `WIND_PINYIN_EVAL_CORRECTION`：不设 = 出厂；`off` = 关纠错；否则为逗号分隔的规则 id
/// （只开这几条，`all` = 八条全开），自定义规则恒为空。
fn apply_correction_env(c: &mut wind_config::CorrectionConfig) {
    let Ok(v) = std::env::var("WIND_PINYIN_EVAL_CORRECTION") else {
        return;
    };
    c.custom.clear();
    if v.trim() == "off" {
        c.enabled = false;
        return;
    }
    c.enabled = true;
    for (_, slot) in correction_rule_slots(&mut c.rules) {
        *slot = false;
    }
    for g in v.split(',').map(str::trim) {
        let mut known = false;
        for (name, slot) in correction_rule_slots(&mut c.rules) {
            if g == "all" || g == name {
                *slot = true;
                known = true;
            }
        }
        assert!(known, "WIND_PINYIN_EVAL_CORRECTION 里有不认识的规则「{g}」");
    }
}

/// 只开 `rule` 一条的纠错规则表，用于构造 F/G 样本。
/// 与引擎运行时开哪几条无关：样本集在各档之间必须相同才能逐条对比。
fn rules_only(rule: &str) -> wind_engine::pinyin::correction::CorrectionRules {
    let mut c = wind_config::CorrectionConfig::default();
    for (name, slot) in correction_rule_slots(&mut c.rules) {
        *slot = name == rule;
    }
    wind_engine::pinyin::correction::CorrectionRules::from_config(&c)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// SplitMix64：固定种子可复现，避免引入 rand 依赖。
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Fisher-Yates 洗牌
    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = (self.next_u64() % (i as u64 + 1)) as usize;
            v.swap(i, j);
        }
    }
}

// ---------------------------------------------------------------- 词库读取

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
}

impl Class {
    fn key(self) -> &'static str {
        match self {
            Class::A => "A_normal",
            Class::B => "B_contracted_short",
            Class::C => "C_multi_syllable_contracted",
            Class::D => "D_mixed_abbrev_sentence",
            Class::E => "E_short_abbrev",
            Class::F => "F_abbrev_correction_conflict",
            Class::G => "G_transposition_typo",
            Class::H => "H_typo_abbrev_tail",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Class::A => "A 普通词 (mm == 真值)",
            Class::B => "B 缩合音短词 (塌缩进单音节边)",
            Class::C => "C 多音节含缩合音 (跨多音节但边界不符)",
            Class::D => "D 简拼+全拼混合整句 (bzd+haobuhao)",
            Class::E => "E 短简拼 (zhge / zhy / baichx)",
            Class::F => "F 简拼/混合简拼撞纠错拼写 (xign / dnu)",
            Class::G => "G 换位错拼 (gognping / dignyue)",
            Class::H => "H 错拼音节后接简拼尾 (yogny / gognp)",
        }
    }

    /// 本类的第四列（表头「切分正确」）实际度量什么。
    ///
    /// A/B/C 度量首选候选的音节切分是否等于真值；D 类的击键串与真值音节不同域
    /// （`bzd` 不是音节），故改为度量首选是否至少给对了第一个词（见 `Score::partial_ok`）。
    fn col4_meaning(self) -> &'static str {
        match self {
            Class::D => "首词命中",
            Class::E | Class::F | Class::G | Class::H => "无（击键不是音节）",
            _ => "切分正确",
        }
    }
}

struct Sample {
    text: String,
    input: String,
    true_syls: Vec<String>,
    mm: Vec<String>,
    weight: u64,
    unigram: u64,
    class: Class,
    /// D 类的组成词（`[W1, W2]`）。其余类为空。
    ///
    /// 用于 `partial_ok`：整句没出来时，确认首选至少给对了 W1。
    parts: Vec<String>,
    /// E 类的击键形态（`R` / `L` / `F`，见 [`build_short_abbrev_samples`]）。其余类为空。
    form: &'static str,
}

fn is_cjk(c: char) -> bool {
    ('\u{4E00}'..='\u{9FFF}').contains(&c)
}

/// 读 rime .dict.yaml 的数据段（`...` 之后），默认列序 `[text, code, weight]`。
fn read_dict(path: &Path, out: &mut Vec<(String, String, u64)>) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    let mut in_data = false;
    for line in raw.lines() {
        if !in_data {
            if line.trim() == "..." {
                in_data = true;
            }
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split('\t');
        let (Some(text), Some(code)) = (it.next(), it.next()) else {
            continue;
        };
        let weight = it
            .next()
            .and_then(|w| w.trim().parse::<u64>().ok())
            .unwrap_or(0);
        out.push((text.to_string(), code.to_string(), weight));
    }
}

/// 读取引擎实际加载的全部词库：`rime_frost.dict.yaml` 的 `import_tables` 声明。
/// 不硬编码文件名，避免与 schema 漂移。
fn read_all_dicts(dir: &Path) -> Vec<(String, String, u64)> {
    let root = dir.join("schemas/pinyin");
    let mut out = Vec::new();
    let Ok(raw) = std::fs::read_to_string(root.join("rime_frost.dict.yaml")) else {
        return out;
    };
    let mut in_imports = false;
    for line in raw.lines() {
        if line.starts_with("import_tables:") {
            in_imports = true;
            continue;
        }
        if in_imports {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix("- ") {
                let name = rest.split('#').next().unwrap_or("").trim();
                if !name.is_empty() {
                    let p = root.join(format!("{}.dict.yaml", name));
                    if p.exists() {
                        read_dict(&p, &mut out);
                    }
                }
            } else if !t.is_empty() && !t.starts_with('#') {
                break; // 离开 import_tables 块
            }
        }
    }
    out
}

/// 词频表：就地从已读入的词库条目汇总，同一词被多库收录时取最高频次（与 `gen_unigram`
/// 口径一致：累加等于重复计数）。就地汇总避免「文件缺失 → 空表 → 抽样分层静默失效」。
fn collect_word_freqs(raw: &[(String, String, u64)]) -> HashMap<String, u64> {
    let mut m: HashMap<String, u64> = HashMap::new();
    for (text, _code, weight) in raw {
        m.entry(text.clone())
            .and_modify(|v| *v = (*v).max(*weight))
            .or_insert(*weight);
    }
    m
}

/// 词条 → 样本；返回 None 表示不入池。同时统计丢弃原因。
fn classify(
    text: &str,
    code: &str,
    weight: u64,
    trie: &SyllableTrie,
    reject: &mut HashMap<&'static str, usize>,
) -> Option<Sample> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 2 || chars.len() > 8 {
        *reject.entry("char_len_out_of_range").or_default() += 1;
        return None;
    }
    if !chars.iter().all(|&c| is_cjk(c)) {
        *reject.entry("non_cjk_text").or_default() += 1;
        return None;
    }

    let true_syls: Vec<String> = code.split_whitespace().map(|s| s.to_string()).collect();
    if true_syls.is_empty()
        || !true_syls
            .iter()
            .all(|s| s.bytes().all(|b| b.is_ascii_lowercase()))
    {
        *reject.entry("non_plain_code").or_default() += 1;
        return None;
    }
    // 一字一音节：不满足说明词条本身格式异常（或含儿化/多音节字），排除以免污染判据
    if chars.len() != true_syls.len() {
        *reject.entry("char_syllable_count_mismatch").or_default() += 1;
        return None;
    }

    let input: String = true_syls.concat();
    let mm = Dag::build(&input, trie).maximum_match();
    if mm.concat() != input {
        *reject.entry("not_fully_segmentable").or_default() += 1;
        return None;
    }

    let class = if mm == true_syls {
        Class::A
    } else if mm.len() == 1 {
        Class::B
    } else {
        Class::C
    };

    Some(Sample {
        text: text.to_string(),
        input,
        true_syls,
        mm,
        weight,
        unigram: 0,
        class,
        parts: Vec::new(),
        form: "",
    })
}

/// D 类样本：把 W1 的简拼与 W2 的全拼拼成一串击键，期望整句 = W1 + W2
/// （微软拼音打 `bzdhaobuhao` 直接出「不知道好不好」）。度量智能组句，作为简拼接进词图前后的基线。
///
/// 入池条件（逐条都是为了避免假样本）：
/// - W1 取 2..=4 音节、W2 取 2..=3 音节：控制串长，也保证简拼有意义（单音节词简拼是裸声母）。
/// - 整串击键不能被完整切成音节序列，否则走纯全拼路径（如「哦哦」简拼 `oo` = `o|o`）。
/// - 击键串 ≤ 16 字节：与 `mixed_abbrev::MAX_INPUT_LEN` 对齐，超出则混合模式不枚举。
/// - 两词不同且拼接后仍是纯 CJK。
fn build_mixed_samples(
    pool: &[Sample],
    n: usize,
    seed: u64,
    trie: &SyllableTrie,
    reject: &mut HashMap<&'static str, usize>,
) -> Vec<Sample> {
    let mut rng = Rng(seed ^ 0xD_1234);

    // 只用常用词配对：用户打简拼是为了少敲熟词，随机配对的串没人会打，拿它调阈值会调偏。
    // 判据取词频前 1/3（须 `unigram > 0`）：权重为 0 的词拼不出整句，只会把指标压成噪声。
    let mut common: Vec<&Sample> = pool.iter().filter(|s| s.unigram > 0).collect();
    common.sort_by(|a, b| b.unigram.cmp(&a.unigram).then_with(|| a.text.cmp(&b.text)));
    common.truncate((common.len() / 3).max(1));

    // 候选池：按音节数分成「可作 W1」与「可作 W2」两组
    let w1: Vec<&Sample> = common
        .iter()
        .copied()
        .filter(|s| (2..=4).contains(&s.true_syls.len()))
        .collect();
    let w2: Vec<&Sample> = common
        .iter()
        .copied()
        .filter(|s| (2..=3).contains(&s.true_syls.len()))
        .collect();
    if w1.is_empty() || w2.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // 尝试次数给足（多数组合会被入池条件筛掉），但设硬上限避免词库形态异常时空转。
    let max_attempts = n.saturating_mul(60).max(2000);
    for _ in 0..max_attempts {
        if out.len() >= n {
            break;
        }
        let a = w1[(rng.next_u64() % w1.len() as u64) as usize];
        let b = w2[(rng.next_u64() % w2.len() as u64) as usize];
        if a.text == b.text {
            continue;
        }
        // W1 → 简拼（各音节首字母）；W2 → 全拼码
        let abbrev: String = a
            .true_syls
            .iter()
            .filter_map(|s| s.chars().next())
            .collect();
        let stroke = format!("{abbrev}{}", b.input);
        if stroke.len() > 16 {
            *reject.entry("D_stroke_too_long").or_default() += 1;
            continue;
        }
        // 整串可完整切分 ⇒ 走全拼路径，测不到简拼
        if Dag::build(&stroke, trie).maximum_match().concat() == stroke {
            *reject.entry("D_stroke_is_full_pinyin").or_default() += 1;
            continue;
        }
        let text = format!("{}{}", a.text, b.text);
        if !seen.insert(stroke.clone()) {
            continue;
        }
        out.push(Sample {
            text,
            input: stroke,
            // D 类不度量切分（击键串与真值音节不同域），真值音节仅供明细展示
            true_syls: a
                .true_syls
                .iter()
                .chain(b.true_syls.iter())
                .cloned()
                .collect(),
            mm: Vec::new(),
            weight: a.weight.min(b.weight),
            unigram: a.unigram.min(b.unigram),
            class: Class::D,
            parts: vec![a.text.clone(), b.text.clone()],
            form: "",
        });
    }
    out
}

/// E 类样本：常用词的短简拼击键，期望 = 该词本身（`zhge`→这个、`zhy`→这样、`baichx`→拜城县）。
/// A/B/C 是全拼、D 是长串，覆盖不到短简拼与混合整句的竞争。
///
/// 每个 2~3 音节常用词生成三种形态（zh/ch/sh 保留两个字母，与用户实际打法一致）：
/// - `R` 全声母：这样 → `zhy`、中国人 → `zhgr`；
/// - `L` 声母 + 末音节全拼：这个 → `zhge`；
/// - `F` 首音节全拼 + 其余声母：拜城县 → `baichx`。
///
/// 入池条件：
/// - 击键串 ≥ 4 字节：更短的不触发混合整句（`MIN_MIXED_SENTENCE_LEN`），歧义由词频裁决。
/// - 整串不能被完整切成音节序列（同 D 类）。
/// - 同一击键串只留词频最高的词当期望（`zhge` 下「这个」与「整个」），否则指标被同键歧义灌满噪声。
fn build_short_abbrev_samples(
    pool: &[Sample],
    trie: &SyllableTrie,
    reject: &mut HashMap<&'static str, usize>,
) -> Vec<Sample> {
    // 与 D 类同一口径：只用常用词（词频前 1/3，须 `unigram > 0`）。
    let mut common: Vec<&Sample> = pool.iter().filter(|s| s.unigram > 0).collect();
    common.sort_by(|a, b| b.unigram.cmp(&a.unigram).then_with(|| a.text.cmp(&b.text)));
    common.truncate((common.len() / 3).max(1));

    let initial = |syl: &str| -> String {
        if ["zh", "ch", "sh"].iter().any(|p| syl.starts_with(p)) {
            syl[..2].to_string()
        } else {
            syl[..1].to_string()
        }
    };
    // 击键串 → 样本；同键留词频高者（`common` 已按词频降序，先到者即最高）。
    let mut by_input: HashMap<String, Sample> = HashMap::new();
    for w in common
        .iter()
        .filter(|s| (2..=3).contains(&s.true_syls.len()))
    {
        let syls = &w.true_syls;
        let n = syls.len();
        let forms: [(&'static str, String); 3] = [
            ("R", syls.iter().map(|s| initial(s)).collect()),
            (
                "L",
                syls[..n - 1].iter().map(|s| initial(s)).collect::<String>() + &syls[n - 1],
            ),
            (
                "F",
                syls[0].clone() + &syls[1..].iter().map(|s| initial(s)).collect::<String>(),
            ),
        ];
        for (form, stroke) in forms {
            if stroke.len() < 4 {
                *reject.entry("E_stroke_too_short").or_default() += 1;
                continue;
            }
            if Dag::build(&stroke, trie).maximum_match().concat() == stroke {
                *reject.entry("E_stroke_is_full_pinyin").or_default() += 1;
                continue;
            }
            by_input.entry(stroke.clone()).or_insert_with(|| Sample {
                text: w.text.clone(),
                input: stroke,
                true_syls: syls.clone(),
                mm: Vec::new(),
                weight: w.weight,
                unigram: w.unigram,
                class: Class::E,
                parts: Vec::new(),
                form,
            });
        }
    }
    let mut out: Vec<Sample> = by_input.into_values().collect();
    // HashMap 迭代序不定：先定序，抽样才可复现。
    out.sort_by(|a, b| a.input.cmp(&b.input));
    out
}

/// 常用词（词频前 1/3，须 `unigram > 0`），按词频降序。D/E/F/G 同一口径。
fn common_words(pool: &[Sample]) -> Vec<&Sample> {
    let mut common: Vec<&Sample> = pool.iter().filter(|s| s.unigram > 0).collect();
    common.sort_by(|a, b| b.unigram.cmp(&a.unigram).then_with(|| a.text.cmp(&b.text)));
    common.truncate((common.len() / 3).max(1));
    common
}

/// 简拼取声母：zh/ch/sh 保留两个字母（与 E 类同）。
fn abbrev_initial(syl: &str) -> &str {
    if ["zh", "ch", "sh"].iter().any(|p| syl.starts_with(p)) {
        &syl[..2]
    } else {
        &syl[..1]
    }
}

/// F 类样本：打对了的简拼 / 混合简拼 / 全拼击键，串里含某条纠错规则的错误拼写，期望 = 该词本身。
///
/// 纠错只收非法拼写，只用 A~E 门禁恒空过（设计 §9）；本类专挑撞车的串（`xign` 既是「习惯」类
/// 混合简拼，又是 `gn_ng` 的错拼）。每个 2~4 音节常用词枚举「每音节全拼或取声母」的组合，
/// 留下含该规则错误拼写的串（子串判定）；`form` = 规则 id，同键留词频最高者，击键串 ≥ 3 字节。
fn build_abbrev_conflict_samples(pool: &[Sample], rule: &'static str) -> Vec<Sample> {
    let wrong: Vec<String> = rules_only(rule).spellings().map(str::to_string).collect();
    let common = common_words(pool);
    let hit = |stroke: &str| wrong.iter().any(|w| stroke.contains(w.as_str()));
    if wrong.is_empty() {
        return Vec::new();
    }
    let mut by_input: HashMap<String, Sample> = HashMap::new();
    for w in common
        .iter()
        .filter(|s| (2..=4).contains(&s.true_syls.len()))
    {
        let syls = &w.true_syls;
        let n = syls.len();
        for mask in 0u32..(1 << n) {
            let stroke: String = syls
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    if mask >> i & 1 == 1 {
                        abbrev_initial(s)
                    } else {
                        s.as_str()
                    }
                })
                .collect();
            if stroke.len() < 3 {
                continue;
            }
            if !hit(&stroke) {
                continue;
            }
            by_input.entry(stroke.clone()).or_insert_with(|| Sample {
                text: w.text.clone(),
                input: stroke,
                true_syls: syls.clone(),
                mm: Vec::new(),
                weight: w.weight,
                unigram: w.unigram,
                class: Class::F,
                parts: Vec::new(),
                form: rule,
            });
        }
    }
    let mut out: Vec<Sample> = by_input.into_values().collect();
    out.sort_by(|a, b| a.input.cmp(&b.input));
    out
}

/// F 类手写样本：隔音符形态等生成不出的串（`x'ai` 喜爱 撞 `ai_ia` 的 xai→xia）。
const F_HAND: &[(&str, &str)] = &[("x'ai", "喜爱"), ("xign", "习惯")];

/// G 类样本：常用词的**换位错拼**，期望 = 该词。按规则分组（`form` = 规则 id）：
/// 取词里第一个有该规则错误拼写的音节，换成它的第一个错误拼写（`gongping` → `gognping`）。
/// 同键留词频最高者。
fn build_transposition_samples(pool: &[Sample], rule: &'static str) -> Vec<Sample> {
    let rules = rules_only(rule);
    let common = common_words(pool);
    let mut by_input: HashMap<String, Sample> = HashMap::new();
    for w in common
        .iter()
        .filter(|s| (2..=4).contains(&s.true_syls.len()))
    {
        let syls = &w.true_syls;
        let Some((i, wrong)) = syls
            .iter()
            .enumerate()
            .find_map(|(i, s)| rules.wrong_spellings_of(s).first().map(|x| (i, x.clone())))
        else {
            continue;
        };
        let stroke: String = syls
            .iter()
            .enumerate()
            .map(|(k, s)| if k == i { wrong.as_str() } else { s.as_str() })
            .collect();
        by_input.entry(stroke.clone()).or_insert_with(|| Sample {
            text: w.text.clone(),
            input: stroke,
            true_syls: syls.clone(),
            mm: Vec::new(),
            weight: w.weight,
            unigram: w.unigram,
            class: Class::G,
            parts: Vec::new(),
            form: rule,
        });
    }
    let mut out: Vec<Sample> = by_input.into_values().collect();
    out.sort_by(|a, b| a.input.cmp(&b.input));
    out
}

/// H 类样本：**错拼音节 + 简拼尾**，期望 = 该词。同 G 取词里第一个有该规则错误拼写的音节，
/// 换成错拼；其后各音节取声母（至少一个），其前各音节写全。同键留词频最高者。
fn build_typo_abbrev_samples(pool: &[Sample], rule: &'static str) -> Vec<Sample> {
    let rules = rules_only(rule);
    let common = common_words(pool);
    let mut by_input: HashMap<String, Sample> = HashMap::new();
    for w in common
        .iter()
        .filter(|s| (2..=4).contains(&s.true_syls.len()))
    {
        let syls = &w.true_syls;
        let Some((i, wrong)) = syls
            .iter()
            .enumerate()
            .find_map(|(i, s)| rules.wrong_spellings_of(s).first().map(|x| (i, x.clone())))
        else {
            continue;
        };
        if i + 1 >= syls.len() {
            continue;
        }
        let stroke: String = syls
            .iter()
            .enumerate()
            .map(|(k, s)| match k {
                k if k < i => s.clone(),
                k if k == i => wrong.clone(),
                _ => abbrev_initial(s).to_string(),
            })
            .collect();
        by_input.entry(stroke.clone()).or_insert_with(|| Sample {
            text: w.text.clone(),
            input: stroke,
            true_syls: syls.clone(),
            mm: Vec::new(),
            weight: w.weight,
            unigram: w.unigram,
            class: Class::H,
            parts: Vec::new(),
            form: rule,
        });
    }
    let mut out: Vec<Sample> = by_input.into_values().collect();
    out.sort_by(|a, b| a.input.cmp(&b.input));
    out
}

// ---------------------------------------------------------------- 抽样

/// 按词频分三层等量抽样，保证不全是低频词。
fn stratified_sample(mut pool: Vec<Sample>, n: usize, seed: u64) -> Vec<Sample> {
    if pool.len() <= n {
        return pool;
    }
    // 频次降序 → 三等分为 高/中/低 三层
    pool.sort_by(|a, b| {
        b.unigram
            .cmp(&a.unigram)
            .then_with(|| b.weight.cmp(&a.weight))
            .then_with(|| a.text.cmp(&b.text)) // 末级定序：消除 HashMap/并列带来的不确定性
    });

    let tier_size = pool.len().div_ceil(3);
    let per_tier = n.div_ceil(3);
    let mut out = Vec::with_capacity(n);
    let mut rng = Rng(seed);

    for (t, chunk) in pool.chunks(tier_size).enumerate() {
        let mut idx: Vec<usize> = (0..chunk.len()).collect();
        let mut r = Rng(seed ^ ((t as u64 + 1).wrapping_mul(0x2545_F491_4F6C_DD1D)));
        r.shuffle(&mut idx);
        for &i in idx.iter().take(per_tier) {
            out.push(Sample {
                text: chunk[i].text.clone(),
                input: chunk[i].input.clone(),
                true_syls: chunk[i].true_syls.clone(),
                mm: chunk[i].mm.clone(),
                weight: chunk[i].weight,
                unigram: chunk[i].unigram,
                class: chunk[i].class,
                parts: chunk[i].parts.clone(),
                form: chunk[i].form,
            });
        }
    }
    out.truncate(n);
    rng.shuffle(&mut out);
    out
}

// ---------------------------------------------------------------- 打分

const TOP_N: usize = 10;

/// 界面序那一路的取数上限：协调器给引擎传的就是它（见 `truncate_with_quotas` 的文档）。
/// 引擎先排序后截断，上限不同则活过截断的候选不同，故必须与协调器同值，不能沿用 [`TOP_N`]。
const UI_LIMIT: usize = 300;

/// 界面每页候选数（`ui.per_page` 出厂值）。`page` 指标 = 界面序进首页。
const PAGE: usize = 7;

/// 界面序候选：与协调器同一串加工——同文纠错让位 → 同一个比较器排序 → 按文本去重
/// （保留排序后的第一条）→ 纠错不占首选（D6）。后两步与协调器调同一组共享函数。
///
/// 可以不跑协调器的依据见模块文档「两套位次」一节。
fn ui_candidates(mgr: &EngineManager, input: &str) -> Vec<wind_candidate::Candidate> {
    let mut c = mgr.convert_with("pinyin", input, UI_LIMIT).candidates;
    wind_candidate::drop_corrected_duplicates(&mut c);
    c.sort_by(|a, b| wind_candidate::candidate_display_order(a, b, false, false, input));
    let mut seen = std::collections::HashSet::new();
    c.retain(|x| seen.insert(x.text.clone()));
    wind_candidate::protect_top_from_corrected(&mut c, input.len());
    c
}

struct Miss {
    input: String,
    expect: String,
    rank: Option<usize>,
    got_top1: String,
    unigram: u64,
    true_syls: String,
    mm: String,
    /// E 类的击键形态（R/L/F），其余类为空。
    form: &'static str,
}

#[derive(Default)]
struct Score {
    total: usize,
    top1: usize,
    top5: usize,
    mrr_sum: f64,
    /// 首选候选的音节切分是否等于真值（只看切分，不看词）。
    ///
    /// top-1 被同音词噪声主导：`guanai` 出「关爱」是切对了只是选错同音词，出「挂乃」才是切分失败；
    /// 本指标把两者分开。判据：首选覆盖整串（`code == input`）且 `boundary` == 真值 mask，
    /// boundary==0（如单字）不算切分正确。
    seg_ok: usize,
    /// 仅 D 类：整句没出来时，首选是否至少给对了第一个词（W1），即分步上屏的行为。
    /// 简拼接进词图后 `top1` 应上升而这一项不应下降，否则说明整句抢走了首选却没抢对。
    partial_ok: usize,
    /// 仅 D 类：W1 出现在候选列表里（任意位次）。与 `partial_ok` 配对区分两种失败：
    /// - `partial_recall` 低 ⇒ 召回本身没做到，是缺陷；
    /// - `partial_recall` 高而 `partial_ok` 低 ⇒ 同简拼歧义（`bzd` 下多词按词频竞争），本就如此。
    partial_recall: usize,
    /// 界面序（见模块文档「两套位次」）的 top-1 / top-5 / MRR 累加。
    ui_top1: usize,
    ui_top5: usize,
    /// 界面序进首页（[`PAGE`] 条）。
    ui_page: usize,
    ui_mrr_sum: f64,
    /// 界面首页上的纠错候选条数 → 样本数。
    corrected_on_page: std::collections::BTreeMap<usize, usize>,
    /// 界面序未进首选的样本（`got_top1` 为界面首选）。
    ui_misses: Vec<Miss>,
    /// 按 `Sample::form` 分列的 (样本数, 界面序首选命中, 界面序首页命中)。E/F/G 类有。
    forms: std::collections::BTreeMap<&'static str, (usize, usize, usize)>,
    misses: Vec<Miss>,
    /// 切分不正确的样本明细（含失败**原因分类**，见 `SegMiss::reason`）
    seg_misses: Vec<SegMiss>,
}

struct SegMiss {
    input: String,
    expect: String,
    true_syls: String,
    mm: String,
    top_text: String,
    top_code: String,
    /// 首选候选 boundary 还原出的切分（`-` = 无边界信息）
    top_syls: String,
    /// 失败原因：
    /// - `not_full_span` 首选候选没覆盖整串输入（是子串/单字，不是"切错"）
    /// - `no_boundary`   首选覆盖整串但无边界信息（boundary==0）
    /// - `wrong_split`   首选覆盖整串且有边界，但切分与真值不符（真正的切错）
    reason: &'static str,
}

impl Score {
    fn rate(hit: usize, total: usize) -> f64 {
        if total == 0 {
            0.0
        } else {
            hit as f64 / total as f64
        }
    }
}

/// 真值切分的 bitmask（各音节起始字节位），与 `Candidate::boundary` 同域。
fn true_mask(true_syls: &[String]) -> u64 {
    let mut m = 0u64;
    let mut pos = 0usize;
    for s in true_syls {
        if pos >= 64 {
            return 0;
        }
        m |= 1u64 << pos;
        pos += s.len();
    }
    m
}

/// 把 bitmask 还原成 `a|b|c` 形式的切分串。
fn decode_mask(code: &str, mask: u64) -> String {
    if mask == 0 {
        return "-".to_string();
    }
    let mut starts: Vec<usize> = (0..code.len().min(64))
        .filter(|i| (mask >> i) & 1 == 1)
        .collect();
    if starts.first() != Some(&0) {
        starts.insert(0, 0);
    }
    let mut out: Vec<&str> = Vec::new();
    for (i, &s) in starts.iter().enumerate() {
        let e = starts.get(i + 1).copied().unwrap_or(code.len());
        if s <= e && e <= code.len() {
            out.push(&code[s..e]);
        }
    }
    out.join("|")
}

fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o
}

// ---------------------------------------------------------------- 主流程

#[test]
#[ignore = "批量评测：慢，且依赖 build_dev 真实词库。用 --ignored 显式运行"]
fn pinyin_eval_report() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：build_dev 拼音词库不存在");
        return;
    };

    // 1000 是「够用又便宜」的折中：实测 A 类 1000 条打分约 1.1 s，整轮含建集 < 5 s。
    // B 类总体只有 80 条，会被全量取用（见报告里 sampled 列）。
    let n_per_class = env_usize("WIND_PINYIN_EVAL_N", 1000);
    let seed = std::env::var("WIND_PINYIN_EVAL_SEED")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(20_260_721);
    let dump = env_usize("WIND_PINYIN_EVAL_DUMP", 40);
    // 注意：测试进程的 cwd 是 crate 目录而非 workspace 根，相对路径会落到
    // `crates/wind-engine/target/...` 这种意外位置。默认路径因此显式锚定到 workspace target。
    let out = match std::env::var("WIND_PINYIN_EVAL_OUT") {
        Ok(p) => PathBuf::from(p),
        Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/pinyin_eval")
            .join("latest.json"),
    };

    // ---- 1. 生成评测集
    let t_gen = Instant::now();
    let raw = read_all_dicts(&dir);
    let unigram = collect_word_freqs(&raw);

    let trie = SyllableTrie::new();
    let mut reject: HashMap<&'static str, usize> = HashMap::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut pools: HashMap<&'static str, Vec<Sample>> = HashMap::new();
    let mut class_totals: HashMap<&'static str, usize> = HashMap::new();

    for (text, code, weight) in &raw {
        if let Some(mut s) = classify(text, code, *weight, &trie, &mut reject) {
            if !seen.insert((s.text.clone(), s.input.clone())) {
                continue;
            }
            s.unigram = unigram.get(&s.text).copied().unwrap_or(0);
            *class_totals.entry(s.class.key()).or_default() += 1;
            pools.entry(s.class.key()).or_default().push(s);
        }
    }
    // D 类是组合样本，在 A/B/C 建池之后单独构造。取材于 A 池：真值切分与最大匹配一致，
    // 混合整句的失败可归因到简拼段而非切分本身。
    let d_pool = {
        let a_pool = pools
            .get(Class::A.key())
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        build_mixed_samples(a_pool, n_per_class, seed, &trie, &mut reject)
    };
    *class_totals.entry(Class::D.key()).or_default() += d_pool.len();
    pools.insert(Class::D.key(), d_pool);
    let e_pool = {
        let a_pool = pools
            .get(Class::A.key())
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        build_short_abbrev_samples(a_pool, &trie, &mut reject)
    };
    *class_totals.entry(Class::E.key()).or_default() += e_pool.len();
    pools.insert(Class::E.key(), e_pool);
    // F/G 按规则分组构造，构造用的规则表与本轮引擎开哪几条无关——各档样本集相同才能逐条比。
    let (f_by_rule, g_by_rule, h_by_rule) = {
        let a_pool = pools
            .get(Class::A.key())
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let by_rule =
            |f: fn(&[Sample], &'static str) -> Vec<Sample>| -> Vec<(&'static str, Vec<Sample>)> {
                CORRECTION_RULE_IDS
                    .iter()
                    .map(|&r| (r, f(a_pool, r)))
                    .collect()
            };
        (
            by_rule(build_abbrev_conflict_samples),
            by_rule(build_transposition_samples),
            by_rule(build_typo_abbrev_samples),
        )
    };
    for (c, groups) in [
        (Class::F, &f_by_rule),
        (Class::G, &g_by_rule),
        (Class::H, &h_by_rule),
    ] {
        let pop: Vec<String> = groups
            .iter()
            .map(|(r, v)| format!("{r} {}", v.len()))
            .collect();
        println!("{} 各规则总体: {}", c.key(), pop.join("，"));
        *class_totals.entry(c.key()).or_default() +=
            groups.iter().map(|(_, v)| v.len()).sum::<usize>();
    }

    let gen_ms = t_gen.elapsed().as_millis();

    println!("\n=== 评测集生成 ({} ms) ===", gen_ms);
    println!("词库原始条目: {}", raw.len());
    for c in ALL_CLASSES {
        println!(
            "  {:<44} 总体 {:>7}",
            c.label(),
            class_totals.get(c.key()).copied().unwrap_or(0)
        );
    }
    let mut rj: Vec<_> = reject.iter().collect();
    rj.sort();
    println!("丢弃（不入池）: {:?}", rj);

    // ---- 2. 抽样
    let mut samples: Vec<(Class, Vec<Sample>)> = Vec::new();
    for c in [Class::A, Class::B, Class::C, Class::D, Class::E] {
        let pool = pools.remove(c.key()).unwrap_or_default();
        samples.push((c, stratified_sample(pool, n_per_class, seed)));
    }
    // F/G 按规则各抽一份（规则间总体差很多，混抽会让大规则占满）。同一击键串可能落进多条规则。
    let per_rule = env_usize("WIND_PINYIN_EVAL_PER_RULE", 300);
    let mut f_set: Vec<Sample> = F_HAND
        .iter()
        .map(|&(input, text)| Sample {
            text: text.into(),
            input: input.into(),
            true_syls: Vec::new(),
            mm: Vec::new(),
            weight: 0,
            unigram: 0,
            class: Class::F,
            parts: Vec::new(),
            form: "hand",
        })
        .collect();
    f_set.extend(
        f_by_rule
            .into_iter()
            .flat_map(|(_, v)| stratified_sample(v, per_rule, seed)),
    );
    samples.push((Class::F, f_set));
    samples.push((
        Class::G,
        g_by_rule
            .into_iter()
            .flat_map(|(_, v)| stratified_sample(v, per_rule, seed))
            .collect(),
    ));
    samples.push((
        Class::H,
        h_by_rule
            .into_iter()
            .flat_map(|(_, v)| stratified_sample(v, per_rule, seed))
            .collect(),
    ));
    for (c, set) in &samples {
        println!("  抽样 {:<44} {:>6}", c.label(), set.len());
    }

    // ---- 3. 打分（全程共享一次 EngineManager 初始化）
    let t_load = Instant::now();
    let mgr = manager(&dir);
    let load_ms = t_load.elapsed().as_millis();
    println!("\n引擎初始化: {} ms", load_ms);

    let t_run = Instant::now();
    // 逐条位次（`<out>.ranks.tsv`）：档间逐位对比、与协调器对拍都用它。
    let mut ranks_tsv = String::from(
        "class\tform\tinput\texpect\tengine_rank\tui_rank\tcorrected_on_page\tui_top1\tui_page\n",
    );
    let mut scores: Vec<(Class, Score, usize)> = Vec::new();
    // 误读统计（`WIND_PINYIN_EVAL_MISREAD=1`）：类 → (样本数, 首页有误读提示的样本数, 提示候选数, 期望词被标数)。
    let mut misread_stats: Vec<(Class, [usize; 4])> = Vec::new();
    let mut misread_rows: Vec<String> = Vec::new();
    for (class, set) in &samples {
        let mut mr = [0usize; 4];
        let t = Instant::now();
        let mut sc = Score::default();
        for s in set {
            sc.total += 1;
            let cands = mgr.convert_with("pinyin", &s.input, TOP_N).candidates;
            let rank = cands.iter().position(|c| c.text == s.text);
            let ui = ui_candidates(&mgr, &s.input);
            let ui_rank = ui.iter().position(|c| c.text == s.text);
            if let Some(r) = ui_rank {
                if r == 0 {
                    sc.ui_top1 += 1;
                }
                if r < 5 {
                    sc.ui_top5 += 1;
                }
                if r < PAGE {
                    sc.ui_page += 1;
                }
                sc.ui_mrr_sum += 1.0 / (r as f64 + 1.0);
            }
            if misread_eval() && !matches!(*class, Class::G | Class::H) {
                mr[0] += 1;
                let marked: Vec<(String, bool)> = ui
                    .iter()
                    .take(PAGE)
                    .filter(|c| !c.is_misread_recall)
                    .filter_map(|c| misread_styled(c).map(|t| (t, c.text == s.text)))
                    .collect();
                if !marked.is_empty() {
                    mr[1] += 1;
                    mr[2] += marked.len();
                    mr[3] += marked.iter().filter(|m| m.1).count();
                    misread_rows.push(format!(
                        "{}\t{}\t{}\t{}",
                        class.key(),
                        s.input,
                        s.text,
                        marked
                            .iter()
                            .map(|m| m.0.as_str())
                            .collect::<Vec<_>>()
                            .join(" ")
                    ));
                }
            }
            let corr_on_page = ui.iter().take(PAGE).filter(|c| c.is_corrected).count();
            *sc.corrected_on_page.entry(corr_on_page).or_default() += 1;
            if !s.form.is_empty() {
                let f = sc.forms.entry(s.form).or_default();
                f.0 += 1;
                f.1 += usize::from(ui_rank == Some(0));
                f.2 += usize::from(ui_rank.is_some_and(|r| r < PAGE));
            }
            let fmt_rank = |r: Option<usize>| r.map_or("-".to_string(), |r| r.to_string());
            let _ = writeln!(
                ranks_tsv,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                class.key(),
                s.form,
                s.input,
                s.text,
                fmt_rank(rank),
                fmt_rank(ui_rank),
                corr_on_page,
                ui.first().map(|c| c.text.as_str()).unwrap_or(""),
                ui.iter()
                    .take(PAGE)
                    .map(|c| if c.is_corrected {
                        format!("*{}", c.text)
                    } else {
                        c.text.clone()
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            if ui_rank != Some(0) {
                sc.ui_misses.push(Miss {
                    input: s.input.clone(),
                    expect: s.text.clone(),
                    rank: ui_rank,
                    got_top1: ui.first().map(|c| c.text.clone()).unwrap_or_default(),
                    unigram: s.unigram,
                    true_syls: s.true_syls.join("|"),
                    mm: s.mm.join("|"),
                    form: s.form,
                });
            }
            let tm = true_mask(&s.true_syls);
            // D 类：击键串与真值音节不同域（`bzd` 不是音节），切分判据无从成立。
            // 第四列改度量「首选至少给对了 W1」——当前分步上屏的实际行为。
            if s.class == Class::D {
                if cands
                    .first()
                    .is_some_and(|t| Some(&t.text) == s.parts.first())
                {
                    sc.partial_ok += 1;
                }
                if s.parts
                    .first()
                    .is_some_and(|w1| cands.iter().any(|c| &c.text == w1))
                {
                    sc.partial_recall += 1;
                }
            }
            // E/F/G 类击键不是音节（`zhge` 的 `zh` 是声母），切分判据无从成立，不计。
            match cands.first() {
                _ if matches!(s.class, Class::E | Class::F | Class::G | Class::H) => {}
                Some(top) if top.code == s.input && top.boundary != 0 && top.boundary == tm => {
                    sc.seg_ok += 1;
                }
                top => {
                    let (top_text, top_code, top_syls, reason) = match top {
                        None => (
                            String::new(),
                            String::new(),
                            "-".to_string(),
                            "no_candidate",
                        ),
                        Some(t) if t.code != s.input => (
                            t.text.clone(),
                            t.code.clone(),
                            decode_mask(&t.code, t.boundary),
                            "not_full_span",
                        ),
                        Some(t) if t.boundary == 0 => (
                            t.text.clone(),
                            t.code.clone(),
                            "-".to_string(),
                            "no_boundary",
                        ),
                        Some(t) => (
                            t.text.clone(),
                            t.code.clone(),
                            decode_mask(&t.code, t.boundary),
                            "wrong_split",
                        ),
                    };
                    sc.seg_misses.push(SegMiss {
                        input: s.input.clone(),
                        expect: s.text.clone(),
                        true_syls: s.true_syls.join("|"),
                        mm: s.mm.join("|"),
                        top_text,
                        top_code,
                        top_syls,
                        reason,
                    });
                }
            }
            if let Some(r) = rank {
                if r == 0 {
                    sc.top1 += 1;
                }
                if r < 5 {
                    sc.top5 += 1;
                }
                sc.mrr_sum += 1.0 / (r as f64 + 1.0);
            }
            if rank != Some(0) {
                sc.misses.push(Miss {
                    input: s.input.clone(),
                    expect: s.text.clone(),
                    rank,
                    got_top1: cands.first().map(|c| c.text.clone()).unwrap_or_default(),
                    unigram: s.unigram,
                    true_syls: s.true_syls.join("|"),
                    mm: s.mm.join("|"),
                    form: s.form,
                });
            }
        }
        let ms = t.elapsed().as_millis() as usize;
        scores.push((*class, sc, ms));
        misread_stats.push((*class, mr));
    }
    let run_ms = t_run.elapsed().as_millis();
    if misread_eval() {
        println!("\n=== 误读表：打对的输入上首页（{PAGE} 条）的误读提示 ===");
        println!("类\t样本\t有提示的样本\t提示候选\t期望词被标");
        let mut tot = [0usize; 4];
        for (class, m) in misread_stats
            .iter()
            .filter(|(c, _)| !matches!(c, Class::G | Class::H))
        {
            println!("{}\t{}\t{}\t{}\t{}", class.key(), m[0], m[1], m[2], m[3]);
            for i in 0..4 {
                tot[i] += m[i];
            }
        }
        println!("合计\t{}\t{}\t{}\t{}", tot[0], tot[1], tot[2], tot[3]);
        let path = out.with_extension("misread.tsv");
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let body = format!(
            "class\tinput\texpect\tmarked\n{}\n",
            misread_rows.join("\n")
        );
        match std::fs::write(&path, body) {
            Ok(()) => println!("明细: {}", path.display()),
            Err(e) => println!("明细写盘失败 {}: {e}", path.display()),
        }
    }

    // ---- 4. 报告
    println!("\n=== 基线报告 (seed={}, top_n={}) ===", seed, TOP_N);
    println!(
        "{:<46} {:>6} {:>9} {:>9} {:>9} {:>9} {:>8}",
        "类别", "样本", "top-1", "top-5", "MRR", "切分正确*", "耗时ms"
    );
    for (c, sc, ms) in &scores {
        // 第四列对 A/B/C 是切分正确率、对 D 是首词命中率（见 Class::col4_meaning）
        let col4 = match c {
            Class::D => Score::rate(sc.partial_ok, sc.total),
            _ => Score::rate(sc.seg_ok, sc.total),
        };
        println!(
            "{:<46} {:>6} {:>8.2}% {:>8.2}% {:>9.4} {:>8.2}% {:>8}",
            c.label(),
            sc.total,
            Score::rate(sc.top1, sc.total) * 100.0,
            Score::rate(sc.top5, sc.total) * 100.0,
            if sc.total == 0 {
                0.0
            } else {
                sc.mrr_sum / sc.total as f64
            },
            col4 * 100.0,
            ms
        );
    }
    println!(
        "* 第四列：A/B/C = 切分正确；{} = {}（击键串与真值音节不同域，切分判据无从成立）",
        Class::D.key(),
        Class::D.col4_meaning()
    );
    println!(
        "  {} 的第四列恒为 0：{}。它的指标看下面的界面序表。",
        Class::E.key(),
        Class::E.col4_meaning()
    );
    println!("  D 类 top-1 度量「智能组句」：简拼段目前不进 lattice/Viterbi，基线预期接近 0。");
    if let Some((_, sc, _)) = scores.iter().find(|(c, _, _)| *c == Class::D) {
        println!(
            "  D 类首词：命中(首选) {:.2}% / 召回(任意位次) {:.2}% —— 两者的差是「简拼同音歧义」，\n  \
             只有召回率低才说明前缀回退本身没做到。",
            Score::rate(sc.partial_ok, sc.total) * 100.0,
            Score::rate(sc.partial_recall, sc.total) * 100.0
        );
    }

    println!(
        "\n=== 界面序（convert 取 {} 条 → candidate_display_order → 同文去重；判断对用户的影响看这张）===",
        UI_LIMIT
    );
    println!(
        "{:<46} {:>6} {:>9} {:>9} {:>9} {:>9}",
        "类别", "样本", "top-1", "top-5", "首页", "MRR"
    );
    for (c, sc, _) in &scores {
        println!(
            "{:<46} {:>6} {:>8.2}% {:>8.2}% {:>8.2}% {:>9.4}",
            c.label(),
            sc.total,
            Score::rate(sc.ui_top1, sc.total) * 100.0,
            Score::rate(sc.ui_top5, sc.total) * 100.0,
            Score::rate(sc.ui_page, sc.total) * 100.0,
            if sc.total == 0 {
                0.0
            } else {
                sc.ui_mrr_sum / sc.total as f64
            },
        );
    }
    for (c, sc, _) in &scores {
        if sc.forms.is_empty() {
            continue;
        }
        let forms: Vec<String> = sc
            .forms
            .iter()
            .map(|(f, (n, hit, page))| {
                format!(
                    "{f} 首选 {:.2}% 首页 {:.2}% (n={n})",
                    Score::rate(*hit, *n) * 100.0,
                    Score::rate(*page, *n) * 100.0
                )
            })
            .collect();
        println!("  {} 按形态: {}", c.key(), forms.join("，"));
    }
    println!("\n=== 界面首页（{PAGE} 条）上的纠错候选条数分布 ===");
    for (c, sc, _) in &scores {
        let dist: Vec<String> = sc
            .corrected_on_page
            .iter()
            .map(|(k, n)| format!("{k}:{n}"))
            .collect();
        let mut all: Vec<usize> = sc
            .corrected_on_page
            .iter()
            .flat_map(|(k, n)| std::iter::repeat_n(*k, *n))
            .collect();
        all.sort_unstable();
        let median = all.get(all.len() / 2).copied().unwrap_or(0);
        println!("  {:<32} 中位 {median}  分布 {}", c.key(), dist.join(" "));
    }

    println!("\n评测总耗时: {} ms（含引擎初始化 {} ms）", run_ms, load_ms);

    // 切分不正确的明细：按原因分桶。`wrong_split` 才是「多路径选错了切分」，
    // 其余两类（首选是子串 / 首选无边界信息）不构成切分错误，只是指标判据的副产物。
    for (c, sc, _) in &scores {
        let mut by_reason: HashMap<&'static str, usize> = HashMap::new();
        for m in &sc.seg_misses {
            *by_reason.entry(m.reason).or_default() += 1;
        }
        let mut v: Vec<_> = by_reason.iter().collect();
        v.sort();
        println!(
            "\n--- {} 切分不正确 {} 条，按原因: {:?} ---",
            c.label(),
            sc.seg_misses.len(),
            v
        );
        for m in sc
            .seg_misses
            .iter()
            .filter(|m| m.reason == "wrong_split")
            .take(dump)
        {
            println!(
                "  [切错] {:<24} 期望 {:<10} 真值 {:<28} 实选 {:<10} 切分 {:<28} mm {}",
                m.input, m.expect, m.true_syls, m.top_text, m.top_syls, m.mm
            );
        }
        for m in sc
            .seg_misses
            .iter()
            .filter(|m| m.reason != "wrong_split")
            .take(dump.min(15))
        {
            println!(
                "  [{}] {:<22} 期望 {:<10} 真值 {:<28} 实选 {:<10} code {}",
                m.reason, m.input, m.expect, m.true_syls, m.top_text, m.top_code
            );
        }
    }

    // C 类 top-1 未命中的样本：核对「首选是否切分正确」——这是
    // 「剩余失败已转为同音词竞争」这一论断的可验证依据。
    for (c, sc, _) in &scores {
        if *c != Class::C {
            continue;
        }
        let seg_bad: std::collections::HashSet<&str> =
            sc.seg_misses.iter().map(|m| m.input.as_str()).collect();
        let (mut same_seg, mut diff_seg) = (0usize, 0usize);
        for m in &sc.misses {
            if seg_bad.contains(m.input.as_str()) {
                diff_seg += 1;
            } else {
                same_seg += 1;
            }
        }
        println!(
            "\n=== C 类 top-1 未命中 {} 条：首选切分正确 {} 条（同音词竞争）/ 切分不正确 {} 条 ===",
            sc.misses.len(),
            same_seg,
            diff_seg
        );
        println!("--- 其中「切分正确、仅选错同音词」的样本（前 {}）---", dump);
        for m in sc
            .misses
            .iter()
            .filter(|m| !seg_bad.contains(m.input.as_str()))
            .take(dump)
        {
            println!(
                "  {:<24} 期望 {:<10} rank={:<5} 首选 {:<10} 真值 {}",
                m.input,
                m.expect,
                m.rank
                    .map(|r| r.to_string())
                    .unwrap_or_else(|| "miss".into()),
                m.got_top1,
                m.true_syls
            );
        }
    }

    for (c, sc, _) in &scores {
        println!("\n--- {} 非首选明细（前 {}）---", c.label(), dump);
        for m in sc.misses.iter().take(dump) {
            println!(
                "  {:<24} 期望 {:<10} rank={:<5} 首选 {:<10} uni={:<8} 真值 {} / mm {}",
                m.input,
                m.expect,
                m.rank
                    .map(|r| r.to_string())
                    .unwrap_or_else(|| "miss".into()),
                m.got_top1,
                m.unigram,
                m.true_syls,
                m.mm
            );
        }
    }

    for (c, sc, _) in &scores {
        if !matches!(c, Class::E | Class::F | Class::G | Class::H) {
            continue;
        }
        println!("\n--- {} 界面序非首选明细（前 {}）---", c.label(), dump);
        for m in sc.ui_misses.iter().take(dump) {
            println!(
                "  {:<14} [{}] 期望 {:<8} 界面位次 {:<5} 界面首选 {:<10} uni={}",
                m.input,
                m.form,
                m.expect,
                m.rank
                    .map(|r| (r + 1).to_string())
                    .unwrap_or_else(|| "miss".into()),
                m.got_top1,
                m.unigram
            );
        }
    }

    // ---- 5. 机器可读输出
    let mut j = String::new();
    j.push_str("{\n");
    let _ = writeln!(
        j,
        "  \"seed\": {}, \"top_n\": {}, \"n_per_class\": {},",
        seed, TOP_N, n_per_class
    );
    let _ = writeln!(
        j,
        "  \"timing_ms\": {{ \"generate\": {}, \"engine_load\": {}, \"score\": {} }},",
        gen_ms, load_ms, run_ms
    );
    j.push_str("  \"classes\": {\n");
    for (i, (c, sc, ms)) in scores.iter().enumerate() {
        let _ = writeln!(
            j,
            "    \"{}\": {{ \"population\": {}, \"sampled\": {}, \"top1\": {:.6}, \"top5\": {:.6}, \"mrr\": {:.6}, \"seg_ok\": {:.6}, \"top1_hits\": {}, \"top5_hits\": {}, \"score_ms\": {},",
            c.key(),
            class_totals.get(c.key()).copied().unwrap_or(0),
            sc.total,
            Score::rate(sc.top1, sc.total),
            Score::rate(sc.top5, sc.total),
            if sc.total == 0 {
                0.0
            } else {
                sc.mrr_sum / sc.total as f64
            },
            Score::rate(sc.seg_ok, sc.total),
            sc.top1,
            sc.top5,
            ms
        );
        let _ = writeln!(
            j,
            "      \"ui_top1\": {:.6}, \"ui_top5\": {:.6}, \"ui_page\": {:.6}, \"ui_mrr\": {:.6}, \"ui_top1_hits\": {},",
            Score::rate(sc.ui_top1, sc.total),
            Score::rate(sc.ui_top5, sc.total),
            Score::rate(sc.ui_page, sc.total),
            if sc.total == 0 {
                0.0
            } else {
                sc.ui_mrr_sum / sc.total as f64
            },
            sc.ui_top1
        );
        if !sc.forms.is_empty() {
            let forms: Vec<String> = sc
                .forms
                .iter()
                .map(|(f, (n, hit, page))| {
                    format!(
                        "\"{f}\": {{ \"n\": {n}, \"ui_top1\": {:.6}, \"ui_page\": {:.6} }}",
                        Score::rate(*hit, *n),
                        Score::rate(*page, *n)
                    )
                })
                .collect();
            let _ = writeln!(j, "      \"forms\": {{ {} }},", forms.join(", "));
        }
        j.push_str("      \"ui_misses\": [\n");
        for (k, m) in sc.ui_misses.iter().take(dump).enumerate() {
            let _ = writeln!(
                j,
                "        {{ \"input\": \"{}\", \"expect\": \"{}\", \"rank\": {}, \"top1\": \"{}\", \"unigram\": {}, \"form\": \"{}\" }}{}",
                json_escape(&m.input),
                json_escape(&m.expect),
                m.rank
                    .map(|r| r.to_string())
                    .unwrap_or_else(|| "null".into()),
                json_escape(&m.got_top1),
                m.unigram,
                m.form,
                if k + 1 == sc.ui_misses.len().min(dump) {
                    ""
                } else {
                    ","
                }
            );
        }
        j.push_str("      ],\n");
        // D 类专有：首词命中率（seg_ok 对它恒为 0，不可用于对账）
        if *c == Class::D {
            let _ = writeln!(
                j,
                "      \"first_word_top1\": {:.6}, \"first_word_hits\": {}, \"first_word_recall\": {:.6},",
                Score::rate(sc.partial_ok, sc.total),
                sc.partial_ok,
                Score::rate(sc.partial_recall, sc.total)
            );
        }
        j.push_str("      \"misses\": [\n");
        for (k, m) in sc.misses.iter().take(dump).enumerate() {
            let _ = writeln!(
                j,
                "        {{ \"input\": \"{}\", \"expect\": \"{}\", \"rank\": {}, \"top1\": \"{}\", \"unigram\": {}, \"true_syls\": \"{}\", \"mm\": \"{}\" }}{}",
                json_escape(&m.input),
                json_escape(&m.expect),
                m.rank
                    .map(|r| r.to_string())
                    .unwrap_or_else(|| "null".into()),
                json_escape(&m.got_top1),
                m.unigram,
                json_escape(&m.true_syls),
                json_escape(&m.mm),
                if k + 1 == sc.misses.len().min(dump) {
                    ""
                } else {
                    ","
                }
            );
        }
        j.push_str("      ]\n    }");
        j.push_str(if i + 1 == scores.len() { "\n" } else { ",\n" });
    }
    j.push_str("  }\n}\n");

    if let Some(p) = out.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    match std::fs::write(&out, &j) {
        Ok(()) => println!("\n结果已写入 {}", out.display()),
        Err(e) => eprintln!("\n写入 {} 失败: {}", out.display(), e),
    }
    let ranks_path = out.with_extension("ranks.tsv");
    match std::fs::write(&ranks_path, &ranks_tsv) {
        Ok(()) => println!("逐条位次已写入 {}", ranks_path.display()),
        Err(e) => eprintln!("写入 {} 失败: {}", ranks_path.display(), e),
    }

    // ---- 6. 逐键耗时（`WIND_PINYIN_EVAL_LATENCY=1`）：每条样本按前缀逐键 convert（协调器同一取数上限）。
    if std::env::var("WIND_PINYIN_EVAL_LATENCY").is_ok_and(|v| v == "1") {
        let mut per_key: Vec<f64> = Vec::new();
        let mut per_input: Vec<f64> = Vec::new();
        let t_all = Instant::now();
        for (_, set) in &samples {
            for s in set {
                let t_in = Instant::now();
                for end in 1..=s.input.len() {
                    let t = Instant::now();
                    let _ = mgr.convert_with("pinyin", &s.input[..end], UI_LIMIT);
                    per_key.push(t.elapsed().as_secs_f64() * 1e6);
                }
                per_input.push(t_in.elapsed().as_secs_f64() * 1e6);
            }
        }
        let wall_ms = t_all.elapsed().as_millis();
        let pct = |v: &mut Vec<f64>, p: f64| {
            v.sort_by(|a, b| a.total_cmp(b));
            v[((v.len() as f64 - 1.0) * p).round() as usize]
        };
        println!(
            "\n=== 逐键耗时 === 总 {wall_ms} ms；逐键 {} 次 p50 {:.0} µs p95 {:.0} µs；逐条 {} 条 p50 {:.0} µs p95 {:.0} µs",
            per_key.len(),
            pct(&mut per_key, 0.5),
            pct(&mut per_key, 0.95),
            per_input.len(),
            pct(&mut per_input, 0.5),
            pct(&mut per_input, 0.95),
        );
    }
}

/// 双拼变体一档（开关关或开）的累计。
#[derive(Default)]
struct SpScore {
    total: usize,
    top1: usize,
    ui_top1: usize,
    ui_top5: usize,
    ui_page: usize,
    ui_mrr_sum: f64,
}

impl SpScore {
    fn add(&mut self, engine: Option<usize>, ui: Option<usize>) {
        self.total += 1;
        self.top1 += usize::from(engine == Some(0));
        if let Some(r) = ui {
            self.ui_top1 += usize::from(r == 0);
            self.ui_top5 += usize::from(r < 5);
            self.ui_page += usize::from(r < PAGE);
            self.ui_mrr_sum += 1.0 / (r as f64 + 1.0);
        }
    }

    fn json(&self) -> String {
        let r = |n: usize| Score::rate(n, self.total);
        format!(
            "{{\"total\": {}, \"top1\": {:.4}, \"ui_top1\": {:.4}, \"ui_top5\": {:.4}, \"ui_page\": {:.4}, \"ui_mrr\": {:.4}}}",
            self.total,
            r(self.top1),
            r(self.ui_top1),
            r(self.ui_top5),
            r(self.ui_page),
            if self.total == 0 {
                0.0
            } else {
                self.ui_mrr_sum / self.total as f64
            }
        )
    }
}

/// 双拼变体：A/B/C 抽样（同种子、同抽法）按真值音节转小鹤击键，同一进程里比较
/// `ConvertOptions::sp_raw_lattice` 关 / 开。引擎序取 `UI_LIMIT` 条里的前 [`TOP_N`]，界面序同
/// [`ui_candidates`] 的加工。
#[test]
#[ignore = "批量评测：慢，且依赖 build_dev 真实词库。用 --ignored 显式运行"]
fn pinyin_eval_shuangpin() {
    if !std::env::var("WIND_PINYIN_EVAL_SHUANGPIN").is_ok_and(|v| v == "1") {
        eprintln!("跳过：未设 WIND_PINYIN_EVAL_SHUANGPIN=1");
        return;
    }
    let Some(dir) = data_dir() else {
        eprintln!("跳过：build_dev 拼音词库不存在");
        return;
    };
    let n_per_class = env_usize("WIND_PINYIN_EVAL_N", 1000);
    let seed = std::env::var("WIND_PINYIN_EVAL_SEED")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(20_260_721);
    let out = match std::env::var("WIND_PINYIN_EVAL_OUT") {
        Ok(p) => PathBuf::from(p),
        Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/pinyin_eval")
            .join("latest.json"),
    };

    let raw = read_all_dicts(&dir);
    let unigram = collect_word_freqs(&raw);
    let trie = SyllableTrie::new();
    let mut reject: HashMap<&'static str, usize> = HashMap::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut pools: HashMap<&'static str, Vec<Sample>> = HashMap::new();
    for (text, code, weight) in &raw {
        if let Some(mut s) = classify(text, code, *weight, &trie, &mut reject) {
            if !seen.insert((s.text.clone(), s.input.clone())) {
                continue;
            }
            s.unigram = unigram.get(&s.text).copied().unwrap_or(0);
            pools.entry(s.class.key()).or_default().push(s);
        }
    }

    let layout = Layout::from_toml(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../data/schemas/shuangpin/xiaohe.toml"),
    )
    .expect("加载小鹤布局");
    let rev = ShuangpinConverter::new(layout).build_reverse();
    let mut cfg = Config::default();
    cfg.schema.available = vec!["shuangpin".to_string()];
    cfg.schema.active = "shuangpin".to_string();
    let mgr = EngineManager::new(&cfg, Some(&dir));
    let ranks = |keys: &str, text: &str, raw: bool| {
        let opts = ConvertOptions {
            sp_raw_lattice: raw,
            ..Default::default()
        };
        let mut c = mgr
            .convert_with_opts("shuangpin", keys, UI_LIMIT, opts)
            .candidates;
        let engine = c.iter().take(TOP_N).position(|x| x.text == text);
        wind_candidate::drop_corrected_duplicates(&mut c);
        c.sort_by(|a, b| wind_candidate::candidate_display_order(a, b, false, false, keys));
        let mut seen = std::collections::HashSet::new();
        c.retain(|x| seen.insert(x.text.clone()));
        wind_candidate::protect_top_from_corrected(&mut c, keys.len());
        let ui = c.iter().position(|x| x.text == text);
        (
            engine,
            ui,
            c.first().map(|x| x.text.clone()).unwrap_or_default(),
        )
    };

    let fmt_rank = |r: Option<usize>| r.map_or("-".to_string(), |r| r.to_string());
    let mut tsv = String::from(
        "class\tkeys\texpect\toff_engine_rank\toff_ui_rank\ton_engine_rank\ton_ui_rank\toff_ui_top1\ton_ui_top1\n",
    );
    let mut j = format!("{{\n  \"layout\": \"xiaohe\",\n  \"seed\": {seed},\n  \"classes\": {{\n");
    let classes = [Class::A, Class::B, Class::C];
    for (ci, c) in classes.iter().enumerate() {
        let set = stratified_sample(pools.remove(c.key()).unwrap_or_default(), n_per_class, seed);
        let (mut off, mut on) = (SpScore::default(), SpScore::default());
        let mut unencodable = 0usize;
        for s in &set {
            let syls: Vec<&str> = s.true_syls.iter().map(String::as_str).collect();
            let Some(keys) = rev.encode_all(&syls) else {
                unencodable += 1;
                continue;
            };
            let (oe, ou, ot) = ranks(&keys, &s.text, false);
            let (ne, nu, nt) = ranks(&keys, &s.text, true);
            off.add(oe, ou);
            on.add(ne, nu);
            let _ = writeln!(
                tsv,
                "{}\t{keys}\t{}\t{}\t{}\t{}\t{}\t{ot}\t{nt}",
                c.key(),
                s.text,
                fmt_rank(oe),
                fmt_rank(ou),
                fmt_rank(ne),
                fmt_rank(nu)
            );
        }
        println!(
            "{:<46} OFF {}  ON {}  (不可编码 {unencodable})",
            c.label(),
            off.json(),
            on.json()
        );
        let _ = writeln!(
            j,
            "    \"{}\": {{\"unencodable\": {unencodable}, \"off\": {}, \"on\": {}}}{}",
            c.key(),
            off.json(),
            on.json(),
            if ci + 1 == classes.len() { "" } else { "," }
        );
    }
    j.push_str("  }\n}\n");
    if let Some(p) = out.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    for (path, body) in [
        (out.with_extension("sp.json"), &j),
        (out.with_extension("sp.ranks.tsv"), &tsv),
    ] {
        match std::fs::write(&path, body) {
            Ok(()) => println!("已写入 {}", path.display()),
            Err(e) => eprintln!("写入 {} 失败: {e}", path.display()),
        }
    }
}

const ALL_CLASSES: [Class; 8] = [
    Class::A,
    Class::B,
    Class::C,
    Class::D,
    Class::E,
    Class::F,
    Class::G,
    Class::H,
];

/// 只跑分类统计，不加载引擎（秒级）。用于核对 B 类总量是否与设计文档的 1110 条吻合。
#[test]
#[ignore = "分类统计：需要 build_dev 真实词库"]
fn pinyin_eval_class_census() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：build_dev 拼音词库不存在");
        return;
    };
    let raw = read_all_dicts(&dir);
    let unigram = collect_word_freqs(&raw);

    let trie = SyllableTrie::new();
    let mut reject = HashMap::new();
    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    let mut in_unigram: HashMap<&'static str, usize> = HashMap::new();
    let mut examples: HashMap<&'static str, Vec<String>> = HashMap::new();

    // 与设计文档「1110 条畸形词条」对账用：该数字的判据是「汉字数 > mm 音节数」，
    // 与本 harness 的 B/C 判据不是同一根轴，必须分别统计才能核对。
    let mut fewer_total = 0usize; // chars > mm.len()
    let mut fewer_in_unigram = 0usize;
    let mut fewer_by_class: HashMap<&'static str, usize> = HashMap::new();
    let mut same_len_diff_split = 0usize; // mm.len() == chars 但切法不同

    for (text, code, weight) in &raw {
        if let Some(s) = classify(text, code, *weight, &trie, &mut reject) {
            *counts.entry(s.class.key()).or_default() += 1;
            if unigram.contains_key(&s.text) {
                *in_unigram.entry(s.class.key()).or_default() += 1;
            }
            if s.mm.len() < s.true_syls.len() {
                fewer_total += 1;
                *fewer_by_class.entry(s.class.key()).or_default() += 1;
                if unigram.contains_key(&s.text) {
                    fewer_in_unigram += 1;
                }
            } else if s.mm != s.true_syls {
                same_len_diff_split += 1;
            }
            let e = examples.entry(s.class.key()).or_default();
            if e.len() < 20 {
                e.push(format!(
                    "{}({} → {})",
                    s.text,
                    s.true_syls.join("|"),
                    s.mm.join("|")
                ));
            }
        }
    }

    println!("\n=== 分类普查（引擎实际加载的全部词库，去重前）===");
    println!("原始条目 {}", raw.len());
    for c in [Class::A, Class::B, Class::C] {
        println!(
            "\n{}\n  条目 {}，其中被 unigram 收录 {}",
            c.label(),
            counts.get(c.key()).copied().unwrap_or(0),
            in_unigram.get(c.key()).copied().unwrap_or(0)
        );
        for e in examples.get(c.key()).map(|v| v.as_slice()).unwrap_or(&[]) {
            println!("    {}", e);
        }
    }
    println!("\n=== 与设计文档「1110 条」对账 ===");
    println!(
        "「汉字数 > mm 音节数」共 {} 条（其中 unigram 收录 {}），按本 harness 判据分布: {:?}",
        fewer_total,
        fewer_in_unigram,
        {
            let mut v: Vec<_> = fewer_by_class.iter().collect();
            v.sort();
            v
        }
    );
    println!(
        "「音节数相同但切法不同」共 {} 条（全部落在 C）",
        same_len_diff_split
    );

    let mut rj: Vec<_> = reject.iter().collect();
    rj.sort();
    println!("\n丢弃: {:?}", rj);
}

/// E 类的形态生成与同键去重（纯逻辑，不需要词库，常规 `cargo test` 就跑）。
#[test]
fn short_abbrev_forms_and_dedup() {
    let sample = |text: &str, syls: &[&str], unigram: u64| Sample {
        text: text.into(),
        input: syls.concat(),
        true_syls: syls.iter().map(|s| s.to_string()).collect(),
        mm: Vec::new(),
        weight: unigram,
        unigram,
        class: Class::A,
        parts: Vec::new(),
        form: "",
    };
    // 常用词取词频前 1/3：3 个目标词 + 6 个低频填充，目标词恰好全进。
    let mut pool = vec![
        sample("这个", &["zhe", "ge"], 900),
        sample("整个", &["zheng", "ge"], 800),
        sample("拜城县", &["bai", "cheng", "xian"], 700),
    ];
    for i in 0..6 {
        pool.push(sample(&format!("填{i}"), &["a", "a"], 1));
    }
    let trie = SyllableTrie::new();
    let mut reject = HashMap::new();
    let got: HashMap<String, (String, &'static str)> =
        build_short_abbrev_samples(&pool, &trie, &mut reject)
            .into_iter()
            .map(|s| (s.input, (s.text, s.form)))
            .collect();

    // zh/ch/sh 保留两个字母；三种形态各就各位。
    assert_eq!(got.get("baichx"), Some(&("拜城县".into(), "F")));
    assert_eq!(got.get("bchxian"), Some(&("拜城县".into(), "L")));
    assert_eq!(got.get("bchx"), Some(&("拜城县".into(), "R")));
    // `zhge` 同时是「这个」与「整个」的 L 形态：留词频高的「这个」。
    assert_eq!(got.get("zhge"), Some(&("这个".into(), "L")));
    // 不足 4 字节（`zhg`）不入池。
    assert!(!got.contains_key("zhg"), "{got:?}");
}
