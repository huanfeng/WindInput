//! `$` 模板引擎：`$name` / `${name}` / `$$` 转义。
//!
//! 两个消费者共用同一份解析：快捷输入的格式表（`system.quick.toml`）与短语层的简单模板
//! （`system.phrases.toml` 的 `$Y年$M月$D日`）。变量名的解析规则若在两个文件里不一致，
//! 「用户学一次」的前提就不成立——故本模块只管**怎么认出变量**，取值交给各自的 resolver。
//!
//! 原实现在 `wind-phrase`，随快捷输入格式表下沉至此（本 crate 零 crate 依赖，是两者的下游）。

/// 展开模板。`resolve` 返回 `None` 表示该变量不受支持 → 整条模板作废（返回 `None`）。
///
/// 整条作废而非留空：模板里出现未知变量说明这条配置本身写错了，让它带着窟窿上屏
/// （`2025年月日`）比不出这条候选更糟。调用方据此跳过该条并告警。
///
/// - `$$` → 字面 `$`
/// - `${name}` → 显式定界，`name` 可含任意非 `}` 字符；缺右括号 → 整条作废
/// - `$name` → `name` 取连续 ASCII 字母（**不含数字**，故 `$Y2` 是 `$Y` 后跟字面 `2`）
/// - 孤立的 `$`（后面不是字母/`{`/`$`）→ 原样输出
pub fn expand<F>(text: &str, resolve: F) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if bytes[i] == b'$' {
            // $$ → 字面 $
            if i + 1 < text.len() && bytes[i + 1] == b'$' {
                out.push('$');
                i += 2;
                continue;
            }
            // ${name} 或 $name
            let (name, next) = if i + 1 < text.len() && bytes[i + 1] == b'{' {
                let rel = text[i + 2..].find('}')?;
                let close = i + 2 + rel;
                (&text[i + 2..close], close + 1)
            } else {
                let start = i + 1;
                let mut j = start;
                while j < text.len() && bytes[j].is_ascii_alphabetic() {
                    j += 1;
                }
                if j == start {
                    // 孤立的 $，原样输出
                    out.push('$');
                    i += 1;
                    continue;
                }
                (&text[start..j], j)
            };
            let val = resolve(name)?;
            out.push_str(&val);
            i = next;
        } else {
            // 拷贝一个 UTF-8 字符
            let len = utf8_len(bytes[i]);
            out.push_str(&text[i..i + len]);
            i += len;
        }
    }
    Some(out)
}

/// 是否含至少一个可展开的变量引用（`$name` / `${name}`）。
///
/// `$$` 与孤立 `$` 不算——它们展开后是字面量，没有取值行为。用于把「纯字面模板」
/// 与「真模板」分开：前者无论 resolver 给不给值都恒成功。
pub fn has_variable(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < text.len() {
        if bytes[i] == b'$' {
            if i + 1 < text.len() && bytes[i + 1] == b'$' {
                i += 2;
                continue;
            }
            if i + 1 < text.len() && (bytes[i + 1] == b'{' || bytes[i + 1].is_ascii_alphabetic()) {
                return true;
            }
        }
        i += utf8_len(bytes[i]);
    }
    false
}

/// 是否含**裸** `{`——即不属于 `${...}` 定界的那种。
///
/// 这是「变量模板」与「表达式模板」的分流判据：`${Y}` 里的 `{` 属于变量语法，
/// 而 `{year()}` 里的是表达式。两者不能在同一条模板里混用（混用会被加载期拒绝），
/// 故一个布尔就够分流。
pub fn has_bare_brace(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < text.len() {
        match bytes[i] {
            // $$ 是转义，跳过整对，免得把 `$${x}` 里的 `{` 误判为变量定界
            b'$' if i + 1 < text.len() && bytes[i + 1] == b'$' => i += 2,
            // ${...}：整段跳过（含右括号）
            b'$' if i + 1 < text.len() && bytes[i + 1] == b'{' => {
                match text[i + 2..].find('}') {
                    Some(rel) => i = i + 2 + rel + 1,
                    // 未闭合的 `${`：交给 expand 去报模板语法错，这里不当作裸括号
                    None => return false,
                }
            }
            b'{' => return true,
            _ => i += utf8_len(bytes[i]),
        }
    }
    false
}

fn utf8_len(lead: u8) -> usize {
    if lead < 0x80 {
        1
    } else if lead < 0xE0 {
        2
    } else if lead < 0xF0 {
        3
    } else {
        4
    }
}

#[cfg(test)]
#[path = "template_tests.rs"]
mod tests;
