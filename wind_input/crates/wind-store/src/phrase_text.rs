//! 短语的**逐行文本**分发格式（聊天窗口即贴即装）。
//!
//! ```text
//! wind:p1 我的直通车
//! kx (＾▽＾)
//! zd $CC("知典 {sub(reverse(last(1)), 1, 1)}", proc.run("D:\\知典\\知典.exe", "--query", "{...}"))
//! ```
//!
//! **行内零解析**是这个格式的全部要点。短语正文可以是 cmdbar 命令，里面有引号、括号、
//! 逗号、花括号和反斜杠——任何行内分隔符方案都会被它打穿，而叠一层转义会直接改坏
//! cmdbar 语法（`wdict::escape_text_field` 对命令类短语刻意不转义 `\` 就是同一个原因）。
//! 所以这里只认两个位置：**行首到第一个空白**是 code，**其余到行尾**原样是 text。
//!
//! ## 反斜杠一律写两个
//!
//! `\\` 表示一个字面 `\`，UNC 路径开头因此是四个（`\\\\nas\\share`）。这**不是本格式的新
//! 规定，恰恰是它没有引入新规定**：设置页输入框、方案词库文件、wdict 备份文件、本格式
//! 四条路径都经同一个 `unescape_text_field` 落库，用户在任一处学到的写法照搬即可，
//! 不用换算。回归测试见 `wind-webdata` 的 `phrase_text_escape_domain_matches_manual_add`。
//!
//! 之所以要一律写两个而不是「看情况」：单反斜杠**不报错**——cmdbar lexer 对未知转义
//! （`\我`、`\x`）原样保留，于是 `D:\我的文档` 恰好能用，让人以为单个没问题；直到路径里
//! 出现 `\notes` / `\tools`，`\n` `\t` 当场变成换行与制表符，路径静默损坏，且失败点在
//! 触发短语时而不是导入时。用户文档 `guides/command-bar.mdx#backslash` 是权威表述。
//!
//! 与 [`crate::wdict`] 的分工：wdict 是**备份/整机迁移**格式（TSV 多段，带 weight /
//! position / enabled / 词频 / 候选调整），本格式是**分发**格式，只承载内容三元组里的
//! `code` 与 `text`。position 刻意不进格式——照抄分发者的位置会打乱接收者的短语顺序。

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
    /// 跳过的行。**不是致命错误**——预览如实列出，用户自行判断是不是要的。
    pub problems: Vec<PhraseTextProblem>,
}

/// 这段文本是否带本格式的标记。用于导入侦测分派：**前缀匹配零歧义，应当先于
/// TOML 系（信封 / 配置片段）判定**，判不中再回落原有的侦测链。
pub fn is_phrase_text(text: &str) -> bool {
    let normalized = normalize_input(text);
    let text = normalized.as_ref();
    first_content_line(text).is_some_and(|l| l.starts_with(MARKER_PREFIX))
}

/// 解析逐行短语文本。
///
/// 返回 `Err` 只在**整段不可用**时：没有标记、版本过高。个别行的毛病进
/// `problems`，不牵连其余条目——群聊里粘贴掉一行格式是常态，为一行拒绝整段
/// 会让用户无从下手。
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

/// 拆一行为 `(code, text)`。**只认第一个空白**，其后原样。
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
    /// 疑似笔误（路径里的反斜杠写少了）。**不阻止导入**，只在预览里提一句。
    pub hints: Vec<wind_cmdbar::Hint>,
    /// cmdbar 语法错误。非 `None` 的条目**一律不可导入**——语法坏掉的短语装进去
    /// 只会在触发时失败，而失败点离导入很远，用户无从关联。
    pub error: Option<String>,
}

impl EntryCheck {
    pub fn is_importable(&self) -> bool {
        self.error.is_none()
    }
}

/// 逐条做静态检查。
///
/// ⚠️ 传入的必须是**存储域**文本（调用方已过 `unescape_text_field`）——运行时 cmdbar
/// 拿到的就是库里那一份，拿显示域文本去查等于查了另一个字符串。
///
/// **只查笔误与语法，不判断「危不危险」**：命令直通车本来就是短语的主要用途，而短语
/// 不会自行执行——要触发得先打出编码、再从候选里选中。把常态当异常来设门槛，只会让
/// 每次导入都多两步无意义的确认。
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
mod tests {
    use super::*;

    #[test]
    fn parses_marker_title_and_entries() {
        let doc = parse_phrase_text("wind:p1 我的直通车\nkx (＾▽＾)\nzw 早上好呀\n").unwrap();
        assert_eq!(doc.title, "我的直通车");
        assert_eq!(doc.entries.len(), 2);
        assert_eq!(doc.entries[0].code, "kx");
        assert_eq!(doc.entries[0].text, "(＾▽＾)");
        assert_eq!(doc.entries[1].line, 3, "行号是源行号（1-based）");
        assert!(doc.problems.is_empty());
    }

    #[test]
    fn title_is_optional() {
        let doc = parse_phrase_text("wind:p1\nkx (＾▽＾)\n").unwrap();
        assert_eq!(doc.title, "");
        assert_eq!(doc.entries.len(), 1);
    }

    /// 这个格式存在的理由：命令短语里有引号、括号、逗号、花括号、反斜杠和空格，
    /// 全部必须原样穿过。
    #[test]
    fn command_phrase_survives_verbatim() {
        let cmd = r#"$CC("知典超精 {sub(reverse(last(1)), 1, 1)}", proc.run("D:\\Program Files\\知典超精\\知典.exe", "--query", "{sub(reverse(last(1)), 1, 1)}"))"#;
        let doc = parse_phrase_text(&format!("wind:p1\nzd {cmd}\n")).unwrap();
        assert_eq!(doc.entries.len(), 1);
        assert_eq!(doc.entries[0].text, cmd, "正文必须逐字保留");
    }

    #[test]
    fn text_may_contain_spaces() {
        let doc = parse_phrase_text("wind:p1\nem 我 的 邮箱 是 a@b.com\n").unwrap();
        assert_eq!(doc.entries[0].text, "我 的 邮箱 是 a@b.com");
    }

    #[test]
    fn multiple_separator_spaces_are_collapsed_only_at_split() {
        let doc = parse_phrase_text("wind:p1\nkx    (＾▽＾)\n").unwrap();
        assert_eq!(doc.entries[0].code, "kx");
        assert_eq!(doc.entries[0].text, "(＾▽＾)");
    }

    #[test]
    fn crlf_and_indentation_are_tolerated() {
        let doc = parse_phrase_text("wind:p1\r\n  kx (＾▽＾)  \r\n\r\nzw 早\r\n").unwrap();
        assert_eq!(doc.entries.len(), 2);
        assert_eq!(doc.entries[0].text, "(＾▽＾)");
    }

    #[test]
    fn missing_marker_is_fatal() {
        assert!(parse_phrase_text("kx (＾▽＾)\n").is_err());
    }

    /// 被聊天软件压扁成一行时整段解析失败——失败方式是安全的：报错，而不是
    /// 把一长串当成一条短语装进去。
    #[test]
    fn flattened_message_fails_loudly() {
        let doc = parse_phrase_text("wind:p1 标题 kx (＾▽＾) zw 早上好").unwrap();
        assert!(doc.entries.is_empty(), "全被当成标题，没有条目");
    }

    #[test]
    fn future_version_is_rejected() {
        let err = parse_phrase_text("wind:p2\nkx x\n").unwrap_err();
        assert!(err.contains("请升级"), "{err}");
    }

    #[test]
    fn marker_without_version_is_rejected() {
        assert!(parse_phrase_text("wind:p\nkx x\n").is_err());
    }

    /// `wind:p1x` 不能被当成版本 1——否则尾巴上的内容会被静默吞掉。
    #[test]
    fn marker_with_trailing_garbage_is_rejected() {
        assert!(parse_phrase_text("wind:p1x\nkx x\n").is_err());
    }

    #[test]
    fn bad_lines_are_skipped_not_fatal() {
        let doc = parse_phrase_text("wind:p1\nkx (＾▽＾)\n没有空格的一行\nzw 早\n").unwrap();
        assert_eq!(doc.entries.len(), 2, "坏行不牵连其余条目");
        assert_eq!(doc.problems.len(), 1);
        assert_eq!(doc.problems[0].line, 3);
        assert_eq!(doc.problems[0].reason, ProblemReason::MissingSeparator);
    }

    #[test]
    fn non_ascii_code_is_rejected() {
        let doc = parse_phrase_text("wind:p1\n编码 内容\n").unwrap();
        assert!(doc.entries.is_empty());
        assert_eq!(doc.problems[0].reason, ProblemReason::BadCode);
    }

    #[test]
    fn duplicates_within_one_document_are_reported() {
        let doc = parse_phrase_text("wind:p1\nkx 好\nkx 好\n").unwrap();
        assert_eq!(doc.entries.len(), 1);
        assert_eq!(doc.problems[0].reason, ProblemReason::Duplicate);
    }

    /// 同 code 不同内容是合法的（短语主键是 `(code, text)`）。
    #[test]
    fn same_code_different_text_both_kept() {
        let doc = parse_phrase_text("wind:p1\nkx 甲\nkx 乙\n").unwrap();
        assert_eq!(doc.entries.len(), 2);
        assert!(doc.problems.is_empty());
    }

    #[test]
    fn check_reports_syntax_errors_and_hints() {
        let bs = char::from_u32(92).unwrap();
        let q = char::from_u32(34).unwrap();
        let texts = vec![
            "(＾▽＾)".to_string(),
            format!("$CC({q}跑{q}, proc.run({q}D:{bs}{bs}notes{bs}{bs}a.exe{q}))"),
            format!("$CC({q}跑{q}, proc.run({q}D:{bs}notes{q}))"),
            format!("$CC({q}坏"),
        ];
        let c = check_entries(&texts);

        assert!(c[0].is_importable() && c[0].hints.is_empty(), "纯文本");
        assert!(
            c[1].is_importable() && c[1].hints.is_empty(),
            "命令短语本身不是问题"
        );
        assert!(c[2].is_importable(), "笔误不阻止导入");
        assert_eq!(c[2].hints.len(), 1, "单写反斜杠要提示");
        assert!(!c[3].is_importable(), "语法错误不可导入");
    }

    #[test]
    fn detection_is_prefix_based() {
        assert!(is_phrase_text("wind:p1\nkx x\n"));
        assert!(is_phrase_text("\n\n  wind:p1 标题\nkx x\n"));
        assert!(!is_phrase_text("[package]\nkind = \"schema_text\"\n"));
        assert!(!is_phrase_text("wind_dict:\n  version: 1\n"));
        assert!(!is_phrase_text(""));
    }
}
