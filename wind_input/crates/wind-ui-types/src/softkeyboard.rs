//! 软键盘面板的表现层载荷。
//!
//! 面板本身是渲染端的事，这里只描述「画什么」：一排面名 + 当前面的键位表。
//!
//! ★ **只下发当前面**，不是全部 13 面。切面时重发一次即可，而重发的那一刻本来就要
//! 重绘整块面板；一次性推全部则要搬 1200 多个键位，其中 92% 当场用不上。

/// 面板上的一个键位。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoftKeyCap {
    /// 键位名（`grave` / `q` / `comma` …）。
    ///
    /// 它同时是**键帽角标要显示的物理键**与**点击回送时的标识**，两者共用一个名字是
    /// 刻意的：面板画在哪个键上、点击算作哪个键，必须由同一个值决定，否则会出现
    /// 「点 A 出了 B 的符号」这类只在个别键位上复现的错位。
    pub slot: String,
    /// 基础层输出。空串 = 空键位（键帽画成灰的，按下吃掉忽略）。
    pub base: String,
    /// 第二层（按住 Shift）输出。空串 = 该层无映射。
    pub shift: String,
}

impl SoftKeyCap {
    /// 取指定层的输出；空键位给 `None`。
    pub fn output(&self, shift: bool) -> Option<&str> {
        let s = if shift { &self.shift } else { &self.base };
        if s.is_empty() { None } else { Some(s.as_str()) }
    }
}

/// 命中 tag 基址：面板上的非键位控件。
///
/// 键位用它在 `keys` 里的下标当 tag（0..47），故这些基址必须远高于键位数——
/// 与候选窗翻页器用 `HOVER_PAGE_PREV` 那套同构。
pub const SOFT_KEY_TAG_BASE: i32 = 0;
/// 标签行第 n 个面：`SOFT_TAG_PAGE_BASE + n`。
pub const SOFT_TAG_PAGE_BASE: i32 = 200_000;
/// 关闭按钮。
pub const SOFT_TAG_CLOSE: i32 = 300_000;
/// 特殊键第 n 个：`SOFT_TAG_FN_BASE + n`（n 是 [`SOFT_FN_KEYS`] 的下标）。
pub const SOFT_TAG_FN_BASE: i32 = 400_000;

/// 面板上可点的特殊键：`(键名, 显示文字)`。
///
/// 键名取 `wind_keys::key_inject` 认得的那套——点击时协调器按这个名字合成真实按键。
/// **面板控制键（Shift / Esc / 翻页 / 面名）不在此列**：它们不合成按键，各有各的语义。
pub const SOFT_FN_KEYS: &[(&str, &str)] = &[
    ("backspace", "⌫"),
    ("tab", "Tab"),
    ("enter", "Enter"),
    ("space", ""),
    ("ins", "Ins"),
    ("del", "Del"),
];
