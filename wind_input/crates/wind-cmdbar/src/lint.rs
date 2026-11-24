//! 短语文本的静态检查（导入预览用）。
//!
//! 只做一件事：**指出很可能是笔误的写法**。不判断短语「危不危险」——命令直通车本来
//! 就是短语的主要用途，而短语不会自行执行，要触发得先打出它的编码、再从候选里选中。
//! 把常态当异常来设门槛，只会让每次导入都多两步无意义的确认。
//!
//! 只认 AST，不做文本匹配：`text.contains("proc.run")` 那种判定既会被字符串拼接绕过，
//! 也会把正文里恰好提到函数名的短语误判。

use crate::ast::{CommandPhrase, Expr, Phrase, StringPart};
use crate::error::Result;
use crate::parser::{is_cmdbar_grammar, parse};

/// 一处疑似笔误。**不阻止导入**，只在预览里提一句。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hint {
    /// 路径参数里混进了控制字符（换行 / 制表 / 回车）。
    ///
    /// 路径里不可能有这些，出现即几乎可以断定是**反斜杠写成了单个**：按约定一个字面
    /// `\` 要写两个，写一个时 `\n` `\t` `\r` 会被 lexer 当成转义吃掉，`D:` 加 notes
    /// 目录就成了「`D:` + 换行 + `otes`」。载荷是函数名。
    ///
    /// 之所以判「解析后含控制字符」而不是「源码里有孤立反斜杠」：AST 拿到的字符串
    /// 已经解析完毕，`\\` 与 `\n` 在那里都只剩结果，源码写法无从还原。而结果里的
    /// 控制字符恰好是这个错误唯一的、确定的指纹。
    ControlCharInPath(String),
}

/// 目标参数是「外部程序或 URL」的函数——只对它们查路径笔误。
///
/// `key.type` / `clip.copy` 的参数是要输入的文本，里面出现换行是正常用法
/// （`key.type` 会把它发成回车键），一起查会让提示泛滥到没人看。
const TARGET_SENSITIVE: &[&str] = &["proc.run", "proc.any", "proc.shell", "open", "wind.cli"];

/// 检查一条短语文本。
///
/// 返回 `Err` 表示**语法不对**——这样的条目导入端一律不装：装进去只会在触发时失败，
/// 而失败点离导入很远，用户无从关联。
pub fn lint_phrase(text: &str) -> Result<Vec<Hint>> {
    if !is_cmdbar_grammar(text) {
        return Ok(Vec::new());
    }
    let phrase = parse(text)?;
    Ok(lint_parsed(&phrase))
}

/// 已解析短语的检查（调用方已有 AST 时免去重复解析）。
pub fn lint_parsed(phrase: &Phrase) -> Vec<Hint> {
    let mut calls = Vec::new();
    match phrase {
        Phrase::Literal(_) => {}
        Phrase::Template(e) => walk_expr(e, &mut calls),
        Phrase::Command(c) => walk_command(c, &mut calls),
        Phrase::Array(a) => {
            for e in &a.elements {
                walk_expr(e, &mut calls);
            }
        }
    }

    let mut out: Vec<Hint> = Vec::new();
    for call in &calls {
        if !TARGET_SENSITIVE.contains(&call.name.as_str()) {
            continue;
        }
        let target_bad = call
            .first_arg_static
            .as_deref()
            .is_some_and(has_control_char);
        let cwd_bad = call
            .named
            .iter()
            .any(|(k, v)| k == "cwd" && v.as_deref().is_some_and(has_control_char));
        if target_bad || cwd_bad {
            let hint = Hint::ControlCharInPath(call.name.clone());
            if !out.contains(&hint) {
                out.push(hint);
            }
        }
    }
    out
}

/// 用 `char::is_control` 而不是逐个列举换行/制表/回车：漏列一个就是漏报，而路径里
/// 出现**任何**控制字符都同样可疑。
fn has_control_char(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// AST 里的一处函数调用，投影成检查需要的最小信息。
struct CallSite {
    name: String,
    /// 第一个位置参数的静态字符串值；`None` = 无参或含插值（求值前不可知）。
    first_arg_static: Option<String>,
    /// 具名参数的静态值，`None` = 含插值。
    named: Vec<(String, Option<String>)>,
}

/// 字面字符串值；含 `{expr}` 插值即返回 `None`——插值的结果要到运行时才知道，
/// 拿插值前的片段冒充完整值只会误报。
fn static_str_of(e: &Expr) -> Option<String> {
    match e {
        Expr::StringLit(parts) => {
            let mut s = String::new();
            for p in parts {
                match p {
                    StringPart::Text(t) => s.push_str(t),
                    StringPart::Interp(_) => return None,
                }
            }
            Some(s)
        }
        _ => None,
    }
}

fn walk_command(c: &CommandPhrase, out: &mut Vec<CallSite>) {
    walk_expr(&c.display, out);
    for a in &c.actions {
        walk_expr(a, out);
    }
}

fn walk_expr(e: &Expr, out: &mut Vec<CallSite>) {
    match e {
        // 插值片段里的表达式同样是调用点——`"{sub(reverse(last(1)), 1, 1)}"` 这种
        // 嵌在 display 字符串里的调用漏掉，检查就成了摆设。
        Expr::StringLit(parts) => {
            for p in parts {
                if let StringPart::Interp(inner) = p {
                    walk_expr(inner, out);
                }
            }
        }
        Expr::Number { .. } | Expr::Object(_) => {}
        // 裸标识符语义等价零参调用（`clip.paste`），无参数可查但仍走一遍。
        Expr::Ident(name) => out.push(CallSite {
            name: name.clone(),
            first_arg_static: None,
            named: Vec::new(),
        }),
        Expr::Call { name, args, named } => {
            out.push(CallSite {
                name: name.clone(),
                first_arg_static: args.first().and_then(static_str_of),
                named: named
                    .iter()
                    .map(|(k, v)| (k.clone(), static_str_of(v)))
                    .collect(),
            });
            for a in args {
                walk_expr(a, out);
            }
            for (_, v) in named {
                walk_expr(v, out);
            }
        }
        Expr::Command(c) => walk_command(c, out),
    }
}

#[cfg(test)]
#[path = "lint_tests.rs"]
mod tests;
