//! 外来文本进入解析器前的统一规整：剥 BOM、抹平行尾差异。
//!
//! 导入端拿到的文件来自用户能想到的任何地方——别的输入法导出的、从网页复制另存的、
//! 老编辑器写的。这些来源差异不该漏进每一个解析器各自去对付。

use std::borrow::Cow;

/// 把外来文本规整成可按行解析的形态：剥 UTF-8 BOM，行尾一律折成 `\n`。
///
/// **为什么需要**：全仓的按行解析走的都是 `str::lines()` 语义——只认 `\n`，顺带剥掉
/// 紧邻其前的 `\r`。孤立 `\r` 因此根本不算换行，整份文件会被当成**一行**。各格式的
/// 表现还互不相同，且没有一处会提到行尾（2026-09-17 实测）：
///
/// - TSV / 纯词列表 → 提示**导入成功、只进 1 条**，`skipped` 还是 0 ← 最坏，用户看到的是成功
/// - Rime `.dict.yaml` → 全文一行必含 TAB，被误判成 TSV → 0 条
/// - WindDict → 报「不支持的 WindDict 版本（需 version: 1）」，把人引向去改 `version`
///
/// 这不是一条设计出来的限制，是 `lines()` 的语义漏出来的副产物。一个导入功能没有
/// 理由对行尾挑食。
///
/// **不会吃掉用户数据**：按行格式里裸 `\r` 本来就无法用来表达词条内容——词条内的真
/// 换行走转义（见 [`crate::wdict::unescape_text_field`]），单条加词更是显式拒绝含换行
/// 的文本（`handle_addword.rs`）。所以把裸 `\r` 一律当行尾处理，不存在把有意义的内容
/// 改掉的情况。这与「上屏换行风格」（`NewlineStyle`，宿主语义不同、CR 有意义）是两回事，
/// 那边不能这么干，这边可以。
///
/// **零拷贝**：不带 BOM 且不含 `\r` 时直接借用。用户词库动辄几 MB，不该为最常见的情况
/// 复制一份。含 `\r` 时也只扫一遍、只分配一次（`\r\n` 折成一个 `\n`）。
pub fn normalize_import_text(text: &str) -> Cow<'_, str> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if !text.contains('\r') {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lf_only_is_borrowed() {
        let s = "a\nb\nc";
        assert!(matches!(normalize_import_text(s), Cow::Borrowed("a\nb\nc")));
    }

    #[test]
    fn no_newline_at_all_is_borrowed() {
        assert!(matches!(normalize_import_text("abc"), Cow::Borrowed("abc")));
    }

    #[test]
    fn bom_is_stripped() {
        assert_eq!(normalize_import_text("\u{feff}a\nb"), "a\nb");
    }

    #[test]
    fn crlf_folds_to_one_newline() {
        assert_eq!(normalize_import_text("a\r\nb\r\n"), "a\nb\n");
    }

    #[test]
    fn lone_cr_becomes_newline() {
        assert_eq!(normalize_import_text("a\rb\rc"), "a\nb\nc");
    }

    #[test]
    fn mixed_endings_all_become_lf() {
        assert_eq!(normalize_import_text("a\r\nb\rc\nd"), "a\nb\nc\nd");
    }

    #[test]
    fn trailing_lone_cr() {
        assert_eq!(normalize_import_text("a\r"), "a\n");
    }

    #[test]
    fn cr_then_crlf_are_two_lines_not_three() {
        // \r 后面跟的是 \r\n：前一个 \r 自己成行，后面的 \r\n 折成一个
        assert_eq!(normalize_import_text("a\r\r\nb"), "a\n\nb");
    }

    #[test]
    fn bom_with_cr_endings() {
        assert_eq!(normalize_import_text("\u{feff}a\rb"), "a\nb");
    }
}
