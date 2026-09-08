//! AST 节点定义
//!
//! 对照 Go `wind_input/internal/cmdbar/ast/ast.go`，并做两处优化：
//! - `Modifiers` 用有序 `Vec`（非随机迭代的 map），debug 可复现、保留源顺序；
//! - options bag 在解析期直接投影为 [`ModValue`]，免去 Go 的二次 evalModifierLiteral。
//!
//! 语法见 docs/redesign（对应 Go docs/design/command-bar-design.md §2）。

use std::fmt;

/// 修饰符字面量值（options bag 的 value 域，解析期已静态投影）。
/// 对应 Go objectLitToMap 的 `any`（string / float64 / bool / 裸 ident 符号）。
#[derive(Debug, Clone, PartialEq)]
pub enum ModValue {
    Str(String),
    Num(f64),
    Bool(bool),
    /// 裸 ident 当作宿主自定义枚举符号（如 `expand: exact`）。
    Sym(String),
}

impl fmt::Display for ModValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModValue::Str(s) => write!(f, "{s:?}"),
            ModValue::Num(n) => write!(f, "{}", fmt_number(*n)),
            ModValue::Bool(b) => write!(f, "{b}"),
            ModValue::Sym(s) => write!(f, "{s}"),
        }
    }
}

impl ModValue {
    /// 取布尔语义：`Bool(b)` 直返；`Sym("true"/"false")` 容错；其余 None。
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            ModValue::Bool(b) => Some(*b),
            ModValue::Sym(s) if s == "true" => Some(true),
            ModValue::Sym(s) if s == "false" => Some(false),
            _ => None,
        }
    }

    /// 取字符串语义（Str/Sym 原样，数字格式化，bool→"true"/"false"）。
    pub fn as_str_value(&self) -> String {
        match self {
            ModValue::Str(s) | ModValue::Sym(s) => s.clone(),
            ModValue::Num(n) => fmt_number(*n),
            ModValue::Bool(b) => b.to_string(),
        }
    }
}

/// 动作链中途失败后的策略（`$CC(…, {on_error: "stop"})`）。
///
/// 默认 `Continue` 是**历史行为**，不能改：既有词条里 `type()` + `key.tap()` 这类组合
/// 依赖"前一步失败后一步照跑"（如剪贴板被占用时仍要把光标移回去）。
/// 需要"前一步成功才做下一步"的场景（`wind.cli(…)` 后接 `ui.toast("成功")`）显式写
/// `{on_error: "stop"}`——否则那句"成功"在失败时照弹，制造假成功。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnError {
    /// 记下第一个错误，余下动作继续跑（默认）。
    #[default]
    Continue,
    /// 首个错误即停，后续动作一概不执行。
    Stop,
}

/// 有序修饰符表（保留源顺序；重复键 last-write-wins 经 [`Modifiers::get`] 反向查找实现）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Modifiers(pub Vec<(String, ModValue)>);

impl Modifiers {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 按 last-write-wins 取键（反向查找最后一次写入）。
    pub fn get(&self, key: &str) -> Option<&ModValue> {
        self.0.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.0.iter().any(|(k, _)| k == key)
    }

    /// 追加一对（保留顺序；查询侧 last-write-wins）。
    pub fn push(&mut self, key: impl Into<String>, val: ModValue) {
        self.0.push((key.into(), val));
    }

    /// 合并：`defaults` 为基底，`explicit` 覆盖（追加在后，get 反向查找即覆盖）。
    pub fn merge(defaults: Modifiers, explicit: Modifiers) -> Modifiers {
        let mut out = defaults;
        out.0.extend(explicit.0);
        out
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(ModValue::as_bool)
    }
}

/// 字符串字面量的一个片段：纯文本或 `{expr}` 插值。
#[derive(Debug, Clone, PartialEq)]
pub enum StringPart {
    Text(String),
    Interp(Box<Expr>),
}

/// 表达式节点。
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// 字符串字面量（可含 `{expr}` 插值片段）。
    StringLit(Vec<StringPart>),
    /// 数字字面量（保留原始词素 `raw` 以便原样回显，对齐 Go NumberLit.Raw）。
    Number { value: f64, raw: String },
    /// 裸标识符（语义等价零参调用）。
    Ident(String),
    /// 函数调用；`name` 可含单个 `.` 作 namespace（如 `clip.copy`）。
    ///
    /// `named` 是具名参数（`cwd="D:/d"`），保留源顺序、值为完整表达式（可含插值）。
    /// 位置参数与具名参数分开存放：位置参数是**变参**（`proc.run` 是 `(1, -1)`），
    /// 若把具名参数混进 `args`，末位到底是第 N 个参数还是一个选项就永远二义。
    Call {
        name: String,
        args: Vec<Expr>,
        named: Vec<(String, Expr)>,
    },
    /// options bag `{k: v, ...}`（仅作为 marker 调用的末参出现）。
    Object(Vec<(String, ModValue)>),
    /// 嵌入的 `$CC(...)`（仅出现在 `$SS` 元素位）。
    Command(Box<CommandPhrase>),
}

/// `$CC(display, action..., {modifiers})`。
#[derive(Debug, Clone, PartialEq)]
pub struct CommandPhrase {
    pub display: Expr,
    pub actions: Vec<Expr>,
    pub modifiers: Modifiers,
}

/// `$SS("name", elem..., {modifiers})`，元素为 StringLit 或嵌入 CommandPhrase。
#[derive(Debug, Clone, PartialEq)]
pub struct ArrayPhrase {
    pub name: String,
    pub elements: Vec<Expr>,
    pub modifiers: Modifiers,
}

/// 顶层短语。
#[derive(Debug, Clone, PartialEq)]
pub enum Phrase {
    /// 无插值、无 marker 的纯文本。
    Literal(String),
    /// 含 `{expr}` 插值但无 marker（内为 StringLit）。
    Template(Expr),
    /// `$CC(...)` 命令短语。
    Command(CommandPhrase),
    /// `$SS(...)` 数组短语。
    Array(ArrayPhrase),
}

/// 把数字格式化为无多余 `.0` 的形式（整值走整数，否则最短浮点）。
pub(crate) fn fmt_number(n: f64) -> String {
    if n.is_finite() && n == n.trunc() && n.abs() < 1e16 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

// ───────────────────────── Display（可往返调试输出）─────────────────────────

impl fmt::Display for StringPart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StringPart::Text(t) => {
                // 重新转义特殊字符，便于往返。
                for ch in t.chars() {
                    match ch {
                        '\\' => write!(f, "\\\\")?,
                        '"' => write!(f, "\\\"")?,
                        '{' => write!(f, "\\{{")?,
                        '}' => write!(f, "\\}}")?,
                        _ => write!(f, "{ch}")?,
                    }
                }
                Ok(())
            }
            StringPart::Interp(e) => write!(f, "{{{e}}}"),
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::StringLit(parts) => {
                write!(f, "\"")?;
                for p in parts {
                    write!(f, "{p}")?;
                }
                write!(f, "\"")
            }
            Expr::Number { raw, value } => {
                if raw.is_empty() {
                    write!(f, "{}", fmt_number(*value))
                } else {
                    write!(f, "{raw}")
                }
            }
            Expr::Ident(name) => write!(f, "{name}"),
            Expr::Call { name, args, named } => {
                write!(f, "{name}(")?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{a}")?;
                }
                // 具名参数恒排在位置参数之后（与解析期规则一致），故可直接续写。
                for (i, (k, v)) in named.iter().enumerate() {
                    if i > 0 || !args.is_empty() {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}={v}")?;
                }
                write!(f, ")")
            }
            Expr::Object(pairs) => write_object(f, pairs),
            Expr::Command(c) => write!(f, "{c}"),
        }
    }
}

fn write_object(f: &mut fmt::Formatter<'_>, pairs: &[(String, ModValue)]) -> fmt::Result {
    write!(f, "{{")?;
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            write!(f, ", ")?;
        }
        write!(f, "{k}: {v}")?;
    }
    write!(f, "}}")
}

fn write_modifiers(f: &mut fmt::Formatter<'_>, m: &Modifiers) -> fmt::Result {
    if m.is_empty() {
        return Ok(());
    }
    write!(f, ", {{")?;
    for (i, (k, v)) in m.0.iter().enumerate() {
        if i > 0 {
            write!(f, ", ")?;
        }
        write!(f, "{k}: {v}")?;
    }
    write!(f, "}}")
}

impl fmt::Display for CommandPhrase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "$CC({}", self.display)?;
        for a in &self.actions {
            write!(f, ", {a}")?;
        }
        write_modifiers(f, &self.modifiers)?;
        write!(f, ")")
    }
}

impl fmt::Display for ArrayPhrase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "$SS(\"{}\"", self.name)?;
        for e in &self.elements {
            write!(f, ", {e}")?;
        }
        write_modifiers(f, &self.modifiers)?;
        write!(f, ")")
    }
}

impl fmt::Display for Phrase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Phrase::Literal(t) => write!(f, "{t}"),
            Phrase::Template(e) => write!(f, "{e}"),
            Phrase::Command(c) => write!(f, "{c}"),
            Phrase::Array(a) => write!(f, "{a}"),
        }
    }
}
