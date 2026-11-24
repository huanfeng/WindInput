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

/// 剥行尾空白，**只用于判定**（空行？`# no comment` 指令？哪一列像码？），
/// **不用于取字段值**。保留前导。
///
/// 曾经整行 `trim()`：Rust 的 `str::trim` 按 Unicode White_Space 判定，而 **U+3000 全角空格
/// 属于该集合**，于是「全角空格」这个词条本身会被当成缩进削掉，整行字段左移一格
/// （`　\tcokg\t\t全角空格` → `cokg\t\t全角空格`，text 变成编码、code 变成空串），
/// 两字段的行则直接掉到列数门槛之下被丢弃。**词条内容不该被当成排版空白。**
///
/// 后来发现这条原则只兑现了一半：前导不再削、**尾随仍在削**，而 CodeFirst 布局下 text
/// 恰好落在末列——「行尾空白」与「词条的尾随空格」是同一段字节。蒙古文 toli 词库
/// （`schemas/toli/toli.dict.yaml`）就整库中招：37.6 万条里 99.9995% 以空格收尾，
/// 那是蒙文的词间分隔，被剥掉后连续上屏的词全粘在一起。
///
/// 故取值改为**逐字段**处理（见 [`parse_rime_line`]）：text 原样，code/weight 各自
/// `trim_end`（那两列的尾随空白无语义）。本函数退回纯判定用途。
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
    /// text 列**带尾随空白**的条目数。这些空白从前被整行 trim 吃掉，现在原样进词库。
    ///
    /// 纯诊断、不改值：无从判断某条尾随空格是蒙文的词间分隔（toli 整库如此）还是排版噪声。
    /// 记一笔是为了让「上屏后莫名多个空格」这类反馈有据可查——那时日志里能直接看到
    /// 是这个词库自己带的。
    pub trailing_ws_text: usize,
}

impl ParseStats {
    fn merge(&mut self, o: &ParseStats) {
        self.short += o.short;
        self.empty_field += o.empty_field;
        self.bad_weight += o.bad_weight;
        self.weighted += o.weighted;
        self.over_range += o.over_range;
        self.max_weight = self.max_weight.max(o.max_weight);
        self.trailing_ws_text += o.trailing_ws_text;
    }

    fn is_clean(&self) -> bool {
        self.short == 0 && self.empty_field == 0 && self.bad_weight == 0
    }

    /// 有异常才出日志——干净的词库不该刷屏。
    fn log_if_dirty(&self, path: &Path) {
        self.log_weight_range(path);
        self.log_trailing_ws(path);
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

    /// **text 尾随空白诊断**：只报数、不改值。见 [`ParseStats::trailing_ws_text`]。
    ///
    /// 用 `info!` 而非 `warn!`——对蒙古文这类以空格作词间分隔的文字，整库带尾随空格是
    /// **正常写法**（toli 词库 37.6 万条几乎条条如此），报 warn 等于把常态当异常喊。
    fn log_trailing_ws(&self, path: &Path) {
        if self.trailing_ws_text == 0 {
            return;
        }
        info!(
            "词库 {} 有 {} 条词条以空白收尾，已按内容保留（不再剥除）。\
             若这是排版噪声而非有意的词间空格，请清理词库文件。",
            path.display(),
            self.trailing_ws_text
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
                if !crate::cached::prefix_capped_keep(
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

    /// 通配查询（内存路径对应 [`crate::datformat::WdatReader::search_pattern`]）：以首个通配前的
    /// 字面前缀做 range 扫描、逐条过滤，等长档先取满 `limit`、余额给更长档。
    /// 首位即通配时字面前缀为空，退化为全表扫描（内存模式只在无 wdat 缓存时出现）。
    pub fn search_pattern(
        &self,
        pattern: &str,
        wildcard: char,
        limit: usize,
        with_prefix: bool,
    ) -> Vec<crate::cached::DictHit> {
        if limit == 0 || pattern.is_empty() {
            return Vec::new();
        }
        let lit = crate::layer::literal_prefix(pattern, wildcard);
        let n = pattern.chars().count();
        let mut exact: Vec<crate::cached::DictHit> = Vec::new();
        let mut longer: Vec<crate::cached::DictHit> = Vec::new();
        for (code, entries) in self.entries.range(lit.to_string()..) {
            if !code.starts_with(lit) {
                break;
            }
            if !crate::layer::pattern_matches(pattern, wildcard, code, with_prefix) {
                continue;
            }
            let bucket = if code.chars().count() == n {
                &mut exact
            } else {
                &mut longer
            };
            for e in entries {
                bucket.push(crate::cached::DictHit {
                    code: code.clone(),
                    text: e.text.clone(),
                    weight: e.weight,
                    order: e.order,
                    boundary: e.boundary,
                });
            }
        }
        let key = |a: &crate::cached::DictHit, b: &crate::cached::DictHit| {
            b.weight
                .cmp(&a.weight)
                .then(a.order.cmp(&b.order))
                .then_with(|| a.code.cmp(&b.code))
        };
        exact.sort_by(key);
        longer.sort_by(key);
        exact.truncate(limit);
        longer.truncate(limit - exact.len());
        exact.extend(longer);
        exact
    }

    /// 按英文词频名次给**权重为 0** 的条目赋权（见 [`crate::freq_rank`]），写了权重的不动。
    /// 赋权后各码下重新按权重排序。返回赋了权的条目数。
    pub fn apply_freq_ranks(&mut self, ranks: &crate::freq_rank::FreqRanks) -> usize {
        let mut n = 0;
        for code_entries in self.entries.values_mut() {
            let mut touched = false;
            for e in code_entries.iter_mut().filter(|e| e.weight == 0) {
                if let Some(w) = ranks.weight_of(&e.text) {
                    e.weight = w;
                    touched = true;
                    n += 1;
                }
            }
            if touched {
                code_entries.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.order.cmp(&b.order)));
            }
        }
        n
    }

    /// 遍历全部条目(供反查索引构建):对每个 (code, text, weight) 调用 `f`。
    pub fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
        for (code, entries) in &self.entries {
            for e in entries {
                f(code, &e.text, e.weight);
            }
        }
    }

    /// 按码升序逐组遍历（`entries` 本就是 BTreeMap，序与 wdat 的 DFS 同为字节序）。
    pub fn groups(&self) -> impl Iterator<Item = (&str, &[CodetableEntry])> {
        self.entries.iter().map(|(c, v)| (c.as_str(), v.as_slice()))
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
    // **行尾空白只参与判定，不参与取值**：末列若是 text（CodeFirst 布局），行尾那段空白
    // 就是词条内容本身——蒙古文 toli 词库整库以空格收尾。前导同理不剥（「全角空格」词条）。
    // 见 [`trim_line_end`]。
    // `comments_on == false` = 本行位于 `# no comment` 之后，`#` 此时是**数据**而非注释。
    if trim_line_end(line).is_empty() || (comments_on && line.starts_with('#')) {
        return None;
    }
    let parts: Vec<&str> = line.split('\t').collect();
    if parts.len() < spec.required_cols() {
        stats.short += 1;
        return None;
    }
    // 列位置由文件级判定给定（头部 `columns:` 声明，或整文件探测），不再逐行猜。
    //
    // **code 剥尾随空白、text 不剥**：编码列末尾的空白只可能是排版噪声（码的字符集里没有
    // 它的位置；音节库靠**列内**空格分音节，剥尾不动边界），而 text 列末尾的空白可能是词条
    // 内容。TextFirst 布局下 code 在末列，这一步接住了从前由整行 trim 承担的活。
    //
    // **命令栏语法条目逐行纠正列序**（t172）：编码列里的 `$CC(` / `$SS(` / `$AA(` 绝不可能
    // 是编码，它落在 code 列只说明这一行的列序与全文相反——用户照着文档里 `zzgo = $SS(...)`
    // 的「编码在前」写法手写进「文本在前」的词库，文件级判定按多数票不会为这一行让步，
    // 结果 text=`u`、code=`$SS(...)`，这条短语永远打不出来，也没有任何提示。
    // 只认 marker、不单凭 `{`：`{` 可以是编码字符集里的正经键位。
    let (code_col, text_col) = if wind_wdict::is_misplaced_cmdbar_code(parts[spec.code_col])
        && !wind_wdict::is_cmdbar_text(parts[spec.text_col])
    {
        (spec.text_col, spec.code_col)
    } else {
        (spec.code_col, spec.text_col)
    };
    let raw_code = parts[code_col].trim_end();
    let text = parts[text_col];
    if text.len() != text.trim_end().len() {
        stats.trailing_ws_text += 1;
    }
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
    // 逐列 `trim`：整行 trim 撤掉后，末列权重的尾随空白得在这里剥，否则 `"5 "` 解析失败。
    let weight: i32 = match spec.weight_col.and_then(|i| parts.get(i)).map(|s| s.trim()) {
        // 空权重列是常态（Rime 语义：留给预设词库补），不计入异常统计。
        None | Some("") => 0,
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
        text: wind_wdict::unescape_text_field(text),
        weight,
        boundary,
    })
}

/// 文本层（`wind-userdata::text`）复用的 Rime 正文布局。规则只在本文件一份：正文起点
/// （[`rime_body_offset`]）、`# no comment`（[`find_no_comment_directive`]）、列规格
/// （[`resolve_columns`]：`columns:` 声明优先，否则探测）。
pub struct RimeLayout {
    pub body_offset: usize,
    pub comment_cutoff: Option<usize>,
    pub text_col: usize,
    pub code_col: usize,
    pub weight_col: Option<usize>,
}

/// 只要正文起点与 `# no comment` 位置、不判列的布局：自带 `columns:` 表头、列名不限于
/// text / code 的表格文本（`wind-userdata::text::table`）用。返回 `(body_offset, comment_cutoff)`，
/// 两者都是相对整个 `content` 的字节偏移。
pub fn rime_body_layout(content: &str) -> Option<(usize, Option<usize>)> {
    let body_offset = rime_body_offset(content)?;
    let cutoff = find_no_comment_directive(&content[body_offset..]);
    Some((body_offset, cutoff.map(|c| c + body_offset)))
}

pub fn rime_layout(content: &str, path: &Path) -> Option<RimeLayout> {
    let body_offset = rime_body_offset(content)?;
    let body = &content[body_offset..];
    let cutoff = find_no_comment_directive(body);
    let spec = resolve_columns(content, body, path, cutoff)?;
    Some(RimeLayout {
        body_offset,
        comment_cutoff: cutoff.map(|c| c + body_offset),
        text_col: spec.text_col,
        code_col: spec.code_col,
        weight_col: spec.weight_col,
    })
}

/// 逐行解析器（设置端只读查看词库用）：列规格按整文件判定一次（同加载器的 [`resolve_columns`]），
/// 逐行走加载器同一个 [`parse_rime_line`]——列数不足、编码或词为空的行跳过，`$CC(` 一类行
/// 纠正列序，都与加载器一致，不另写一份。
#[derive(Clone, Copy, Debug)]
pub struct RimeRowParser {
    spec: ColumnSpec,
    lowercase_code: bool,
}

/// [`RimeRowParser::parse`] 的一行。`code` 已拼平（音节库去掉了空格），音节边界在 `boundary`
/// （同 [`syllable_boundary_mask`]，非音节库为 0）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RimeRow {
    pub code: String,
    pub text: String,
    pub weight: i32,
    pub boundary: u64,
}

impl RimeRowParser {
    /// `content` 是整篇文本。没有 `...` 正文分隔行，或 `columns:` 声明残缺（加载器整库跳过）
    /// 时为 `None`；前者先用 [`rime_body_layout`] 区分。`lowercase_code` 同加载器（英文库为真）。
    pub fn new(content: &str, path: &Path, lowercase_code: bool) -> Option<Self> {
        let body_offset = rime_body_offset(content)?;
        let body = &content[body_offset..];
        let cutoff = find_no_comment_directive(body);
        let spec = resolve_columns(content, body, path, cutoff)?;
        Some(Self {
            spec,
            lowercase_code,
        })
    }

    /// 解析一行（不含换行）；`comments_on` = 本行在 `# no comment` 之前。加载器不收的行为 `None`。
    pub fn parse(&self, line: &str, comments_on: bool) -> Option<RimeRow> {
        let mut st = ParseStats::default();
        parse_rime_line(line, comments_on, self.lowercase_code, self.spec, &mut st).map(|r| {
            RimeRow {
                code: r.code,
                text: r.text,
                weight: r.weight,
                boundary: r.boundary,
            }
        })
    }

    /// 编码列下标（搜索粗筛用；`$CC(` 一类行的列序以 [`Self::parse`] 为准）。
    pub fn code_col(&self) -> usize {
        self.spec.code_col
    }
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
#[path = "codetable_tests.rs"]
mod tests;
