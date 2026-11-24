//! 单字反查表与五笔词组取码。
//!
//! 两个用途：修复 extra 里被错填的 code 列，以及给自定义词表反查编码。

use crate::config::Config;
use crate::entry::Entry;
use crate::weight::Unigram;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

pub type CharCodes = HashMap<char, String>;

/// 从 jidian 单字条目建 汉字 → 首选编码 的反查表。
///
/// 每字取权重最高的编码（极点词库里 weight=30 的是首选全码）。
pub fn build_char_code_map(entries: &[Entry]) -> CharCodes {
    let mut best: HashMap<char, (String, i64)> = HashMap::new();
    for e in entries {
        let mut it = e.text.chars();
        let (Some(c), None) = (it.next(), it.next()) else {
            continue; // 只收单字
        };
        match best.get(&c) {
            Some((_, w)) if *w >= e.weight => {}
            _ => {
                best.insert(c, (e.code.clone(), e.weight));
            }
        }
    }
    best.into_iter().map(|(c, (code, _))| (c, code)).collect()
}

/// 按五笔 86 词组取码规则合成编码：
///
/// - 2 字：字1前 2 码 + 字2前 2 码
/// - 3 字：字1首码 + 字2首码 + 字3前 2 码
/// - 4 字及以上：字1/2/3 首码 + **末字**首码
///
/// 任一取码所需的字不在反查表中即返回 `None`——宁可丢弃也不产出错码。
pub fn encode_phrase(text: &str, char_codes: &CharCodes) -> Option<String> {
    let runes: Vec<char> = text.chars().collect();
    if runes.is_empty() {
        return None;
    }
    let get = |r: char| char_codes.get(&r);
    // 取前 n 码，不足补 'l'（五笔末笔识别码占位）
    let prefix = |code: &str, n: usize| -> String {
        let mut s: String = code.chars().take(n).collect();
        while s.chars().count() < n {
            s.push('l');
        }
        s
    };

    match runes.len() {
        1 => get(runes[0]).cloned(),
        2 => {
            let (c1, c2) = (get(runes[0])?, get(runes[1])?);
            Some(format!("{}{}", prefix(c1, 2), prefix(c2, 2)))
        }
        3 => {
            let (c1, c2, c3) = (get(runes[0])?, get(runes[1])?, get(runes[2])?);
            Some(format!(
                "{}{}{}",
                prefix(c1, 1),
                prefix(c2, 1),
                prefix(c3, 2)
            ))
        }
        n => {
            let (c1, c2, c3, cl) = (
                get(runes[0])?,
                get(runes[1])?,
                get(runes[2])?,
                get(runes[n - 1])?,
            );
            Some(format!(
                "{}{}{}{}",
                prefix(c1, 1),
                prefix(c2, 1),
                prefix(c3, 1),
                prefix(cl, 1)
            ))
        }
    }
}

/// 加载自定义词表：每行一词，可选 `<TAB>频率`（当前仅作说明性字段，不参与计算），
/// `#` 开头为注释。编码由 [`encode_phrase`] 反查，权重优先取 unigram。
pub fn load_custom_words(
    path: &Path,
    char_codes: &CharCodes,
    unigram: &Unigram,
    log_median: f64,
    cfg: &Config,
    log: &mut dyn FnMut(String),
) -> anyhow::Result<Vec<Entry>> {
    let f = std::fs::File::open(path)?;
    let mut entries = Vec::new();
    let mut skipped = 0usize;

    for line in BufReader::new(f).lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let word = line.split('\t').next().unwrap_or("").trim();
        if word.is_empty() {
            continue;
        }
        let Some(code) = encode_phrase(word, char_codes) else {
            log(format!("        [跳过] 无法编码: {word}"));
            skipped += 1;
            continue;
        };

        // 未命中 unigram 时给 target_median，使自定义词落在中位档而非沉底
        let mut weight = cfg.target_median;
        if log_median > 0.0
            && let Some(&freq) = unigram.get(word)
        {
            let w = cfg.target_median as f64 * ((freq as f64) + 1.0).log10() / log_median;
            weight = (w.round() as i64).clamp(cfg.weight_min, cfg.weight_max);
        }
        entries.push(Entry::new(word.to_string(), code, weight, 0));
    }
    if skipped > 0 {
        log(format!("        跳过 {skipped} 条（无法反查编码）"));
    }
    Ok(entries)
}

#[cfg(test)]
#[path = "reverse_tests.rs"]
mod tests;
