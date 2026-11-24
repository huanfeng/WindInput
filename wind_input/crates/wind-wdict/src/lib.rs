//! wdict（.wdict.yaml）格式读写 — 复刻旧 Go pkg/dictio。
//! 支持 phrases / words / shadow 三种 section；文件可含多段。
//! 独立 crate（原 `wind-store::wdict`，抽出以免 `wind-dict` 为几个文本函数依赖用户库）。
//!
//! 文件 = `# 注释` + `wind_dict:` YAML 头 + `\n--- !<tag>\n` 分隔的 TSV 数据段。
//! TSV 字段转义：`\`→`\\`、换行→`\n`、制表→`\t`；bool→"1"/"0"。
//!
//! 词条文本域（text / word）走 [`escape_text_field`] / [`unescape_text_field`]，
//! 命令栏语法条目在那里只保护分隔符、反斜杠原样穿过。编码域（code / action / cand_id）
//! 不可能是命令栏语法，仍用下面这对原始函数。

use wind_utils::text::normalize_input;

/// TSV 字段转义（与 Go EscapeField 一致）。
pub fn escape_field(s: &str) -> String {
    if !s.contains(['\\', '\n', '\t']) {
        return s.to_string();
    }
    s.replace('\\', r"\\")
        .replace('\n', r"\n")
        .replace('\t', r"\t")
}

/// TSV 字段反转义（与 Go UnescapeField 一致）。
pub fn unescape_field(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// 词条文本域（`text` / `word`）的 TSV / 设置页转义。
///
/// 命令栏语法条目（`$CC` / `$SS` / `$AA` / 含 `{}` 模板）不转义反斜杠：`\` 已由 cmdbar
/// lexer 负责（见 `wind_cmdbar::decode_escape`），本层再转一次就是双重展开，`D:\\notes`
/// 会变成含换行的路径，且同一条命令在 `system.phrases.toml` 里写法又不同。
///
/// 仍然保护换行与制表：`.wdict.yaml` 是一条记录一行的 TSV，而短语编辑框允许多行，不折成
/// `\n` 文件结构就坏了。
///
/// 代价是一处规范化：源码里的 `\n` 两字符经一轮导出导入会变成真换行。字符串字面量里二者
/// 语义相同、设置页出口又显示回 `\n`，用户无感；反过来不还原的话，字符串外部的排版换行会
/// 固化成字面 `\n`，在表达式位置即语法错误。
pub fn escape_text_field(s: &str) -> String {
    if !is_cmdbar_text(s) {
        return escape_field(s);
    }
    if !s.contains(['\n', '\t']) {
        return s.to_string();
    }
    s.replace('\n', r"\n").replace('\t', r"\t")
}

/// [`escape_text_field`] 的逆。
pub fn unescape_text_field(s: &str) -> String {
    if !is_cmdbar_text(s) {
        return unescape_field(s);
    }
    if !s.contains('\\') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            // `\\` 整体放行而不是拆开逐个复制：拆开会让 `\\n` 的后半 `\n` 在下一轮被
            // 当成换行，把「字面反斜杠 + n」改写成「反斜杠 + 换行」。识别它是为了跳过它。
            Some('\\') => out.push_str(r"\\"),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// 该文本是否按命令栏语法解析（[`escape_text_field`] 一对函数的分流判据）。
///
/// 判据须在存储形态与文件形态上给出同一答案，否则两侧分流不一致，往返即损坏。
/// 这里成立：两种形态的差别只有换行/制表与 `\n`/`\t` 的互换，而 `is_cmdbar_grammar`
/// 看的是顶层 `$CC` 一类 marker 与顶层未转义 `{`，二者都不受影响。
pub fn is_cmdbar_text(s: &str) -> bool {
    wind_cmdbar::is_cmdbar_grammar(s)
}

/// 词库列序纠错的判据：编码列里出现 `$CC(` / `$SS(` / `$AA(` marker
/// ⇒ 这一行的列序写反了，该与文本列对调。
///
/// 刻意比 [`is_cmdbar_text`] 窄：后者单凭顶层 `{` 就成立，而 `{` 可以是某些方案编码
/// 字符集里的正经键位，拿它当对调依据会把合法行翻成镜像垃圾。
/// marker 表与命令栏解析器同源（`wind_cmdbar::contains_marker_call`）。
pub fn is_misplaced_cmdbar_code(code: &str) -> bool {
    wind_cmdbar::contains_marker_call(code)
}

pub fn format_bool(b: bool) -> &'static str {
    if b { "1" } else { "0" }
}

pub fn parse_bool(s: &str) -> bool {
    s == "1" || s.eq_ignore_ascii_case("true")
}

/// wdict phrases 段的一行（导入导出用；不含 is_system——导出仅用户短语）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhraseIo {
    pub code: String,
    pub text: String,
    pub weight: i32,
    pub position: i32,
    pub enabled: bool,
}

const PHRASE_COLUMNS: &[&str] = &["code", "text", "weight", "position", "enabled"];

/// 导出 phrases 为 wdict 文本（YAML 头 + `--- !phrases` TSV 段）。
pub fn export_phrases_wdict(rows: &[PhraseIo], exported_at: &str) -> String {
    let mut s = String::new();
    s.push_str("# WindInput 用户数据文件\n");
    s.push_str("wind_dict:\n");
    s.push_str("  version: 1\n");
    s.push_str("  generator: WindInput\n");
    s.push_str(&format!("  exported_at: {exported_at}\n"));
    s.push_str("  sections:\n");
    s.push_str("    phrases:\n");
    s.push_str(&format!("      columns: [{}]\n", PHRASE_COLUMNS.join(", ")));
    s.push_str("\n--- !phrases\n");
    for r in rows {
        s.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            escape_field(&r.code),
            escape_text_field(&r.text),
            r.weight,
            r.position,
            format_bool(r.enabled),
        ));
    }
    s
}

/// 解析 wdict 文本的 phrases 段。返回 (行, 跳过的非法行数)。
/// 只认 version==1；无 phrases 段返回空。列按 header 声明顺序解析，缺 header 用默认列。
pub fn parse_phrases_wdict(text: &str) -> Result<(Vec<PhraseIo>, usize), String> {
    let normalized = normalize_input(text);
    let text = normalized.as_ref();
    // 1. 头部 = 第一个 "\n---" 之前
    let header = text.split("\n---").next().unwrap_or("");
    if !header.contains("wind_dict:") {
        return Err("不是 WindDict 文件（缺 wind_dict 头）".into());
    }
    // version 校验（简单行扫描，避免引入 yaml 依赖）
    let version_ok = header.lines().any(|l| {
        let t = l.trim();
        t.starts_with("version:") && t.trim_start_matches("version:").trim() == "1"
    });
    if !version_ok {
        return Err("不支持的 WindDict 版本（需 version: 1）".into());
    }
    // 2. 定位 phrases 段：`--- !phrases` 之后到下一个 `\n---` 之前
    let Some(after_tag) = find_section_body(text, "phrases") else {
        return Ok((Vec::new(), 0));
    };
    // 3. 逐行 TSV
    let cols = section_columns_from_header(header, "phrases", PHRASE_COLUMNS);
    let mut rows = Vec::new();
    let mut skipped = 0usize;
    for line in after_tag.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < cols.len() {
            skipped += 1;
            continue;
        }
        let get = |name: &str| -> &str {
            cols.iter()
                .position(|c| c == name)
                .map(|i| fields[i])
                .unwrap_or("")
        };
        rows.push(PhraseIo {
            code: unescape_field(get("code")),
            text: unescape_text_field(get("text")),
            weight: get("weight").trim().parse().unwrap_or(0),
            position: get("position").trim().parse().unwrap_or(0),
            enabled: parse_bool(get("enabled").trim()),
        });
    }
    Ok((rows, skipped))
}

/// wdict words 段的一行（用户词导入导出）。
///
/// count = 选词次数（调频热度）；随导出流转，导入时取 max 合并（见 `import_user_words`）。
/// created_at 属纯本机数据（创建时间），不导出。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WordIo {
    pub code: String,
    pub text: String,
    pub weight: i32,
    /// 选词次数（调频）。外部格式（Rime/TSV）无此列时为 0。
    pub count: u32,
    /// 显式音节边界，优先于 `code` 里的空格。`None` = 未提供，落库时按空格推断。
    ///
    /// 不能只靠 code 里的空格当载体：空格表达不了「单音节」（`xian` 的 0b1 往返后是 0，见测试
    /// `single_syllable_boundary_does_not_survive_text_roundtrip`）。导入闸口的求解结果一律走本字段。
    pub boundary: Option<u64>,
}

const WORD_COLUMNS: &[&str] = &["code", "text", "weight", "count"];

// ─────────────────── 音节码：空格表示 ↔ 存储表示（flat + boundary）───────────────────
//
// wdict 文本里拼音方案的 code 列写成带空格的音节码（`ni hao`），与 rime 源词库同形；
// 落库时拆成 `flat + mask`。列结构不变（仍是 code/text/weight/count），只是 code 列
// 的内容多了空格，故老文件天然兼容。
//
// key 必须保持扁平：`ni hao` 作存储键会让 `niha` 无法前缀匹配，而前缀查询是逐键出候选
// 的命脉（见 docs/design/pinyin-code-domains.md §2.2）。
//
// 判据是「有没有空格」，不看方案类型（本层拿不到引擎类型）：拼音多音节词带空格 → 拆出边界；
// 五笔码与旧版无空格文件 → 0。
//
// 故意不复用 `wind_dict::codetable::syllable_boundary_mask`：它对无空格串返回 `0b1`
// （整串一个音节），这里无空格必须解释为「未知」。依赖方向上也不行（wind-dict 依赖本 crate）。

/// 存储表示 → 空格表示（导出用）。
///
/// `boundary` 语义同 `wind_dict::binformat::DictEntry::boundary`：各音节起始字节位 bitmask。
///
/// 单音节不可逆：`boundary=0b1`（「整串一个音节」）join 后无空格，
/// [`split_spaced_code`] 读回来是 0。单音节无切分歧义，该信息价值极低；且 0 在消费端
/// 一律是「放行」而非「拒绝」（`boundary_compatible` 任一侧为 0 即放行），不会误杀。
pub fn join_code_by_boundary(flat: &str, boundary: u64) -> String {
    // 0 = 无边界信息；0b1 = 整串一个音节。两者都没有可插入的内部边界。
    if boundary == 0 || boundary == 1 {
        return flat.to_string();
    }
    let mut out = String::with_capacity(flat.len() + 8);
    for (i, ch) in flat.char_indices() {
        // bit 位是字节偏移；拼音码为 ASCII，char_indices 的下标即字节位。
        if i > 0 && i < 64 && (boundary >> i) & 1 == 1 {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

/// 空格表示 → 存储表示（导入用）。返回 `(flat_code, boundary)`。
///
/// 无空格 → `(原串, 0)`：可能是五笔码、单音节词，或旧版导出的扁平拼音码，一律按
/// 「无边界信息」处理，消费方降级回 DAG。
pub fn split_spaced_code(spaced: &str) -> (String, u64) {
    if !spaced.contains(' ') {
        return (spaced.to_string(), 0);
    }
    let mut mask = 0u64;
    let mut pos = 0usize;
    for syl in spaced.split(' ').filter(|s| !s.is_empty()) {
        if pos >= 64 {
            // 超出 bitmask 表达范围 → 整体降级，不给半截错误边界（对齐
            // wind_dict::syllable_boundary_mask 的同款契约）。
            return (spaced.replace(' ', ""), 0);
        }
        mask |= 1u64 << pos;
        pos += syl.len();
    }
    (spaced.replace(' ', ""), mask)
}

/// 导出 words 为 wdict 文本（YAML 头 + `--- !words` TSV 段）。
pub fn export_words_wdict(rows: &[WordIo], exported_at: &str) -> String {
    let mut s = String::new();
    s.push_str("# WindInput 用户数据文件\n");
    s.push_str("wind_dict:\n");
    s.push_str("  version: 1\n");
    s.push_str("  generator: WindInput\n");
    s.push_str(&format!("  exported_at: {exported_at}\n"));
    s.push_str("  sections:\n");
    s.push_str("    words:\n");
    s.push_str(&format!("      columns: [{}]\n", WORD_COLUMNS.join(", ")));
    s.push_str("\n--- !words\n");
    for r in rows {
        s.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            escape_field(&r.code),
            escape_text_field(&r.text),
            r.weight,
            r.count,
        ));
    }
    s
}

/// 校验 wdict 头部（含 `wind_dict:` 且 version==1）。各段解析器共用。
fn check_wdict_header(header: &str) -> Result<(), String> {
    if !header.contains("wind_dict:") {
        return Err("不是 WindDict 文件（缺 wind_dict 头）".into());
    }
    let version_ok = header.lines().any(|l| {
        let t = l.trim();
        t.starts_with("version:") && t.trim_start_matches("version:").trim() == "1"
    });
    if !version_ok {
        return Err("不支持的 WindDict 版本（需 version: 1）".into());
    }
    Ok(())
}

/// 解析 wdict 文本的 words 段。返回 (行, 跳过的非法行数)。只认 version==1。
pub fn parse_words_wdict(text: &str) -> Result<(Vec<WordIo>, usize), String> {
    let normalized = normalize_input(text);
    let text = normalized.as_ref();
    parse_word_rows(text, "words")
}

/// 解析 wdict 文本的 temp_words 段（临时词库；列与 words 相同 code/text/weight/count）。
pub fn parse_temp_words_wdict(text: &str) -> Result<(Vec<WordIo>, usize), String> {
    let normalized = normalize_input(text);
    let text = normalized.as_ref();
    parse_word_rows(text, "temp_words")
}

/// words / temp_words 通用行解析（同为 WordIo 列布局，仅段名不同）。无该段返回空。
fn parse_word_rows(text: &str, tag: &str) -> Result<(Vec<WordIo>, usize), String> {
    let header = text.split("\n---").next().unwrap_or("");
    check_wdict_header(header)?;
    let Some(after_tag) = find_section_body(text, tag) else {
        return Ok((Vec::new(), 0));
    };
    let cols = section_columns_from_header(header, tag, WORD_COLUMNS);
    let mut rows = Vec::new();
    let mut skipped = 0usize;
    for line in after_tag.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        // 至少要有 code+text 两列；缺的其它列（如新增的 count）回退默认。
        if fields.len() < 2 {
            skipped += 1;
            continue;
        }
        let get = |name: &str| -> &str {
            cols.iter()
                .position(|c| c == name)
                .and_then(|i| fields.get(i).copied())
                .unwrap_or("")
        };
        rows.push(WordIo {
            code: unescape_field(get("code")),
            text: unescape_text_field(get("text")),
            weight: get("weight").trim().parse().unwrap_or(0),
            count: get("count").trim().parse().unwrap_or(0),
            boundary: None,
        });
    }
    Ok((rows, skipped))
}

// ───────────────────────── freq 段（词频，code/text/count/last_used）─────────────────────────

const FREQ_COLUMNS: &[&str] = &["code", "text", "count", "last_used"];

/// wdict freq 段的一行（词频：真实使用数据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreqIo {
    pub code: String,
    pub text: String,
    pub count: u32,
    pub last_used: i64,
}

/// 解析 wdict 文本的 freq 段。返回 (行, 跳过的非法行数)。无该段返回空。
pub fn parse_freq_wdict(text: &str) -> Result<(Vec<FreqIo>, usize), String> {
    let normalized = normalize_input(text);
    let text = normalized.as_ref();
    let header = text.split("\n---").next().unwrap_or("");
    check_wdict_header(header)?;
    let Some(after_tag) = find_section_body(text, "freq") else {
        return Ok((Vec::new(), 0));
    };
    let cols = section_columns_from_header(header, "freq", FREQ_COLUMNS);
    let mut rows = Vec::new();
    let mut skipped = 0usize;
    for line in after_tag.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 2 {
            skipped += 1;
            continue;
        }
        let get = |name: &str| -> &str {
            cols.iter()
                .position(|c| c == name)
                .and_then(|i| fields.get(i).copied())
                .unwrap_or("")
        };
        rows.push(FreqIo {
            code: unescape_field(get("code")),
            text: unescape_text_field(get("text")),
            count: get("count").trim().parse().unwrap_or(0),
            last_used: get("last_used").trim().parse().unwrap_or(0),
        });
    }
    Ok((rows, skipped))
}

// ───────────────────────── shadow 段（候选调序/删除，动作式）─────────────────────────
//
// 对齐旧 Go wind_dict 的 `--- !shadow` 段：每行一个动作（del/pin）。
// 列 = action, code, word, position, cand_id。
//   - pin：把 word 固定到 position（页内下标）；cand_id 非空 = 动态短语按 id 精准匹配（Rust 扩展列，Go 无）。
//   - del：屏蔽 word（position/cand_id 列留空）。
// 展平/重放的语义（LIFO 存储序、pin/delete 互斥）由 store 层负责，本层只做纯文本编解码。

const SHADOW_COLUMNS: &[&str] = &["action", "code", "word", "position", "cand_id"];

/// wdict shadow 段的一行（动作式）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShadowActionIo {
    /// "pin" | "del"
    pub action: String,
    pub code: String,
    pub word: String,
    /// pin 目标下标；del 忽略（导出为空）。
    pub position: i32,
    /// 动态短语稳定 id；无则 None（导出为空列）。
    pub cand_id: Option<String>,
}

/// 导出「用户词 + shadow」为单个 wdict 文本（`--- !words` + `--- !shadow` 两段）。
///
/// words 段列 = [code, text, weight, count]；shadow 段列 = [action, code, word, position, cand_id]。
/// 老版本只认 words 段、忽略未知 shadow 段与多余列，故向后兼容。
pub fn export_dict_wdict(words: &[WordIo], shadow: &[ShadowActionIo], exported_at: &str) -> String {
    let mut s = String::new();
    s.push_str("# WindInput 用户数据文件\n");
    s.push_str("wind_dict:\n");
    s.push_str("  version: 1\n");
    s.push_str("  generator: WindInput\n");
    s.push_str(&format!("  exported_at: {exported_at}\n"));
    s.push_str("  sections:\n");
    s.push_str("    words:\n");
    s.push_str(&format!("      columns: [{}]\n", WORD_COLUMNS.join(", ")));
    s.push_str("    shadow:\n");
    s.push_str(&format!("      columns: [{}]\n", SHADOW_COLUMNS.join(", ")));
    s.push_str("\n--- !words\n");
    for r in words {
        s.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            escape_field(&r.code),
            escape_text_field(&r.text),
            r.weight,
            r.count,
        ));
    }
    s.push_str("\n--- !shadow\n");
    for r in shadow {
        // del 行的 position/cand_id 列留空，保持列数一致（对齐 Go）。
        let (pos, cid) = if r.action == "pin" {
            (
                r.position.to_string(),
                r.cand_id.clone().unwrap_or_default(),
            )
        } else {
            (String::new(), String::new())
        };
        s.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            escape_field(&r.action),
            escape_field(&r.code),
            escape_text_field(&r.word),
            pos,
            escape_field(&cid),
        ));
    }
    s
}

/// 解析 wdict 文本的 shadow 段。返回 (行, 跳过的非法行数)。
/// 无 shadow 段返回空；version 非 1 报错（与 words 段一致）。
pub fn parse_shadow_wdict(text: &str) -> Result<(Vec<ShadowActionIo>, usize), String> {
    let normalized = normalize_input(text);
    let text = normalized.as_ref();
    let header = text.split("\n---").next().unwrap_or("");
    if !header.contains("wind_dict:") {
        return Err("不是 WindDict 文件（缺 wind_dict 头）".into());
    }
    let version_ok = header.lines().any(|l| {
        let t = l.trim();
        t.starts_with("version:") && t.trim_start_matches("version:").trim() == "1"
    });
    if !version_ok {
        return Err("不支持的 WindDict 版本（需 version: 1）".into());
    }
    let Some(after_tag) = find_section_body(text, "shadow") else {
        return Ok((Vec::new(), 0));
    };
    let cols = section_columns_from_header(header, "shadow", SHADOW_COLUMNS);
    let mut rows = Vec::new();
    let mut skipped = 0usize;
    for line in after_tag.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 3 {
            skipped += 1;
            continue;
        }
        let get = |name: &str| -> &str {
            cols.iter()
                .position(|c| c == name)
                .and_then(|i| fields.get(i).copied())
                .unwrap_or("")
        };
        let action = unescape_field(get("action").trim());
        let word = unescape_text_field(get("word"));
        let code = unescape_field(get("code"));
        if (action != "pin" && action != "del") || code.is_empty() || word.is_empty() {
            skipped += 1;
            continue;
        }
        let cand_id = {
            let c = unescape_field(get("cand_id"));
            if c.is_empty() { None } else { Some(c) }
        };
        rows.push(ShadowActionIo {
            action,
            code,
            word,
            position: get("position").trim().parse().unwrap_or(0),
            cand_id,
        });
    }
    Ok((rows, skipped))
}

/// 从头部读某 section 的 `columns:` 定义（定位到 `<section>:` 之后的首个 `columns:`）；缺则默认列。
fn section_columns_from_header(header: &str, section: &str, default: &[&str]) -> Vec<String> {
    let mut in_section = false;
    for l in header.lines() {
        let t = l.trim();
        if t.strip_suffix(':') == Some(section) {
            in_section = true;
            continue;
        }
        if in_section {
            if let Some(rest) = t.strip_prefix("columns:") {
                let inner = rest.trim().trim_start_matches('[').trim_end_matches(']');
                let cols: Vec<String> = inner
                    .split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect();
                if !cols.is_empty() {
                    return cols;
                }
            } else if t.ends_with(':') {
                break; // 进入下一个 section，本段无 columns 声明
            }
        }
    }
    default.iter().map(|s| s.to_string()).collect()
}

/// 返回 `--- !<tag>\n` 之后、下一个 `\n---` 之前的正文。
fn find_section_body<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let marker = format!("--- !{tag}");
    let start = text.find(&marker)? + marker.len();
    // 跳到该行行尾之后
    let after = &text[start..];
    let body_start = after
        .find('\n')
        .map(|i| start + i + 1)
        .unwrap_or(text.len());
    let body = &text[body_start..];
    // 到下一个 "\n---" 为止
    match body.find("\n---") {
        Some(i) => Some(&body[..i]),
        None => Some(body),
    }
}

// ───────────────────────── 多段组合导出（词库导入导出主入口）─────────────────────────

/// 多段 wdict 导出容器：仅 `Some` 的段写入文件。
#[derive(Debug, Default, Clone)]
pub struct DictWdict {
    pub words: Option<Vec<WordIo>>,
    pub temp_words: Option<Vec<WordIo>>,
    pub freq: Option<Vec<FreqIo>>,
    pub shadow: Option<Vec<ShadowActionIo>>,
}

fn push_word_row(s: &mut String, r: &WordIo) {
    s.push_str(&format!(
        "{}\t{}\t{}\t{}\n",
        escape_field(&r.code),
        escape_text_field(&r.text),
        r.weight,
        r.count,
    ));
}

fn push_shadow_row(s: &mut String, r: &ShadowActionIo) {
    let (pos, cid) = if r.action == "pin" {
        (
            r.position.to_string(),
            r.cand_id.clone().unwrap_or_default(),
        )
    } else {
        (String::new(), String::new())
    };
    s.push_str(&format!(
        "{}\t{}\t{}\t{}\t{}\n",
        escape_field(&r.action),
        escape_field(&r.code),
        escape_text_field(&r.word),
        pos,
        escape_field(&cid),
    ));
}

/// 导出多段 wdict 文本（用户词库/临时词库/词频/候选调整；仅所选段写入）。
/// `schema_id` / `engine_type` 写入头部，供导入时校验来源方案与引擎类型（防跨类型误导，
/// 如五笔词库导入拼音方案致编码错乱）。均可空。
pub fn export_dict_sections(
    d: &DictWdict,
    exported_at: &str,
    schema_id: &str,
    engine_type: &str,
) -> String {
    let mut s = String::new();
    s.push_str("# WindInput 用户数据文件\n");
    s.push_str("wind_dict:\n");
    s.push_str("  version: 1\n");
    s.push_str("  generator: WindInput\n");
    s.push_str(&format!("  exported_at: {exported_at}\n"));
    if !schema_id.is_empty() {
        s.push_str(&format!("  schema_id: {schema_id}\n"));
    }
    if !engine_type.is_empty() {
        s.push_str(&format!("  engine_type: {engine_type}\n"));
    }
    s.push_str("  sections:\n");
    if d.words.is_some() {
        s.push_str("    words:\n");
        s.push_str(&format!("      columns: [{}]\n", WORD_COLUMNS.join(", ")));
    }
    if d.temp_words.is_some() {
        s.push_str("    temp_words:\n");
        s.push_str(&format!("      columns: [{}]\n", WORD_COLUMNS.join(", ")));
    }
    if d.freq.is_some() {
        s.push_str("    freq:\n");
        s.push_str(&format!("      columns: [{}]\n", FREQ_COLUMNS.join(", ")));
    }
    if d.shadow.is_some() {
        s.push_str("    shadow:\n");
        s.push_str(&format!("      columns: [{}]\n", SHADOW_COLUMNS.join(", ")));
    }
    if let Some(rows) = &d.words {
        s.push_str("\n--- !words\n");
        for r in rows {
            push_word_row(&mut s, r);
        }
    }
    if let Some(rows) = &d.temp_words {
        s.push_str("\n--- !temp_words\n");
        for r in rows {
            push_word_row(&mut s, r);
        }
    }
    if let Some(rows) = &d.freq {
        s.push_str("\n--- !freq\n");
        for r in rows {
            s.push_str(&format!(
                "{}\t{}\t{}\t{}\n",
                escape_field(&r.code),
                escape_text_field(&r.text),
                r.count,
                r.last_used,
            ));
        }
    }
    if let Some(rows) = &d.shadow {
        s.push_str("\n--- !shadow\n");
        for r in rows {
            push_shadow_row(&mut s, r);
        }
    }
    s
}

/// 读取头部标量字段（`  key: value`，第一个 `\n---` 之前）。用于取 schema_id / engine_type。
/// 只匹配以 `key:` 打头的行（trim 后），返回其值；无则 None。
pub fn read_header_field(text: &str, key: &str) -> Option<String> {
    let normalized = normalize_input(text);
    let text = normalized.as_ref();
    let header = text.split("\n---").next().unwrap_or("");
    let prefix = format!("{key}:");
    for l in header.lines() {
        let t = l.trim();
        if let Some(rest) = t.strip_prefix(&prefix) {
            let v = rest.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// 文件中实际出现的段标签（`--- !<tag>`），保序去重。用于导入预览"文件含哪些段"。
pub fn sections_present(text: &str) -> Vec<String> {
    let normalized = normalize_input(text);
    let text = normalized.as_ref();
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("--- !") {
            let tag = rest.trim().to_string();
            if !tag.is_empty() && !out.contains(&tag) {
                out.push(tag);
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
