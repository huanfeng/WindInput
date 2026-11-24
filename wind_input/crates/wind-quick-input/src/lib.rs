//! 快捷输入：内置候选来源（日期 / 计算 / 数字金额）的纯逻辑提供器。
//!
//! 本模块只负责把输入缓冲（如 "12.25" / "1+2*3"）转换为候选文本列表，
//! 不涉及按键流程与 UI（由协调器状态机驱动）。
//!
//! ## 来源与开关
//!
//! 三个来源各自是一个 **mix 成员 id**（`quick_input.date` / `.calc` / `.number`），
//! 连同协调器实现的 `quick_input.repeat`（重复上屏）一起，由 `mix_modes.members`
//! 列表决定**有无与顺序**——开关即增删，优先级即排序，不再另设 bool 旁路开关
//! （旧的 `schema.quick_input.enable_english` 曾与 `members` 构成双真相源）。
//!
//! ## 格式取舍
//!
//! 候选格式按国标精简，冗余与不规范写法不再产出（见各来源函数文档）：
//! - 日期：GB/T 7408（≡ISO 8601）+ GB/T 15835（中文数字用法，月日**不补前导零**）
//! - 金额：《会计基础工作规范》第五十二条（大写金额与「整」的写法）
//!
//! ## 格式表（自定义）
//!
//! 上述格式集是**出厂默认**，不是硬编码：候选的文本与组内顺序由 [`FormatTable`] 决定，
//! 出厂表来自 `data/system.quick.toml`，用户可在配置目录放同名文件整份覆盖
//! （高级特性，详见 `docs/design/quick-input-format-table.md`）。
//!
//! **解析归代码、渲染归配置**：`"2025.12.25"` 怎么切成年月日是本模块的文法，
//! 切出来之后长什么样才是格式表的事。不带表的 `generate*` 入口一律用
//! [`FormatTable::builtin`]，与改造前逐条等价。

use chrono::Datelike;
use std::sync::OnceLock;

mod format_table;
/// 农历换算。公开供 `wind-phrase` 复用——短语的 `$L*` 绑当前时间、快捷输入的绑
/// 用户打进去的日期，但「今天农历几号」必须是同一个答案。
pub mod lunar;
/// `$` 模板引擎。公开供 `wind-phrase` 的简单模板（`system.phrases.toml`）复用——
/// 两个配置文件的变量写法必须由同一份解析器决定。
pub mod template;
pub mod user_file;
mod vars;

pub use format_table::{
    FormatAdjust, FormatEntry, FormatEntryView, FormatKind, FormatTable, is_user_format_id,
    next_user_format_id, validate_format_text,
};
pub use vars::QuickValues;

/// 表达式模板（`{amt(unit='圆')}`）的求值回调，由宿主提供。
///
/// 本 crate **不依赖 `wind-cmdbar`**：求值器在上层（coordinator 同时依赖两者），
/// 这里只把「模板 + 本次解析出的量」交出去。返回 `None` = 求值失败（未知函数、
/// 参数错误…），该条候选被丢弃，宿主负责告警。
pub type ExprEval<'a> = &'a dyn Fn(&str, &QuickValues) -> Option<String>;

/// 内置默认表的进程级单例。不带表的入口都走它——每次调用重建一份的话，
/// 每次按键都要重新分配十几个 String。
fn builtin_table() -> &'static FormatTable {
    static T: OnceLock<FormatTable> = OnceLock::new();
    T.get_or_init(FormatTable::builtin)
}

/// 按格式表渲染某一类的全部候选。
///
/// 每条按 [`FormatEntry::is_expression`] 分流：变量模板本地展开，表达式模板交给
/// 宿主的 `eval`（无 `eval` 时该条静默跳过——不带求值器的调用方拿不到表达式候选，
/// 但绝不能把 `{amt()}` 当字面量上屏）。
///
/// 两道过滤各有分工，都不能省：
/// - 展开/求值返回 `None`：模板含本类不支持的变量、或表达式求值失败 → 整条作废；
/// - 结果为空串：该条在本次输入下不适用（如 `$AMT` 遇三位小数）→ 丢弃。
///   这是格式表表达「条件」的唯一方式，配置里不写 if。
///
/// 用户调整同样在这里生效（[`FormatTable::entries_of_adjusted`]）：停用的条目不渲染，
/// 移动过的按新序输出。空调整时与改造前逐条等价。
fn render(
    table: &FormatTable,
    adjust: &FormatAdjustMap,
    values: &QuickValues,
    eval: Option<ExprEval>,
) -> Vec<Rendered> {
    // ★ 按**实际渲染的类别**取调整，而不是按来源。`QuickSource::Date` 会产出 date
    // 或 year_month 之一（视输入形态），只有到了这里才知道是哪个——在调用方按 src
    // 猜类别，年月的调序就会静默不生效。
    let empty = FormatAdjust::default();
    let adjust = adjust.get(values.kind().as_str()).unwrap_or(&empty);
    table
        .entries_of_adjusted(values.kind(), adjust)
        .into_iter()
        .filter_map(|e| {
            let text = if e.is_expression() {
                eval.and_then(|f| f(&e.text, values))
            } else {
                template::expand(&e.text, |name| values.get(name))
            }?;
            (!text.is_empty()).then(|| Rendered {
                id: e.id.clone(),
                text,
            })
        })
        .collect()
}

/// 一条渲染结果：**格式 id + 候选文本**。
///
/// id 是候选的稳定身份——右键调序要知道用户点的是哪条格式，而 `text` 逐次输入都不同
/// （`2026年6月19日` / `2026年6月20日`），按文本认人必然失配。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    /// 格式表里的条目 id（如 `date.lunar`）。
    pub id: String,
    pub text: String,
}

/// 按类别索引的用户调整，键是 [`FormatKind::as_str`]。
///
/// 用 map 而不是单个 [`FormatAdjust`]：一次 `generate_adjusted` 可能渲染 date 或
/// year_month（视输入形态），只有 `render` 内部才知道实际类别。
pub type FormatAdjustMap = std::collections::HashMap<String, FormatAdjust>;

/// 丢掉 id 只留文本。不带表/不带调整的公开入口用它保持原返回类型——
/// 两种入口共用同一条渲染路径，不各写一份。
fn texts(v: Vec<Rendered>) -> Vec<String> {
    v.into_iter().map(|r| r.text).collect()
}

// ───────────────────────── 成员 id ─────────────────────────

/// 旧的合并成员 id。存量配置里出现时展开为 [`LEGACY_EXPANSION`]。
pub const MEMBER_LEGACY: &str = "quick_input";
/// 日期 / 年月来源。
pub const MEMBER_DATE: &str = "quick_input.date";
/// 计算来源。
pub const MEMBER_CALC: &str = "quick_input.calc";
/// 数字 / 金额来源。
pub const MEMBER_NUMBER: &str = "quick_input.number";
/// 重复上屏来源（**由协调器实现**：候选取自上屏历史，本 crate 不产出）。
pub const MEMBER_REPEAT: &str = "quick_input.repeat";
/// 快捷输入历史来源（**由协调器实现**：候选取自快捷输入里上屏过的字面文本，t46）。
///
/// 与 [`MEMBER_REPEAT`] 的分工：那个只在空缓冲时给「上一次上屏」；本来源在有缓冲时按
/// 前缀给历史。**出厂不在任何融合实例里**，由用户自己加进成员列表。
pub const MEMBER_HISTORY: &str = "quick_input.history";

/// 旧值 `quick_input` 的展开序，同时是内置「快捷」融合的默认来源序。
///
/// calc 在最前：它与 date 的输入形态互斥（表达式必含二元运算符，日期只有数字与点），
/// 与 number 则**刻意共存**（`123*4` 先求值再转金额），而计算结果作首选是明确诉求。
///
/// ★ number 与 date **不会同时出候选**：小数点个数就是归属判据（见 [`has_second_dot`]），
/// 一个点归数字、两个点归日期。故这两者的相对次序对任何一次输入都不再可观测，
/// 此处保留原序只为不动 calc 与 number 的相对优先级。
pub const LEGACY_EXPANSION: &[&str] = &[MEMBER_CALC, MEMBER_NUMBER, MEMBER_DATE, MEMBER_REPEAT];

/// 是否为快捷输入家族的内置成员 id（含 `quick_input.repeat` 与旧值 `quick_input`）。
/// 用于把它们从「真实方案成员」中排除——它们没有对应的 `.schema.toml`。
pub fn is_quick_member(member: &str) -> bool {
    member == MEMBER_LEGACY || member.starts_with("quick_input.")
}

/// 本 crate 实现的候选来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickSource {
    /// 日期与年月。
    Date,
    /// 算式求值。
    Calc,
    /// 数字、中文数字与金额。
    Number,
}

impl QuickSource {
    /// 成员 id → 来源。`quick_input.repeat` 与旧值 `quick_input` 返回 `None`
    /// （前者由协调器实现，后者应先经 [`LEGACY_EXPANSION`] 展开）。
    pub fn from_member(member: &str) -> Option<Self> {
        match member {
            MEMBER_DATE => Some(Self::Date),
            MEMBER_CALC => Some(Self::Calc),
            MEMBER_NUMBER => Some(Self::Number),
            _ => None,
        }
    }
}

/// 按来源生成候选（用内置默认表）。
pub fn generate(src: QuickSource, buffer: &str, decimal_places: i32) -> Vec<String> {
    generate_with(src, buffer, decimal_places, builtin_table())
}

/// 按来源生成候选，指定格式表。**表达式模板会被跳过**（无求值器）。
pub fn generate_with(
    src: QuickSource,
    buffer: &str,
    decimal_places: i32,
    table: &FormatTable,
) -> Vec<String> {
    generate_with_eval(src, buffer, decimal_places, table, None)
}

/// 按来源生成候选，指定格式表与表达式求值器。协调器走这个入口。
///
/// `eval` 为 `None` 时表达式模板静默跳过——只有 `{...}` 那几条不出候选，
/// 变量模板不受影响。
pub fn generate_with_eval(
    src: QuickSource,
    buffer: &str,
    decimal_places: i32,
    table: &FormatTable,
    eval: Option<ExprEval>,
) -> Vec<String> {
    generate_adjusted(
        src,
        buffer,
        decimal_places,
        table,
        &FormatAdjustMap::new(),
        eval,
    )
    .into_iter()
    .map(|r| r.text)
    .collect()
}

/// 按来源生成候选，**带格式 id 与用户调整**。协调器走这个入口。
///
/// 与 [`generate_with_eval`] 同源（都经 `render`）：那边只是丢掉 id、按空调整渲染。
/// 两条入口若各自实现，「右键看到的顺序」与「实际出的候选」就会分叉。
pub fn generate_adjusted(
    src: QuickSource,
    buffer: &str,
    decimal_places: i32,
    table: &FormatTable,
    adjust: &FormatAdjustMap,
    eval: Option<ExprEval>,
) -> Vec<Rendered> {
    match src {
        QuickSource::Date => render_date(buffer, table, adjust, eval),
        QuickSource::Calc => render_calc(buffer, decimal_places, table, adjust, eval),
        QuickSource::Number => render_number(buffer, decimal_places, table, adjust, eval),
    }
}

/// 三个来源全开时的合并候选（按 [`LEGACY_EXPANSION`] 序去重）。
/// 便捷入口，主要供测试与不读配置的调用方使用；协调器按 `members` 逐个调 [`generate`]。
pub fn generate_quick_input_candidates(buffer: &str, decimal_places: i32) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // 序取自 LEGACY_EXPANSION 而不是另写一份字面量：那是出厂来源序的单一真相源，
    // 两处各写一遍的话，调整来源优先级时这个便捷入口会静默落后（`repeat` 无来源，被滤掉）。
    for src in LEGACY_EXPANSION
        .iter()
        .filter_map(|m| QuickSource::from_member(m))
    {
        for c in generate(src, buffer, decimal_places) {
            if !c.is_empty() && !out.contains(&c) {
                out.push(c);
            }
        }
    }
    out
}

// ───────────────────────── 输入归一 ─────────────────────────

/// 裁掉尾部「未写完」的运算符与点号，使输入过程中候选不中断：
/// `"123+"` 等同 `"123"`、`"1+2*"` 等同 `"1+2"`、`"2026.3."` 等同 `"2026.3"`。
///
/// 全部裁完则返回原串（`"+++"` 不该被当成空输入）。
fn trim_pending_tail(s: &str) -> &str {
    let t = s.trim_end_matches(['+', '-', '*', '/', '^', '.']);
    if t.is_empty() { s } else { t }
}

/// 原始缓冲里是否已经出现**第二个**小数点——**数字与日期的归属判据**。
///
/// 一个小数点的输入（`12.25` / `2026.2`）在文法上是真歧义：既是合法小数，也是合法
/// 月日 / 年月。本判据把它一刀切开——**一个点归数字，两个点归日期**，两组互斥：
///
/// - 数字只容一个小数点，第二个点一出现就不再可能是数字（打 `2026.2.3` 的中途
///   不该冒出一屏金额读法）；
/// - 反过来，只打一个点的人多半在打金额（`12.25` 作金额远比作月日常见），
///   此时日期整组让开，想要日期多打一个点即可（`12.25.` / `2026.2.`）。
///
/// 两个方向都有确定出口，不靠猜，也不必为一次输入翻两屏候选。三段完整日期
/// （`2025.12.25`）天然带两个点，用户无需为它多做任何事。
///
/// ⚠️ 必须看**裁剪前**的串。[`trim_pending_tail`] 抹掉尾点是为了让 `123.`（小数位
/// 还没打）仍出「壹佰贰拾叁元整」，但它连带抹掉了「用户已经打下第二个点」这一信号：
/// `2026.2.` 被裁成合法小数 `2026.2`，判据当场反转，两侧同时失效。
///
/// ★ 判据只能是点的**个数**，不能是「首段像不像年份」：`5000.5`（伍仟元伍角整）与
/// `2000.5`（贰仟元伍角整）和年月形态完全同构，按首段范围砍会静默吃掉常见金额——
/// 多几条候选是可见噪音，金额消失是不可见失败，两侧代价不对称。
///
/// 算式形态不受影响（`1.5*2.5` 走求值那条路，不经这里）。
fn has_second_dot(buffer: &str) -> bool {
    buffer.matches('.').count() > 1
}

// ───────────────────────── 日期 ─────────────────────────

/// 解析 "m.d" 或 "y.m.d"；省略年份时 year=0。
fn parse_date_parts(s: &str) -> Option<(i32, u32, u32)> {
    let parts: Vec<&str> = s.split('.').collect();
    match parts.len() {
        2 => {
            let m: u32 = parts[0].parse().ok()?;
            let d: u32 = parts[1].parse().ok()?;
            if (1..=12).contains(&m) && (1..=31).contains(&d) {
                Some((0, m, d))
            } else {
                None
            }
        }
        3 => {
            let y: i32 = parts[0].parse().ok()?;
            let m: u32 = parts[1].parse().ok()?;
            let d: u32 = parts[2].parse().ok()?;
            if (1..=12).contains(&m) && (1..=31).contains(&d) {
                Some((y, m, d))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// 日期来源：完整日期优先，否则试年月。
///
/// **要求缓冲里已有第二个小数点**（见 [`has_second_dot`]）：`"2026.3"` 归数字、
/// `"2026.3."` 归日期。尾点因而同时是「小数位还没打完」与「我要的是日期」两个信号，
/// 后者由本闸门解释，前者交给 [`trim_pending_tail`] 裁掉，故 `"2026.3."` 出的是
/// 完整的「2026年3月」而非因第三段为空而全空。
///
/// 格式集（按序）：中文 → 全汉字 → ISO 扩展 → ISO 基本 → 斜杠。
/// **不产出**中文补零写法（`2025年03月05日`）——GB/T 15835 的中文日期不加前导零，
/// 它与不补零的那条只在月/日 <10 时不同，属纯冗余。
pub fn generate_date_candidates(input: &str) -> Vec<String> {
    texts(render_date(
        input,
        builtin_table(),
        &FormatAdjustMap::new(),
        None,
    ))
}

/// 日期渲染：完整日期优先，产出为空再试年月。
///
/// 「为空再试」而非「解析成功即归属」：用户把 date 一组删空时，`2025.12` 这种
/// 既可解析为完整日期也可解析为年月的输入仍应给出年月候选。
///
/// ★ 闸门装在这里而不是 [`render_full_date`] / [`render_year_month`] 里：那两者是
/// 纯解析，公开入口 [`generate_year_month_candidates`] 的语义就是「按年月渲染这串」，
/// 不该被输入形态的归属判据管；只有 [`QuickSource::Date`] 这条**分派**路径才该管。
/// 同理，设置页示例列走 [`generate_adjusted`]（受管），而 `quick_eval` 的示例值直接
/// 构造 [`QuickValues`]（不受管）。
fn render_date(
    input: &str,
    table: &FormatTable,
    adjust: &FormatAdjustMap,
    eval: Option<ExprEval>,
) -> Vec<Rendered> {
    // 归属判据（见 [`has_second_dot`]）：一个点归数字、两个点归日期。**必须在
    // `trim_pending_tail` 之前问**——它会把 `2026.2.` 裁回 `2026.2`，信号当场丢失。
    if !has_second_dot(input) {
        return Vec::new();
    }
    let input = trim_pending_tail(input);
    let ymd = render_full_date(input, table, adjust, eval);
    if !ymd.is_empty() {
        return ymd;
    }
    render_year_month(input, table, adjust, eval)
}

/// 年份的全汉字写法：逐位改写，`2025` → 「二〇二五」（GB/T 15835）。
///
/// 与短语层的 `${YC}` 共用同一份实现——同一个写法在两处取值不同，用户无从分辨。
pub fn year_to_chinese(year: i32) -> String {
    digits_to_chinese_chars(&year.to_string())
}

/// 0–99 的中文位值读法（月 / 日 / 时分用）：`6`→六，`10`→十，`12`→十二，
/// `20`→二十，`25`→二十五。短语层的 `${MC}`/`${DC}` 同用此函数。
///
/// 十位不写「一」——中文日期是「十二月」不是「一十二月」，故不复用
/// [`number_to_chinese`]（它按通用位值制输出「一十二」）。
/// 超出 0–99 回退逐位改写：调用点（月/日/时分）都在范围内，但公开 API 不该 panic。
pub fn small_int_to_chinese(n: u32) -> String {
    if n > 99 {
        return digits_to_chinese_chars(&n.to_string());
    }
    let d = |x: u32| CHAR_DIGITS[x as usize];
    match n {
        0..=9 => d(n).to_string(),
        10..=19 if n.is_multiple_of(10) => "十".to_string(),
        10..=19 => format!("十{}", d(n % 10)),
        _ if n.is_multiple_of(10) => format!("{}十", d(n / 10)),
        _ => format!("{}十{}", d(n / 10), d(n % 10)),
    }
}

/// 完整日期（`y.m.d`）或月日（`m.d`，年补当前年）。
///
/// ★ 两者**分派到不同的格式类别**：用户只打两段时想要的多半是「12月25日」这种不带年的
/// 短写法，而三段输入里的年份是他自己打的。同一套 `date` 条目伺候两种形态，就只能二选一
/// ——要么两段输入被强行补年（改造前的行为），要么三段输入冒出不带年的候选。
/// 变量集两类相同（月日的年 = 当前年），差别只在出厂条目与用户调整各自记账。
fn render_full_date(
    input: &str,
    table: &FormatTable,
    adjust: &FormatAdjustMap,
    eval: Option<ExprEval>,
) -> Vec<Rendered> {
    let (year, month, day) = match parse_date_parts(input) {
        Some(v) => v,
        None => return Vec::new(),
    };
    // year == 0 是 `parse_date_parts` 对「只打了两段」的标记，同时也隐含了
    // 「首段是合法月份」——`2026.2` 在那里就已被拒（月份越界），落到年月路径去。
    let values = if year == 0 {
        QuickValues::MonthDay {
            y: chrono::Local::now().year(),
            m: month,
            d: day,
        }
    } else {
        QuickValues::Date {
            y: year,
            m: month,
            d: day,
        }
    };
    render(table, adjust, &values, eval)
}

/// 年月表达式（首段>31，第二段 1-12）。
///
/// 首段 >31 是与「月.日」的分界：`12.25` 只可能是 12 月 25 日，`2025.12` 只可能是年月。
/// 同样不产出中文补零写法（`2025年06月`）。格式集与完整日期同构：中文 → 全汉字 → ISO → 斜杠。
///
/// 这是**按年月渲染**的直接入口，不经 [`has_second_dot`] 那道归属判据——传 `"2025.12"`
/// 照常出候选。走 [`QuickSource::Date`] 的调用方才受判据管。
pub fn generate_year_month_candidates(input: &str) -> Vec<String> {
    texts(render_year_month(
        input,
        builtin_table(),
        &FormatAdjustMap::new(),
        None,
    ))
}

fn render_year_month(
    input: &str,
    table: &FormatTable,
    adjust: &FormatAdjustMap,
    eval: Option<ExprEval>,
) -> Vec<Rendered> {
    let input = trim_pending_tail(input);
    let parts: Vec<&str> = input.split('.').collect();
    if parts.len() != 2 {
        return Vec::new();
    }
    let y: i32 = match parts[0].parse() {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let m: u32 = match parts[1].parse() {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    if y <= 31 || !(1..=12).contains(&m) {
        return Vec::new();
    }
    render(table, adjust, &QuickValues::YearMonth { y, m }, eval)
}

// ───────────────────────── 计算器 ─────────────────────────

/// 是否含**二元**运算符：开头的、以及紧跟另一运算符或左括号的 `+`/`-` 是一元号，不算。
///
/// 这道区分让 `"-5"` 不被当成算式（它只是个负数，交给数字来源），而 `"-5+3"` 是。
fn has_binary_operator(s: &str) -> bool {
    let b = s.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        if !matches!(c, b'+' | b'-' | b'*' | b'/' | b'^') {
            continue;
        }
        if matches!(c, b'*' | b'/' | b'^') {
            return true;
        }
        // `+`/`-`：前一个非空字符是数字或右括号才是二元运算
        match b[..i].iter().rev().find(|&&p| p != b' ') {
            Some(&p) if p.is_ascii_digit() || p == b')' => return true,
            _ => {}
        }
    }
    false
}

/// 单个字符是否属于表达式字符集：数字、四则、幂、括号、点。
///
/// **公开的 char 级谓词**：协调器的自由输入透镜要判断「这个字符还能不能算表达式编码」，
/// 必须与本 crate 的求值器认同同一个字符集。抽出来是为了不让那份集合出现第二份拷贝——
/// 两处各写一遍的话，日后给求值器加个 `%` 运算符就会静默地让透镜判据落后一个字符。
pub fn is_expr_char(c: char) -> bool {
    c.is_ascii_digit() || matches!(c, '+' | '-' | '*' | '/' | '^' | '.' | '(' | ')')
}

/// 表达式字符集：数字、四则、幂、括号、点。
fn is_expr_charset(s: &str) -> bool {
    s.chars().all(is_expr_char)
}

/// 计算来源：**结果作首候选**，完整等式次之。
///
/// 用户打算式多半是为了拿结果，等式形态（`1+2*3=7`）留作次选，供需要留痕的场景。
/// 用户手打的 `=` 及其右侧被忽略（取首个 `=` 前求值），使「再按 =」乃至续打答案时
/// 候选不清空。
pub fn generate_calc_candidates(expr: &str, decimal_places: i32) -> Vec<String> {
    texts(render_calc(
        expr,
        decimal_places,
        builtin_table(),
        &FormatAdjustMap::new(),
        None,
    ))
}

fn render_calc(
    expr: &str,
    decimal_places: i32,
    table: &FormatTable,
    adjust: &FormatAdjustMap,
    eval: Option<ExprEval>,
) -> Vec<Rendered> {
    let lhs = expr.split('=').next().unwrap_or(expr);
    let clean: &str = trim_pending_tail(lhs);
    if clean.is_empty() || !has_binary_operator(clean) || !is_expr_charset(clean) {
        return Vec::new();
    }
    // 以数字、左括号或一元号开头
    let first = clean.as_bytes()[0];
    if first != b'(' && first != b'-' && first != b'+' && !first.is_ascii_digit() {
        return Vec::new();
    }
    let val = match evaluate_expression(clean) {
        Some(v) if v.is_finite() => v,
        _ => return Vec::new(),
    };
    let result = format_calc_result_prec(val, decimal_places);
    render(
        table,
        adjust,
        &QuickValues::Calc {
            expr: clean.to_string(),
            result,
            // `{}` 是 f64 的最短可回读表示，不受 decimal_places 影响——
            // `{pct()}` 等函数据此自行换算，避免对已截断的 result 二次舍入。
            exact: format!("{val}"),
        },
        eval,
    )
}

/// 递归下降求值。支持 `+ - * /`、幂 `^`、一元正负号与括号。返回 None 表示解析失败。
///
/// 优先级（低→高）：`+ -` < `* /` < 一元 `+ -` < `^`（右结合）。
/// 一元号低于 `^` 是数学惯例：`-2^2 = -(2^2) = -4`；指数侧仍接受一元号，故 `2^-1 = 0.5`。
pub fn evaluate_expression(expr: &str) -> Option<f64> {
    let bytes: Vec<u8> = expr.bytes().collect();
    let mut p = ExprParser {
        input: &bytes,
        pos: 0,
    };
    let v = p.parse_expr()?;
    // 必须消费完整输入
    if p.pos != p.input.len() {
        return None;
    }
    Some(v)
}

struct ExprParser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl ExprParser<'_> {
    fn parse_expr(&mut self) -> Option<f64> {
        let mut left = self.parse_term()?;
        while self.pos < self.input.len() {
            let op = self.input[self.pos];
            if op != b'+' && op != b'-' {
                break;
            }
            self.pos += 1;
            let right = self.parse_term()?;
            if op == b'+' {
                left += right;
            } else {
                left -= right;
            }
        }
        Some(left)
    }

    fn parse_term(&mut self) -> Option<f64> {
        let mut left = self.parse_unary()?;
        while self.pos < self.input.len() {
            let op = self.input[self.pos];
            if op != b'*' && op != b'/' {
                break;
            }
            self.pos += 1;
            let right = self.parse_unary()?;
            if op == b'*' {
                left *= right;
            } else {
                if right == 0.0 {
                    return None; // 除零
                }
                left /= right;
            }
        }
        Some(left)
    }

    /// 一元正负号（可叠加，如 `--3`）。作用于整个幂，故 `-2^2 = -4`。
    fn parse_unary(&mut self) -> Option<f64> {
        if self.pos < self.input.len() {
            let c = self.input[self.pos];
            if c == b'-' || c == b'+' {
                self.pos += 1;
                let v = self.parse_unary()?;
                return Some(if c == b'-' { -v } else { v });
            }
        }
        self.parse_power()
    }

    /// 幂运算，右结合：`2^3^2 = 2^(3^2) = 512`。
    /// 指数侧递归到 `parse_unary` 而非 `parse_power`，使 `2^-1` 合法。
    fn parse_power(&mut self) -> Option<f64> {
        let base = self.parse_primary()?;
        if self.pos < self.input.len() && self.input[self.pos] == b'^' {
            self.pos += 1;
            let exp = self.parse_unary()?;
            let v = base.powf(exp);
            return Some(v);
        }
        Some(base)
    }

    fn parse_primary(&mut self) -> Option<f64> {
        if self.pos < self.input.len() && self.input[self.pos] == b'(' {
            self.pos += 1;
            let v = self.parse_expr()?;
            if self.pos >= self.input.len() || self.input[self.pos] != b')' {
                return None;
            }
            self.pos += 1;
            return Some(v);
        }
        self.parse_number()
    }

    fn parse_number(&mut self) -> Option<f64> {
        let start = self.pos;
        while self.pos < self.input.len() {
            let c = self.input[self.pos];
            if c.is_ascii_digit() || c == b'.' {
                self.pos += 1;
            } else {
                break;
            }
        }
        if start == self.pos {
            return None;
        }
        std::str::from_utf8(&self.input[start..self.pos])
            .ok()?
            .parse::<f64>()
            .ok()
    }
}

/// 结果格式化：decimal_places<=0 四舍五入为整数，否则最多保留位数并去尾零。
/// 超出 i64 量程的值走定点浮点格式，避免 `as i64` 饱和成 9223372036854775807。
pub fn format_calc_result_prec(val: f64, decimal_places: i32) -> String {
    if val.is_nan() || val.is_infinite() {
        return val.to_string();
    }
    let fits_i64 = val.abs() < i64::MAX as f64;
    if decimal_places <= 0 {
        let rounded = val.round();
        return if fits_i64 {
            format!("{}", rounded as i64)
        } else {
            format!("{:.0}", rounded)
        };
    }
    // 整数结果直接输出
    if val == val.trunc() {
        return if fits_i64 {
            format!("{}", val as i64)
        } else {
            format!("{:.0}", val)
        };
    }
    let mut s = format!("{:.*}", decimal_places as usize, val);
    if s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    s
}

// ───────────────────────── 数字 / 金额 / 中文数字 ─────────────────────────

const LOWER_DIGITS: [&str; 10] = ["零", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
/// 逐位改写专用数字表。
///
/// 与 [`LOWER_DIGITS`] 只差首项：阿拉伯数字**逐位**改写为汉字时，0 写「〇」
/// （GB/T 15835，如「二〇二六年」）；而位值读法里表示数位空缺的是「零」
/// （「一万零一」「二百零五」），财务大写同理（「壹仟零贰拾元」）。
/// 两者不是一个字，故不能共用一份表。
const CHAR_DIGITS: [&str; 10] = ["〇", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
const UPPER_DIGITS: [&str; 10] = ["零", "壹", "贰", "叁", "肆", "伍", "陆", "柒", "捌", "玖"];
const LOWER_UNITS: [&str; 4] = ["", "十", "百", "千"];
const UPPER_UNITS: [&str; 4] = ["", "拾", "佰", "仟"];
const GROUP_UNITS: [&str; 4] = ["", "万", "亿", "万亿"];

/// 是否为纯数字（整数或小数，允许尾部点号，不允许多点/点开头）。
fn is_decimal_number(s: &str) -> bool {
    if s.is_empty() || !s.as_bytes()[0].is_ascii_digit() {
        return false;
    }
    let mut dots = 0;
    for ch in s.bytes() {
        if ch == b'.' {
            dots += 1;
            if dots > 1 {
                return false;
            }
        } else if !ch.is_ascii_digit() {
            return false;
        }
    }
    true
}

/// "123.45" → ("123","45")，"123" → ("123","")
fn split_decimal(s: &str) -> (&str, &str) {
    match s.find('.') {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, ""),
    }
}

fn needs_leading_zero(group: &str) -> bool {
    group.len() < 4 || group.as_bytes()[0] == b'0'
}

fn group_to_chinese(group: &str, digits: &[&str; 10], units: &[&str; 4]) -> String {
    let mut result = String::new();
    let mut all_zero = true;
    let mut prev_zero = false;
    let length = group.len();
    for (i, b) in group.bytes().enumerate() {
        let d = (b - b'0') as usize;
        let unit_idx = length - 1 - i;
        if d == 0 {
            prev_zero = true;
            continue;
        }
        all_zero = false;
        if prev_zero && !result.is_empty() {
            result.push_str(digits[0]);
        }
        prev_zero = false;
        result.push_str(digits[d]);
        if unit_idx < units.len() {
            result.push_str(units[unit_idx]);
        }
    }
    if all_zero { String::new() } else { result }
}

/// 数字串 → 中文（按每 4 位一组：个/万/亿/万亿）
fn number_to_chinese(num: &str, digits: &[&str; 10], units: &[&str; 4]) -> String {
    let num = num.trim_start_matches('0');
    if num.is_empty() {
        return digits[0].to_string();
    }
    // 从右往左切 4 位一组
    let mut groups: Vec<&str> = Vec::new();
    let mut end = num.len();
    while end > 0 {
        let start = end.saturating_sub(4);
        groups.push(&num[start..end]);
        end = start;
    }
    let mut result = String::new();
    for i in (0..groups.len()).rev() {
        let group_str = groups[i];
        let group_text = group_to_chinese(group_str, digits, units);
        if group_text.is_empty() {
            continue;
        }
        if !result.is_empty() && needs_leading_zero(group_str) {
            result.push_str(digits[0]);
        }
        result.push_str(&group_text);
        if i < GROUP_UNITS.len() {
            result.push_str(GROUP_UNITS[i]);
        }
    }
    if result.is_empty() {
        digits[0].to_string()
    } else {
        result
    }
}

/// 大写金额（《会计基础工作规范》第五十二条）：整数到「元」写「整」。
fn number_to_amount(num: &str) -> String {
    format!(
        "{}元整",
        number_to_chinese(num, &UPPER_DIGITS, &UPPER_UNITS)
    )
}

/// 带角分的大写金额（≤2 位小数）；超 2 位返回空串。
///
/// 「整」的写法遵规范：到元、到角写「整」，到分不写。
fn decimal_to_amount(int_part: &str, dec_part: &str) -> String {
    let int_text = number_to_chinese(int_part, &UPPER_DIGITS, &UPPER_UNITS);
    if dec_part.is_empty() {
        return format!("{}元整", int_text);
    }
    if dec_part.len() > 2 {
        return String::new();
    }
    let jiao = (dec_part.as_bytes()[0] - b'0') as usize;
    let fen = if dec_part.len() == 2 {
        (dec_part.as_bytes()[1] - b'0') as usize
    } else {
        0
    };
    if jiao == 0 && fen == 0 {
        return format!("{}元整", int_text);
    }
    let mut b = format!("{}元", int_text);
    if jiao == 0 {
        b.push('零');
        b.push_str(UPPER_DIGITS[fen]);
        b.push('分');
    } else if fen == 0 {
        b.push_str(UPPER_DIGITS[jiao]);
        b.push_str("角整");
    } else {
        b.push_str(UPPER_DIGITS[jiao]);
        b.push('角');
        b.push_str(UPPER_DIGITS[fen]);
        b.push('分');
    }
    b
}

/// 中文小数读法："123","456" → "一百二十三点四五六"
fn decimal_to_chinese_text(int_part: &str, dec_part: &str, upper: bool) -> String {
    let int_text = if upper {
        number_to_chinese(int_part, &UPPER_DIGITS, &UPPER_UNITS)
    } else {
        number_to_chinese(int_part, &LOWER_DIGITS, &LOWER_UNITS)
    };
    if dec_part.is_empty() {
        return int_text;
    }
    let digits = if upper { &UPPER_DIGITS } else { &LOWER_DIGITS };
    let mut b = int_text;
    b.push('点');
    for ch in dec_part.bytes() {
        if ch.is_ascii_digit() {
            b.push_str(digits[(ch - b'0') as usize]);
        }
    }
    b
}

/// 逐位中文（含小数点）："123" → "一二三"，"2026" → "二〇二六"
///
/// 用 [`CHAR_DIGITS`]：这里是逐位改写，0 是「〇」而非位值读法的「零」。
fn digits_to_chinese_chars(num: &str) -> String {
    let mut b = String::new();
    for ch in num.chars() {
        if ch.is_ascii_digit() {
            b.push_str(CHAR_DIGITS[(ch as u8 - b'0') as usize]);
        } else if ch == '.' {
            b.push('点');
        }
    }
    if b.is_empty() {
        CHAR_DIGITS[0].to_string()
    } else {
        b
    }
}

/// 千分位分组："1234567" → "1,234,567"；小数部分不分组（GB/T 15835）。
fn format_thousands(int_part: &str, dec_part: &str) -> String {
    let grouped = if int_part.len() <= 3 {
        int_part.to_string()
    } else {
        let mut b = String::new();
        let remainder = int_part.len() % 3;
        if remainder > 0 {
            b.push_str(&int_part[..remainder]);
        }
        let mut i = remainder;
        while i < int_part.len() {
            if !b.is_empty() {
                b.push(',');
            }
            b.push_str(&int_part[i..i + 3]);
            i += 3;
        }
        b
    };
    if dec_part.is_empty() {
        grouped
    } else {
        format!("{}.{}", grouped, dec_part)
    }
}

/// 数字来源的取值：纯数字直接用；**算式先求值再转**，使「算完顺手要金额」一步到位
/// （`123*4` 也能出「肆佰玖拾贰元整」）。负数结果无金额读法，返回 None。
fn number_subject(buffer: &str, decimal_places: i32) -> Option<String> {
    let s = trim_pending_tail(buffer);
    if is_decimal_number(s) && !has_second_dot(buffer) {
        return Some(s.to_string());
    }
    if !has_binary_operator(s) || !is_expr_charset(s) {
        return None;
    }
    let val = evaluate_expression(s).filter(|v| v.is_finite())?;
    if val < 0.0 {
        return None;
    }
    let text = format_calc_result_prec(val, decimal_places);
    is_decimal_number(&text).then_some(text)
}

/// 数字来源：金额、中文数字、千分位。
///
/// 格式集按规范精简，**不产出**：
/// - 「一百二十三元整」——财务金额只有「大写壹佰贰拾叁元整」与「小写 ¥123.00」两种合法写法，
///   中文小写加「元整」不属任何规范；
/// - 逐位大写「壹贰叁」——逐位读法用于念号码，与财务大写无关，无使用场景。
pub fn generate_number_candidates(s: &str, decimal_places: i32) -> Vec<String> {
    texts(render_number(
        s,
        decimal_places,
        builtin_table(),
        &FormatAdjustMap::new(),
        None,
    ))
}

/// 「>2 位小数无角分写法」这类条件不再写在这里：`$AMT` 此时渲染为空串，由 [`render`] 丢弃。
fn render_number(
    s: &str,
    decimal_places: i32,
    table: &FormatTable,
    adjust: &FormatAdjustMap,
    eval: Option<ExprEval>,
) -> Vec<Rendered> {
    let Some(subject) = number_subject(s, decimal_places) else {
        return Vec::new();
    };
    render(table, adjust, &QuickValues::Number { subject }, eval)
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
