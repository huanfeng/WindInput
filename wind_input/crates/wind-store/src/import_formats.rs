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

use crate::text_source::normalize_import_text;
use crate::wdict::WordIo;

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
    let normalized = normalize_import_text(text);
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
    let normalized = normalize_import_text(text);
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
    let normalized = normalize_import_text(text);
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
        let word = get("text");
        let code_raw = get("code");
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
    let normalized = normalize_import_text(text);
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
        let code_raw = fields[0].trim();
        let word = fields[1].trim();
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
mod tests {
    use super::*;

    const RIME_SAMPLE: &str = "# Rime dictionary\n---\nname: luna_pinyin\nversion: \"0.9\"\nsort: by_weight\n...\n\n你好\tni hao\t100\n世界\tshi jie\t50\n";

    #[test]
    fn detect_winddict_rime_tsv_unknown() {
        let wd = "# c\nwind_dict:\n  version: 1\n\n--- !words\na\t工\t1\n";
        assert_eq!(detect_dict_format(wd), DictFormat::WindDict);
        assert_eq!(detect_dict_format(RIME_SAMPLE), DictFormat::Rime);
        assert_eq!(detect_dict_format("nihao\t你好\t10\n"), DictFormat::Tsv);
        assert_eq!(
            detect_dict_format("只有词\n没有制表符\n"),
            DictFormat::Unknown
        );
        // BOM 不影响探测
        assert_eq!(
            detect_dict_format("\u{feff}wind_dict:\n  version: 1\n\n--- !words\n"),
            DictFormat::WindDict
        );
    }

    /// ⚠️ **断言方向已反转**（原为「拼音码去空格连写」，测试名亦为 `..._space_join`）。
    ///
    /// rime 源里 `你好\tni hao\t100` 的空格是词库作者标注的**音节真值**。旧的
    /// `normalize_code` 直接把它删掉，导入的词一律 boundary=0 —— 信息拿在手上、用完即弃。
    /// 现保留为单空格，落库时由 `wdict::split_spaced_code` 拆成 flat key + 边界
    /// （key 仍是扁平的 `nihao`，见 docs/design/pinyin-code-domains.md §2.2）。
    #[test]
    fn rime_default_columns_keeps_syllable_spaces() {
        let (rows, skipped) = parse_words_rime(RIME_SAMPLE, CodePolicy::PINYIN).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(rows.len(), 2);
        // 默认列序 text 在前；音节空格保留，供落库端拆出边界
        assert_eq!(rows[0].text, "你好");
        assert_eq!(rows[0].code, "ni hao");
        assert_eq!(rows[0].weight, 100);
        assert_eq!(rows[1].code, "shi jie");
        // 落库端拆分后 key 仍是扁平码，边界随之得到
        assert_eq!(
            crate::wdict::split_spaced_code(&rows[0].code),
            ("nihao".to_string(), 0b101)
        );
    }

    /// 空白折叠：多空格/制表列内空白归一为单个空格；码表码（无空格）幂等。
    /// 两条导入路径都须反转义词条文本，且与 `escape_field` 往返自洽
    /// （导出 → 再导入应还原原文，这是备份还原的基本契约）。
    #[test]
    fn import_unescapes_text_on_both_paths() {
        let rime = "---\nname: t\n...\n甲\\n乙\tjy\t10\nC:\\Users\tcu\t20\n";
        let (rows, _) = parse_words_rime(rime, CodePolicy::PINYIN).unwrap();
        assert_eq!(rows[0].text, "甲\n乙", "Rime 路径须反转义 \\n");
        assert_eq!(
            rows[1].text, "C:\\Users",
            "未知转义序列原样保留，路径类词条不被改写"
        );

        let tsv = "jy\t甲\\n乙\t10\ncu\tC:\\Users\t20\n";
        let (rows, _) = parse_words_tsv(tsv, CodePolicy::PINYIN).unwrap();
        assert_eq!(rows[0].text, "甲\n乙", "TSV 路径须反转义 \\n");
        assert_eq!(rows[1].text, "C:\\Users");
    }

    /// 反转义必须发生在 trim **之后**：转义序列在 trim 阶段是可见字符，剥不掉；
    /// 而围绕字段的裸空格是排版噪声，照剥不误。
    #[test]
    fn import_trims_bare_space_but_keeps_escaped_whitespace() {
        let tsv = "jy\t  甲乙  \t10\nbd\t丙\\t丁\t20\n";
        let (rows, _) = parse_words_tsv(tsv, CodePolicy::PINYIN).unwrap();
        assert_eq!(rows[0].text, "甲乙", "字段两侧的裸空格按既有语义剥除");
        assert_eq!(
            rows[1].text, "丙\t丁",
            "转义制表符须存活——它在 trim 阶段还不是空白"
        );
    }

    #[test]
    fn normalize_code_folds_whitespace_and_is_idempotent_for_flat() {
        let p = CodePolicy::PINYIN;
        assert_eq!(normalize_code("ni  hao", p), "ni hao");
        assert_eq!(normalize_code("  ni hao  ", p), "ni hao");
        assert_eq!(normalize_code("abcd", p), "abcd");
    }

    /// ★ 撇号是**音节真值**，不是噪声：Rime 的 `ni'hao` 与 `ni hao` 表达同一件事。
    /// 归一成空格后落库端才拆得出边界；留着它则会进 flat key，那条词永远查不到。
    #[test]
    fn pinyin_policy_upgrades_apostrophe_to_boundary() {
        let p = CodePolicy::PINYIN;
        assert_eq!(normalize_code("ni'hao", p), "ni hao");
        assert_eq!(normalize_code("xi'an'ning", p), "xi an ning");
        // 连写与混用都归一到单空格
        assert_eq!(normalize_code("ni''hao", p), "ni hao");
        assert_eq!(normalize_code("ni' hao", p), "ni hao");
        // 落库端据此拆出边界——这正是本条归一化的目的
        assert_eq!(
            crate::wdict::split_spaced_code(&normalize_code("ni'hao", p)),
            ("nihao".to_string(), 0b101)
        );
    }

    /// ★ 大写码落库后在设置页看得见却永远打不出来：查询侧判据全链路是
    /// `is_ascii_lowercase`，而用户词表是裸字节前缀匹配、不做大小写归一。
    #[test]
    fn pinyin_policy_lowercases() {
        assert_eq!(normalize_code("NiHao", CodePolicy::PINYIN), "nihao");
        assert_eq!(normalize_code("NI HAO", CodePolicy::PINYIN), "ni hao");
    }

    /// ⚠️ 码表策略**不做任何改写**——快符码里就有 `@`，五笔方案也可能自定义字符集。
    /// 与本策略引入之前逐字节一致。
    #[test]
    fn codetable_policy_leaves_code_untouched() {
        let p = CodePolicy::CODETABLE;
        assert_eq!(normalize_code("ni'hao", p), "ni'hao");
        assert_eq!(normalize_code("NiHao", p), "NiHao");
        assert_eq!(normalize_code("@ab", p), "@ab");
        // 空白折叠是两类策略共有的（列内排版噪声与引擎无关）
        assert_eq!(normalize_code("  a  b ", p), "a b");
    }

    #[test]
    fn rime_explicit_columns_reorder() {
        let s = "---\nname: t\ncolumns:\n  - code\n  - text\n  - weight\n...\nnihao\t你好\t7\n";
        let (rows, _) = parse_words_rime(s, CodePolicy::PINYIN).unwrap();
        assert_eq!(rows[0].code, "nihao");
        assert_eq!(rows[0].text, "你好");
        assert_eq!(rows[0].weight, 7);
    }

    #[test]
    fn rime_skips_incomplete_and_comment_lines() {
        let s = "---\nname: t\n...\n# 注释行\n\n只有词没有码\n好\tni hao\n";
        let (rows, skipped) = parse_words_rime(s, CodePolicy::PINYIN).unwrap();
        // 缺 code 的行跳过;缺 weight 列回退 0
        assert_eq!(rows.len(), 1);
        assert_eq!(skipped, 1);
        assert_eq!(rows[0].weight, 0);
    }

    #[test]
    fn rime_missing_separator_is_error() {
        assert!(parse_words_rime("name: t\n你好\tni hao\t1\n", CodePolicy::PINYIN).is_err());
    }

    #[test]
    fn rime_float_weight_truncates() {
        let s = "---\nname: t\n...\n你好\tni hao\t520.9\n";
        let (rows, _) = parse_words_rime(s, CodePolicy::PINYIN).unwrap();
        assert_eq!(rows[0].weight, 520, "浮点权重截断取整");
    }

    #[test]
    fn tsv_basic_and_optional_weight() {
        let s = "# 注释\nnihao\t你好\t10\nshijie\t世界\n";
        let (rows, skipped) = parse_words_tsv(s, CodePolicy::PINYIN).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].weight, 10);
        assert_eq!(rows[1].weight, 0, "缺权重列回退 0");
    }

    #[test]
    fn tsv_skips_bad_lines() {
        let s = "单列无制表符x\n你好\tnihao\t1\nab\t好\t2\n";
        let (rows, skipped) = parse_words_tsv(s, CodePolicy::PINYIN).unwrap();
        // 第 1 行列数不足;第 2 行 code 为汉字(列序颠倒防护)→ 均跳过
        assert_eq!(rows.len(), 1);
        assert_eq!(skipped, 2);
        assert_eq!(rows[0].code, "ab");
    }

    #[test]
    fn auto_dispatch_matches_detection() {
        let (fmt, rows, _) = parse_words_auto(RIME_SAMPLE, CodePolicy::PINYIN).unwrap();
        assert_eq!(fmt, DictFormat::Rime);
        assert_eq!(rows.len(), 2);
        let (fmt, rows, _) = parse_words_auto("a\t工\t5\n", CodePolicy::PINYIN).unwrap();
        assert_eq!(fmt, DictFormat::Tsv);
        assert_eq!(rows[0].code, "a");
        assert!(parse_words_auto("既无头也无制表符\n", CodePolicy::PINYIN).is_err());
    }
}
