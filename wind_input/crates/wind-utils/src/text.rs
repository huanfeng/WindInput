//! 外来文本进入解析器前的统一规整。
//!
//! 「外来」指的是**不由我们生成**的文本：用户导入的词库、手写的辅助码表 / 常用字表、
//! 从别处搬来的方案文件。它们经过的编辑器、操作系统、以及 git 的 `core.autocrlf`
//! 各不相同，这些差异不该漏进每一个解析器各自去对付。

use std::borrow::Cow;

/// 把外来文本规整成可按行解析的形态：剥 UTF-8 BOM，把**孤立** `\r` 折成 `\n`。
///
/// # 为什么需要
///
/// 全仓的按行解析走的都是 `str::lines()` 语义——只认 `\n`，顺带剥掉紧邻其前的 `\r`。
/// 孤立 `\r` 因此根本不算换行，整份文件会被当成**一行**。各处的表现还互不相同，
/// 且没有一处会提到行尾（2026-09-17 实测）：
///
/// - TSV / 纯词列表 → 提示**导入成功、只进 1 条**，`skipped` 还是 0 ← 最坏，用户看到的是成功
/// - Rime `.dict.yaml` → 全文一行必含 TAB，被误判成 TSV → 0 条
/// - WindDict → 报「不支持的 WindDict 版本（需 version: 1）」，把人引向去改 `version`
/// - 辅助码表 `.txt` → 带 `# name:` 头的（出厂文件都带）整份被当成一条注释 → 0 字
/// - 常用字表 `.txt` → 同上 → 空表
///
/// 这不是一条设计出来的限制，是 `lines()` 的语义漏出来的副产物。读用户文件的功能
/// 没有理由对行尾挑食。
///
/// # 不会吃掉用户数据
///
/// 按行格式里裸 `\r` 本来就无法用来表达内容——词条内的真换行走转义（wdict 的
/// `unescape_text_field`），单条加词更是显式拒绝含换行的文本。所以把裸 `\r` 一律
/// 当行尾处理，不存在把有意义的内容改掉的情况。
///
/// 这与**上屏**换行（`NewlineStyle`）是两回事：那边宿主语义不同、CR 是有意义的字符，
/// 不能这么干。区别在于这里读的是「按行组织的文件」，那里写的是「用户的正文」。
///
/// # 只改真正需要改的
///
/// `\r\n` 原样留着——`lines()` 本来就正确处理它，改写它既没有收益，又要为最常见的
/// Windows 文件复制一整份。实测 5.6MB / 20 万行的 CRLF 词库：
///
/// | 判据 | LF 5.4MB | CRLF 5.6MB |
/// | --- | --- | --- |
/// | 含 `\r` 就改写 | 0.40ms 借用 | **11.1ms + 分配一份** |
/// | 只认孤立 `\r`（当前） | 0.40ms 借用 | **2.04ms 借用** |
///
/// 这一点也与 `NewlineStyle` 的教训同向：能不动用户的字节就不动。
///
/// # 重复调用是刻意的
///
/// 上层入口归一化后往下传，下层入口各自再调一次（此时已无孤立 `\r`，走快路，
/// 约 0.4ms/5MB）。让每个公开入口自包含地保证行尾正确，比省下那几毫秒重要——
/// 单独调用其中任何一个也得是对的。
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

/// 是否含**孤立** `\r`（不被 `\n` 紧跟的）。
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
mod tests {
    use super::*;

    #[test]
    fn lf_only_is_borrowed() {
        let s = "a\nb\nc";
        assert!(matches!(normalize_input(s), Cow::Borrowed("a\nb\nc")));
    }

    #[test]
    fn no_newline_at_all_is_borrowed() {
        assert!(matches!(normalize_input("abc"), Cow::Borrowed("abc")));
    }

    #[test]
    fn bom_is_stripped() {
        assert_eq!(normalize_input("\u{feff}a\nb"), "a\nb");
    }

    #[test]
    fn crlf_is_left_alone_and_borrowed() {
        // `lines()` 本来就把 \r\n 当一个换行，没有理由为它复制一份
        let s = "a\r\nb\r\n";
        assert!(matches!(normalize_input(s), Cow::Borrowed(x) if x == s));
    }

    #[test]
    fn crlf_still_splits_into_the_same_lines_as_lf() {
        // 不改写不等于不生效：下游按 lines() 读到的东西必须一致
        let (a, b) = (normalize_input("a\r\nb\r\n"), normalize_input("a\nb\n"));
        assert_eq!(a.lines().collect::<Vec<_>>(), b.lines().collect::<Vec<_>>());
    }

    #[test]
    fn lone_cr_becomes_newline() {
        assert_eq!(normalize_input("a\rb\rc"), "a\nb\nc");
    }

    #[test]
    fn mixed_endings_all_become_lf() {
        // 只要存在一个孤立 \r 就整份改写：混排文件里 \r\n 一并折成 \n，
        // 结果仍是「每行一条」，不会因为 \r\n 被动过而多出空行
        assert_eq!(normalize_input("a\r\nb\rc\nd"), "a\nb\nc\nd");
    }

    #[test]
    fn trailing_lone_cr() {
        assert_eq!(normalize_input("a\r"), "a\n");
    }

    #[test]
    fn cr_then_crlf_are_two_lines_not_three() {
        // \r 后面跟的是 \r\n：前一个 \r 自己成行，后面的 \r\n 折成一个
        assert_eq!(normalize_input("a\r\r\nb"), "a\n\nb");
    }

    #[test]
    fn bom_with_cr_endings() {
        assert_eq!(normalize_input("\u{feff}a\rb"), "a\nb");
    }
}
