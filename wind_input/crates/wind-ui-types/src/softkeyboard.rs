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
/// 面板上的 Shift 键。点击**锁定/解锁**第二层，是面板自己的状态，不回送协调器。
pub const SOFT_TAG_SHIFT: i32 = 310_000;
/// 底行的上一面 / 下一面键。
///
/// ★ **不能复用 `SOFT_TAG_PAGE_BASE + 目标下标`**：那样它与标签行里同一个面的标签
/// 撞成同一个 tag，鼠标悬停在翻页键上时对应的标签也会跟着亮——表现为「一次移动出现
/// 多处高亮」。tag 是命中标识，同一时刻只该有一个控件认领它。
pub const SOFT_TAG_PAGE_PREV: i32 = 320_000;
pub const SOFT_TAG_PAGE_NEXT: i32 = 320_001;
/// 标签行的左右滚动键（面多到一行放不下时才出现）。
pub const SOFT_TAG_TAB_LEFT: i32 = 330_000;
pub const SOFT_TAG_TAB_RIGHT: i32 = 330_001;

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
    // 大写锁定。用 `vk:0x14` 而不是名字——`parse_key` 的键名表里没有 capslock。
    //
    // ⛔ 这不违反「不拦截 CapsLock」那条禁令：禁的是**拦截物理键**（toggle 键的
    // keydown/keyup 处理有坑，「翻转再回敲复原」已被删除）。这里是用户点面板时我们
    // **主动敲一次**，与用户自己按下没有区别。物理 CapsLock 仍然完全不接管。
    ("vk:0x14", "Caps"),
];

/// [`SOFT_FN_KEYS`] 里大写锁定那一项的下标（面板要给它单独的高亮状态）。
pub const SOFT_FN_CAPS_INDEX: usize = 6;
