//! 误读表（设计 `docs/design/pinyin-correction-and-hints.md` §6）：常见读错的音 → 正确读音。
//!
//! 用途两个：
//! - **提示**：候选文字 + 用户打的拼音双重命中表项 ⇒ 按表标正确拼音（`HintKind::Misread`），
//!   与模糊 / 纠错来源的提示冲突时表优先。
//! - **召回**：整串音节等于某条的错误读音 ⇒ 词库读不出也以低权重放进候选。
//!
//! 内置一份（`schemas/pinyin_misread.yaml`）+ 用户目录追加一份（`<userdata>/data/pinyin_misread.yaml`），
//! 两份格式相同；用户表同键（文字 + 错误读音）覆盖内置。

use std::collections::HashMap;
use tracing::warn;

/// 内置表在 `schemas/` 下的相对路径，也是用户追加表在用户数据目录下的文件名。
pub const MISREAD_FILE: &str = wind_store::MISREAD_FILE;

/// meta 头与列表体的分隔行（与 librime `.dict.yaml`、字符类文件一致）。
const HEAD_BODY_SEP: &str = "...";

/// 一条误读：`wrong` / `correct` 一字一音节、与 `text` 字数相同，且两者不同。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MisreadEntry {
    pub text: String,
    pub wrong: Vec<String>,
    pub correct: Vec<String>,
}

impl MisreadEntry {
    /// 错误读音连写（`xinxin`），召回的键。
    fn wrong_code(&self) -> String {
        self.wrong.concat()
    }
}

/// 文件里的一条（列表体的一个 mapping）。未知字段（`note`、`group` 等）忽略。
#[derive(serde::Deserialize)]
struct RawEntry {
    text: String,
    wrong: OneOrMany,
    right: String,
}

/// `wrong` 可写一个读音，也可写读音列表（同一个词有几种常见读错法）。
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

/// 误读表：条目 + 两张索引（文字 → 条目、错误读音连写 → 条目）。相等 = 条目逐条相同。
#[derive(Debug, Default)]
pub struct MisreadTable {
    entries: Vec<MisreadEntry>,
    by_text: HashMap<String, Vec<usize>>,
    by_wrong: HashMap<String, Vec<usize>>,
}

impl PartialEq for MisreadTable {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl MisreadTable {
    /// 解析 YAML 误读表：meta 头（`name` / `version` / `description`，引擎不读）、`...` 行、
    /// 列表体（每项 `{text, wrong, right}`，读音以空格分隔音节、不带声调，`ü` 记作 `v`）。
    ///
    /// 坏条目（缺字段、音节非纯字母、音节数与字数不等、错误读音与正确读音相同）warn 跳过；
    /// 列表体整体不是合法 YAML 时逐行解析，只丢坏的那几行。
    pub fn parse(src: &str, origin: &str) -> Self {
        // 行尾守卫（`line_ending_coverage`）：用户在 Windows 记事本里维护的表常带 CRLF，
        // 孤立 \r 会让逐行回退与 `head_has_entries` 把整份文件算成一行。
        let normalized = wind_utils::text::normalize_input(src);
        let (head, body) = split_head_body(&normalized);
        if head_has_entries(head) {
            warn!(
                "误读表 {origin} 的 meta 头（`...` 之前）里有条目，这些条目不生效，应写在 `...` 之后"
            );
        }
        let items: Vec<serde_yaml::Value> = match serde_yaml::from_str(body) {
            Ok(serde_yaml::Value::Sequence(items)) => items,
            Ok(serde_yaml::Value::Null) => Vec::new(),
            Ok(_) => {
                warn!("误读表 {origin} 的列表体不是列表，已忽略");
                Vec::new()
            }
            Err(e) => {
                warn!("误读表 {origin} 不是合法 YAML（{e}），改为逐行解析");
                body.lines()
                    .filter(|l| l.trim_start().starts_with('-'))
                    .filter_map(|l| {
                        let v = serde_yaml::from_str::<Vec<serde_yaml::Value>>(l).ok();
                        if v.is_none() {
                            warn!("误读表 {origin} 有一行不是合法 YAML，已跳过");
                        }
                        v
                    })
                    .flatten()
                    .collect()
            }
        };
        let mut entries = Vec::new();
        for (no, item) in items.into_iter().enumerate() {
            match entries_of(item) {
                Some(es) => entries.extend(es),
                // 日志不带条目内容（词库内容不进日志），序号足够定位。
                None => warn!("误读表 {origin} 第 {} 条格式不对，已跳过", no + 1),
            }
        }
        Self::from_entries(entries)
    }

    /// 用户表同键（文字 + 错误读音）覆盖内置，其余追加。
    pub fn merge(builtin: Self, user: Self) -> Self {
        let mut entries = builtin.entries;
        for u in user.entries {
            match entries
                .iter_mut()
                .find(|e| e.text == u.text && e.wrong == u.wrong)
            {
                Some(e) => *e = u,
                None => entries.push(u),
            }
        }
        Self::from_entries(entries)
    }

    /// 文字 + 用户打的音节（连写比较，切法不论）双重命中的条目。
    pub fn lookup(&self, text: &str, typed: &[&str]) -> Option<&MisreadEntry> {
        let ids = self.by_text.get(text)?;
        let typed = typed.concat();
        ids.iter()
            .map(|&i| &self.entries[i])
            .find(|e| e.wrong_code() == typed)
    }

    /// 错误读音连写恰为 `typed_code` 的条目（召回用）。
    pub fn recall(&self, typed_code: &str) -> impl Iterator<Item = &MisreadEntry> {
        self.by_wrong
            .get(typed_code)
            .into_iter()
            .flatten()
            .map(|&i| &self.entries[i])
    }

    /// 表里有没有这个文字（热路径先零分配判掉）。
    pub fn has_text(&self, text: &str) -> bool {
        self.by_text.contains_key(text)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn from_entries(entries: Vec<MisreadEntry>) -> Self {
        let mut by_text: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_wrong: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, e) in entries.iter().enumerate() {
            by_text.entry(e.text.clone()).or_default().push(i);
            by_wrong.entry(e.wrong_code()).or_default().push(i);
        }
        Self {
            entries,
            by_text,
            by_wrong,
        }
    }
}

/// 按 `...` 行切成 meta 头与列表体；没有分隔行时整份都是列表体。
fn split_head_body(src: &str) -> (&str, &str) {
    let mut at = 0;
    for line in src.split_inclusive('\n') {
        at += line.len();
        if line.trim_end() == HEAD_BODY_SEP {
            return (&src[..at - line.len()], &src[at..]);
        }
    }
    ("", src)
}

/// meta 头里出现了列表项（`- {…}`）：多半是条目写到了 `...` 之前。
fn head_has_entries(head: &str) -> bool {
    head.lines().any(|l| l.trim_start().starts_with("- {"))
}

/// 一项展开成条目（`wrong` 是列表时每个读音一条）；任一处不合格整项作废。
fn entries_of(item: serde_yaml::Value) -> Option<Vec<MisreadEntry>> {
    let raw: RawEntry = serde_yaml::from_value(item).ok()?;
    let text = raw.text.trim();
    let n = text.chars().count();
    let correct = syllables(&raw.right).filter(|c| c.len() == n)?;
    let wrongs = match raw.wrong {
        OneOrMany::One(w) => vec![w],
        OneOrMany::Many(ws) if !ws.is_empty() => ws,
        OneOrMany::Many(_) => return None,
    };
    wrongs
        .iter()
        .map(|w| {
            let wrong = syllables(w).filter(|w| w.len() == n && *w != correct)?;
            Some(MisreadEntry {
                text: text.to_string(),
                wrong,
                correct: correct.clone(),
            })
        })
        .collect()
}

/// 空格分隔的音节，小写、`ü` → `v`；空或含非字母 ⇒ `None`。
fn syllables(col: &str) -> Option<Vec<String>> {
    let out: Vec<String> = col
        .split_whitespace()
        .map(|s| s.to_lowercase().replace('ü', "v"))
        .collect();
    let ok = !out.is_empty()
        && out
            .iter()
            .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase()));
    ok.then_some(out)
}

#[cfg(test)]
#[path = "misread_tests.rs"]
mod tests;
