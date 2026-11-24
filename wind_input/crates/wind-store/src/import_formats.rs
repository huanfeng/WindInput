//! 外部词库格式导入：格式自动探测 + Rime / TSV 解析。
//!
//! 与旧 Go pkg/dictio 的 import_rime.go / import_tsv.go 对齐，产出与 wdict
//! words 段相同的 [`wdict::WordIo`] 行，后续复用 import_user_words 管线。
//!
//! 格式探测按内容而非扩展名（UI 不要求用户选格式）：
//!  - WindDict：头部（首个 `\n---` 之前）含 `wind_dict:`
//!  - Rime：存在整行 `...`（YAML 文档结束标记，头/体分隔）
//!  - TSV：任一非空非注释行含制表符
//!  - 其余 → Unknown

use crate::wdict::WordIo;
use wind_utils::text::normalize_input;

/// 词库文本格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictFormat {
    WindDict,
    Rime,
    Tsv,
    Unknown,
}

impl DictFormat {
    /// RPC/UI 用的稳定标识。
    pub fn as_str(&self) -> &'static str {
        match self {
            DictFormat::WindDict => "winddict",
            DictFormat::Rime => "rime",
            DictFormat::Tsv => "tsv",
            DictFormat::Unknown => "unknown",
        }
    }
}

/// 编码归一化策略。
///
/// ★ **为什么由调用方传入而不是在这里判引擎**：`wind-store` 拿不到 `engine_mgr`
/// （落库规则须对两类方案通用，见 `docs/design/pinyin-code-domains.md`）。故本层只认识
/// 「策略」，不认识「引擎」——由 `wind-webdata` 按目标方案的引擎类型挑一个常量传进来。
#[derive(Debug, Clone, Copy)]
pub struct CodePolicy {
    /// 视作音节分隔符、归一成空格的字符。
    ///
    /// ★ 这不是「清洗」而是**信息升级**：Rime 的 `ni'hao` 里那个撇号与空格一样是词库
    /// 作者标注的音节真值，转成空格后 [`crate::wdict::split_spaced_code`] 才吃得到边界。
    /// 留着它则会成为 flat key 的一部分（flat 域的不变量是「剥除 `'` 后的串」），
    /// 那条词永远查不到。
    pub syllable_separators: &'static str,
    /// 是否小写化。查询侧判据全链路是 `is_ascii_lowercase`，而用户词表是**裸字节前缀
    /// 匹配、不做大小写归一** ⇒ 大写码落库后在设置页看得见，却永远打不出来。
    pub lowercase: bool,
}

impl CodePolicy {
    /// 拼音族：撇号是音节分隔符，码恒为小写。
    pub const PINYIN: Self = Self {
        syllable_separators: "'",
        lowercase: true,
    };
    /// 码表 / 五笔 / 快符等：码的字符集由方案自定（快符码里就有 `@`），**不做任何改写**
    /// ——与本策略引入之前的行为逐字节一致。
    pub const CODETABLE: Self = Self {
        syllable_separators: "",
        lowercase: false,
    };
}

impl Default for CodePolicy {
    /// 默认取最保守的一档：不改写。新调用点忘了传也不会改变既有语义。
    fn default() -> Self {
        Self::CODETABLE
    }
}

/// 按内容探测词库格式。
pub fn detect_dict_format(text: &str) -> DictFormat {
    let normalized = normalize_input(text);
    let text = normalized.as_ref();
    if text
        .split("\n---")
        .next()
        .unwrap_or("")
        .contains("wind_dict:")
    {
        return DictFormat::WindDict;
    }
    if text.lines().any(|l| l.trim() == "...") {
        return DictFormat::Rime;
    }
    let has_tsv_line = text.lines().any(|l| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with('#') && l.contains('\t')
    });
    if has_tsv_line {
        return DictFormat::Tsv;
    }
    DictFormat::Unknown
}

/// 探测格式并解析为 words 行。返回 (格式, 行, 跳过数)。
/// Unknown 格式报错并列出支持的格式。
pub fn parse_words_auto(
    text: &str,
    policy: CodePolicy,
) -> Result<(DictFormat, Vec<WordIo>, usize), String> {
    let normalized = normalize_input(text);
    let text = normalized.as_ref();
    let fmt = detect_dict_format(text);
    let (rows, skipped) = match fmt {
        DictFormat::WindDict => crate::wdict::parse_words_wdict(text)?,
        DictFormat::Rime => parse_words_rime(text, policy)?,
        DictFormat::Tsv => parse_words_tsv(text, policy)?,
        DictFormat::Unknown => {
            return Err(
                "无法识别的词库格式（支持 WindDict .wdict.yaml / Rime .dict.yaml / TSV 文本）"
                    .into(),
            );
        }
    };
    Ok((fmt, rows, skipped))
}

/// 解析 Rime 词库（.dict.yaml）。返回 (行, 跳过的非法行数)。
///
/// 头 = 整行 `...` 之前；缺 `columns:` 声明用 Rime 默认列 `[text, code, weight]`。
/// 编码列去内部空格（拼音音节 `ni hao` → `nihao`）；缺 text/code 的行跳过；
/// 权重解析失败回退 0。
pub fn parse_words_rime(text: &str, policy: CodePolicy) -> Result<(Vec<WordIo>, usize), String> {
    let normalized = normalize_input(text);
    let text = normalized.as_ref();
    let mut lines = text.lines();
    let mut header_lines: Vec<&str> = Vec::new();
    let mut found_sep = false;
    for line in lines.by_ref() {
        if line.trim() == "..." {
            found_sep = true;
            break;
        }
        header_lines.push(line);
    }
    if !found_sep {
        return Err("无效的 Rime 词库格式（缺 `...` 头部分隔行）".into());
    }
    let cols = rime_columns_from_header(&header_lines);

    let mut rows = Vec::new();
    let mut skipped = 0usize;
    for line in lines {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let get = |name: &str| -> &str {
            cols.iter()
                .position(|c| c == name)
                .and_then(|i| fields.get(i).copied())
                .map(str::trim)
                .unwrap_or("")
        };
        let (mut word, mut code_raw) = (get("text"), get("code"));
        // 命令栏语法条目列序写反时逐行对调（t172），与码表加载 `parse_rime_line` 同判据。
        if crate::wdict::is_misplaced_cmdbar_code(code_raw) && !crate::wdict::is_cmdbar_text(word) {
            std::mem::swap(&mut word, &mut code_raw);
        }
        if word.is_empty() || code_raw.is_empty() {
            skipped += 1;
            continue;
        }
        rows.push(WordIo {
            code: normalize_code(code_raw, policy),
            // 反转义在 trim **之后**：`\n` 在 trim 阶段还是反斜杠加字母 n 两个可见字符，
            // 天然免疫空白剥离，到这一步才变成换行。有意的空白由转义序列表达、排版噪声
            // 交给 trim，两者在管线的不同阶段产生，就不需要互相区分。
            text: crate::wdict::unescape_text_field(word),
            weight: parse_weight(get("weight")),
            count: 0,
            boundary: None,
        });
    }
    Ok((rows, skipped))
}

/// 从 Rime 头部提取 `columns:` 多行列表（`  - text` 形态）；缺则默认列。
fn rime_columns_from_header(header: &[&str]) -> Vec<String> {
    let mut cols = Vec::new();
    let mut in_columns = false;
    for line in header {
        let t = line.trim();
        if in_columns {
            if let Some(item) = t.strip_prefix("- ") {
                cols.push(item.trim().trim_matches('"').to_string());
                continue;
            }
            if t.starts_with('-') && t.len() > 1 {
                cols.push(t[1..].trim().trim_matches('"').to_string());
                continue;
            }
            break; // 非 `- xxx` 行即列表结束
        }
        if t == "columns:" {
            in_columns = true;
        }
    }
    if cols.is_empty() {
        vec!["text".into(), "code".into(), "weight".into()]
    } else {
        cols
    }
}

/// 解析纯文本 TSV（`编码\t词条[\t权重]`）。返回 (行, 跳过的非法行数)。
///
/// 列数 <2、code/text 为空、code 含非可打印 ASCII（乱码/列序颠倒防护）的行跳过；
/// 权重缺省或解析失败回退 0。
pub fn parse_words_tsv(text: &str, policy: CodePolicy) -> Result<(Vec<WordIo>, usize), String> {
    let normalized = normalize_input(text);
    let text = normalized.as_ref();
    let mut rows = Vec::new();
    let mut skipped = 0usize;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 2 {
            skipped += 1;
            continue;
        }
        let (mut code_raw, mut word) = (fields[0].trim(), fields[1].trim());
        // 同 Rime 路径：命令栏语法条目写在编码位时对调（t172）。
        if crate::wdict::is_misplaced_cmdbar_code(code_raw) && !crate::wdict::is_cmdbar_text(word) {
            std::mem::swap(&mut word, &mut code_raw);
        }
        if code_raw.is_empty() || word.is_empty() || !is_valid_code(code_raw) {
            skipped += 1;
            continue;
        }
        rows.push(WordIo {
            code: normalize_code(code_raw, policy),
            // 同 Rime 路径：trim 在前、反转义在后。见 `parse_words_rime`。
            text: crate::wdict::unescape_text_field(word),
            weight: parse_weight(fields.get(2).map(|s| s.trim()).unwrap_or("")),
            count: 0,
            boundary: None,
        });
    }
    Ok((rows, skipped))
}

/// 编码归一化：空白折叠为**单个空格**，前后剥净。
///
/// 此前是 `split_whitespace().collect()`（直接删空格，注释写作「拼音音节合并」）——
/// 而 rime 词库的 `你好\tni hao\t1200` 里，那些空格正是词库作者标注的**音节真值**，
/// 删掉即永久丢失，落库只能 boundary=0。现改为保留，由
/// [`crate::wdict::split_spaced_code`] 在落库时拆成 flat key + 边界。
///
/// 对码表码（五笔等，本就无空格）幂等，行为与改动前一致。
fn normalize_code(code: &str, policy: CodePolicy) -> String {
    let mut s = if policy.syllable_separators.is_empty() {
        code.to_string()
    } else {
        code.replace(|c| policy.syllable_separators.contains(c), " ")
    };
    if policy.lowercase {
        s = s.to_ascii_lowercase();
    }
    // 空白折叠放在最后：分隔符刚被换成空格，这一步顺带把 `ni''hao` 这类连写归一。
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 编码合法性：可打印 ASCII（0x20-0x7E）。拦乱码与"词在前码在后"的列序颠倒文件。
fn is_valid_code(code: &str) -> bool {
    code.chars().all(|c| ('\x20'..='\x7e').contains(&c))
}

/// 权重解析：整数优先，浮点截断兜底（部分 Rime 词库权重为浮点），失败回退 0。
fn parse_weight(s: &str) -> i32 {
    if s.is_empty() {
        return 0;
    }
    if let Ok(v) = s.parse::<i64>() {
        return v.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
    }
    if let Ok(v) = s.parse::<f64>() {
        return v.clamp(i32::MIN as f64, i32::MAX as f64) as i32;
    }
    0
}

#[cfg(test)]
#[path = "import_formats_tests.rs"]
mod tests;
