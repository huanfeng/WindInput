//! `CharPinyinIndex` 紧凑化（`docs/design/memory-footprint.md` S5）的等价对拍。
//!
//! [`LegacyCharPinyinIndex`] 是紧凑化**之前**的实现原样搬来（两张 `HashMap`，逐条 `String`），
//! 只留在测试里当参照。在 `build_dev/data` 的真实拼音词库上：
//!
//! 1. 逐字比：两边收录的字集合相同，每个字的代表读音、全部读音（含顺序）逐一相同；
//! 2. 上游抽样：一批真实词（词库里两三音节的词条 + 手挑的多音字长词 / 混排词）上，
//!    `generate_word_pinyin` / `boundary_by_char_count` 两边产出相同；引擎层的
//!    `EngineManager::word_pinyin_syllables` / `resolve_boundaries` 与「用旧表照原流程算」相同。
//!
//! ⚠️ 依赖 `build_dev/data`；缺失时**静默跳过**（判据：耗时 0.00s）。

use std::collections::HashMap;

use wind_dict::cached::CachedDict;

use super::super::syllable::{STANDARD_SYLLABLES, SyllableTrie};
use super::{CharPinyinIndex, ReadingTable, boundary_by_char_count, generate_word_pinyin};
use crate::engine::BoundaryResolution;

/// 紧凑化之前的 `CharPinyinIndex`（逐字搬运，只改了名字）。
#[derive(Debug, Default)]
struct LegacyCharPinyinIndex {
    char: HashMap<char, String>,
    char_all: HashMap<char, Vec<String>>,
}

impl LegacyCharPinyinIndex {
    fn build(dict: &CachedDict) -> Self {
        let mut all: HashMap<char, Vec<(String, i32)>> = HashMap::new();
        for &syl in STANDARD_SYLLABLES {
            for (text, weight, _order) in dict.search(syl) {
                let mut chars = text.chars();
                let (Some(c), None) = (chars.next(), chars.next()) else {
                    continue;
                };
                let entry = all.entry(c).or_default();
                if let Some(e) = entry.iter_mut().find(|(s, _)| s == syl) {
                    if weight > e.1 {
                        e.1 = weight;
                    }
                } else {
                    entry.push((syl.to_string(), weight));
                }
            }
        }
        let mut char = HashMap::with_capacity(all.len());
        let mut char_all = HashMap::with_capacity(all.len());
        for (c, mut list) in all {
            list.sort_by_key(|(_, w)| std::cmp::Reverse(*w));
            let readings: Vec<String> = list.into_iter().map(|(s, _)| s).collect();
            char.insert(c, readings[0].clone());
            char_all.insert(c, readings);
        }
        Self { char, char_all }
    }
}

impl ReadingTable for LegacyCharPinyinIndex {
    fn representative(&self, c: char) -> Option<&str> {
        self.char.get(&c).map(String::as_str)
    }

    fn reading_count(&self, c: char) -> Option<usize> {
        self.char_all.get(&c).map(Vec::len)
    }

    fn reading(&self, c: char, i: usize) -> Option<&str> {
        self.char_all.get(&c)?.get(i).map(String::as_str)
    }
}

fn data_dir() -> Option<std::path::PathBuf> {
    let d = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    d.join("schemas/pinyin/rime_frost.dict.yaml")
        .exists()
        .then_some(d)
}

/// 某个表上该字的全部读音（按序）。
fn readings_of(t: &impl ReadingTable, c: char) -> Option<Vec<String>> {
    let n = t.reading_count(c)?;
    Some(
        (0..n)
            .map(|i| t.reading(c, i).expect("下标 < reading_count").to_string())
            .collect(),
    )
}

/// 一批真实词条 `(code, text)`：取常用 / 多音音节两两、三三拼起来的码在词库里的多字词，
/// 外加手挑的多音字长词与中英混排词（后者 code 给代表读音拼法，供边界求解走无读音分支）。
fn sample_words(dict: &CachedDict) -> Vec<(String, String)> {
    const SYLS: &[&str] = &[
        "zhong", "chong", "chang", "zhang", "hang", "xing", "yin", "le", "yue", "de", "di", "shi",
        "ren", "guo", "da", "xue", "sheng", "huo", "dian", "nao", "ji", "jia", "you", "kai", "xin",
        "tian", "qi", "wen", "ti", "fa", "jing", "hui", "ke", "li", "shu", "dao", "xiang", "yao",
        "qu", "lai", "bu", "yi", "yang", "shan", "shui", "hua", "feng", "yu", "che", "lu", "zhe",
        "zhao", "zhuo", "can", "shen", "cen", "ba", "pa", "mo", "wei",
    ];
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut take = |code: String, out: &mut Vec<(String, String)>| {
        for (text, _, _) in dict.search(&code) {
            if text.chars().count() >= 2 && seen.insert((code.clone(), text.clone())) {
                out.push((code.clone(), text));
            }
        }
    };
    for a in SYLS {
        for b in SYLS {
            take(format!("{a}{b}"), &mut out);
        }
    }
    for a in &SYLS[..20] {
        for b in &SYLS[..20] {
            for c in &SYLS[..20] {
                take(format!("{a}{b}{c}"), &mut out);
            }
        }
    }
    for (code, text) in [
        ("hangzhang", "行长"),
        ("yinhang", "银行"),
        ("chongqing", "重庆"),
        ("changjiangsanjiaozhou", "长江三角洲"),
        ("zhongguorenminyinhang", "中国人民银行"),
        ("yinyuehui", "音乐会"),
        ("agu", "A股"),
        ("kalaok", "卡拉OK"),
        ("xv", "需"),
        ("nvren", "女人"),
    ] {
        out.push((code.to_string(), text.to_string()));
    }
    out
}

/// 引擎层 `resolve_boundary` 的原流程（`pinyin/mod.rs`），把读音表换成 `t`。
fn resolve_with(
    mgr: &crate::EngineManager,
    dict: &CachedDict,
    t: &impl ReadingTable,
    trie: &SyllableTrie,
    code: &str,
    text: &str,
) -> BoundaryResolution {
    let code = super::super::spelling::normalize_u_umlaut(code);
    let code = code.as_ref();
    let exact = mgr.syllable_boundary_of("pinyin", code, text);
    if exact != 0 {
        return BoundaryResolution::Exact(exact);
    }
    if code.len() > 64 {
        return BoundaryResolution::NoInfo;
    }
    let Some(sol) = boundary_by_char_count(t, trie, code, text) else {
        return BoundaryResolution::Unresolvable;
    };
    if sol.no_reading {
        return BoundaryResolution::NoReading(sol.mask);
    }
    if !sol.ambiguous {
        return BoundaryResolution::Derived(sol.mask);
    }
    if let Some(spaced) = generate_word_pinyin(dict, t, text) {
        let (flat, derived) = wind_store::wdict::split_spaced_code(&spaced);
        if flat == code && derived != 0 {
            return BoundaryResolution::Derived(derived);
        }
    }
    BoundaryResolution::Ambiguous(sol.mask)
}

#[test]
fn compact_index_matches_legacy_on_real_dict() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：缺 build_dev/data 拼音词库");
        return;
    };
    // 先建管理器：它设好缓存根，下面单独加载词库时 merged.wdat 落进缓存目录而不是数据目录。
    let mut cfg = wind_config::Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    let mgr = crate::EngineManager::new(&cfg, Some(&dir));
    let dict =
        crate::EngineManager::load_sentence_freq_dict(&dir.join("schemas")).expect("加载拼音词库");

    let legacy = LegacyCharPinyinIndex::build(&dict);
    let compact = CharPinyinIndex::build(&dict);

    // ① 逐字：字集合相同、代表读音与全部读音（含顺序）逐一相同。
    assert!(
        legacy.char.len() > 40_000,
        "前提：真实词库（{} 字）",
        legacy.char.len()
    );
    assert_eq!(compact.char_count(), legacy.char.len(), "收录字数");
    let mut diffs = Vec::new();
    for (&c, all) in &legacy.char_all {
        let got = (compact.representative(c), readings_of(&compact, c));
        let want = (legacy.char.get(&c).map(String::as_str), Some(all.clone()));
        if got != want {
            diffs.push(format!("{c}: 紧凑={got:?} 旧={want:?}"));
        }
    }
    assert!(
        diffs.is_empty(),
        "{} 个字不一致，前 10：{:?}",
        diffs.len(),
        &diffs[..diffs.len().min(10)]
    );
    // 没收录的字（旧表里查不到的）两边都是 None；越界下标也是 None。
    for c in ['😀', '\u{10FFFF}', '\u{1}'] {
        assert_eq!(legacy.reading_count(c), None, "前提：旧表没收录 {c:?}");
        assert_eq!(compact.representative(c), None);
        assert_eq!(compact.reading_count(c), None);
        assert_eq!(compact.reading(c, 0), None);
    }
    assert_eq!(compact.reading('中', 99), None);

    // ② 上游抽样。
    let words = sample_words(&dict);
    assert!(words.len() > 2_000, "前提：样本够大（{}）", words.len());
    let trie = SyllableTrie::new();
    let mut bad = Vec::new();
    for (code, text) in &words {
        let (a, b) = (
            generate_word_pinyin(&dict, &compact, text),
            generate_word_pinyin(&dict, &legacy, text),
        );
        if a != b {
            bad.push(format!("generate_word_pinyin {text}: {a:?} vs {b:?}"));
        }
        let key = |s: Option<super::BoundarySolve>| s.map(|s| (s.mask, s.ambiguous, s.no_reading));
        let (a, b) = (
            key(boundary_by_char_count(&compact, &trie, code, text)),
            key(boundary_by_char_count(&legacy, &trie, code, text)),
        );
        if a != b {
            bad.push(format!(
                "boundary_by_char_count {code}/{text}: {a:?} vs {b:?}"
            ));
        }
        let (a, b) = (
            mgr.word_pinyin_syllables(text),
            generate_word_pinyin(&dict, &legacy, text).unwrap_or_default(),
        );
        if a != b {
            bad.push(format!("word_pinyin_syllables {text}: {a:?} vs {b:?}"));
        }
    }
    let pairs: Vec<(&str, &str)> = words
        .iter()
        .map(|(c, t)| (c.as_str(), t.as_str()))
        .collect();
    let got = mgr.resolve_boundaries("pinyin", &pairs);
    for ((code, text), g) in pairs.iter().zip(&got) {
        let want = resolve_with(&mgr, &dict, &legacy, &trie, code, text);
        if *g != want {
            bad.push(format!("resolve_boundary {code}/{text}: {g:?} vs {want:?}"));
        }
    }
    assert!(
        bad.is_empty(),
        "{} 处不一致（样本 {} 条），前 10：{:?}",
        bad.len(),
        words.len(),
        &bad[..bad.len().min(10)]
    );
    eprintln!(
        "对拍通过：{} 字 / {} 条读音逐一相同；上游样本 {} 条 × 4 个入口相同",
        compact.char_count(),
        compact.total_readings(),
        words.len()
    );
}
