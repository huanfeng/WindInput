//! 短语的逐行文本分发格式（聊天窗口即贴即装）。
//!
//! ```text
//! wind:p1 我的直通车
//! kx (＾▽＾)
//! zd $CC("知典 {sub(reverse(last(1)), 1, 1)}", proc.run("D:\\知典\\知典.exe", "--query", "{...}"))
//! ```
//!
//! 行内零解析：正文可以是 cmdbar 命令，含引号、括号、逗号和反斜杠，任何行内分隔符都会
//! 被打穿，叠一层转义又会改坏 cmdbar 语法。所以只认行首到第一个空白为 code，其余到行尾
//! 原样为 text。
//!
//! 反斜杠一律写两个（`\\` 即字面 `\`）：设置页、方案词库、wdict、本格式四条路径都经同一个
//! `unescape_text_field` 落库，写法通用。单反斜杠遇未知转义会原样保留，看似能用，但碰上
//! `\n` `\t` 就静默损坏路径。回归测试见 `wind-webdata` 的
//! `phrase_text_escape_domain_matches_manual_add`，权威表述在用户文档 `guides/command-bar.mdx#backslash`。
//!
//! 与 [`crate::wdict`] 的分工：wdict 是备份/整机迁移格式，本格式是分发格式，只承载
//! `code` 与 `text`。position 刻意不进格式，照抄会打乱接收者的短语顺序。

use wind_utils::text::normalize_input;

/// 格式标记前缀。`p` = phrases，其后是格式版本号。
const MARKER_PREFIX: &str = "wind:p";

/// 本实现支持的最高格式版本。
///
/// 「宽容只给过去，不给未来」（同 `package-format.md` §3.2）：低版本按老规则读，
/// 高版本硬拒绝并提示升级——更高版本的行语义可能已变，用当前规则去读它本身就是错的。
pub const SUPPORTED_VERSION: u32 = 1;

/// 一条解析出的短语。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhraseTextEntry {
    /// 源文件行号（1-based）。预览要能指出「第几行有问题」。
    pub line: usize,
    pub code: String,
    pub text: String,
}

/// 被跳过的行及原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhraseTextProblem {
    pub line: usize,
    /// 原始行内容（已 trim），供预览回显。
    pub raw: String,
    pub reason: ProblemReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProblemReason {
    /// 整行没有空白，分不出 code 与 text。
    MissingSeparator,
    /// code 含非可打印 ASCII——多半是粘贴带进了不可见字符，或列序搞反了
    /// （`import_formats` 对 TSV 同款判据）。
    BadCode,
    /// text 为空。
    EmptyText,
    /// 与前面某行 code+text 完全相同。
    Duplicate,
}

impl ProblemReason {
    pub fn message(self) -> &'static str {
        match self {
            Self::MissingSeparator => "缺少空格分隔，无法区分编码与内容",
            Self::BadCode => "编码含非法字符（需可打印 ASCII）",
            Self::EmptyText => "内容为空",
            Self::Duplicate => "与前面的条目重复",
        }
    }
}

/// 解析结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhraseTextDoc {
    /// 首行标记之后的可选标题，无则空串。
    pub title: String,
    pub entries: Vec<PhraseTextEntry>,
    /// 跳过的行。不是致命错误，预览如实列出，用户自行判断是不是要的。
    pub problems: Vec<PhraseTextProblem>,
}

/// 这段文本是否带本格式的标记。前缀匹配零歧义，导入侦测应先于 TOML 系（信封 / 配置片段）判定。
pub fn is_phrase_text(text: &str) -> bool {
    let normalized = normalize_input(text);
    let text = normalized.as_ref();
    first_content_line(text).is_some_and(|l| l.starts_with(MARKER_PREFIX))
}

/// 解析逐行短语文本。
///
/// 只在整段不可用（无标记、版本过高）时返回 `Err`；个别行的毛病进 `problems`，
/// 不牵连其余条目。
pub fn parse_phrase_text(text: &str) -> Result<PhraseTextDoc, String> {
    let normalized = normalize_input(text);
    let text = normalized.as_ref();
    let mut lines = text.lines().enumerate();
    let marker_line = lines
        .by_ref()
        .map(|(_, l)| l.trim())
        .find(|l| !l.is_empty())
        .ok_or_else(|| "内容为空".to_string())?;
    let rest = marker_line
        .strip_prefix(MARKER_PREFIX)
        .ok_or_else(|| format!("缺少格式标记（首行应以 `{MARKER_PREFIX}<版本>` 开头）"))?;

    // 版本号 = 标记后紧跟的连续数字；其后必须是空白或行尾，否则 `wind:p1x` 之类
    // 会被当成版本 1 而把 `x` 悄悄吞掉。
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return Err(format!("格式标记缺少版本号（应形如 `{MARKER_PREFIX}1`）"));
    }
    let after = &rest[digits.len()..];
    if !after.is_empty() && !after.starts_with(char::is_whitespace) {
        return Err("格式标记后应是空格或换行".into());
    }
    let version: u32 = digits.parse().map_err(|_| "格式版本号过大".to_string())?;
    if version > SUPPORTED_VERSION {
        return Err(format!(
            "格式版本 {version} 高于本版本支持的 {SUPPORTED_VERSION}，请升级应用后再导入"
        ));
    }

    let mut doc = PhraseTextDoc {
        title: after.trim().to_string(),
        ..Default::default()
    };

    for (idx, raw) in lines {
        let line_no = idx + 1;
        // 两端 trim：聊天软件转发常带缩进与尾随空格，不 trim 会把它们静默混进
        // code/text。代价是「以空白结尾的短语」表达不了——那种走 wdict 文件。
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        match split_entry(line) {
            Err(reason) => doc.problems.push(PhraseTextProblem {
                line: line_no,
                raw: line.to_string(),
                reason,
            }),
            Ok((code, text)) => {
                if doc.entries.iter().any(|e| e.code == code && e.text == text) {
                    doc.problems.push(PhraseTextProblem {
                        line: line_no,
                        raw: line.to_string(),
                        reason: ProblemReason::Duplicate,
                    });
                    continue;
                }
                doc.entries.push(PhraseTextEntry {
                    line: line_no,
                    code,
                    text,
                });
            }
        }
    }
    Ok(doc)
}

/// 拆一行为 `(code, text)`。只认第一个空白，其后原样。
fn split_entry(line: &str) -> Result<(String, String), ProblemReason> {
    let Some(pos) = line.find(char::is_whitespace) else {
        return Err(ProblemReason::MissingSeparator);
    };
    let (code, rest) = line.split_at(pos);
    // 分隔空白可以有多个（对齐排版），但 text 内部与尾部的空白不动。
    let text = rest.trim_start();
    if text.is_empty() {
        return Err(ProblemReason::EmptyText);
    }
    if !is_valid_code(code) {
        return Err(ProblemReason::BadCode);
    }
    Ok((code.to_string(), text.to_string()))
}

/// code 必须是非空可打印 ASCII（不含空格——拆分已保证）。
fn is_valid_code(code: &str) -> bool {
    !code.is_empty()
        && code
            .chars()
            .all(|c| c.is_ascii_graphic() && !c.is_whitespace())
}

/// 首个非空行（已 trim）。
fn first_content_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|l| !l.is_empty())
}

/// 一条短语的静态检查结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryCheck {
    /// 疑似笔误（路径里的反斜杠写少了）。不阻止导入，只在预览里提一句。
    pub hints: Vec<wind_cmdbar::Hint>,
    /// cmdbar 语法错误。非 `None` 的条目不可导入：否则要到触发时才失败，用户无从关联。
    pub error: Option<String>,
}

impl EntryCheck {
    pub fn is_importable(&self) -> bool {
        self.error.is_none()
    }
}

/// 逐条做静态检查。
///
/// 传入的必须是存储域文本（已过 `unescape_text_field`），即运行时 cmdbar 拿到的那一份。
///
/// 只查笔误与语法，不判断「危不危险」：命令本是短语的主要用途，且要打出编码并选中才会触发。
pub fn check_entries(texts: &[String]) -> Vec<EntryCheck> {
    texts
        .iter()
        .map(|t| match wind_cmdbar::lint_phrase(t) {
            Ok(hints) => EntryCheck { hints, error: None },
            Err(e) => EntryCheck {
                hints: Vec::new(),
                error: Some(e.to_string()),
            },
        })
        .collect()
}

#[cfg(test)]
#[path = "phrase_text_tests.rs"]
mod tests;
