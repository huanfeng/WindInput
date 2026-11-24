//! 工具栏状态数据（由协调器推送，渲染端呈现）。

/// 工具栏的一个渲染项（`SetToolbarLayout` 的载荷元素，顺序即渲染顺序）。
///
/// 下发「项」而不是配置字符串：`ui.toolbar.items` 的取值是配置层的词汇，让渲染端也去认，
/// 非法值、留空的语义就得两边各答一次。协调器解析完再下发，渲染端只负责画。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolbarItem {
    /// 中英状态（含方案标签），高亮表示有效中文。
    Mode,
    /// 中/英标点。
    Punct,
    /// 全/半角。
    FullWidth,
    /// 简/繁（关着显「简」淡显，开着显「繁」高亮）。
    ///
    /// 显隐只归 `ui.toolbar.items` 管，别与 [`ToolbarState::s2t_shown`] 合取：它是简繁的唯一
    /// 鼠标入口，关着就不画会自锁。
    S2t,
    /// 繁入简出（恒显「简」，开着时高亮）。
    ///
    /// 与 [`Self::S2t`] 的轮换刻意不同构：若也轮换，两格都关时会一起显「简」分不出谁是谁。
    /// 恒显一个字、开合由格底表达，同软键盘格。本格出厂不显示（`-t2s`）。
    T2s,
    /// 软键盘（键盘图标，点击开关面板；开着时高亮）。
    SoftKeyboard,
    /// 设置（齿轮图标，点击弹主菜单）。
    Settings,
    /// 自定义按钮（`[[ui.toolbar.buttons]]` 的一项，经 `items` 里的 `custom:<id>` 引用）。
    ///
    /// 携带 `label`：渲染端读不到配置，下发的就是它需要的全部。`index` 只用于点击时回指，
    /// 见 [`crate::ToolbarAction::Custom`]。
    Custom {
        /// 该按钮在 `ui.toolbar.buttons` 里的下标。
        index: u8,
        /// 已按显示宽度截断过的格内文字（协调器截好再下发）。
        label: String,
    },
}

/// 默认渲染顺序：`ui.toolbar.items` 留空 / 解析后为空时用它，语义是全部显示。
///
/// 与 `wind_config::DEFAULT_TOOLBAR_SHOWN`（出厂排布）不同：那份答「出厂长什么样」，这份答
/// 「手写空列表该理解成什么」。与 `wind_config::TOOLBAR_ITEM_KEYS` 逐项对应但刻意各存一份，
/// 免得把配置 crate 拖进 headless / Android 的依赖图。
pub const DEFAULT_TOOLBAR_ITEMS: [ToolbarItem; 7] = [
    ToolbarItem::Mode,
    ToolbarItem::Punct,
    ToolbarItem::FullWidth,
    ToolbarItem::S2t,
    ToolbarItem::T2s,
    ToolbarItem::SoftKeyboard,
    ToolbarItem::Settings,
];

/// 工具栏状态（由协调器推送）
///
/// `PartialEq` 供协调器做推送去重（见 `notify_toolbar`）。用 derive 而不是手写比较：
/// 手写漏一个字段就是「改了状态工具栏不更新」，且没有任何报错。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolbarState {
    pub chinese_mode: bool,
    /// 有效显示标签：中文模式取方案 icon_label（如 "拼"/"五"），无则 "中"；
    /// 英文小写为 "英"，大写锁定为 "A"（由协调器预计算后填入）。
    pub icon_label: String,
    pub caps_lock: bool,
    pub full_width: bool,
    pub chinese_punct: bool,
    /// 简入繁出当前是否启用（格内显示 "繁" 并高亮）
    pub s2t_enabled: bool,
    /// 繁入简出当前是否启用（格内恒显 "简"，据此高亮）。
    ///
    /// 没有对应的 `t2s_shown`：`s2t_shown` 那个字段是给没有 `items` 机制的宿主
    /// （macOS 状态菜单 / 移动端）用的，而繁入简出按设计不进那两端的开关面板。
    pub t2s_enabled: bool,
    /// 是否该给出简繁开关（取值 = 简繁转换当前开着）。
    ///
    /// 桌面工具栏不读它（显隐归 `ui.toolbar.items`，见 [`ToolbarItem::S2t`]）。留给没有
    /// `items` 机制的宿主：macOS 状态菜单与移动端 `InputStatus`，改前先确认那两端。
    pub s2t_shown: bool,
    /// 软键盘面板是否开着（格子据此高亮）。
    pub soft_keyboard_on: bool,
    /// 当前打不出中文（密码框 / 焦点不在可编辑控件里 / 系统级禁用）：仅影响呈现
    /// （模式格显 "英" 且不高亮）。取值来自协调器的 `effective_input_block()`——
    /// 语言栏图标读的是同一个判定。
    ///
    /// 独立于 `icon_label` 而非直接改写它：后者是「当前方案标签」的单一语义，且会经
    /// StatusUpdate 写入 TSF 的 `_inputTypeLabel`（持久值），烧进临时态会让图标长期卡 "英"。
    pub input_blocked: bool,
}

impl Default for ToolbarState {
    fn default() -> Self {
        Self {
            chinese_mode: true,
            icon_label: "中".to_string(),
            caps_lock: false,
            full_width: false,
            chinese_punct: true,
            s2t_enabled: false,
            t2s_enabled: false,
            s2t_shown: false,
            soft_keyboard_on: false,
            input_blocked: false,
        }
    }
}
