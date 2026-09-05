//! Rime Codetable 词典读取器 (.dict.yaml 格式)
//!
//! 格式：YAML 头部 + TSV 正文（code\ttext\tweight）
//! 与 Go 版 `wind_input/internal/dict/codetable/` 对齐。

use crate::WEIGHT_RANGE_MAX;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use tracing::{error, info, warn};

/// 词典条目
#[derive(Debug, Clone)]
pub struct CodetableEntry {
    pub text: String,
    pub weight: i32,
    pub order: i32,
    /// 音节边界 bitmask，见 [`crate::binformat::DictEntry::boundary`]。
    /// 拼音词库取自源数据空格（`ni hao` → {0,2}）；五笔等无空格码为 0（无边界信息）。
    pub boundary: u64,
}

/// 由 rime 的空格分隔码算音节起始位 bitmask：`"ni hao"` → 音节 ni|hao → 起始 {0,2} → `0b101`。
///
/// **这个空格就是音节边界的真值来源**——词库作者写下的、无需推断的事实。丢掉它就只能靠 DAG
/// 反猜切分，而 DAG 只按「覆盖字符数」最大化，`xian` 是 xi'an 还是 xian 它无从分辨。
///
/// 返回 0 仅表示「无边界信息」，消费方须降级回 DAG：空码，或拼接后 ≥64 字节的超长码
/// （bitmask 装不下，宁可整体降级也不给半截错误边界；拼音词长上限远小于此，实际不触发）。
/// 单音节返回 `0b1` 而非 0——「整串是一个音节」是真实信息，不是「不知道」。
/// 五笔等非拼音码不走本函数（其 boundary 恒 0），故无「把无空格码误标成单音节」之虞。
fn syllable_boundary_mask(spaced_code: &str) -> u64 {
    let mut mask = 0u64;
    let mut pos = 0usize;
    for syl in spaced_code.split(' ').filter(|s| !s.is_empty()) {
        if pos >= 64 {
            return 0; // 超出 bitmask 表达范围 → 整体降级，不给出半截错误边界
        }
        mask |= 1u64 << pos;
        pos += syl.len();
    }
    mask
}

/// 正文的列序。**文件级属性**：同一词库所有行的列序必然一致，故只判定一次、全文固定。
///
/// 曾经这是逐行猜的（`parts[0].chars().all(is_ascii)` → 认作码列），导致纯 ASCII 词条
/// （`@`、`$CC("[End]", …)`）被当成码、与编码列整个对调，静默装出一条镜像垃圾词条。
/// 更糟的是同一文件不同行可能被判成不同格式——列序是文件属性，逐行决策本身就是错的。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum ColumnLayout {
    /// `text\tcode\tweight`（拼音、符号、快符词库）。缺声明且探测无结论时的默认。
    #[default]
    TextFirst,
    /// `code\ttext\tweight`（五笔类）。
    CodeFirst,
}

impl ColumnLayout {
    /// 供日志展示的列名对（第一列, 第二列）。
    fn column_names(self) -> (&'static str, &'static str) {
        match self {
            ColumnLayout::TextFirst => ("text", "code"),
            ColumnLayout::CodeFirst => ("code", "text"),
        }
    }
}

/// 某列是否呈「编码」形态：小写字母 / 数字 / 音节分隔空格 / 隔音符 / 双拼与仓颉常用的 `;/-`。
///
/// **判据必须建在 code 列而非 text 列**——这是本模块此前出错的根源。code 的形态约束是强的
/// （码只能长成码的样子）；text 列可以是任何东西：汉字、`@`、`$CC("[End]", key.seq("End"))`、
/// 英文单词。对无约束的一侧做形态测试等于赌，对强约束的一侧做才成立。
///
/// 刻意**不含** `.` 与 `,`：符号类词库的词条本身常常就是这两个字符（快符 `。`/`，` 的半角形态），
/// 收进来会让 text 列也呈码形态。多收一个字符换来的是误判风险，不划算。
fn is_code_shape(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| {
            c.is_ascii_lowercase()
                || c.is_ascii_digit()
                || c == ' '
                || c == '\''
                || c == ';'
                || c == '/'
                || c == '-'
        })
}

/// 剥行尾空白，**保留前导**。对齐 librime 的 `boost::algorithm::trim_right`。
///
/// 曾经这里是 `trim()`：Rust 的 `str::trim` 按 Unicode White_Space 判定，而 **U+3000 全角空格
/// 属于该集合**，于是「全角空格」这个词条本身会被当成缩进削掉，整行字段左移一格
/// （`　\tcokg\t\t全角空格` → `cokg\t\t全角空格`，text 变成编码、code 变成空串），
/// 两字段的行则直接掉到列数门槛之下被丢弃。**词条内容不该被当成排版空白。**
fn trim_line_end(line: &str) -> &str {
    line.trim_end()
}

/// librime 的注释开关指令：**整行恰好**等于它时，其后所有 `#` 开头的行按**数据**解析。
/// 这是 Rime 让 `#` 本身能当编码或词条用的唯一途径（`entry_collector.cc`）。
const NO_COMMENT_DIRECTIVE: &str = "# no comment";

/// 在正文中定位 [`NO_COMMENT_DIRECTIVE`] 所在行的起始字节偏移。
///
/// 先用子串搜索快速定位（大词库上这是一次 memmem，代价可忽略），再校验它**独占一行**——
/// `# no commentX` 或行中间出现的同名子串都不算。
///
/// **行尾空白按 [`trim_line_end`] 剥除后再判定**：librime 是先 `trim_right` 整行、
/// 再与本指令做相等比较，故 `# no comment ` 这类带尾随空格的写法它同样认。
/// 数据行走的也是同一套行尾规则，一个文件里不该有两种行尾语义。
///
/// 之所以要预先求出这个偏移：该指令是**跨行有状态**的，而正文解析会按字节切块并行，
/// 每块无从知道自己之前有没有出现过它。先在全局求出偏移，各块便可按「本行起始偏移是否
/// 越过它」独立判定，不必引入跨线程状态。
fn find_no_comment_directive(body: &str) -> Option<usize> {
    let mut from = 0usize;
    while let Some(rel) = body[from..].find(NO_COMMENT_DIRECTIVE) {
        let start = from + rel;
        let end = start + NO_COMMENT_DIRECTIVE.len();
        let at_line_start = start == 0 || body.as_bytes()[start - 1] == b'\n';
        // 取到本行末尾（下一个 \n 之前），剥掉行尾空白后须一无所剩，才算独占一行。
        let rest = &body[end..];
        let tail = rest.split('\n').next().unwrap_or(rest);
        let at_line_end = trim_line_end(tail).is_empty();
        if at_line_start && at_line_end {
            return Some(start);
        }
        from = end;
    }
    None
}

/// 按行遍历正文（或其中一块），产出 `(行内容, 本行的 `#` 是否仍作注释)`。
///
/// `base` 是本切片在整篇正文中的起始偏移，`cutoff` 是 [`find_no_comment_directive`] 的结果。
/// 指令行自身仍按注释跳过（`start <= cutoff`），其后各行才转为数据。
fn body_lines(
    slice: &str,
    base: usize,
    cutoff: Option<usize>,
) -> impl Iterator<Item = (&str, bool)> {
    let mut off = base;
    slice.split_inclusive('\n').map(move |raw| {
        let start = off;
        off += raw.len();
        // 与 str::lines() 同语义：剥掉行尾 \n 及其前的 \r
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line = line.strip_suffix('\r').unwrap_or(line);
        (line, cutoff.is_none_or(|c| start <= c))
    })
}

/// 单行投票。仅当两列中**恰有一列**像码时才给结论；两列都像（英文词库 `abandon\tabandon`）
/// 或都不像时弃权——弃权比瞎猜安全，多数票和默认值会兜住。
fn vote_layout(line: &str, comments_on: bool) -> Option<ColumnLayout> {
    let line = trim_line_end(line);
    if line.is_empty() || (comments_on && line.starts_with('#')) {
        return None;
    }
    let parts: Vec<&str> = line.split('\t').collect();
    if parts.len() < 2 {
        return None;
    }
    match (is_code_shape(parts[0]), is_code_shape(parts[1])) {
        (true, false) => Some(ColumnLayout::CodeFirst),
        (false, true) => Some(ColumnLayout::TextFirst),
        _ => None,
    }
}

/// 探测取样上限：最多扫这么多行正文。
const LAYOUT_SAMPLE_LINES: usize = 200;
/// 攒够这么多张有效票就提前收工，不必扫满 [`LAYOUT_SAMPLE_LINES`]。
const LAYOUT_SAMPLE_VOTES: usize = 32;

/// 按正文前若干行投票判列序，返回 `(列序, text优先票数, code优先票数)`。
/// 平票或零票 → [`ColumnLayout::TextFirst`]（默认）。
fn detect_layout(body: &str, cutoff: Option<usize>) -> (ColumnLayout, usize, usize) {
    let (mut text_first, mut code_first) = (0usize, 0usize);
    for (line, comments_on) in body_lines(body, 0, cutoff).take(LAYOUT_SAMPLE_LINES) {
        match vote_layout(line, comments_on) {
            Some(ColumnLayout::TextFirst) => text_first += 1,
            Some(ColumnLayout::CodeFirst) => code_first += 1,
            None => {}
        }
        if text_first + code_first >= LAYOUT_SAMPLE_VOTES {
            break;
        }
    }
    let layout = if code_first > text_first {
        ColumnLayout::CodeFirst
    } else {
        ColumnLayout::TextFirst
    };
    (layout, text_first, code_first)
}

/// 正文各列的角色分配。**文件级属性**，判定一次、全文固定。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct ColumnSpec {
    text_col: usize,
    code_col: usize,
    /// 权重列下标。`None` = 该词库无权重列（`columns:` 声明里没有 `weight`）。
    ///
    /// 未声明 `columns:` 时取第 3 列（下标 2），对齐 librime 的默认列序
    /// `[text, code, weight]`（`dict_settings.cc` 的 `GetColumnIndex` 在 `columns` 为空时
    /// 硬编码 text=0/code=1/weight=2）。声明之外的列一律不读——真实反例
    /// `wubi86_jidian_extra_district.dict.yaml` 是 `text\tcode\t\t区号` 四列，
    /// 第 4 列是行政区划编号；它的第 3 列为空，故按 Rime 语义同样得权重 0。
    weight_col: Option<usize>,
    /// 本词库的 code 列是否承载**音节**语义（→ 算 boundary、出简拼、去空格拼平）。
    ///
    /// 判据 = `code 列采样到空格` **或** `text 列在 code 列之前`。
    ///
    /// 两个条件各自补一个洞：
    /// - **空格**是音节边界的正面证据，与列顺序无关。加上它，`columns: [code, text, weight]`
    ///   这类**编码在前的拼音库**不再被当成形码——那才是真正严重的方向：
    ///   整张简拼表丢失、音节边界全归零，双拼与整句逻辑集体降级回 DAG 猜切分。
    /// - **列顺序**保留下来是因为「无空格」推不出「无音节」：`好\thao` 这样的单音节拼音
    ///   词条同样没有空格，而它的 `0b1`（整串是一个音节）是**真信息**，双拼真值校验要用。
    ///   无空格时既可能是单音节拼音、也可能是形码，**数据本身无法区分**——五笔类惯例把
    ///   编码写在前面，故沿用列顺序作这一情形的兜底。
    ///
    /// 残留缺口：声明成 `[text, weight, code, stem]` 的**形码**库（真实样本
    /// `tigercode/tigress.dict.yaml`）仍会被判为有音节，每条拿到 `boundary=0b1`。
    /// 这与本判据引入前的行为一致（无回归），且码表引擎不消费 boundary，故当前无损害。
    /// 要根治须把词库类型（schema 的 `dict_type`）传进解析层——**那才是权威判据**，
    /// 本字段的两条都只是数据侧的近似。
    has_syllables: bool,
}

impl ColumnSpec {
    /// 由探测/默认列序构造。权重仍取第 3 列，与 librime 无声明时的默认一致；
    /// 探测只负责 text/code 谁先谁后（librime 不做探测，恒 text 在前）。
    fn from_layout(layout: ColumnLayout, has_syllables: bool) -> Self {
        let (text_col, code_col) = match layout {
            ColumnLayout::TextFirst => (0, 1),
            ColumnLayout::CodeFirst => (1, 0),
        };
        Self {
            text_col,
            code_col,
            weight_col: Some(2),
            has_syllables,
        }
    }

    /// 本行至少要有这么多列才能取齐**必需**字段（text/code）。
    ///
    /// **权重不计入**：它是「有则取、无则 0」。若把 weight_col 也算进门槛，
    /// 只有两列的词库（快符 `12_kf.dict.yaml` 全部 26 行皆两列）会被整体丢弃。
    /// librime 同样是逐字段做 `num_columns > x_column` 的越界保护，而非整行门槛。
    fn required_cols(&self) -> usize {
        self.text_col.max(self.code_col) + 1
    }
}

/// 头部 `columns:` 声明的解析结果。区分三态是为了给出**准确**的诊断——
/// 把「声明不完整」误报成「未声明」，会让照日志排查的人去看头部、发现声明确实存在，
/// 从而排除掉正确的线索。
#[derive(Debug, PartialEq, Eq)]
enum ColumnsDecl {
    /// 声明完整可用。
    Usable(ColumnSpec),
    /// 声明了 `columns:` 但没有 `code` 列。
    ///
    /// 这在 librime 里是**合法的自动编码词库**——交给 encoder 按方案字表 +
    /// `encoder.rules` 从构成字反推编码。本项目未实现该特性，故整库跳过而非降级探测：
    /// 降级探测会把权重列当成编码（`is_code_shape("100")` 为真），静默灌进一整库垃圾编码。
    MissingCode,
    /// 声明了 `columns:` 但没有 `text` 列。librime 对此直接丢弃整个文件。
    MissingText,
    /// 没有 `columns:` 声明。
    Absent,
}

/// 从 YAML 头部解析 `columns:` 声明，按声明顺序定位 text/code/weight 各列。
///
/// 两种 YAML 写法都认：块序列（`columns:` 换行后 `  - text`）与流式序列
/// （`columns: [text, code, weight]`）。**流式必须支持**——它是合法 YAML，而我们的
/// 警告文案就建议用户这么写；只认块序列会让照建议改完的用户看到「改了没用」。
fn parse_columns_header(header: &str) -> ColumnsDecl {
    let mut in_columns = false;
    let mut names: Vec<String> = Vec::new();
    for raw in header.lines() {
        // 剥行内注释：flypy 词库写作 `columns:    # 码表格式` / `  - text    # 文字`
        let line = raw.split('#').next().unwrap_or("");
        let trimmed = line.trim();
        if !in_columns {
            // 顶格的 `columns:` 才是块起点（缩进的同名键属于别的映射）
            let Some(rest) = trimmed.strip_prefix("columns:") else {
                continue;
            };
            if line.starts_with([' ', '\t']) {
                continue;
            }
            in_columns = true;
            // 流式：`columns: [text, code, weight]` —— 同一行取完即收工
            let rest = rest.trim();
            if let Some(inner) = rest.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                names.extend(
                    inner
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty()),
                );
                break;
            }
            continue;
        }
        let Some(item) = trimmed.strip_prefix('-') else {
            if trimmed.is_empty() {
                continue; // 块内空行/纯注释行
            }
            break; // 回到非缩进键 → columns 块结束
        };
        names.push(item.trim().to_string());
    }
    if !in_columns {
        return ColumnsDecl::Absent;
    }
    let find = |k: &str| names.iter().position(|n| n == k);
    // stem 等未支持的列名占位但不取用——占位会顺延其后各列的下标，必须计入。
    let Some(text_col) = find("text") else {
        return ColumnsDecl::MissingText;
    };
    let Some(code_col) = find("code") else {
        return ColumnsDecl::MissingCode;
    };
    ColumnsDecl::Usable(ColumnSpec {
        text_col,
        code_col,
        weight_col: find("weight"),
        has_syllables: false, // 由 resolve_columns 采样正文后填入
    })
}

/// 从 YAML 头部取出 librime 的 `sort:` 声明值（无声明返回 `None`）。
///
/// 该键**不被本输入法消费**，取出仅为告警——此前它被静默跳过，配置者把 `by_weight` 改成
/// `original` 会观察到毫无变化且拿不到任何诊断。排序语义见 `resolve_columns` 中的告警文案。
///
/// 与 `columns:` 同规则：只认顶格键（缩进的同名键属于别的映射），并剥除行内注释。
fn parse_sort_header(header: &str) -> Option<String> {
    for raw in header.lines() {
        let line = raw.split('#').next().unwrap_or("");
        if line.starts_with([' ', '\t']) {
            continue;
        }
        if let Some(rest) = line.trim().strip_prefix("sort:") {
            let v = rest.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// 判定 code 列是否承载音节语义。见 [`ColumnSpec::has_syllables`] 对两条判据的说明。
fn detect_syllables(body: &str, text_col: usize, code_col: usize, cutoff: Option<usize>) -> bool {
    // text 在 code 之前 → 拼音惯例（五笔类惯例是编码在前）。无空格时靠它兜底。
    if text_col < code_col {
        return true;
    }
    // 编码在前，但采样到空格 → 仍是拼音（音节分隔的正面证据胜过列顺序惯例）。
    let need = text_col.max(code_col) + 1;
    for (line, comments_on) in body_lines(body, 0, cutoff).take(LAYOUT_SAMPLE_LINES) {
        let line = trim_line_end(line);
        if line.is_empty() || (comments_on && line.starts_with('#')) {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < need {
            continue;
        }
        if parts[code_col].contains(' ') {
            return true;
        }
    }
    false
}

/// 文件级列规格判定。返回 `None` = **整库跳过**（声明残缺，宁可显式少一个库，
/// 也不静默灌进错位数据）。
///
/// 优先级：头部 `columns:` 声明 → 无声明则探测正文列序并 WARN 建议补声明。
fn resolve_columns(
    content: &str,
    body: &str,
    path: &Path,
    cutoff: Option<usize>,
) -> Option<ColumnSpec> {
    let header = &content[..content.len() - body.len()];
    if let Some(sort) = parse_sort_header(header) {
        warn!(
            "词库 {} 声明了 `sort: {}`，本输入法**不解析此键**（librime 用它决定库内同码条目的排列，\
             我们没有对应实现）。词库顺序请改用方案 .schema.toml 的 `[[dictionaries]]` 配置：\
             `base_order` 定库间硬分档，`default_weight` 抹平整库权重使其退化为文件顺序\
             （等价于 rime 的 `sort: original`），`[engine.codetable].base_sort` 定全局排序维度。",
            path.display(),
            sort
        );
    }
    match parse_columns_header(header) {
        ColumnsDecl::Usable(mut spec) => {
            spec.has_syllables = detect_syllables(body, spec.text_col, spec.code_col, cutoff);
            Some(spec)
        }
        ColumnsDecl::MissingCode => {
            error!(
                "词库 {} 的 columns: 声明中没有 code 列，整库跳过。\
                 这在 Rime 里是「自动编码」词库（由 encoder.rules 从构成字反推编码），\
                 本输入法尚未支持；若该库本就有编码列，请把它加进 columns: 声明。",
                path.display()
            );
            None
        }
        ColumnsDecl::MissingText => {
            error!(
                "词库 {} 的 columns: 声明中没有 text 列，整库跳过（Rime 同样丢弃此类文件）。",
                path.display()
            );
            None
        }
        ColumnsDecl::Absent => {
            let (layout, text_first, code_first) = detect_layout(body, cutoff);
            let (c1, c2) = layout.column_names();
            let basis = if text_first + code_first == 0 {
                "无一行给出有效判据，直接采用默认列序"
            } else {
                "依据正文投票"
            };
            warn!(
                "词库 {} 未声明 columns:，{}判定列序为 {}\\t{}\\tweight\
                 （取样前 {} 行：{} 票 text 优先 / {} 票 code 优先）。\
                 探测是启发式的，纯 ASCII 词条（如 @、$CC(...)）可能判错；\
                 建议在 YAML 头部显式声明，两种写法均可：\
                 单行 `columns: [{}, {}, weight]`，或换行后逐项 `  - {}` / `  - {}` / `  - weight`。",
                path.display(),
                basis,
                c1,
                c2,
                LAYOUT_SAMPLE_LINES,
                text_first,
                code_first,
                c1,
                c2,
                c1,
                c2,
            );
            let (text_col, code_col) = match layout {
                ColumnLayout::TextFirst => (0, 1),
                ColumnLayout::CodeFirst => (1, 0),
            };
            let has_syllables = detect_syllables(body, text_col, code_col, cutoff);
            Some(ColumnSpec::from_layout(layout, has_syllables))
        }
    }
}

/// 解析过程中被跳过/降级的行的计数，收尾时汇总输出。
///
/// 这不修任何 bug，但把整类**静默失败**变成可见：以前一行因空字段被丢、
/// 一个权重因格式不认识变成 0，都是无声无息的，用户只看到「某些词打不出来」。
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ParseStats {
    /// 列数不足，取不齐 text/code。
    pub short: usize,
    /// text 或 code 为空串。
    pub empty_field: usize,
    /// 权重列存在但解析失败（如 Rime 的 `50%` 相对权重，本项目未实现预设词库故无基准）。
    pub bad_weight: usize,
    /// 带非零权重的条目数（值域诊断的分母）。
    pub weighted: usize,
    /// 权重**超出约定值域 `0~10000`** 的条目数。见 [`WEIGHT_RANGE_MAX`]。
    pub over_range: usize,
    /// 实测最大权重（诊断用；变换用的是方案声明值，见 `dict-weight-normalization.md` §4.3）。
    pub max_weight: i32,
}

impl ParseStats {
    fn merge(&mut self, o: &ParseStats) {
        self.short += o.short;
        self.empty_field += o.empty_field;
        self.bad_weight += o.bad_weight;
        self.weighted += o.weighted;
        self.over_range += o.over_range;
        self.max_weight = self.max_weight.max(o.max_weight);
    }

    fn is_clean(&self) -> bool {
        self.short == 0 && self.empty_field == 0 && self.bad_weight == 0
    }

    /// 有异常才出日志——干净的词库不该刷屏。
    fn log_if_dirty(&self, path: &Path) {
        self.log_weight_range(path);
        if self.is_clean() {
            return;
        }
        warn!(
            "词库 {} 解析期跳过/降级统计：列数不足 {} 行、text或code为空 {} 行、权重无法解析 {} 处（按 0 计）。",
            path.display(),
            self.short,
            self.empty_field,
            self.bad_weight
        );
    }

    /// **权重值域诊断**：约定是 `0~10000`（与短语权重同轴，见
    /// `docs/design/dict-weight-normalization.md`），但这条约定此前只写在注释里、
    /// 没有任何环节在执行。超范围的词库会让「短语 vs 码表」的权重比较失真——
    /// 短语上限 10000，对手若是 1e7 量级的原始语料词频，用户把短语权重拉满也压不过。
    ///
    /// 只告警、不改值。变换是**按库 opt-in** 的（`[dictionaries.weight_spec]`），
    /// 理由见设计文档 §3.2：强制归一会把守约词库的分布也一起改掉。
    fn log_weight_range(&self, path: &Path) {
        if self.over_range == 0 {
            return;
        }
        let pct = 100.0 * self.over_range as f64 / self.weighted.max(1) as f64;
        warn!(
            "词库 {} 的权重超出约定值域 0~{}：{}/{} 条（{:.1}%）超范围，实测最大 {}。\
             跨来源排序（短语 vs 码表）会因此失真——短语权重上限 {}，压不过更大的对手。\
             请在方案的 `[[dictionaries]]` 下配 `[dictionaries.weight_spec]`（median/max/mode=\"log\"）\
             开启归一化，或先把词库权重规范到该值域。",
            path.display(),
            WEIGHT_RANGE_MAX,
            self.over_range,
            self.weighted,
            pct,
            self.max_weight,
            WEIGHT_RANGE_MAX
        );
    }
}

/// Rime Codetable 词典（内存模式，按 code 分组的 BTreeMap）
pub struct CodetableDict {
    /// code -> entries（按 weight 降序排列）
    entries: BTreeMap<String, Vec<CodetableEntry>>,
    /// 总条目数
    total_entries: usize,
}

impl CodetableDict {
    /// 从 .dict.yaml 文件加载（code 保持原样）
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        Self::load_impl(path, false)
    }

    /// 从 .dict.yaml 加载并把 code 列小写化（英文词库用：大小写不敏感前缀匹配，text 保留原样大小写）
    pub fn load_lowercased(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        Self::load_impl(path, true)
    }

    fn load_impl(path: impl AsRef<Path>, lowercase_code: bool) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)?;

        let mut entries: BTreeMap<String, Vec<CodetableEntry>> = BTreeMap::new();
        let mut order: i32 = 0;

        // 无 `...` 分隔行 → 无正文 → 零条目（与并行解析路径一致）。
        let body = match rime_body_offset(&content) {
            Some(off) => &content[off..],
            None => {
                warn!(
                    "词库 {} 缺少 YAML 正文分隔行 `...`，按零条目处理（文件可能被截断或损坏）。",
                    path.display()
                );
                ""
            }
        };
        // `# no comment` 指令位置：其后各行的 `#` 转为数据。先于列判定求出，供全程共用。
        let cutoff = find_no_comment_directive(body);
        // 列规格判定一次、全文固定——不再逐行猜（见 [`ColumnLayout`]）。
        // None = 声明残缺，整库跳过（已在 resolve_columns 内 error! 说明原因）。
        let Some(spec) = resolve_columns(&content, body, path, cutoff) else {
            return Ok(Self {
                entries: BTreeMap::new(),
                total_entries: 0,
            });
        };
        let mut stats = ParseStats::default();

        for (line, comments_on) in body_lines(body, 0, cutoff) {
            let Some(parsed) = parse_rime_line(line, comments_on, lowercase_code, spec, &mut stats)
            else {
                continue;
            };

            let entry = CodetableEntry {
                text: parsed.text,
                weight: parsed.weight,
                order,
                boundary: parsed.boundary,
            };

            entries.entry(parsed.code).or_default().push(entry);
            order += 1;
        }

        // 每个 code 下按 weight 降序排列
        for code_entries in entries.values_mut() {
            code_entries.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.order.cmp(&b.order)));
        }

        let total: usize = entries.values().map(|v| v.len()).sum();
        info!(
            "Loaded codetable: {} ({} keys, {} entries)",
            path.display(),
            entries.len(),
            total
        );
        stats.log_if_dirty(path);

        Ok(Self {
            entries,
            total_entries: total,
        })
    }

    /// 精确查找
    pub fn search(&self, code: &str) -> Vec<(String, i32, i32)> {
        self.entries
            .get(code)
            .map(|entries| {
                entries
                    .iter()
                    .map(|e| (e.text.clone(), e.weight, e.order))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 精确查找，并带出音节边界（内存路径对应 [`crate::cached::CachedDict::search_with_boundary`]）。
    pub fn search_with_boundary(&self, code: &str) -> Vec<crate::cached::DictHit> {
        self.entries
            .get(code)
            .map(|entries| {
                entries
                    .iter()
                    .map(|e| crate::cached::DictHit {
                        code: code.to_string(),
                        text: e.text.clone(),
                        weight: e.weight,
                        order: e.order,
                        boundary: e.boundary,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 前缀查找，并带出音节边界（内存路径对应
    /// [`crate::cached::CachedDict::search_prefix_with_boundary`]）。排序/截断语义同
    /// [`Self::search_prefix`]。
    pub fn search_prefix_with_boundary(
        &self,
        prefix: &str,
        limit: usize,
    ) -> Vec<crate::cached::DictHit> {
        let mut results: Vec<crate::cached::DictHit> = Vec::new();
        for (code, entries) in self.entries.range(prefix.to_string()..) {
            if !code.starts_with(prefix) {
                break;
            }
            for e in entries {
                results.push(crate::cached::DictHit {
                    code: code.clone(),
                    text: e.text.clone(),
                    weight: e.weight,
                    order: e.order,
                    boundary: e.boundary,
                });
            }
            if results.len() >= limit * 2 {
                break; // 收集足够多后排序截断（同 search_prefix）
            }
        }
        results.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.order.cmp(&b.order)));
        results.truncate(limit);
        results
    }

    /// 前缀查找，只保留音节数不超过 `max_syllables` **且在 `completed_len` 处音节边界对齐**
    /// 的条目（内存路径对应
    /// [`crate::datformat::WdatReader::search_prefix_syllable_capped`]，理由见那里）。
    ///
    /// ⚠️ **过滤必须在 `limit * 2` 提前中断之前施加**，否则那道中断会被不合格条目填满、
    /// 提前跳出，合格条目一条都收不到 —— 与 wdat 侧「配额被丢弃项吃光」是同一个坑，
    /// 只是换了个形态。
    pub fn search_prefix_with_boundary_syllable_capped(
        &self,
        prefix: &str,
        limit: usize,
        max_syllables: u32,
        completed_len: usize,
    ) -> Vec<crate::cached::DictHit> {
        let mut results: Vec<crate::cached::DictHit> = Vec::new();
        for (code, entries) in self.entries.range(prefix.to_string()..) {
            if !code.starts_with(prefix) {
                break;
            }
            for e in entries {
                if !crate::cached::prefix_entry_keep(
                    e.boundary,
                    code.len(),
                    max_syllables,
                    completed_len,
                ) {
                    continue;
                }
                results.push(crate::cached::DictHit {
                    code: code.clone(),
                    text: e.text.clone(),
                    weight: e.weight,
                    order: e.order,
                    boundary: e.boundary,
                });
            }
            if results.len() >= limit * 2 {
                break;
            }
        }
        results.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.order.cmp(&b.order)));
        results.truncate(limit);
        results
    }

    /// 是否存在**严格长于** `prefix` 的编码。内存路径对应
    /// [`crate::datformat::WdatReader::has_longer_code`]，语义与之一致。
    ///
    /// BTreeMap 有序：从 `prefix` 起扫，遇到第一个不以 `prefix` 开头的 key 即止——
    /// 实际只看常数条（`prefix` 自身 + 至多一个后继）。
    pub fn has_longer_code(&self, prefix: &str) -> bool {
        for code in self.entries.range(prefix.to_string()..).map(|(c, _)| c) {
            if !code.starts_with(prefix) {
                return false;
            }
            // 已知 code 以 prefix 开头 → 字节更长 ⟺ 字符更多（UTF-8 同前缀）。
            if code.len() > prefix.len() {
                return true;
            }
        }
        false
    }

    /// 前缀查找
    pub fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<(String, String, i32, i32)> {
        let mut results = Vec::new();

        // BTreeMap 范围查询：找到所有以 prefix 开头的 key
        let range = self.entries.range(prefix.to_string()..);
        for (code, entries) in range {
            if !code.starts_with(prefix) {
                break;
            }
            for e in entries {
                results.push((code.clone(), e.text.clone(), e.weight, e.order));
            }
            if results.len() >= limit * 2 {
                break; // 收集足够多后排序截断
            }
        }

        // 按 weight 降序排序
        results.sort_by(|a, b| b.2.cmp(&a.2).then(a.3.cmp(&b.3)));
        results.truncate(limit);
        results
    }

    /// 遍历全部条目(供反查索引构建):对每个 (code, text, weight) 调用 `f`。
    pub fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
        for (code, entries) in &self.entries {
            for e in entries {
                f(code, &e.text, e.weight);
            }
        }
    }

    /// 总条目数
    pub fn len(&self) -> usize {
        self.total_entries
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.total_entries == 0
    }

    /// 创建空词典
    pub fn empty() -> Self {
        Self {
            entries: BTreeMap::new(),
            total_entries: 0,
        }
    }

    /// 导出到 DictWriter（用于写入 .wdb 缓存）
    pub fn export_to_writer(&self, writer: &mut crate::binformat::DictWriter) {
        for (code, entries) in &self.entries {
            let entries_data: Vec<(String, i32)> =
                entries.iter().map(|e| (e.text.clone(), e.weight)).collect();
            writer.add(code.clone(), entries_data);
        }
    }

    /// 同 [`export_to_writer`]，导出到 wdat（DAT）写入器。
    /// 携带每条的全局 `order`（词库文件出现序）：使无权重候选跨编码按出现顺序排序，
    /// 而非退化为编码字母序（对应 wdat v3 的 order 字段，见 datformat.rs）。
    /// 一并携带 `boundary`（wdat v4 音节边界；非拼音词库为 0）。
    pub fn export_to_wdat(&self, writer: &mut crate::datformat::WdatWriter) {
        for (code, entries) in &self.entries {
            let entries_data: Vec<(String, i32, u32, u64)> = entries
                .iter()
                .map(|e| (e.text.clone(), e.weight, e.order.max(0) as u32, e.boundary))
                .collect();
            writer.add_with_boundary(code.clone(), entries_data);
        }
    }

    /// 合并单个条目（用于从 CachedDict 提取数据）。
    /// 入参只有扁平 code，无音节信息 → boundary=0（消费方降级回 DAG）。
    pub fn merge_single(&mut self, code: String, text: String, weight: i32, _order: i32) {
        let existing = self.entries.entry(code).or_default();
        existing.push(CodetableEntry {
            text,
            weight,
            order: existing.len() as i32,
            boundary: 0,
        });
        self.total_entries += 1;
    }

    /// 写入 .wdb 缓存文件
    pub fn write_to_wdb(&self, path: &std::path::Path) -> anyhow::Result<()> {
        use crate::binformat::DictWriter;
        let mut writer = DictWriter::new();
        self.export_to_writer(&mut writer);
        writer.write(path)
    }
}

/// 解析一行 rime 词条 → `(code, abbrev, text, weight)`，格式自适配（五笔 `code\ttext\tweight`
/// 或拼音 `text\tcode\tweight`）。`abbrev`=简拼（声母缩写）：仅拼音多音节词有，取每个空格
/// 分隔音节的首字母（如 `ni hao`→`nh`）；五笔/单音节为 None。返回 None 表示跳过该行。
pub(crate) struct RimeLine {
    pub code: String,
    pub abbrev: Option<String>,
    pub text: String,
    pub weight: i32,
    /// 音节边界 bitmask（见 [`syllable_boundary_mask`]）；五笔码为 0。
    pub boundary: u64,
}

fn parse_rime_line(
    line: &str,
    comments_on: bool,
    lowercase_code: bool,
    spec: ColumnSpec,
    stats: &mut ParseStats,
) -> Option<RimeLine> {
    // 只剥行尾：词条内容可能以空白开头（「全角空格」这个词条本身就是 U+3000），
    // 前导 trim 会把它当缩进削掉、导致整行字段左移。见 [`trim_line_end`]。
    let line = trim_line_end(line);
    // `comments_on == false` = 本行位于 `# no comment` 之后，`#` 此时是**数据**而非注释。
    if line.is_empty() || (comments_on && line.starts_with('#')) {
        return None;
    }
    let parts: Vec<&str> = line.split('\t').collect();
    if parts.len() < spec.required_cols() {
        stats.short += 1;
        return None;
    }
    // 列位置由文件级判定给定（头部 `columns:` 声明，或整文件探测），不再逐行猜。
    let raw_code = parts[spec.code_col];
    let text = parts[spec.text_col];
    if text.is_empty() || raw_code.is_empty() {
        // 空 code 会让整批条目挤进 entries[""]；空 text 是无意义候选。librime 亦跳过。
        stats.empty_field += 1;
        return None;
    }
    // 是否有音节语义由**文件级采样 code 列是否含空格**决定，与列顺序无关（见 ColumnSpec）。
    let (mut code, mut abbrev, boundary) = if spec.has_syllables {
        // 简拼：2+ 音节时取每个空格分隔音节的首字母（对齐 Go loadRimeFile）。
        let syllables: Vec<&str> = raw_code.split(' ').filter(|s| !s.is_empty()).collect();
        let abbrev = if syllables.len() >= 2 {
            Some(
                syllables
                    .iter()
                    .filter_map(|s| s.chars().next())
                    .collect::<String>(),
            )
        } else {
            None
        };
        // 同一批空格既供简拼取首字母，也供 boundary 记边界——此前只用了前者，
        // 转手就 replace(' ',"") 把边界扔了，逼得查询侧用 DAG 猜、造词侧暴力反推。
        (
            raw_code.replace(' ', ""),
            abbrev,
            syllable_boundary_mask(raw_code),
        )
    } else {
        // 形码/五笔类：无音节概念，boundary=0（= 无边界信息，消费方降级回 DAG）、无简拼。
        (raw_code.to_string(), None, 0u64)
    };
    if lowercase_code {
        code = code.to_lowercase();
        abbrev = abbrev.map(|a| a.to_lowercase());
    }
    // weight_col 为 None = 该词库声明了 columns: 但其中不含 weight（对齐 librime：声明后
    // 未列出的字段不读）。未声明 columns: 的词库走 librime 默认，weight_col = Some(2)。
    let weight: i32 = match spec.weight_col.and_then(|i| parts.get(i)) {
        // 空权重列是常态（Rime 语义：留给预设词库补），不计入异常统计。
        None | Some(&"") => 0,
        Some(s) => match s.parse() {
            Ok(w) => w,
            Err(_) => {
                // Rime 的 `50%` 相对权重会落这里：本项目未实现 use_preset_vocabulary，
                // 无基准可缩放，故与 librime 一样降级为 0，但记一笔让用户可见。
                stats.bad_weight += 1;
                0
            }
        },
    };
    // 值域诊断的取数点（只统计不改值，见 `ParseStats::log_weight_range`）。
    // 权重 0 = 无权重列/空列，不构成「超范围」，故不计入分母。
    if weight > 0 {
        stats.weighted += 1;
        stats.max_weight = stats.max_weight.max(weight);
        if weight > WEIGHT_RANGE_MAX {
            stats.over_range += 1;
        }
    }
    Some(RimeLine {
        code,
        abbrev,
        // 词条文本反转义（`\n`→换行、`\t`→制表、`\\`→反斜杠）。**只对 text 做，不碰 code**：
        // code 要经 `syllable_boundary_mask` 与扁平化处理，混进转义会让边界位与实际码错位。
        //
        // 复用 wdict 的转义表而非另写一份——同一套语义在导出（`escape_field`）、备份还原、
        // 本处解析三条路径上必须一致，抄第二份就是下次漂移的起点。其「未知转义序列原样保留」
        // 的性质同时把破坏面锁死在 `\n`/`\t`/`\\` 三个序列上：`C:\Users` 的 `\U` 不受影响。
        //
        // 命令栏语法条目（`$CC(...)` 等）在 `unescape_text_field` 里只还原换行/制表、
        // **反斜杠原样穿过**——那条源码的 `\` 归 cmdbar lexer 管，本层再转一次就是双重
        // 展开：`open("D:\\notes")` 会先被这里吃掉一个反斜杠，lexer 再把 `\n` 解成换行。
        //
        // 注：`PARSE_SEMANTICS_VERSION` 已 +1（见 `cache_fp.rs`）——本次改动会让含
        // `$CC` 且带反斜杠的词库解析出不同结果，不 bump 则存量 .wdat 静默复用旧结果。
        text: wind_store::wdict::unescape_text_field(text),
        weight,
        boundary,
    })
}

/// 正文起点：首个（按 `str::lines()` 语义，即剥除 `\r` 后）等于 `...` 的行之后的字节偏移。
/// 无该分隔行 → None（与 load_impl 一致：无正文标记则零条目）。
fn rime_body_offset(content: &str) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut line_start = 0usize;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            let raw = &content[line_start..i];
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            if line == "..." {
                return Some(i + 1);
            }
            line_start = i + 1;
        }
    }
    if content[line_start..]
        .strip_suffix('\r')
        .unwrap_or(&content[line_start..])
        == "..."
    {
        Some(content.len())
    } else {
        None
    }
}

/// 并行解析 rime `.dict.yaml` 正文为 `(全拼条目, 简拼条目)` 两组，各元素 `(code,text,weight)`。
/// 简拼条目即声母缩写表（如 `nh`→你好），供 wdat 独立 AbbrevSection。
///
/// 跳过 YAML 头部（到首个独占一行的 `...` 为止），正文按**行边界**切成 N 块、`thread::scope`
/// 多线程解析（行解析是纯 CPU、可完美并行——拼音大词库的主要耗时）。块边界对齐 `\n`
/// （该字节不会落在 UTF-8 多字节序列内部），故切片始终在合法 char 边界。
/// 顺序不保证与文件一致：merged 路径会按权重重排，无需稳定顺序。
/// `(fulls, abbrevs)`；fulls 每条 `(code, text, weight, boundary)`，
/// abbrevs 每条 **`(abbrev, 全拼码, weight)`**。
///
/// # 简拼存的是全拼码，不是词（wdat v5）
///
/// AbbrevSection 是**二级索引**，指向主键（全拼码）而非复制数据。此前存的是词本身
/// （`nh` → 「你好」），带来三个连带问题：简拼候选不知道自己的全拼码，只能把 code 设成
/// 简拼串 ⇒ 同一个词在简拼与全拼下走两个互不相认的词频计数；候选拿不到 boundary
/// （硬编码 0）；词频表因此混着全拼码与简拼码两种键。
///
/// 改存全拼码后，简拼查询变成「查索引拿码 → 走主表装配候选」，上述三项一并解决。
/// 这里的 `weight` 只用于**挑选该简拼下取哪些码**（截断前排序），候选自身的权重来自主表。
///
/// 简拼码（`nh`）本身不带 boundary——它是各音节首字母的拼接，不构成音节序列；
/// 边界随主表条目一起拿到。
type RimeEntries = (Vec<(String, String, i32, u64)>, Vec<(String, String, i32)>);

/// 一个词库的**权重值域画像**（`wind_input dict weight-check` 的产出）。
///
/// 只统计不建库：走与正式加载**完全相同**的列序判定与行解析（`resolve_columns` +
/// `parse_rime_line`），故画像与真实加载所见一致——另抄一份按 TSV 切列的扫描器，
/// 会在列序声明残缺、`# no comment` 指令等边角上与真实行为分叉。
#[derive(Debug, Default, Clone)]
pub struct WeightScan {
    /// 带非零权重的条目数（0 = 无权重列/空列，不计入）。
    pub weighted: usize,
    /// 权重为 0 的条目数（诊断「整库无权重」用）。
    pub zero: usize,
    pub min: i32,
    pub median: i32,
    /// 99 分位。**归一化的上锚点建议取它而非 `max`**：离群值会吃掉整个量程——
    /// 虎码方案级 max=1e11（12 条脏数据）而 p99=343,880，相差 30 万倍。
    pub p99: i32,
    pub max: i32,
    /// 超出 [`WEIGHT_RANGE_MAX`] 的条目数。
    pub over_range: usize,
}

impl WeightScan {
    /// 是否守约（全部权重在 `0..=WEIGHT_RANGE_MAX`）。整库无权重也算守约——那是
    /// 「退化为文件顺序」的有意设计，由 `default_weight` 另行处置。
    pub fn is_compliant(&self) -> bool {
        self.over_range == 0
    }

    /// 超范围占比（0.0~100.0）。
    pub fn over_pct(&self) -> f64 {
        100.0 * self.over_range as f64 / self.weighted.max(1) as f64
    }
}

/// 读取一个 `.dict.yaml` 的全部非零权重（未排序）与零权重条目数。
///
/// 供**跨词库聚合**：方案级归一化的参数取自全方案合并后的分布，而中位数/分位数
/// 无法由各库的摘要合并得出，必须并原始值再算。
pub fn scan_weight_values(path: impl AsRef<Path>) -> anyhow::Result<(Vec<i32>, usize)> {
    let path = path.as_ref();
    let content = fs::read_to_string(path)?;
    let Some(off) = rime_body_offset(&content) else {
        anyhow::bail!("{}: 缺少 YAML 正文分隔行 `...`", path.display());
    };
    let body = &content[off..];
    let cutoff = find_no_comment_directive(body);
    let Some(spec) = resolve_columns(&content, body, path, cutoff) else {
        anyhow::bail!("{}: 列声明残缺，无法判定权重列", path.display());
    };
    let mut stats = ParseStats::default();
    let mut ws: Vec<i32> = Vec::new();
    let mut zero = 0usize;
    for (line, comments_on) in body_lines(body, 0, cutoff) {
        if let Some(r) = parse_rime_line(line, comments_on, false, spec, &mut stats) {
            if r.weight > 0 {
                ws.push(r.weight);
            } else {
                zero += 1;
            }
        }
    }
    Ok((ws, zero))
}

/// 由一组权重（**将被就地排序**）与零权重条目数生成画像。
pub fn weight_scan_of(ws: &mut [i32], zero: usize) -> WeightScan {
    if ws.is_empty() {
        return WeightScan {
            zero,
            ..Default::default()
        };
    }
    ws.sort_unstable();
    let n = ws.len();
    WeightScan {
        weighted: n,
        zero,
        min: ws[0],
        median: ws[n / 2],
        // 向下取整的 99 分位；不足百条时退化为最大值。
        p99: ws[(n * 99 / 100).min(n - 1)],
        max: ws[n - 1],
        over_range: ws.iter().filter(|&&w| w > WEIGHT_RANGE_MAX).count(),
    }
}

/// 扫描一个 `.dict.yaml` 的权重分布，供离线体检使用。
///
/// ⚠️ 与运行时诊断（[`ParseStats::log_weight_range`]）的关系：后者只在**解析 yaml** 时触发，
/// 而词库一旦建了 `.wdat` 缓存就不再解析 —— 于是「老词库 + 新版本」这个最需要报警的组合
/// 反而一声不吭。本函数无视缓存、直接读源文件，是**权威**的那条路径。
pub fn scan_weight_stats(path: impl AsRef<Path>) -> anyhow::Result<WeightScan> {
    let path = path.as_ref();
    let content = fs::read_to_string(path)?;
    let Some(off) = rime_body_offset(&content) else {
        anyhow::bail!("{}: 缺少 YAML 正文分隔行 `...`", path.display());
    };
    let body = &content[off..];
    let cutoff = find_no_comment_directive(body);
    let Some(spec) = resolve_columns(&content, body, path, cutoff) else {
        anyhow::bail!("{}: 列声明残缺，无法判定权重列", path.display());
    };
    let mut stats = ParseStats::default();
    let mut ws: Vec<i32> = Vec::new();
    let mut zero = 0usize;
    for (line, comments_on) in body_lines(body, 0, cutoff) {
        if let Some(r) = parse_rime_line(line, comments_on, false, spec, &mut stats) {
            if r.weight > 0 {
                ws.push(r.weight);
            } else {
                zero += 1;
            }
        }
    }
    Ok(weight_scan_of(&mut ws, zero))
}

pub fn parse_rime_entries_parallel(
    path: impl AsRef<Path>,
    lowercase_code: bool,
) -> anyhow::Result<RimeEntries> {
    let path = path.as_ref();
    let content = fs::read_to_string(path)?;
    let Some(off) = rime_body_offset(&content) else {
        // 此前这里静默返回零条目——连 resolve_columns 的日志都走不到，是全链路最沉默的一处。
        warn!(
            "词库 {} 缺少 YAML 正文分隔行 `...`，按零条目处理（文件可能被截断或损坏）。",
            path.display()
        );
        return Ok((Vec::new(), Vec::new()));
    };
    let body = &content[off..];
    // `# no comment` 指令位置。**必须在切块前于全局求出**：该指令跨行有状态，
    // 各块无从知道自己之前有没有出现过它；求出偏移后各块即可独立判定。
    let cutoff = find_no_comment_directive(body);
    // 列规格判定一次、全文固定，随后传给每个并行块——保证跨块一致（逐行猜时同文件可能分裂）。
    // None = 声明残缺，整库跳过（已在 resolve_columns 内 error! 说明原因）。
    let Some(spec) = resolve_columns(&content, body, path, cutoff) else {
        return Ok((Vec::new(), Vec::new()));
    };

    // 解析一块 → (全拼, 简拼, 统计)。`base` 是本块在正文中的起始偏移，供注释开关定位。
    // 统计每块独立累加，最后合并——不引入跨线程共享。
    let parse_chunk = |chunk: &str, base: usize| -> (RimeEntries, ParseStats) {
        let mut fulls = Vec::new();
        let mut abbrevs = Vec::new();
        let mut stats = ParseStats::default();
        for (line, comments_on) in body_lines(chunk, base, cutoff) {
            if let Some(r) = parse_rime_line(line, comments_on, lowercase_code, spec, &mut stats) {
                if let Some(ab) = r.abbrev {
                    // 存**全拼码**而非词：AbbrevSection 是二级索引，应指向主键。
                    // 详见 RimeEntries 的类型注释。
                    abbrevs.push((ab, r.code.clone(), r.weight));
                }
                fulls.push((r.code, r.text, r.weight, r.boundary));
            }
        }
        ((fulls, abbrevs), stats)
    };

    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    // 小文件 / 单核：串行，省去切块与起线程开销。
    if threads <= 1 || body.len() < (1 << 20) {
        let (entries, stats) = parse_chunk(body, 0);
        stats.log_if_dirty(path);
        return Ok(entries);
    }

    // 按字节均分，再各自前推到下一个换行后，得到不跨行的块边界。
    let bytes = body.as_bytes();
    let mut bounds = vec![0usize];
    for k in 1..threads {
        let mut p = (body.len() as u64 * k as u64 / threads as u64) as usize;
        while p < body.len() && bytes[p] != b'\n' {
            p += 1;
        }
        if p < body.len() {
            p += 1; // 跨过换行，块从下一行起
        }
        if p > *bounds.last().unwrap() {
            bounds.push(p);
        }
    }
    bounds.push(body.len());
    bounds.dedup();

    // 连同各块起始偏移一起带上——注释开关要靠它定位本行在整篇正文中的位置。
    let chunks: Vec<(&str, usize)> = bounds
        .windows(2)
        .map(|w| (&body[w[0]..w[1]], w[0]))
        .collect();

    // 复用同一个 parse_chunk——此前这里另抄了一份循环体，而「同一段解析逻辑存在两份拷贝」
    // 正是本模块列序 bug 的成因（两份各自演化，修了一份另一份照旧）。
    let parse_chunk = &parse_chunk;
    let parts: Vec<(RimeEntries, ParseStats)> = std::thread::scope(|s| {
        let handles: Vec<_> = chunks
            .iter()
            .map(|&(chunk, base)| s.spawn(move || parse_chunk(chunk, base)))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let mut fulls = Vec::new();
    let mut abbrevs = Vec::new();
    let mut stats = ParseStats::default();
    for ((f, a), st) in parts {
        fulls.extend(f);
        abbrevs.extend(a);
        stats.merge(&st);
    }
    stats.log_if_dirty(path);
    Ok((fulls, abbrevs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// `scan_weight_stats` 的画像：分位数、超范围计数、无权重条目分离统计。
    ///
    /// 这是 `dict weight-check` 的**全部实质**——CLI 只是把它排版打印。
    #[test]
    fn scan_weight_stats_profiles_a_dict() {
        let path = std::env::temp_dir().join("wind_wscan_demo.dict.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(
                f,
                "---
name: x
columns:
  - code
  - text
  - weight
..."
            )
            .unwrap();
            for (c, t, w) in [
                ("a", "甲", "1000"),
                ("b", "乙", "5000"),
                ("c", "丙", "50000"),
                ("d", "丁", "500000"),
                ("e", "戊", "1000000"),
                ("f", "己", ""), // 空权重列：不计入分母
            ] {
                writeln!(f, "{c}	{t}	{w}").unwrap();
            }
        }
        let sc = scan_weight_stats(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(sc.weighted, 5, "空权重列不计入分母");
        assert_eq!(sc.zero, 1);
        assert_eq!(sc.min, 1000);
        assert_eq!(sc.median, 50000, "5 条取中间那条");
        assert_eq!(sc.max, 1_000_000);
        assert_eq!(sc.over_range, 3, "50000/500000/1000000 超 10000");
        assert!(!sc.is_compliant());
        assert!((sc.over_pct() - 60.0).abs() < 0.01);
    }

    /// 守约词库：`is_compliant` 为真，CLI 据此不给建议。
    #[test]
    fn scan_weight_stats_marks_compliant_dict() {
        let path = std::env::temp_dir().join("wind_wscan_ok.dict.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(
                f,
                "---
name: x
columns:
  - code
  - text
  - weight
..."
            )
            .unwrap();
            writeln!(f, "a	工	120").unwrap();
            writeln!(f, "b	弗	9950").unwrap();
            writeln!(f, "c	王	10000").unwrap();
        }
        let sc = scan_weight_stats(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(sc.is_compliant(), "10000 是边界内，不算超范围");
        assert_eq!(sc.over_range, 0);
        assert_eq!(sc.median, 9950);
    }

    /// 整库无权重列：`weighted == 0`，算守约（那是「退化为文件顺序」的有意设计）。
    #[test]
    fn scan_weight_stats_handles_dict_without_weights() {
        let path = std::env::temp_dir().join("wind_wscan_nw.dict.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(
                f,
                "---
name: x
columns:
  - code
  - text
..."
            )
            .unwrap();
            writeln!(f, "a	甲").unwrap();
            writeln!(f, "b	乙").unwrap();
        }
        let sc = scan_weight_stats(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(sc.weighted, 0);
        assert_eq!(sc.zero, 2);
        assert!(sc.is_compliant(), "无权重不是「超范围」");
    }

    /// 权重值域诊断的取数：超出 `WEIGHT_RANGE_MAX` 的条目被计入 `over_range`，
    /// 权重 0（无权重列/空列）**不计入分母**——那是「未定义」不是「超范围」。
    ///
    /// 这条锁的是诊断的**准确性**：`over_range/weighted` 这个比例会出现在告警里，
    /// 把 0 权重算进分母会让百分比虚低，方案作者据此低估问题。
    #[test]
    fn weight_range_diagnostic_counts_only_real_weights() {
        let spec = ColumnSpec::from_layout(ColumnLayout::CodeFirst, false);
        let mut st = ParseStats::default();
        // 守约、超范围、恰好在边界、空权重、零权重
        for line in [
            "a	工	9999",
            "b	的	10359470",
            "c	一	10000",
            "d	是	",
            "e	不	0",
        ] {
            parse_rime_line(line, true, false, spec, &mut st);
        }
        assert_eq!(st.weighted, 3, "只有 9999/10359470/10000 三条算「有权重」");
        assert_eq!(st.over_range, 1, "只有 10359470 超范围；10000 是边界内");
        assert_eq!(st.max_weight, 10_359_470);
    }

    /// 反向锁：全部守约时诊断**一声不吭**（`over_range == 0`）。
    /// 干净的词库不该刷屏——这是 `log_if_dirty` 一贯的约定。
    #[test]
    fn well_behaved_dict_triggers_no_weight_warning() {
        let spec = ColumnSpec::from_layout(ColumnLayout::CodeFirst, false);
        let mut st = ParseStats::default();
        for line in ["a	工	120", "aa	弗	9950", "aaa	工	9000"] {
            parse_rime_line(line, true, false, spec, &mut st);
        }
        assert_eq!(st.weighted, 3);
        assert_eq!(st.over_range, 0, "五笔量级的权重不得触发告警");
    }

    #[test]
    fn syllable_boundary_mask_basics() {
        // 多音节：起始字节位 {0,2}（ni 占 0..2，hao 占 2..5）。
        assert_eq!(syllable_boundary_mask("ni hao"), 0b101);
        // 变长音节：zhuang(6B) 起始 0，ni 起始 6。
        assert_eq!(syllable_boundary_mask("zhuang ni"), 0b1000001);
        // 单音节：整串一个音节，起始 {0}。是真实信息，不是「未知」。
        assert_eq!(syllable_boundary_mask("ni"), 0b1);
        // 空码 → 无信息。
        assert_eq!(syllable_boundary_mask(""), 0);
        // 超长码（拼接 ≥64B）：bitmask 装不下 → 整体降级为 0，不给半截错误边界。
        let long = ["zhuang"; 12].join(" "); // 12*6 = 72B
        assert_eq!(syllable_boundary_mask(&long), 0);
    }

    /// 端到端：rime 源 → 解析 → wdat 落盘 → mmap 读回，边界必须原样穿过整条链路。
    /// 这是 v4 的核心契约——此前边界在解析期就被 replace(' ',"") 丢弃，根本到不了磁盘。
    #[test]
    fn boundary_survives_wdat_roundtrip() {
        let dir = std::env::temp_dir().join("wind_boundary_roundtrip");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("py.dict.yaml");
        {
            let mut f = std::fs::File::create(&src).unwrap();
            writeln!(f, "---\nname: py\n...").unwrap();
            writeln!(f, "你好\tni hao\t1200").unwrap();
            writeln!(f, "你\tni\t800").unwrap();
            // 同 code 不同切分：xi'an（西安，2 音节）vs xian（先，1 音节）。
            // 正是 DAG 无从分辨、必须靠词典真值的场景（两者覆盖字符数相同）。
            writeln!(f, "西安\txi an\t500").unwrap();
            writeln!(f, "先\txian\t900").unwrap();
        }
        let dict = CodetableDict::load(&src).unwrap();

        let mut w = crate::datformat::WdatWriter::new();
        dict.export_to_wdat(&mut w);
        let wdat = dir.join("py.wdat");
        w.write(&wdat).unwrap();

        let reader = crate::datformat::WdatReader::open(&wdat).unwrap();
        let find = |code: &str, text: &str| -> Option<u64> {
            reader
                .search(code)
                .into_iter()
                .find(|e| e.text == text)
                .map(|e| e.boundary)
        };

        assert_eq!(find("nihao", "你好"), Some(0b101), "ni|hao 边界应读回");
        assert_eq!(find("ni", "你"), Some(0b1));
        // 关键：同一 key "xian" 下两条候选各自带边界，据此可区分 xi|an 与 xian。
        assert_eq!(find("xian", "西安"), Some(0b101), "xi|an → 起始 {{0,2}}");
        assert_eq!(find("xian", "先"), Some(0b1), "xian → 单音节，起始 {{0}}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 英文词库格式 `word<TAB>word`（混合大小写）：load_lowercased 应小写化 code、
    /// 保留 text 原样，使大小写不敏感前缀匹配生效。
    #[test]
    fn load_lowercased_english() {
        let path = std::env::temp_dir().join("wind_en_lowercase_test.dict.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "---\nname: en\n...").unwrap();
            writeln!(f, "# ab\tab").unwrap(); // 注释行跳过
            writeln!(f, "Aaron\tAaron").unwrap();
            writeln!(f, "abandon\tabandon").unwrap();
            writeln!(f, "ABC\tABC").unwrap();
        }
        let d = CodetableDict::load_lowercased(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        // 小写前缀 "aa" 命中 Aaron（原样大小写 text）
        let r = d.search_prefix("aa", 10);
        assert!(
            r.iter()
                .any(|(code, text, _, _)| code == "aaron" && text == "Aaron"),
            "应小写码命中、保留原样 text: {:?}",
            r
        );
        // 精确小写 "abc" 命中 ABC
        assert!(d.search("abc").iter().any(|(t, _, _)| t == "ABC"));
    }

    /// 并行解析：拼音格式（text\tcode\tweight，code 去空格）+ 注释/空行跳过，
    /// 小文件走串行分支，结果应完整正确。
    /// 取 fulls（(code, text, weight, boundary)）中某 text 的 (code, weight)。
    fn collect(entries: &[(String, String, i32, u64)], text: &str) -> Vec<(String, i32)> {
        entries
            .iter()
            .filter(|(_, t, _, _)| t == text)
            .map(|(c, _, w, _)| (c.clone(), *w))
            .collect()
    }

    /// 取 abbrevs（(abbrev, text, weight)，无 boundary）中某 text 的 (abbrev, weight)。
    /// 取 abbrevs 中某**全拼码**对应的 `(简拼, weight)`。
    ///
    /// ⚠️ 查询键已随 v5 从「词」改为「全拼码」——abbrevs 的第二个字段现在存的是码
    /// （AbbrevSection 是指向主键的二级索引，不再复制词本身）。
    fn collect_ab(entries: &[(String, String, i32)], full_code: &str) -> Vec<(String, i32)> {
        entries
            .iter()
            .filter(|(_, c, _)| c == full_code)
            .map(|(ab, _, w)| (ab.clone(), *w))
            .collect()
    }

    /// 取 fulls 中某 text 的 boundary。
    fn boundary_of(entries: &[(String, String, i32, u64)], text: &str) -> Option<u64> {
        entries
            .iter()
            .find(|(_, t, _, _)| t == text)
            .map(|(_, _, _, b)| *b)
    }

    #[test]
    fn parallel_parse_pinyin_format_small() {
        let path = std::env::temp_dir().join("wind_parrime_small.dict.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            // 声明 columns 才会取权重列（未声明时保守只认 text/code）
            writeln!(
                f,
                "---\nname: py\ncolumns:\n  - text\n  - code\n  - weight\n..."
            )
            .unwrap();
            writeln!(f, "# 注释跳过").unwrap();
            writeln!(f).unwrap(); // 空行跳过
            writeln!(f, "你好\tni hao\t1200").unwrap(); // code 去空格 -> nihao
            writeln!(f, "你\tni\t800").unwrap();
        }
        let (e, ab) = parse_rime_entries_parallel(&path, false).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(e.len(), 2, "应解析 2 条，跳过注释/空行: {e:?}");
        assert_eq!(collect(&e, "你好"), vec![("nihao".to_string(), 1200)]);
        assert_eq!(collect(&e, "你"), vec![("ni".to_string(), 800)]);
        // 简拼：多音节 "ni hao"→"nh"；单音节 "ni" 无简拼。
        // 查询键是**全拼码**（v5：AbbrevSection 存码不存词）。
        assert_eq!(collect_ab(&ab, "nihao"), vec![("nh".to_string(), 1200)]);
        assert!(collect_ab(&ab, "ni").is_empty(), "单音节不产简拼");
        // 音节边界（v4）：源数据 "ni hao" 的空格是真值边界，不得随 code 拼平而丢弃。
        // "nihao" 音节 ni|hao → 起始字节 {0,2} → 0b101。
        assert_eq!(
            boundary_of(&e, "你好"),
            Some(0b101),
            "「你好」应记住 ni|hao 的边界"
        );
        // 单音节：整串一个音节 → 起始 {0} → 0b1（是真实信息，非「未知」）。
        assert_eq!(boundary_of(&e, "你"), Some(0b1));
    }

    /// 跨 1MB 阈值触发并行切块：构造大量行，断言总数与抽样正确、块边界不丢/不重行。
    #[test]
    fn parallel_parse_large_chunked_no_loss() {
        let path = std::env::temp_dir().join("wind_parrime_large.dict.yaml");
        let n = 60_000; // 每行约 20+ 字节 → 正文 > 1MB，触发并行分支
        {
            let mut f = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
            writeln!(
                f,
                "---\nname: big\ncolumns:\n  - code\n  - text\n  - weight\n..."
            )
            .unwrap();
            for i in 0..n {
                // 五笔格式 code\ttext\tweight，code 全 ASCII
                writeln!(f, "code{i}\t文{i}\t{i}").unwrap();
            }
        }
        let (e, _ab) = parse_rime_entries_parallel(&path, false).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(e.len(), n, "并行切块不应丢行/重复");
        // 抽样首/中/尾
        assert_eq!(collect(&e, "文0"), vec![("code0".to_string(), 0)]);
        assert_eq!(
            collect(&e, "文59999"),
            vec![("code59999".to_string(), 59999)]
        );
        // 五笔码无音节概念 → boundary 恒 0（消费方据此降级，不会误当拼音边界）。
        assert_eq!(boundary_of(&e, "文0"), Some(0), "五笔码不应有音节边界");
        // 全部 code 唯一（边界未把某行切成两半）
        let mut codes: Vec<&str> = e.iter().map(|(c, _, _, _)| c.as_str()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), n, "所有 code 应唯一");
    }

    /// 构造期望的 `Usable`（`has_syllables` 由 resolve_columns 采样填入，此处恒 false）。
    fn usable(text_col: usize, code_col: usize, weight_col: Option<usize>) -> ColumnsDecl {
        ColumnsDecl::Usable(ColumnSpec {
            text_col,
            code_col,
            weight_col,
            has_syllables: false,
        })
    }

    /// 头部 `columns:` 声明是权威列规格，两种顺序都要认，且能穿过行内注释。
    #[test]
    fn columns_header_is_authoritative() {
        // flypy 风格：带行内注释
        let flypy = "---\nname: x\ncolumns:    # 码表格式\n  - text    # 文字\n  - code    # 输入码\n  - weight  # 权重\n...\n";
        assert_eq!(parse_columns_header(flypy), usable(0, 1, Some(2)));
        // wubi 风格：code 在前
        let wubi = "---\nname: y\nsort: by_weight\ncolumns:\n  - code\n  - text\n  - weight\n...\n";
        assert_eq!(parse_columns_header(wubi), usable(1, 0, Some(2)));
        // 只声明两列（用户为 12_kf 补的正是这种）→ 无权重列
        let two = "---\nname: kf\ncolumns:\n  - text\n  - code\n...\n";
        assert_eq!(parse_columns_header(two), usable(0, 1, None));
        // 无声明
        assert_eq!(
            parse_columns_header("---\nname: z\n...\n"),
            ColumnsDecl::Absent
        );
        // 声明里出现未支持的列名：占位并顺延后续列下标，不得错位
        let stem = "---\ncolumns:\n  - text\n  - code\n  - stem\n  - weight\n...\n";
        assert_eq!(
            parse_columns_header(stem),
            usable(0, 1, Some(3)),
            "stem 占一列，weight 应顺延到下标 3"
        );
        // columns 块后回到别的键，不应越界把后续键读成列名
        let trailing = "---\ncolumns:\n  - code\n  - text\nsort: by_weight\n...\n";
        assert_eq!(parse_columns_header(trailing), usable(1, 0, None));
    }

    /// `sort:` 是 librime 的库内同码排序键，本输入法不消费，取出仅为告警。
    /// 此前它被静默跳过：配置者把 `by_weight` 改成 `original` 观察不到任何变化也拿不到诊断。
    #[test]
    fn sort_header_is_detected_for_warning() {
        assert_eq!(
            parse_sort_header("---\nname: x\nsort: by_weight\n...\n").as_deref(),
            Some("by_weight")
        );
        assert_eq!(
            parse_sort_header("---\nsort: original\n...\n").as_deref(),
            Some("original")
        );
        // 行内注释剥离（district 库就带注释）
        assert_eq!(
            parse_sort_header("---\nsort: original  # 原始顺序\n...\n").as_deref(),
            Some("original")
        );
        // 无声明 → 不告警
        assert_eq!(
            parse_sort_header("---\nname: x\ncolumns: [text, code]\n...\n"),
            None
        );
        // 缩进的同名键属于别的映射，不算（与 columns: 同规则）
        assert_eq!(
            parse_sort_header("---\nfoo:\n  sort: by_weight\n...\n"),
            None
        );
        // 空值不触发
        assert_eq!(parse_sort_header("---\nsort:\n...\n"), None);
    }

    /// **流式序列必须支持**：它是合法 YAML，且我们的警告文案就建议用户这么写。
    /// 只认块序列会让照建议改完的用户看到「改了没用」——本项目在这条路上吃过亏。
    #[test]
    fn columns_header_accepts_flow_sequence() {
        assert_eq!(
            parse_columns_header("---\nname: x\ncolumns: [text, code, weight]\n...\n"),
            usable(0, 1, Some(2))
        );
        // 紧凑写法 + 行内注释 + code 在前
        assert_eq!(
            parse_columns_header("---\ncolumns: [code,text,weight]  # 五笔\n...\n"),
            usable(1, 0, Some(2))
        );
        // 两列流式
        assert_eq!(
            parse_columns_header("---\ncolumns: [text, code]\n...\n"),
            usable(0, 1, None)
        );
    }

    /// 残缺声明要能被**区分地**诊断：把「声明不完整」误报成「未声明」，
    /// 会让照日志排查的人去看头部、发现声明确实存在，从而排除掉正确的线索。
    #[test]
    fn incomplete_columns_declaration_is_distinguished() {
        // 缺 code：librime 的自动编码词库形态（如 `columns: [text, weight]`）
        assert_eq!(
            parse_columns_header("---\ncolumns:\n  - text\n  - weight\n...\n"),
            ColumnsDecl::MissingCode
        );
        // 缺 text：librime 直接丢弃整个文件
        assert_eq!(
            parse_columns_header("---\ncolumns:\n  - code\n  - weight\n...\n"),
            ColumnsDecl::MissingText
        );
    }

    /// 声明缺 code 的词库**整库跳过**，而不是降级探测。
    /// 降级探测会把权重列当成编码（`is_code_shape("100")` 为真），静默灌进一整库垃圾编码。
    #[test]
    fn missing_code_column_skips_whole_dict() {
        let path = std::env::temp_dir().join("wind_cols_no_code.dict.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "---\nname: auto\ncolumns:\n  - text\n  - weight\n...").unwrap();
            writeln!(f, "〇〇八\t100").unwrap();
            writeln!(f, "〇一二\t100").unwrap();
        }
        let (e, ab) = parse_rime_entries_parallel(&path, false).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(
            e.is_empty() && ab.is_empty(),
            "无 code 列的词库应整库跳过，不得把权重当编码：{e:?}"
        );
    }

    /// **词条内容不该被当成排版空白**：以 U+3000 全角空格为词条的行，此前会被
    /// `str::trim()` 当缩进削掉（U+3000 属 Unicode White_Space），整行字段左移一格。
    #[test]
    fn leading_fullwidth_space_text_is_preserved() {
        let path = std::env::temp_dir().join("wind_leading_ws.dict.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "---\nname: ws\n...").unwrap();
            writeln!(f, "\u{3000}\tcokg\t\t全角空格").unwrap(); // 四列，第 3 列空
            writeln!(f, "\u{3000}\tpwst").unwrap(); // 两列
            writeln!(f, "字\tabc\t5").unwrap();
        }
        let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(
            collect(&e, "\u{3000}"),
            vec![("cokg".to_string(), 0), ("pwst".to_string(), 0)],
            "全角空格词条应完整保留，两条都在：{e:?}"
        );
        // 不得产出 code 为空串的垃圾条目（字段左移的副产物）
        assert!(
            !e.iter().any(|(c, _, _, _)| c.is_empty()),
            "不应出现空编码条目：{e:?}"
        );
        assert_eq!(collect(&e, "字"), vec![("abc".to_string(), 5)]);
    }

    /// 词条文本的转义序列须还原为真字符：`\n`→换行、`\t`→制表、`\\`→反斜杠。
    ///
    /// **未知序列必须原样保留**——这是破坏面的边界：`C:\Users` 里的 `\U` 不在转义表中，
    /// 反转义后仍是 `\U`，存量词库里的 Windows 路径不会被静默改写。想要字面 `\n` 两字符
    /// 的用户写 `\\n` 即可。
    #[test]
    fn text_escape_sequences_are_unescaped() {
        let path = std::env::temp_dir().join("wind_text_escape.dict.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "---\nname: esc\n...").unwrap();
            writeln!(f, "甲\\n乙\tjy\t10").unwrap(); // \n → 换行
            writeln!(f, "丙\\t丁\tbd\t20").unwrap(); // \t → 制表（注意不是列分隔符）
            writeln!(f, "戊\\\\己\twj\t30").unwrap(); // \\ → 单个反斜杠
            writeln!(f, "C:\\Users\tcu\t40").unwrap(); // \U 未知 → 原样保留
            writeln!(f, "庚\\\\n辛\tgx\t50").unwrap(); // \\n → 字面 \n 两字符，不是换行
        }
        let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(collect(&e, "甲\n乙"), vec![("jy".to_string(), 10)]);
        assert_eq!(collect(&e, "丙\t丁"), vec![("bd".to_string(), 20)]);
        assert_eq!(collect(&e, "戊\\己"), vec![("wj".to_string(), 30)]);
        assert_eq!(
            collect(&e, "C:\\Users"),
            vec![("cu".to_string(), 40)],
            "未知转义序列须原样保留，存量词库里的 Windows 路径不得被改写"
        );
        assert_eq!(
            collect(&e, "庚\\n辛"),
            vec![("gx".to_string(), 50)],
            "`\\\\n` 应还原为字面 \\n 两字符，而非换行"
        );
    }

    /// **转义序列免疫行尾 trim**：`\n` 在 trim 阶段是两个可见字符，剥不掉；而裸空格会被
    /// `trim_line_end` 剥除——这正是本设计的用意（有意空白用转义表达、排版噪声交给 trim）。
    ///
    /// 顺带锁住 CodeFirst 布局的既有行为：text 落在**末列**时，其尾随裸空格会被行尾 trim
    /// 吃掉（TextFirst 布局下 text 在首列则不受影响）。这个列序不对称是 librime `trim_right`
    /// 语义的自然结果，此处**明确记录而非修复**——要保留尾部空白请用转义序列。
    #[test]
    fn trailing_whitespace_trimmed_but_escapes_survive() {
        let path = std::env::temp_dir().join("wind_trail_ws.dict.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "---\nname: t\ncolumns:\n  - code\n  - text\n...").unwrap();
            writeln!(f, "ka\t甲   ").unwrap(); // 末列尾随裸空格 → 被行尾 trim 剥掉
            writeln!(f, "yi\t乙\\t").unwrap(); // 转义制表在末列 → 存活
        }
        let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(
            collect(&e, "甲"),
            vec![("ka".to_string(), 0)],
            "末列 text 的尾随裸空格被行尾 trim 剥除（librime trim_right 语义）"
        );
        assert!(collect(&e, "甲   ").is_empty(), "带尾随空格的形态不应存在");
        assert_eq!(
            collect(&e, "乙\t"),
            vec![("yi".to_string(), 0)],
            "转义序列在 trim 阶段是可见字符，必须活到反转义那一步"
        );
    }

    /// text 或 code 为空的行必须跳过：空 code 会让条目全挤进 `entries[""]`。
    #[test]
    fn empty_text_or_code_rows_are_skipped() {
        let path = std::env::temp_dir().join("wind_empty_fields.dict.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "---\nname: e\n...").unwrap();
            writeln!(f, "我\t").unwrap(); // code 空（行尾 trim 后仅一列，落列数门槛）
            writeln!(f, "\tabc").unwrap(); // text 空
            writeln!(f, "好\thao\t9").unwrap();
        }
        let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(e.len(), 1, "只应留下完整的那一条：{e:?}");
        assert_eq!(collect(&e, "好"), vec![("hao".to_string(), 9)]);
    }

    /// **编码在前的拼音库不得被当成形码**：librime 完全允许 `columns: [code, text, weight]`，
    /// 而旧判据只看列顺序，会把它整库当五笔处理——**丢掉整张简拼表、音节边界全归零**，
    /// 双拼与整句逻辑集体降级回 DAG 猜切分。空格是音节的正面证据，应胜过列顺序惯例。
    #[test]
    fn code_first_pinyin_dict_keeps_syllables() {
        let py = std::env::temp_dir().join("wind_syl_codefirst.dict.yaml");
        {
            let mut f = std::fs::File::create(&py).unwrap();
            writeln!(
                f,
                "---\nname: p\ncolumns:\n  - code\n  - text\n  - weight\n..."
            )
            .unwrap();
            writeln!(f, "ni hao\t你好\t1200").unwrap();
            writeln!(f, "ni\t你\t800").unwrap();
        }
        let (e, ab) = parse_rime_entries_parallel(&py, false).unwrap();
        let _ = std::fs::remove_file(&py);
        assert_eq!(collect(&e, "你好"), vec![("nihao".to_string(), 1200)]);
        assert_eq!(
            boundary_of(&e, "你好"),
            Some(0b101),
            "code 在前的拼音库同样应保留音节边界"
        );
        assert_eq!(
            collect_ab(&ab, "nihao"),
            vec![("nh".to_string(), 1200)],
            "code 在前的拼音库不应丢简拼表（键为全拼码，v5）"
        );
    }

    /// 编码在前 + 无空格 → 五笔类形码，boundary 恒 0（= 无边界信息，消费方降级 DAG）。
    /// 与之对照，**text 在前且无空格**的单音节拼音词条（`好\thao`）必须保留 `0b1`——
    /// 「整串是一个音节」是真信息，双拼真值校验要用它。无空格时数据本身区分不了这两者，
    /// 故沿用列顺序惯例兜底。
    #[test]
    fn spaceless_code_first_is_form_code_but_text_first_keeps_single_syllable() {
        let wubi = std::env::temp_dir().join("wind_syl_wubi.dict.yaml");
        {
            let mut f = std::fs::File::create(&wubi).unwrap();
            writeln!(
                f,
                "---\nname: w\ncolumns:\n  - code\n  - text\n  - weight\n..."
            )
            .unwrap();
            writeln!(f, "aaaa\t工\t99").unwrap();
        }
        let (e, ab) = parse_rime_entries_parallel(&wubi, false).unwrap();
        let _ = std::fs::remove_file(&wubi);
        assert_eq!(boundary_of(&e, "工"), Some(0), "形码不应有音节边界");
        assert!(ab.is_empty(), "形码不应产出简拼");

        let py = std::env::temp_dir().join("wind_syl_single.dict.yaml");
        {
            let mut f = std::fs::File::create(&py).unwrap();
            writeln!(f, "---\nname: s\n...").unwrap(); // 无声明 → 探测得 text 在前
            writeln!(f, "好\thao\t2000").unwrap();
        }
        let (e2, _) = parse_rime_entries_parallel(&py, false).unwrap();
        let _ = std::fs::remove_file(&py);
        assert_eq!(
            boundary_of(&e2, "好"),
            Some(0b1),
            "单音节拼音词条的「整串一个音节」是真信息，不得归零"
        );
    }

    /// `# no comment` 是**整行精确匹配**：行中间出现、或带后缀的同名子串都不算。
    #[test]
    fn no_comment_directive_requires_exact_line() {
        assert_eq!(find_no_comment_directive("a\n# no comment\nb\n"), Some(2));
        assert_eq!(find_no_comment_directive("# no comment"), Some(0)); // 无行尾换行
        assert_eq!(find_no_comment_directive("x\n# no comment\r\ny"), Some(2)); // CRLF
        assert_eq!(
            find_no_comment_directive("# no commentX\n"),
            None,
            "带后缀不算"
        );
        // librime 先 trim_right 整行再比较，故尾随空白仍算指令（与数据行同一套行尾规则）
        assert_eq!(
            find_no_comment_directive("# no comment  \n"),
            Some(0),
            "尾随空格应算"
        );
        assert_eq!(
            find_no_comment_directive("a\n# no comment\t\r\nb"),
            Some(2),
            "尾随制表符应算"
        );
        assert_eq!(
            find_no_comment_directive("a\t# no comment\n"),
            None,
            "行中间不算"
        );
        assert_eq!(
            find_no_comment_directive("## no comment\n"),
            None,
            "前缀多一个#不算"
        );
        assert_eq!(find_no_comment_directive("abc\ndef\n"), None);
        // 首个合法出现处生效（前面有个不合法的干扰项）
        assert_eq!(
            find_no_comment_directive("# no commentX\n# no comment\n"),
            Some(14)
        );
    }

    /// `# no comment` 之后 `#` 是**数据**：这是 Rime 让 `#` 本身能当词条/编码的唯一途径。
    /// 与用户报的 `@` 打不出来是同一类缺口——只是触发条件不同。
    #[test]
    fn hash_becomes_data_after_no_comment_directive() {
        let path = std::env::temp_dir().join("wind_no_comment.dict.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "---\nname: kf\n...").unwrap();
            writeln!(f, "# 这行在指令之前，仍是注释").unwrap();
            writeln!(f, "＃\th").unwrap(); // 全角井号，正常词条
            writeln!(f, "# no comment").unwrap();
            writeln!(f, "#\tj").unwrap(); // 指令之后：半角 # 是词条
            writeln!(f, "##\tk").unwrap(); // 连 ## 也是数据了
        }
        let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(collect(&e, "＃"), vec![("h".to_string(), 0)]);
        assert_eq!(
            collect(&e, "#"),
            vec![("j".to_string(), 0)],
            "指令之后半角 # 应作为词条被收下：{e:?}"
        );
        assert_eq!(collect(&e, "##"), vec![("k".to_string(), 0)]);
        // 指令之前的注释行与指令行自身都不该变成条目
        assert_eq!(e.len(), 3, "只应有 3 条：{e:?}");
    }

    /// 无指令时，`#` 一律仍是注释（保持既有行为，别把普通词库搞坏）。
    #[test]
    fn hash_stays_comment_without_directive() {
        let path = std::env::temp_dir().join("wind_no_directive.dict.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "---\nname: x\n...").unwrap();
            writeln!(f, "## 次选").unwrap(); // 第三方编辑器的分组名，仍按注释丢弃
            writeln!(f, "# 普通注释").unwrap();
            writeln!(f, "字\tabc\t5").unwrap();
        }
        let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(e.len(), 1, "两行注释都应被丢弃：{e:?}");
        assert_eq!(collect(&e, "字"), vec![("abc".to_string(), 5)]);
    }

    /// **并行分块边界**：指令在正文靠前，而后续 `#` 词条散落在各块。
    /// 注释开关是跨行有状态的，若不预先求出全局偏移，后面的块会误判为「注释仍生效」。
    #[test]
    fn no_comment_directive_survives_parallel_chunking() {
        let path = std::env::temp_dir().join("wind_no_comment_parallel.dict.yaml");
        let n = 60_000; // 正文 > 1MB，触发并行分块
        {
            let mut f = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
            writeln!(
                f,
                "---\nname: big\ncolumns:\n  - text\n  - code\n  - weight\n..."
            )
            .unwrap();
            writeln!(f, "# no comment").unwrap();
            for i in 0..n {
                // 每条 text 都以 # 开头：若某块误判注释仍生效，该块条目会整批消失
                writeln!(f, "#{i}\tc{i}\t{i}").unwrap();
            }
        }
        let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(e.len(), n, "分块后 # 词条不得丢失");
        assert_eq!(collect(&e, "#0"), vec![("c0".to_string(), 0)]);
        assert_eq!(collect(&e, "#59999"), vec![("c59999".to_string(), 59999)]);
    }

    /// 缺 `...` 分隔行 → 零条目（此前是全链路最沉默的一处，现已有 warn）。
    #[test]
    fn missing_body_separator_yields_zero_entries() {
        let path = std::env::temp_dir().join("wind_no_sep.dict.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "---\nname: broken\n你好\tni hao\t1").unwrap(); // 没有 `...`
        }
        let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(e.is_empty());
    }

    /// 未声明 `columns:` 时按 librime 默认取第 3 列作权重，**第 4 列及以后一律不读**。
    /// 真实样本：`wubi86_jidian_extra_district.dict.yaml` 是 `text\tcode\t\t区号` 四列，
    /// 第 3 列为空（→ 权重 0）、第 4 列是行政区划编号——它不得被当成权重。
    #[test]
    fn undeclared_columns_ignore_everything_past_weight() {
        let path = std::env::temp_dir().join("wind_cols_undeclared_extra.dict.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "---\nname: district\n...").unwrap(); // 无 columns 声明
            writeln!(f, "北京市\tuyym\t\t110000").unwrap();
            writeln!(f, "东城区\tafaq\t\t110101").unwrap();
        }
        let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            collect(&e, "北京市"),
            vec![("uyym".to_string(), 0)],
            "区号列不得被当作权重"
        );
        assert_eq!(collect(&e, "东城区"), vec![("afaq".to_string(), 0)]);
    }

    /// 未声明 `columns:` 时按 librime 默认 `[text, code, weight]` 取第 3 列作权重。
    #[test]
    fn undeclared_columns_follow_rime_default_weight() {
        let path = std::env::temp_dir().join("wind_cols_undeclared_w.dict.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "---\nname: x\n...").unwrap();
            writeln!(f, "你好\tni hao\t1200").unwrap();
        }
        let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            collect(&e, "你好"),
            vec![("nihao".to_string(), 1200)],
            "未声明时应按 Rime 默认取第 3 列权重"
        );
    }

    /// 只有两列的词库（快符 `12_kf.dict.yaml` 全 26 行皆两列）不得因缺权重列被丢弃。
    /// 权重是「有则取、无则 0」，不能进最低列数门槛。
    #[test]
    fn two_column_rows_survive_when_weight_column_absent() {
        let path = std::env::temp_dir().join("wind_cols_two_only.dict.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "---\nname: kf\n...").unwrap();
            writeln!(f, "、\ty").unwrap();
            writeln!(f, "@\tt").unwrap();
        }
        let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(e.len(), 2, "两列行不得被整体丢弃: {e:?}");
        assert_eq!(collect(&e, "@"), vec![("t".to_string(), 0)]);
    }

    /// 声明与内容形态冲突时以声明为准：英文词库两列都是 ASCII，探测必然弃权，
    /// 只有声明能救。这正是 wubi86_jidian_english（`abs\tABS\t100`）的形状。
    #[test]
    fn declared_columns_win_over_ambiguous_content() {
        let path = std::env::temp_dir().join("wind_cols_declared.dict.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(
                f,
                "---\nname: en\ncolumns:\n  - code\n  - text\n  - weight\n..."
            )
            .unwrap();
            writeln!(f, "abs\tABS\t100").unwrap();
            writeln!(f, "adob\tAdobe\t20").unwrap();
        }
        let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            collect(&e, "ABS"),
            vec![("abs".to_string(), 100)],
            "声明 code 在前 → text=ABS/code=abs"
        );
    }

    /// **本次修复的核心回归**：纯 ASCII 词条（快符 `@`、ASCII 参数的 `$CC(...)`）此前会被
    /// 当成码列，与编码整个对调、静默装出镜像垃圾词条。列序须由全文一次判定，不受单行影响。
    #[test]
    fn ascii_text_entries_not_mistaken_for_code_column() {
        let path = std::env::temp_dir().join("wind_kf_ascii.dict.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            // 无 columns: 声明 —— 正是 12_kf.dict.yaml 的情况，走探测
            writeln!(f, "---\nname: kf\n...").unwrap();
            writeln!(f, "｀\tq").unwrap(); // 全角，非 ASCII → 投 TextFirst
            writeln!(f, "、\ty").unwrap();
            writeln!(f, "@\tt").unwrap(); // 纯 ASCII 词条：曾被判反
            writeln!(f, "$CC(last(), type(last()))\tf").unwrap(); // 纯 ASCII 命令：曾被判反
            writeln!(f, r#"$CC("[End]", key.seq("End"))	n"#).unwrap();
        }
        let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(
            collect(&e, "@"),
            vec![("t".to_string(), 0)],
            "敲 t 应出 @（此前反成 code=@ / text=t）"
        );
        assert_eq!(
            collect(&e, "$CC(last(), type(last()))"),
            vec![("f".to_string(), 0)],
            "纯 ASCII 的 $CC 命令同样不得反转"
        );
        assert_eq!(
            collect(&e, r#"$CC("[End]", key.seq("End"))"#),
            vec![("n".to_string(), 0)]
        );
        // 非 ASCII 词条保持正确（回归保护）
        assert_eq!(collect(&e, "｀"), vec![("q".to_string(), 0)]);
    }

    /// 无声明的 code-first 词库仍应探测正确（码列含数字，text 为汉字）。
    #[test]
    fn detects_code_first_without_declaration() {
        let body = "a\t工\t9999\nggg\t三\t100\ncode1\t文\t5\n";
        let (layout, tf, cf) = detect_layout(body, None);
        assert_eq!(layout, ColumnLayout::CodeFirst, "票数 text={tf} code={cf}");
        assert_eq!((tf, cf), (0, 3));
    }

    /// 无声明的 text-first 词库（含纯 ASCII 词条）探测应判 TextFirst，
    /// 且纯 ASCII 行不干扰多数票。
    #[test]
    fn detects_text_first_without_declaration() {
        let body = "你好\tni hao\t1200\n@\tt\n、\ty\n";
        let (layout, tf, cf) = detect_layout(body, None);
        assert_eq!(layout, ColumnLayout::TextFirst, "票数 text={tf} code={cf}");
        assert_eq!((tf, cf), (3, 0), "`@\\tt` 也应投 TextFirst（@ 不是码形态）");
    }

    /// 两列都像码 / 都不像码 → 弃权，不瞎猜；零票时落到默认 TextFirst。
    #[test]
    fn ambiguous_lines_abstain_and_default_to_text_first() {
        assert_eq!(
            vote_layout("abandon\tabandon", true),
            None,
            "两列都像码 → 弃权"
        );
        assert_eq!(vote_layout("你好\t、", true), None, "两列都不像码 → 弃权");
        assert_eq!(vote_layout("# 注释\tx", true), None);
        assert_eq!(vote_layout("单列无tab", true), None);
        let (layout, tf, cf) = detect_layout("abandon\tabandon\nABC\tABC\n", None);
        assert_eq!(layout, ColumnLayout::TextFirst, "零票应落默认");
        assert_eq!((tf, cf), (0, 0));
    }

    /// 列序是文件级属性：同一文件内少数派行不得把自己那行翻过来。
    /// （旧实现逐行猜，同文件可出现两种列序并存。）
    #[test]
    fn layout_is_file_level_not_per_line() {
        let path = std::env::temp_dir().join("wind_layout_filelevel.dict.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "---\nname: mixed\n...").unwrap();
            for i in 0..10 {
                writeln!(f, "字{i}\tcode{i}\t{i}").unwrap(); // 多数：TextFirst
            }
            writeln!(f, "~\tz").unwrap(); // 纯 ASCII 词条，仍须按 TextFirst 解
        }
        let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            collect(&e, "~"),
            vec![("z".to_string(), 0)],
            "少数派 ASCII 行须服从文件级列序"
        );
        assert_eq!(collect(&e, "字0"), vec![("code0".to_string(), 0)]);
    }
}
