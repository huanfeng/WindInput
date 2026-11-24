//! `data/` 下程序写出的文本文件的第一行说明（设计 `user-dict-libraries-and-external-edit.md` §5）。
//! 文案只在这张表里；说明行不参与解析，整文件重写时按当前文案生成；按行补丁时只给直接以 `---`
//! 开头的文件补上，第一行是注释（含用户自己的）或带 BOM 的原样保留。
//! 用户目录根的 `meta.toml` 与字符类文件由 wind-config 渲染，文案在那边就近存放（字符类的那行这里
//! 另存一份 [`CHARSETS`]，供启动时补说明行用）。

pub const USER_DICT: &str =
    "# 清风输入法 · 用户词库：造的词、加的词都在这里。可手动编辑；文件头（--- 到 ...）请勿修改。";
pub const USER_DICT_LIB: &str = "# 清风输入法 · 用户分库：自己整理的词表，可按方案开关。可手动编辑；文件头（--- 到 ...）请勿修改。";
pub const USER_DICTS_META: &str = "# 清风输入法 · 用户分库的开关（按方案）。可手动编辑。";
pub const SHADOW: &str =
    "# 清风输入法 · 候选调整（置顶 / 删除）。可手动编辑；文件头（--- 到 ...）请勿修改。";
pub const TEMP: &str = "# 清风输入法 · 临时词库：自动学到、尚未转正的词。可在设置里用「编辑文件」修改，编辑期间新学到的内容会在完成后合并；文件头（--- 到 ...）请勿修改。";
pub const DRAFT: &str =
    "# 清风输入法 · 造词草稿：自动组出、尚未选用的词。由程序自动维护，请勿手动修改。";
pub const FREQ: &str = "# 清风输入法 · 词频：候选被选用的次数与时间。可在设置里用「编辑文件」修改，编辑期间新学到的内容会在完成后合并；文件头（--- 到 ...）请勿修改。";
pub const ASSOC: &str = "# 清风输入法 · 联想历史。由程序自动维护，请勿手动修改。";
pub const PHRASES: &str =
    "# 清风输入法 · 短语：用户短语与对系统短语的修改。可手动编辑；文件头（--- 到 ...）请勿修改。";
pub const COMPLETION: &str =
    "# 清风输入法 · 输入补全历史（邮箱后缀、网址等）。由程序自动维护，请勿手动修改。";
pub const QUICK_FORMAT: &str =
    "# 清风输入法 · 快捷输入的个人调整（调序、停用、自定义条目）。可手动编辑。";
pub const COMMON_CHARS: &str = "# 清风输入法 · 常用字 / 生僻字的个人调整。可手动编辑。";
/// 字符类的用户层文件（`charsets/<key>.yaml`）。由 wind-config 渲染，那边同名常量须与这里一致。
pub const CHARSETS: &str = "# 清风输入法 · 字符类的个人调整。可手动编辑。";
pub const STATS: &str = "# 清风输入法 · 输入统计。由程序自动维护，请勿手动修改。";
pub const PHRASE_SYS_HASH: &str = "# 清风输入法 · 系统短语同步标记。由程序自动维护，请勿手动修改。";

/// 表格文本按头部 `wind: kind` 取说明行；不认识的种类 ⇒ None（不写说明行）。
pub fn for_kind(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "words" => USER_DICT,
        "shadow" => SHADOW,
        "phrases" => PHRASES,
        "temp" => TEMP,
        "draft" => DRAFT,
        "freq" => FREQ,
        "assoc" => ASSOC,
        "completion" => COMPLETION,
        _ => return None,
    })
}

/// 按行补丁的输入：第一行恰为 `---`（无 BOM）⇒ 在最前面补上 `line`，行尾随第一行；其余（第一行是
/// 注释、带 BOM、`line` 为 None）原样借出。
pub fn prepend_if_bare<'a>(bytes: &'a [u8], line: Option<&str>) -> std::borrow::Cow<'a, [u8]> {
    let nl = if bytes.starts_with(b"---\n") {
        "\n"
    } else if bytes.starts_with(b"---\r\n") {
        "\r\n"
    } else {
        return bytes.into();
    };
    match line {
        Some(l) => [l.as_bytes(), nl.as_bytes(), bytes].concat().into(),
        None => bytes.into(),
    }
}

#[cfg(test)]
#[path = "banner_tests.rs"]
mod tests;
