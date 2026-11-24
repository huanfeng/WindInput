//! 外来文本进入解析器前的统一规整。
//!
//! 「外来」指的是不由我们生成的文本：用户导入的词库、手写的辅助码表 / 常用字表、从别处搬来的
//! 方案文件。编辑器、操作系统与 git `core.autocrlf` 的差异不该漏进每一个解析器各自去对付。

use std::borrow::Cow;

/// 把外来文本规整成可按行解析的形态：剥 UTF-8 BOM，把孤立 `\r` 折成 `\n`。
///
/// 全仓按行解析走 `str::lines()`，只认 `\n`，孤立 `\r` 会让整份文件变成一行，且各处症状都不提
/// 行尾：TSV 词库「导入成功、只进 1 条」，Rime `.dict.yaml` 被误判成 TSV，WindDict 报版本不支持，
/// 辅助码表 / 常用字表整份被当成注释而为空。
///
/// 不会吃掉用户数据：按行格式里裸 `\r` 无法表达内容（词条内换行走转义）。这与上屏换行
/// （`NewlineStyle`）是两回事，那边 CR 是有意义的字符。
///
/// `\r\n` 原样留着：`lines()` 本就正确处理它，改写要为最常见的 Windows 文件复制一整份
/// （20 万行 CRLF 词库：只认孤立 `\r` 约 2ms 借用，含 `\r` 就改写约 11ms 加一份分配）。
///
/// 重复调用是刻意的：每个公开入口各自再调一次（已规整的文本走快路），单独调用任何一个都得对。
pub fn normalize_input(text: &str) -> Cow<'_, str> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if !has_lone_cr(text) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\r' {
            out.push(c);
            continue;
        }
        // CRLF 折成一个换行，而不是两个
        if it.peek() == Some(&'\n') {
            it.next();
        }
        out.push('\n');
    }
    Cow::Owned(out)
}

/// 是否含孤立 `\r`（不被 `\n` 紧跟的）。
fn has_lone_cr(text: &str) -> bool {
    // 快路：`contains` 走标准库的向量化搜索，没有 `\r` 的文件到此为止（最常见的 LF）。
    if !text.contains('\r') {
        return false;
    }
    // 慢路：确实有 `\r` 了，逐个看它后面是不是 `\n`。纯 CRLF 会一路走到底返回 false，
    // 代价是一趟扫描，仍远低于无条件复制一份几 MB 的文本。
    let b = text.as_bytes();
    let mut i = 0;
    while let Some(off) = b[i..].iter().position(|&c| c == b'\r') {
        let p = i + off;
        if b.get(p + 1) != Some(&b'\n') {
            return true;
        }
        i = p + 2;
    }
    false
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
