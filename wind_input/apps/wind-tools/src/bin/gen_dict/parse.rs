//! rime `.dict.yaml` 词库解析。
//!
//! 两个来源的列布局不同，故分两个函数：
//! - jidian 主库：头部声明 `columns:`，按列名定位（无声明时退回 `[text, code, weight]`）
//! - extra 扩展库：无 `columns:` 声明，固定 `text<TAB>code[<TAB>weight]`
//!
//! 列名缺省顺序是 librime 的规定（见记忆 reference_rime_dict_yaml_format），
//! **不是**猜的——按行内容猜列序正是本仓踩过的坑。

use crate::entry::{Entry, is_valid_code};
use crate::reverse::{CharCodes, encode_phrase};
use std::io::{BufRead, BufReader};
use std::path::Path;

/// 解析 jidian 主词库。
pub fn parse_jidian(path: &Path) -> anyhow::Result<Vec<Entry>> {
    let f = std::fs::File::open(path)
        .map_err(|e| anyhow::anyhow!("打开 jidian 失败 {}: {e}", path.display()))?;

    let (mut col_text, mut col_code, mut col_weight) = (0i32, 1i32, 2i32);
    let mut in_header = true;
    let mut in_columns = false;
    let mut col_names: Vec<String> = Vec::new();
    let mut entries = Vec::new();

    for line in BufReader::new(f).lines() {
        let line = line?;
        if in_header {
            let trimmed = line.trim();
            if trimmed == "..." {
                if !col_names.is_empty() {
                    // 有显式声明就完全以它为准：未出现的列标 -1（取值恒为空）
                    col_text = -1;
                    col_code = -1;
                    col_weight = -1;
                    for (i, name) in col_names.iter().enumerate() {
                        match name.as_str() {
                            "text" => col_text = i as i32,
                            "code" => col_code = i as i32,
                            "weight" => col_weight = i as i32,
                            _ => {}
                        }
                    }
                }
                in_header = false;
                continue;
            }
            if trimmed.starts_with("columns:") {
                in_columns = true;
                col_names.clear();
                continue;
            }
            if in_columns {
                if let Some(name) = trimmed.strip_prefix("- ") {
                    let name = match name.find('#') {
                        Some(i) => &name[..i],
                        None => name,
                    };
                    let name = name.trim();
                    if !name.is_empty() {
                        col_names.push(name.to_string());
                    }
                } else if !trimmed.is_empty() && !trimmed.starts_with('#') {
                    in_columns = false;
                }
            }
            continue;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = trimmed.split('\t').collect();
        let get = |idx: i32| -> &str {
            if idx < 0 || idx as usize >= parts.len() {
                ""
            } else {
                parts[idx as usize].trim()
            }
        };
        let text = get(col_text);
        let code = get(col_code);
        if text.is_empty() || code.is_empty() {
            continue;
        }
        // 缺省优先级 10 = jidian 的最低档，与 Go 版一致
        let weight = match get(col_weight) {
            "" => 10,
            s => s.parse::<i64>().ok().filter(|w| *w > 0).unwrap_or(10),
        };
        let pos = entries.len();
        entries.push(Entry::new(text.to_string(), code.to_string(), weight, pos));
    }
    Ok(entries)
}

/// 解析 extra 扩展词库。
///
/// 源数据偶有 code 列被错填（如 `白狐<TAB>白狐<TAB>5` 把词本身填进编码列）。这类行
/// 会用单字反查表按五笔词组取码规则重新合成；合成不出来（缺字）才丢弃。纯 a-z 编码
/// （含英文桶里的 z 码如 `brz`）视为合法直接放行，不做五笔 a-y 校验。
pub fn parse_extra_dict(
    path: &Path,
    char_codes: &CharCodes,
    log: &mut dyn FnMut(String),
) -> anyhow::Result<Vec<Entry>> {
    let f = std::fs::File::open(path)
        .map_err(|e| anyhow::anyhow!("打开 extra 失败 {}: {e}", path.display()))?;

    let mut in_header = true;
    let mut entries = Vec::new();
    let mut pos = 0usize;

    for line in BufReader::new(f).lines() {
        let line = line?;
        if in_header {
            if line.trim() == "..." {
                in_header = false;
            }
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // 按原始行切分（与 Go 一致）：行首若有缩进，text 由后续 trim 兜住
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 2 {
            continue;
        }
        let text = parts[0].trim().to_string();
        // 容错：源数据偶有大写 code（如 "api<TAB>API"）
        let mut code = parts[1].trim().to_lowercase();
        if text.is_empty() || code.is_empty() {
            continue;
        }

        if !code.chars().all(|c| c.is_ascii_lowercase()) {
            match encode_phrase(&text, char_codes) {
                Some(fixed) if is_valid_code(&fixed) => {
                    log(format!(
                        "      [extra] 修正非法编码: {text:?}  {code:?} → {fixed:?}"
                    ));
                    code = fixed;
                }
                _ => {
                    log(format!(
                        "      [extra] 跳过非法编码行: {text:?} (code={code:?}，无法按五笔规律合成)"
                    ));
                    continue;
                }
            }
        }

        let weight = parts
            .get(2)
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(0);
        entries.push(Entry::new(text, code, weight, pos));
        pos += 1;
    }
    Ok(entries)
}

#[cfg(test)]
#[path = "parse_tests.rs"]
mod tests;
