//! 从白霜基础词库生成低权重的五笔补充词条。

use crate::entry::{Entry, is_valid_code};
use crate::reverse::{CharCodes, encode_phrase};
use std::collections::{HashMap, HashSet};
use std::path::Path;

const MIN_FREQUENCY: u64 = 1000;

pub fn generate(
    source: &Path,
    main: &[Entry],
    extra: &[Entry],
    main_floor: i64,
) -> anyhow::Result<Vec<Entry>> {
    if main_floor <= 1 {
        anyhow::bail!("主词库最低权重必须大于 1，才能生成低权重补充词库");
    }
    let content = std::fs::read_to_string(source)
        .map_err(|e| anyhow::anyhow!("读取白霜基础词库失败 {}: {e}", source.display()))?;
    let normalized = wind_utils::text::normalize_input(&content);
    let mut best: HashMap<char, &Entry> = HashMap::new();
    for entry in main.iter().chain(extra) {
        let mut chars = entry.text.chars();
        let (Some(ch), None) = (chars.next(), chars.next()) else {
            continue;
        };
        let len = entry.code.len();
        if !(2..=4).contains(&len) || !is_valid_code(&entry.code) {
            continue;
        }
        let replace = best.get(&ch).is_none_or(|cur| {
            len > cur.code.len()
                || (len == cur.code.len()
                    && (entry.weight > cur.weight
                        || (entry.weight == cur.weight && entry.code < cur.code)))
        });
        if replace {
            best.insert(ch, entry);
        }
    }
    let codes: CharCodes = best
        .into_iter()
        .map(|(ch, e)| (ch, e.code.clone()))
        .collect();
    let existing: HashSet<&str> = main.iter().chain(extra).map(|e| e.text.as_str()).collect();
    let mut frequencies: HashMap<String, u64> = HashMap::new();
    let mut in_body = false;
    let mut eligible = 0usize;
    for line in normalized.lines() {
        if !in_body {
            if line.trim() == "..." {
                in_body = true;
            }
            continue;
        }
        let mut columns = line.split('\t');
        let Some(text) = columns.next() else { continue };
        let _pinyin = columns.next();
        let Some(freq) = columns.next().and_then(|v| v.trim().parse::<u64>().ok()) else {
            continue;
        };
        if freq < MIN_FREQUENCY
            || !(2..=10).contains(&text.chars().count())
            || !text.chars().all(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
        {
            continue;
        }
        eligible += 1;
        if existing.contains(text) {
            continue;
        }
        frequencies
            .entry(text.to_owned())
            .and_modify(|f| *f = (*f).max(freq))
            .or_insert(freq);
    }
    if !in_body {
        anyhow::bail!("白霜基础词库缺少正文分隔符: {}", source.display());
    }
    if eligible == 0 {
        anyhow::bail!("白霜基础词库没有符合条件的高频词: {}", source.display());
    }
    let total = frequencies.len();
    let mut out: Vec<Entry> = frequencies
        .into_iter()
        .filter_map(|(text, freq)| {
            let code = encode_phrase(&text, &codes)?;
            let weight = ((freq as f64).log10() * 10.0).round() as i64;
            Some(Entry::new(text, code, weight.clamp(1, main_floor - 1), 0))
        })
        .collect();
    eprintln!(
        "      白霜补充词库跳过 {} 条无法编码的词",
        total - out.len()
    );
    if total > 0 && out.is_empty() {
        anyhow::bail!("白霜高频缺词均无法按五笔单字码编码");
    }
    out.sort_by(|a, b| {
        a.code
            .cmp(&b.code)
            .then_with(|| b.weight.cmp(&a.weight))
            .then_with(|| a.text.cmp(&b.text))
    });
    Ok(out)
}

#[cfg(test)]
#[path = "frost_tests.rs"]
mod tests;
