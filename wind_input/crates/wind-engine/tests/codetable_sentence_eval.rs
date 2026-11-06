//! 码表整句在**真实句子**上的评测：首选 / 前 3 / 前 5 还原率。
//!
//! ```text
//! WIND_CT_CORPUS=sentences.txt \
//!   cargo test --release -p wind-engine --test codetable_sentence_eval -- --ignored --nocapture
//! ```
//!
//! 语料每行一句；带制表符的行取最后一列（直接吃 `scripts/lm/sentence_eval_prep.py` 的产物）。
//! 需要 `build_dev/data`（五笔词库、拼音词库、`pinyin/grammar/wanxiang-mini.gram`），
//! 或用 `WIND_CT_SCHEMAS` 指到别处的 `data/schemas`。
//!
//! # 怎么从句子造出码串
//!
//! 四种打法，模拟两个维度：
//!
//! - **按词还是按字**：`word_*` 用码表词库正向最大匹配切词，每个词打它的码；`char_*` 逐字打。
//!   后者是「这个词码表里没有」的极端情形，词典组词主要救的就是它。
//! - **全码还是最短码**：`*_full` 打每个单元最长的码，`*_short` 打最短的（五笔用户的简码习惯）。
//!
//! ⚠️ 两个维度都是理想化的：真人不会全打全码或全打简码，切词也不是机械的最大匹配。
//! 这里的数字只适合**横向比较**（改动前后、参数之间），不能当真实准确率报。
//!
//! # 基线（2026-11-06，Tatoeba 3000 句，release）
//!
//! | 打法 | 首选（无模型 → 语法 0.5） | 前 3（语法 0.5） |
//! |---|---|---|
//! | 词组全码 | 89.2 → 95.5 | 99.3 |
//! | 词组最短码 | 74.8 → 90.0 | 94.6 |
//! | 逐字全码 | 98.4 → 98.2 | 99.9 |
//! | 逐字最短码 | 70.5 → 89.4 | 94.1 |
//!
//! 「无模型」一列已含词典组词。改动前（无组词、无模型）是 86.5 / 56.8 / 83.5 / 7.7。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use wind_dict::cached::CachedDict;
use wind_dict::{DictManager, SystemDictLayer};
use wind_engine::codetable::CodeSentenceDecoder;
use wind_engine::codetable::sentence::GRAMMAR_MODEL;

const MAX_CODE_LEN: usize = 4;

/// 数据目录。worktree 里没有 build_dev 时用 `WIND_CT_SCHEMAS` 指到主仓那份。
fn schemas_dir() -> PathBuf {
    std::env::var("WIND_CT_SCHEMAS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data/schemas")
        })
}

struct Fixture {
    dm: DictManager,
    /// text → 最长码 / 最短码。
    full: HashMap<String, String>,
    short: HashMap<String, String>,
}

fn fixture() -> Option<Fixture> {
    let base = schemas_dir().join("wubi86");
    let dm = DictManager::new();
    let mut full: HashMap<String, String> = HashMap::new();
    let mut short: HashMap<String, String> = HashMap::new();
    for name in ["wubi86_jidian", "wubi86_jidian_extra"] {
        let yaml = base.join(format!("{name}.dict.yaml"));
        if !yaml.exists() {
            return None;
        }
        let wdat = base.join(format!("{name}.dict.wdat"));
        let d = CachedDict::load_at_with(&yaml, &wdat, false).ok()?;
        d.for_each_entry(&mut |code, text, _w| {
            if !code.chars().all(|c| c.is_ascii_lowercase()) {
                return;
            }
            let longer = |c: &mut String| {
                if code.len() > c.len() {
                    *c = code.to_string();
                }
            };
            let shorter = |c: &mut String| {
                if code.len() < c.len() {
                    *c = code.to_string();
                }
            };
            full.entry(text.to_string())
                .and_modify(longer)
                .or_insert_with(|| code.to_string());
            short
                .entry(text.to_string())
                .and_modify(shorter)
                .or_insert_with(|| code.to_string());
        });
        dm.register_layer(Box::new(SystemDictLayer::new(d, name)));
    }
    Some(Fixture { dm, full, short })
}

/// 正向最大匹配。任何一个字码表里没有 ⇒ `None`（这句不参评）。
fn fmm(s: &str, known: &HashMap<String, String>) -> Option<Vec<String>> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let l = (1..=(chars.len() - i).min(8))
            .rev()
            .find(|&l| known.contains_key(&chars[i..i + l].iter().collect::<String>()))?;
        out.push(chars[i..i + l].iter().collect());
        i += l;
    }
    Some(out)
}

/// `(期望文本, 码串)`。
fn cases(fx: &Fixture, sentences: &[String], style: &str) -> Vec<(String, String)> {
    let table = if style.ends_with("short") {
        &fx.short
    } else {
        &fx.full
    };
    sentences
        .iter()
        .filter_map(|s| {
            let units = if style.starts_with("word") {
                fmm(s, &fx.full)?
            } else {
                s.chars()
                    .map(|c| Some(c.to_string()).filter(|w| fx.full.contains_key(w)))
                    .collect::<Option<Vec<_>>>()?
            };
            (units.len() >= 2)
                .then(|| (s.clone(), units.iter().map(|u| table[u].as_str()).collect()))
        })
        .collect()
}

fn corpus() -> Option<Vec<String>> {
    let path = std::env::var("WIND_CT_CORPUS").ok()?;
    let text = std::fs::read_to_string(path).ok()?;
    Some(
        text.lines()
            .filter_map(|l| l.rsplit('\t').next())
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect(),
    )
}

#[test]
#[ignore = "评测：依赖 build_dev 真实词库与 WIND_CT_CORPUS 语料"]
fn codetable_sentence_eval() {
    let (Some(fx), Some(sentences)) = (fixture(), corpus()) else {
        eprintln!("!!! 跳过：build_dev 五笔词库不存在或未设 WIND_CT_CORPUS");
        return;
    };
    let fx = Arc::new(fx);
    let gram = schemas_dir().join("pinyin/grammar").join(GRAMMAR_MODEL);
    let decoders = [
        (
            "无模型",
            CodeSentenceDecoder::new(MAX_CODE_LEN).with_schemas_dir(schemas_dir()),
        ),
        (
            "语法 0.5",
            CodeSentenceDecoder::new(MAX_CODE_LEN)
                .with_schemas_dir(schemas_dir())
                .with_grammar_file(gram, 0.5),
        ),
    ];
    for style in ["word_full", "word_short", "char_full", "char_short"] {
        let cs = cases(&fx, &sentences, style);
        println!("\n=== {style}：{} 句 ===", cs.len());
        for (name, dec) in &decoders {
            let mut hit = [0usize; 3];
            let t0 = std::time::Instant::now();
            for (want, code) in &cs {
                let pos = dec
                    .decode_nbest(code, &fx.dm, 5)
                    .iter()
                    .position(|r| &r.text == want);
                for (h, k) in hit.iter_mut().zip([1, 3, 5]) {
                    *h += usize::from(pos.is_some_and(|p| p < k));
                }
            }
            let pct = |h: usize| h as f64 * 100.0 / cs.len() as f64;
            println!(
                "  {name:<8} 首选 {:>5.1}%  前3 {:>5.1}%  前5 {:>5.1}%   ({:.2} ms/句)",
                pct(hit[0]),
                pct(hit[1]),
                pct(hit[2]),
                t0.elapsed().as_secs_f64() * 1000.0 / cs.len() as f64
            );
        }
    }
}
