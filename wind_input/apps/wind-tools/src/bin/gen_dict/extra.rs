//! 扩展词库拆分：把 jidian_extra 按字符类型分成 4 个独立文件。
//!
//! 拆分的意义是让用户能单独开关：emoji 和英文词条对纯中文输入是噪音，
//! 但对另一部分用户是刚需。分成独立词库后由方案的 `[[dictionaries]]` 各自控制。

use crate::config::Config;
use crate::entry::{Entry, has_cjk, has_emoji};
use crate::reverse::{CharCodes, encode_phrase};
use crate::weight::{Unigram, compute_weight, fallback_weight};
use std::io::{BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    /// 含 CJK 的中文词条（主类）
    Cjk,
    /// 含 emoji 的条目
    Emoji,
    /// 全 ASCII 且含字母的英文/品牌词
    English,
    /// 其余：非 CJK、非 emoji、非 ASCII 字母的特殊符号
    Symbol,
}

impl Category {
    pub fn suffix(self) -> &'static str {
        match self {
            Category::Cjk => "extra",
            Category::Emoji => "emoji",
            Category::English => "english",
            Category::Symbol => "symbols",
        }
    }

    pub const ALL: [Category; 4] = [
        Category::Cjk,
        Category::Emoji,
        Category::English,
        Category::Symbol,
    ];
}

/// 按 text 的字符构成归类。
///
/// 优先级 emoji > CJK > english > symbol：emoji 优先是为了让「🐶 + 中文备注」这类
/// 条目落进 emoji 桶而不是 CJK 桶——用户关掉 emoji 库时期待它一起消失。
pub fn classify(text: &str) -> Category {
    if has_emoji(text) {
        return Category::Emoji;
    }
    if has_cjk(text) {
        return Category::Cjk;
    }
    let mut only_ascii = true;
    let mut has_letter = false;
    for c in text.chars() {
        if !('\u{20}'..='\u{7E}').contains(&c) {
            only_ascii = false;
            break;
        }
        if c.is_ascii_alphabetic() {
            has_letter = true;
        }
    }
    if only_ascii && has_letter {
        Category::English
    } else {
        Category::Symbol
    }
}

/// 把扩展库 CJK 桶的权重**线性压缩**进扩展带 `[weight_min, extra.weight_max]`，
/// 使整库恒低于主库最低档，同时保留库内相对次序。
///
/// 不用 default_weight 整库抹平：那会丢掉库内的真实词频差异。必须在数据侧做：引擎排序链
/// 是 `weight 降 → base_order 升 → natural_order 升`，`base_order` 只在等权时起作用，
/// 压不住权重更高的扩展库词条（见 `docs/design/dict-weight-normalization.md`）。
///
/// 压缩后同权条目增多不影响呈现：写出前按「code 升 → weight 降」排序，文件序即压缩前的
/// 权重序，引擎等权时落到 `natural_order` 仍按它排。
fn compress_into_extra_band(list: &mut [Entry], cfg: &Config) {
    let cap = cfg.extra.weight_max;
    if cap <= 0 || list.is_empty() {
        return; // 0 = 不压缩（旧行为）
    }
    let lo_bound = cfg.weight_min.max(1);
    let (Some(min), Some(max)) = (
        list.iter().map(|e| e.weight).min(),
        list.iter().map(|e| e.weight).max(),
    ) else {
        return;
    };
    // 全库同权：直接落在带顶，相对序本就由文件序承载。
    if max <= min {
        for e in list.iter_mut() {
            e.weight = cap;
        }
        return;
    }
    let span = (cap - lo_bound).max(0) as f64;
    let range = (max - min) as f64;
    for e in list.iter_mut() {
        let ratio = (e.weight - min) as f64 / range;
        e.weight = lo_bound + (ratio * span).round() as i64;
    }
}

/// 给各桶赋权：CJK 走 unigram 归一化，其余桶保留原权重、缺失则给兜底值。
pub fn assign_weights(
    buckets: &mut [(Category, Vec<Entry>)],
    unigram: &Unigram,
    log_median: f64,
    cfg: &Config,
) -> (usize, usize) {
    let default_weight = if cfg.extra.default_weight > 0 {
        cfg.extra.default_weight
    } else {
        100
    };
    let (mut cjk_hit, mut cjk_total) = (0usize, 0usize);

    for (cat, list) in buckets.iter_mut() {
        match cat {
            Category::Cjk => {
                cjk_total = list.len();
                for e in list.iter_mut() {
                    if log_median > 0.0
                        && let Some(&freq) = unigram.get(&e.text)
                    {
                        e.weight = compute_weight(freq, log_median, cfg);
                        cjk_hit += 1;
                        continue;
                    }
                    // 未命中：有原始优先级就按档保底，否则落最低档
                    e.weight = if e.weight > 0 {
                        fallback_weight(e.weight, cfg)
                    } else {
                        cfg.fallback.priority_10
                    };
                }
                compress_into_extra_band(list, cfg);
            }
            _ => {
                for e in list.iter_mut() {
                    if e.weight <= 0 {
                        e.weight = default_weight;
                    }
                }
            }
        }
    }
    (cjk_hit, cjk_total)
}

/// 加载自定义 emoji 列表：每行一个，按行序从 200 递减分配权重，编码固定 `emoj`。
///
/// 递减权重是这批常用表情的**展示顺序**载体（😂 > 🤣 > 😊…）。方案侧因此绝不能给
/// emoji 库配 `default_weight`——那会把整库抹平成文件序，毁掉这里的排序意图。
pub fn load_custom_emoji(path: Option<&Path>) -> anyhow::Result<Vec<Entry>> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    if !path.exists() {
        return Ok(Vec::new());
    }
    let f = std::fs::File::open(path)?;
    let mut emojis = Vec::new();
    for line in BufReader::new(f).lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        emojis.push(line.to_string());
    }

    const BASE_WEIGHT: i64 = 200;
    Ok(emojis
        .into_iter()
        .enumerate()
        .map(|(i, emoji)| {
            let w = (BASE_WEIGHT - i as i64).max(1);
            Entry::new(emoji, "emoj".into(), w, 0)
        })
        .collect())
}

/// named emoji 的权重档位。
///
/// 三档必须严格递减且都高于 `extra.default_weight`(100)，使同码内 CLDR 主名 > CLDR
/// 关键词 > 上游条目。tts 单独抬一档：它是每个 emoji 唯一的确定名称（主名之间不共享）。
const NAMED_TTS_WEIGHT: i64 = 130;
const NAMED_KEYWORD_WEIGHT: i64 = 110;

/// 加载 emoji 中文命名表，把中文名反查成五笔码。
///
/// 输入是 gen_emoji_names 的产物，每行 `emoji<TAB>中文名<TAB>tts|kw`。编码不在表里——
/// 这里用与自定义词表同一套 [`encode_phrase`] 现场反查，于是「⚽ 足球」得到 `khgf`，
/// 与上游 rime-wubi 手工编的码天然一致。
///
/// 反查失败（中文名含反查表里没有的字）时跳过该行并计数，绝不产出错码。
pub fn load_named_emoji(
    path: Option<&Path>,
    char_codes: &CharCodes,
    log: &mut dyn FnMut(String),
) -> anyhow::Result<Vec<Entry>> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    if !path.exists() {
        log(format!(
            "      [emoji_named] 命名表不存在，跳过: {}",
            path.display()
        ));
        return Ok(Vec::new());
    }

    let f = std::fs::File::open(path)?;
    let mut entries = Vec::new();
    let (mut skipped, mut malformed) = (0usize, 0usize);

    for line in BufReader::new(f).lines() {
        let line = line?;
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut cols = line.split('\t');
        let (Some(emoji), Some(name)) = (cols.next(), cols.next()) else {
            malformed += 1;
            continue;
        };
        let is_tts = cols.next() == Some("tts");
        let (emoji, name) = (emoji.trim(), name.trim());
        if emoji.is_empty() || name.is_empty() {
            malformed += 1;
            continue;
        }
        let Some(code) = encode_phrase(name, char_codes) else {
            skipped += 1;
            continue;
        };
        let weight = if is_tts {
            NAMED_TTS_WEIGHT
        } else {
            NAMED_KEYWORD_WEIGHT
        };
        entries.push(Entry::new(emoji.to_string(), code, weight, 0));
    }

    if skipped > 0 {
        log(format!(
            "      [emoji_named] {skipped} 条无法反查编码，已跳过"
        ));
    }
    if malformed > 0 {
        log(format!(
            "      [emoji_named] {malformed} 行格式不符，已跳过"
        ));
    }
    Ok(entries)
}

/// 按 (编码, emoji) 去重，同键保留权重最高的一条。
///
/// 上游 extra 自带完全重复的条目，named 表也与上游有重合，两者都要去重。
///
/// 去重键剥掉 U+FE0F（`⚽` 与 `⚽️` 渲染相同），但保留条目仍用自己的原文：named 条目
/// 胜出时写进词库的是带 VS16 的规范形态，宿主才渲染彩色字形。
pub fn dedup_emoji_entries(list: &mut Vec<Entry>) -> usize {
    let mut best: std::collections::HashMap<(String, String), usize> = Default::default();
    let mut drop_flags = vec![false; list.len()];

    for (i, e) in list.iter().enumerate() {
        let key = (e.code.clone(), e.text.replace('\u{FE0F}', ""));
        match best.get(&key) {
            Some(&j) if list[j].weight >= e.weight => drop_flags[i] = true,
            Some(&j) => {
                drop_flags[j] = true;
                best.insert(key, i);
            }
            None => {
                best.insert(key, i);
            }
        }
    }

    let before = list.len();
    let mut i = 0usize;
    list.retain(|_| {
        let keep = !drop_flags[i];
        i += 1;
        keep
    });
    before - list.len()
}

/// 把主输出路径里的 output_name 换成 `output_name_<suffix>`，其余部分保留。
///
/// 例：`.../wubi86_jidian.dict.yaml` + `emoji` → `.../wubi86_jidian_emoji.dict.yaml`
pub fn extra_output_path(main_path: &Path, output_name: &str, suffix: &str) -> std::path::PathBuf {
    let dir = main_path.parent().unwrap_or(Path::new("."));
    let base = main_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let new_base = base.replacen(output_name, &format!("{output_name}_{suffix}"), 1);
    dir.join(new_base)
}

#[cfg(test)]
#[path = "extra_tests.rs"]
mod tests;
