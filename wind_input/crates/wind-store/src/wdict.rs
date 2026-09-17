//! wdict（.wdict.yaml）格式读写 — 复刻旧 Go pkg/dictio。
//! 支持 phrases / words / shadow 三种 section；文件可含多段。
//!
//! 文件 = `# 注释` + `wind_dict:` YAML 头 + `\n--- !<tag>\n` 分隔的 TSV 数据段。
//! TSV 字段转义：`\`→`\\`、换行→`\n`、制表→`\t`；bool→"1"/"0"。
//!
//! **词条文本域（text / word）走 [`escape_text_field`] / [`unescape_text_field`]**，
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
/// **命令栏语法条目（`$CC` / `$SS` / `$AA` / 含 `{}` 模板）不转义反斜杠**：那条源码里的
/// `\` 已由 cmdbar lexer 负责（`\\` `\"` `\n` … 见 `wind_cmdbar::decode_escape`），本层
/// 再转一次就是双重展开——用户按文档写 `open("D:\\notes")`，两层各吃一个反斜杠后 lexer
/// 拿到的是 `D:\notes`，`\n` 当场变成换行，路径静默损坏。要写对得写四个反斜杠，而同一条
/// 命令写进 `system.phrases.toml`（不过本层）却只需两个，同一份语法在不同载体规则不同。
///
/// 仍然保护换行与制表：`.wdict.yaml` 是 TSV，一条记录一行、Tab 分列，而短语编辑框是
/// **多行**输入（`dialogs.rs` 的 `text_multi`），源码里排版换行是允许的。不折成 `\n`
/// 导出的文件会被那个换行切成两行，结构直接坏掉——这条与命令栏语法无关，是文件格式的
/// 硬约束，故不可省。
///
/// 反斜杠不动带来一处**规范化**：源码里的 `\n`（两字符，lexer 的换行转义）经一轮
/// 导出导入会变成真换行。二者在字符串字面量里语义相同（lexer 把 `\n` 解成换行，真换行
/// 直接就是换行），且设置页出口又会把真换行显示回 `\n`，用户无感。反过来「不还原 `\n`」
/// 才是真错：字符串**外部**的排版换行会被固化成字面 `\n`，在表达式位置即语法错误。
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
/// 判据须在**存储形态与文件形态上给出同一答案**，否则两侧分流不一致，往返即损坏。
/// 这里成立：两种形态的差别只有换行/制表与 `\n`/`\t` 的互换，而 `is_cmdbar_grammar`
/// 看的是顶层 `$CC` 一类 marker 与顶层未转义 `{`，二者都不受影响。
fn is_cmdbar_text(s: &str) -> bool {
    wind_cmdbar::is_cmdbar_grammar(s)
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
/// created_at 属纯本机数据（创建时间），**不**导出。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WordIo {
    pub code: String,
    pub text: String,
    pub weight: i32,
    /// 选词次数（调频）。外部格式（Rime/TSV）无此列时为 0。
    pub count: u32,
    /// 显式音节边界，优先于 `code` 里的空格。`None` = 未提供，落库时按空格推断。
    ///
    /// ★★ **为什么不能只靠 code 里的空格当载体**：空格表达不了「单音节」。
    /// `join_code_by_boundary("xian", 0b1)` 产出的是无空格的 `xian`，
    /// `split_spaced_code` 再读回来就是 `0`（既有测试
    /// `single_syllable_boundary_does_not_survive_text_roundtrip` 记录着这条行为）。
    /// 导入闸口求解出的单字词边界若走 code 传递，会**静默退化为 0 而调用方以为补上了**。
    /// 故求解结果一律走本字段。
    pub boundary: Option<u64>,
}

const WORD_COLUMNS: &[&str] = &["code", "text", "weight", "count"];

// ─────────────────── 音节码：空格表示 ↔ 存储表示（flat + boundary）───────────────────
//
// wdict 文本里拼音方案的 code 列写成**带空格的音节码**（`ni hao`），与 rime 源词库同形；
// 落库时拆成 `flat + mask`。**列结构不变**（仍是 code/text/weight/count），只是 code 列
// 的内容多了空格，故老文件天然兼容。
//
// key 必须保持扁平：`ni hao` 作存储键会让 `niha` 无法前缀匹配，而前缀查询是逐键出候选
// 的命脉（见 docs/design/pinyin-code-domains.md §2.2）。
//
// **判据是「有没有空格」，不看方案类型** —— wind-store 拿不到 engine_mgr，无从判断引擎
// 类型。这条规则对三种输入同时正确：拼音多音节词导出带空格 → 拆出边界；五笔码无空格
// → 0；旧版无空格文件 → 0（与改动前等价）。
//
// ⚠️ 故意**不复用** `wind_dict::codetable::syllable_boundary_mask`：那个函数对无空格串
// 返回 `0b1`（「整串一个音节」，因为它的输入保证是已知有音节语义的拼音码），而这里无空格
// 必须解释为「未知」。语义不同，各自实现才是对的。依赖方向上也不可能复用——
// wind-dict 依赖 wind-store，反向依赖会成环。

/// 存储表示 → 空格表示（导出用）。
///
/// `boundary` 语义同 `wind_dict::binformat::DictEntry::boundary`：各音节起始字节位 bitmask。
///
/// ⚠️ **单音节不可逆**：`boundary=0b1`（「整串一个音节」）join 后无空格，
/// [`split_spaced_code`] 读回来是 0。单音节无切分歧义，该信息价值极低；且 0 在消费端
/// 一律是「放行」而非「拒绝」（`boundary_compatible` 任一侧为 0 即放行），不会误杀。
pub fn join_code_by_boundary(flat: &str, boundary: u64) -> String {
    // 0 = 无边界信息；0b1 = 整串一个音节。两者都没有可插入的内部边界。
    if boundary == 0 || boundary == 1 {
        return flat.to_string();
    }
    let mut out = String::with_capacity(flat.len() + 8);
    for (i, ch) in flat.char_indices() {
        // bit 位是**字节**偏移；拼音码为 ASCII，char_indices 的下标即字节位。
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
mod tests {
    use super::*;

    /// 空格表示 ↔ 存储表示的往返。多音节必须无损，否则备份还原仍会丢边界。
    #[test]
    fn spaced_code_roundtrip() {
        // ni|hao → 起始字节位 {0,2}
        assert_eq!(join_code_by_boundary("nihao", 0b101), "ni hao");
        assert_eq!(split_spaced_code("ni hao"), ("nihao".into(), 0b101));
        // xi|an|ning → {0,2,4}
        assert_eq!(join_code_by_boundary("xianning", 0b10101), "xi an ning");
        assert_eq!(
            split_spaced_code("xi an ning"),
            ("xianning".into(), 0b10101)
        );
        // 变长音节 zhuang|ni → {0,6}
        assert_eq!(join_code_by_boundary("zhuangni", 0b1000001), "zhuang ni");
        assert_eq!(
            split_spaced_code("zhuang ni"),
            ("zhuangni".into(), 0b1000001)
        );
    }

    /// 无空格一律解释为「无边界信息」——五笔码、旧版导出的扁平拼音码走的都是这条路，
    /// 结果与本次改动前完全等价（boundary=0 → 消费方降级回 DAG）。
    #[test]
    fn flat_code_means_unknown_boundary() {
        assert_eq!(split_spaced_code("abcd"), ("abcd".into(), 0)); // 五笔
        assert_eq!(split_spaced_code("nihao"), ("nihao".into(), 0)); // 旧版导出
        assert_eq!(split_spaced_code(""), ("".into(), 0));
        // 反向：无边界 / 单音节都不加空格（没有可插入的内部边界）
        assert_eq!(join_code_by_boundary("nihao", 0), "nihao");
        assert_eq!(join_code_by_boundary("ni", 0b1), "ni");
    }

    /// ⚠️ 单音节 `0b1` 不可逆（文档已声明）：join 后无空格，split 回来是 0。
    /// 锁住这个**已知且可接受**的损失，避免日后被当成 bug 顺手「修」成 0b1 ——
    /// 那会让五笔码也被判成单音节，语义反而错了。
    #[test]
    fn single_syllable_boundary_is_lossy_by_design() {
        let joined = join_code_by_boundary("ni", 0b1);
        assert_eq!(split_spaced_code(&joined).1, 0, "单音节边界不经文本往返");
    }

    /// 超过 64 字节的拼接：bitmask 装不下 → 整体降级为 0，不给半截错误边界。
    #[test]
    fn overlong_code_degrades_to_zero() {
        let spaced = ["zhuang"; 12].join(" "); // 12*6=72B
        let (flat, mask) = split_spaced_code(&spaced);
        assert_eq!(flat.len(), 72);
        assert_eq!(mask, 0, "超长码整体降级");
    }

    #[test]
    fn phrases_wdict_roundtrip() {
        let rows = vec![
            PhraseIo {
                code: "bj".into(),
                text: "北京".into(),
                weight: 1000,
                position: 0,
                enabled: true,
            },
            PhraseIo {
                code: "ml".into(),
                text: "多行\n第二行\t带制表".into(),
                weight: 500,
                position: 2,
                enabled: false,
            },
        ];
        let s = export_phrases_wdict(&rows, "2026-07-02T00:00:00+08:00");
        assert!(s.contains("wind_dict:"));
        assert!(s.contains("--- !phrases"));
        let (parsed, skipped) = parse_phrases_wdict(&s).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(parsed, rows, "导出→解析应无损往返（含换行/制表）");
    }

    #[test]
    fn parse_rejects_bad_version() {
        let bad = "wind_dict:\n  version: 2\n  sections:\n    phrases:\n      columns: [code, text, weight, position, enabled]\n\n--- !phrases\nx\t词\t1\t0\t1\n";
        assert!(parse_phrases_wdict(bad).is_err(), "version!=1 应拒绝");
    }

    #[test]
    fn parse_tolerates_bad_lines() {
        let s = "wind_dict:\n  version: 1\n  sections:\n    phrases:\n      columns: [code, text, weight, position, enabled]\n\n--- !phrases\nok\t好\t1000\t0\t1\nbadline_no_tabs\nkw\t坏权重\tNaN\t0\t1\n";
        let (rows, skipped) = parse_phrases_wdict(s).unwrap();
        // 第 2 行列数不足跳过；第 3 行权重非数字 → 回退 0，仍收（不算跳过）
        assert_eq!(rows.len(), 2);
        assert_eq!(skipped, 1);
        assert_eq!(rows[0].code, "ok");
        assert_eq!(rows[1].weight, 0, "非法数字回退 0");
    }

    #[test]
    fn escape_roundtrip() {
        for s in ["普通", "含\t制表", "含\n换行", "反斜杠\\结尾", "混\\t\n合"] {
            assert_eq!(unescape_field(&escape_field(s)), s, "往返应还原: {s:?}");
        }
    }
    #[test]
    fn escape_only_special() {
        assert_eq!(escape_field("abc"), "abc");
        assert_eq!(escape_field("a\tb"), r"a\tb");
        assert_eq!(escape_field("a\nb"), r"a\nb");
        assert_eq!(escape_field(r"a\b"), r"a\\b");
    }
    /// 命令栏语法条目的反斜杠**零层数变化**：这是「文档写 `\\`、照做就对」的全部依据。
    ///
    /// 两层各吃一个是本次修复前的实际行为——用户按文档写两个，落库剩一个，lexer 再把
    /// `\n` 解成换行，路径静默坏掉且不报错。
    #[test]
    fn cmdbar_text_keeps_backslashes() {
        let src = r#"$CC("[打开]", open("D:\\notes\\temp"))"#;
        assert_eq!(escape_text_field(src), src, "出口不得再转义反斜杠");
        assert_eq!(unescape_text_field(src), src, "入口不得再还原反斜杠");
        // `\\n`（字面反斜杠 + n）不可被拆成「反斜杠 + 换行」
        let lit = r#"$CC("x", type("a\\nb"))"#;
        assert_eq!(unescape_text_field(lit), lit);
    }

    /// 普通词条不受影响：仍按原表转义，`C:\Users` 那类未知序列原样保留。
    #[test]
    fn plain_text_still_escaped() {
        assert_eq!(escape_text_field(r"C:\Users"), r"C:\\Users");
        assert_eq!(unescape_text_field(r"C:\\Users"), r"C:\Users");
        assert_eq!(unescape_text_field("甲\\n乙"), "甲\n乙");
    }

    /// 分隔符保护不可省：短语编辑框是多行输入，命令源码里的真换行若原样写进 TSV
    /// 会把一条记录切成两行。**文件形态往返自洽**是这里的验收标准。
    #[test]
    fn cmdbar_text_protects_separators() {
        let stored = "$CC(\"打开\",\n\topen(\"https://x\"))";
        let file = escape_text_field(stored);
        assert!(
            !file.contains('\n') && !file.contains('\t'),
            "真换行/制表必须折成转义序列，否则 TSV 行列结构损坏: {file:?}"
        );
        assert_eq!(unescape_text_field(&file), stored, "文件形态往返应还原");
        assert_eq!(escape_text_field(&unescape_text_field(&file)), file);
    }

    #[test]
    fn bool_format_parse() {
        assert_eq!(format_bool(true), "1");
        assert_eq!(format_bool(false), "0");
        assert!(parse_bool("1"));
        assert!(!parse_bool("0"));
        assert!(!parse_bool(""));
    }

    #[test]
    fn words_wdict_roundtrip() {
        let rows = vec![
            WordIo {
                code: "a".into(),
                text: "工".into(),
                weight: 100,
                count: 42,
                boundary: None,
            },
            WordIo {
                code: "ml".into(),
                text: "多行\n带\t制表".into(),
                weight: 0,
                count: 0,
                boundary: None,
            },
        ];
        let s = export_words_wdict(&rows, "2026-07-11T00:00:00+08:00");
        assert!(s.contains("wind_dict:"));
        assert!(s.contains("--- !words"));
        assert!(s.contains("count"), "列头应含 count");
        let (parsed, skipped) = parse_words_wdict(&s).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(parsed, rows, "导出→解析应无损往返(含换行/制表/count)");
    }

    #[test]
    fn words_wdict_reads_legacy_3col() {
        // 老文件：words 段只声明 3 列、每行 3 字段（无 count）→ count 回退 0，不跳过。
        let legacy = "wind_dict:\n  version: 1\n  sections:\n    words:\n      columns: [code, text, weight]\n\n--- !words\na\t工\t100\n";
        let (rows, skipped) = parse_words_wdict(legacy).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].count, 0, "缺 count 列回退 0");
        assert_eq!(rows[0].weight, 100);
    }

    #[test]
    fn words_parse_rejects_bad_version() {
        let bad = "wind_dict:\n  version: 2\n\n--- !words\na\t工\t1\n";
        assert!(parse_words_wdict(bad).is_err(), "version!=1 应拒绝");
    }

    #[test]
    fn words_parse_tolerates_bad_lines() {
        let s = "wind_dict:\n  version: 1\n  sections:\n    words:\n      columns: [code, text, weight, count]\n\n--- !words\nok\t好\t10\t0\nbadline\nkw\t坏权重\tNaN\t0\n";
        let (rows, skipped) = parse_words_wdict(s).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(skipped, 1, "单字段行跳过");
        assert_eq!(rows[1].weight, 0, "非法数字回退 0");
    }

    #[test]
    fn dict_wdict_roundtrip_words_and_shadow() {
        let words = vec![WordIo {
            code: "a".into(),
            text: "工".into(),
            weight: 100,
            count: 7,
            boundary: None,
        }];
        let shadow = vec![
            ShadowActionIo {
                action: "pin".into(),
                code: "xhwp".into(),
                word: "[小鹤网盘]".into(),
                position: 0,
                cand_id: None,
            },
            ShadowActionIo {
                action: "pin".into(),
                code: "aaaa".into(),
                word: "日期".into(),
                position: 1,
                cand_id: Some("phrase:aaaa:date".into()),
            },
            ShadowActionIo {
                action: "del".into(),
                code: "j".into(),
                word: "见".into(),
                position: 0,
                cand_id: None,
            },
        ];
        let s = export_dict_wdict(&words, &shadow, "2026-07-14T00:00:00+08:00");
        assert!(s.contains("--- !words"));
        assert!(s.contains("--- !shadow"));

        let (pw, sk1) = parse_words_wdict(&s).unwrap();
        assert_eq!(sk1, 0);
        assert_eq!(pw, words, "words 段往返无损（含 count）");

        let (ps, sk2) = parse_shadow_wdict(&s).unwrap();
        assert_eq!(sk2, 0);
        // del 行 position 归 0（导出留空、解析回退 0）
        let expected: Vec<ShadowActionIo> = shadow
            .iter()
            .cloned()
            .map(|mut r| {
                if r.action == "del" {
                    r.position = 0;
                }
                r
            })
            .collect();
        assert_eq!(ps, expected, "shadow 段往返无损（含 cand_id/del）");
    }

    #[test]
    fn parse_shadow_skips_unknown_action() {
        let s = "wind_dict:\n  version: 1\n  sections:\n    shadow:\n      columns: [action, code, word, position, cand_id]\n\n--- !shadow\npin\txhwp\t网盘\t0\t\nbogus\tj\t见\t\t\ndel\tj\t见\t\t\n";
        let (rows, skipped) = parse_shadow_wdict(s).unwrap();
        assert_eq!(rows.len(), 2, "pin + del 收，未知 action 跳过");
        assert_eq!(skipped, 1);
    }

    #[test]
    fn parse_shadow_missing_section_is_empty() {
        // 只有 words 段的老文件：解析 shadow 段返回空，不报错。
        let s = "wind_dict:\n  version: 1\n\n--- !words\na\t工\t1\t0\n";
        let (rows, skipped) = parse_shadow_wdict(s).unwrap();
        assert!(rows.is_empty());
        assert_eq!(skipped, 0);
    }
}
