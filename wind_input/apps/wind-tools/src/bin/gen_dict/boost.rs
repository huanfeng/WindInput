//! 词序提升表：对指定 (code, text) 手工调整权重。
//!
//! 在自动权重计算与简码降权**之后**生效，是人工微调的最后一道闸——用来修正
//! 词频统计与实际使用习惯不符的个别条目。
//!
//! 格式（TAB 分隔）：`code<TAB>text[<TAB>adjust]`
//! - adjust 留空 → 顶置（设为该 code 当前最高权重 +1）
//! - adjust = `+N` / `-N` → 相对位置：上移 / 下移 N 位
//! - adjust = `N` → 绝对权重

use crate::entry::Entry;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BoostMode {
    /// 顶置到该编码的首位
    Top,
    /// 相对移动：正数上移、负数下移
    Relative(i64),
    /// 直接指定权重
    Absolute(i64),
}

#[derive(Debug, Clone)]
pub struct BoostRule {
    pub code: String,
    pub text: String,
    pub mode: BoostMode,
}

pub fn load_boost_rules(path: &Path) -> anyhow::Result<Vec<BoostRule>> {
    let f = std::fs::File::open(path)?;
    let mut rules = Vec::new();
    for (i, line) in BufReader::new(f).lines().enumerate() {
        let line_no = i + 1;
        let line = line?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 2 {
            anyhow::bail!("第 {line_no} 行格式错误（需 code<TAB>text [<TAB>adjust]）: {line}");
        }
        let code = parts[0].trim().to_string();
        let text = parts[1].trim().to_string();
        if code.is_empty() || text.is_empty() {
            anyhow::bail!("第 {line_no} 行 code/text 不可为空");
        }
        let mode = match parts.get(2).map(|s| s.trim()).unwrap_or("") {
            "" => BoostMode::Top,
            adj => {
                let n: i64 = adj
                    .parse()
                    .map_err(|_| anyhow::anyhow!("第 {line_no} 行 adjust 解析失败: {adj}"))?;
                // 带符号写法表示相对移动，裸数字是绝对权重
                if adj.starts_with('+') || adj.starts_with('-') {
                    BoostMode::Relative(n)
                } else {
                    BoostMode::Absolute(n)
                }
            }
        };
        rules.push(BoostRule { code, text, mode });
    }
    Ok(rules)
}

/// 按规则调整权重，返回 (生效条数, 未匹配条数)。
///
/// 同一 code 下多条规则按书写顺序依次生效，**后一条看到的是前一条改过的顺序**——
/// 所以每次改动后都要重排该 code 的候选列表。
pub fn apply_boost_rules(
    entries: &mut [Entry],
    rules: &[BoostRule],
    log: &mut dyn FnMut(String),
) -> (usize, usize) {
    if rules.is_empty() {
        return (0, 0);
    }
    let mut idx: HashMap<(String, String), usize> = HashMap::new();
    let mut by_code: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        idx.insert((e.code.clone(), e.text.clone()), i);
        by_code.entry(e.code.clone()).or_default().push(i);
    }

    let (mut applied, mut missing) = (0usize, 0usize);
    for r in rules {
        let Some(&i) = idx.get(&(r.code.clone(), r.text.clone())) else {
            log(format!(
                "        [警告] boost 未匹配: code={} text={}",
                r.code, r.text
            ));
            missing += 1;
            continue;
        };

        let list = by_code.get_mut(&r.code).expect("code 必然已登记");
        sort_by_weight_desc(list, entries);

        match r.mode {
            BoostMode::Absolute(w) => entries[i].weight = w,
            BoostMode::Top => {
                let top = entries[list[0]].weight;
                entries[i].weight = top + 1;
            }
            BoostMode::Relative(delta) => {
                let Some(cur) = list.iter().position(|&v| v == i) else {
                    missing += 1;
                    continue;
                };
                // +N 上移 → 目标索引减小
                let target = (cur as i64 - delta).clamp(0, list.len() as i64 - 1) as usize;
                if target == cur {
                    applied += 1;
                    continue;
                }
                if target < cur {
                    entries[i].weight = entries[list[target]].weight + 1;
                } else {
                    entries[i].weight = entries[list[target]].weight - 1;
                }
            }
        }

        // 该 code 的顺序已变，重排以便后续规则看到新次序
        let list = by_code.get_mut(&r.code).expect("code 必然已登记");
        sort_by_weight_desc(list, entries);
        applied += 1;
    }
    (applied, missing)
}

fn sort_by_weight_desc(list: &mut [usize], entries: &[Entry]) {
    list.sort_by(|&a, &b| {
        entries[b]
            .weight
            .cmp(&entries[a].weight)
            .then_with(|| entries[a].text.cmp(&entries[b].text))
    });
}

#[cfg(test)]
#[path = "boost_tests.rs"]
mod tests;
