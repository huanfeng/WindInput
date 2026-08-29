//! 工具栏状态数据（由协调器推送，渲染端呈现）。

/// 工具栏的一个渲染项（`SetToolbarLayout` 的载荷元素，顺序即渲染顺序）。
///
/// # 为什么下发的是「项」而不是配置字符串
///
/// `ui.toolbar.items` 的取值（`"mode"` / `"punct"` / …）是**配置层的词汇**。渲染端读不到
/// 配置，让它去认这些字符串等于把配置语义复制一份到 UI 侧——非法值怎么办、留空是什么
/// 意思，两边各答一次就迟早不同步。协调器解析完再下发，UI 侧收到的已经是一份「照这个
/// 顺序画这些东西」的声明。
///
/// 同一条原则刚在 `wind-ui/src/toolbar.rs` 的 `mode_text` 上应用过（`[ui.labels]` 那次）：
/// 判据归协调器、取值也归协调器，渲染端只负责画。
///
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolbarItem {
    /// 中英状态（含方案标签），高亮表示有效中文。
    Mode,
    /// 中/英标点。
    Punct,
    /// 全/半角。
    FullWidth,
    /// 简/繁。⚠️ 与 [`ToolbarState::s2t_shown`] 是**合取**：本项只表示「用户允许它出现」，
    /// 简繁转换当前关着时照样不画。
    S2t,
    /// 软键盘（键盘图标，点击开关面板；开着时高亮）。
    SoftKeyboard,
    /// 设置（齿轮图标，点击弹主菜单）。
    Settings,
    /// 自定义按钮（`[[ui.toolbar.buttons]]` 的一项，经 `items` 里的 `custom:<id>` 引用）。
    ///
    /// 携带 `label` 而不是让 UI 侧再去查一份按钮表：渲染端读不到配置，下发的这一份
    /// 就是它需要的全部。`index` 只用于**点击时回指**——见 [`crate::ToolbarAction::Custom`]。
    Custom {
        /// 该按钮在 `ui.toolbar.buttons` 里的下标。
        index: u8,
        /// 已按显示宽度截断过的格内文字（协调器截好再下发）。
        label: String,
    },
}

/// 默认渲染顺序：`ui.toolbar.items` 留空 / 解析后为空时用它。
///
/// 与 `wind_config::TOOLBAR_ITEM_KEYS` 逐项对应，但**刻意各存一份**：那份是配置层的键名
/// （字符串），这份是协议层的项（枚举）。让 wind-ui-types 去依赖 wind-config 只为共享
/// 五个常量，会把配置 crate 拖进 headless / Android 的依赖图里。
pub const DEFAULT_TOOLBAR_ITEMS: [ToolbarItem; 5] = [
    ToolbarItem::Mode,
    ToolbarItem::Punct,
    ToolbarItem::FullWidth,
    ToolbarItem::S2t,
    ToolbarItem::Settings,
];

/// 工具栏状态（由协调器推送）
///
/// `PartialEq` 供协调器做**推送去重**（见 `notify_toolbar`）：宿主焦点抖动时同一份状态
/// 会被连推数次，全挤到 UI 线程上。用 derive 而不是手写比较——加字段时编译器自动带上，
/// 手写的那种漏一个字段就是「改了状态工具栏不更新」，且没有任何报错。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolbarState {
    pub chinese_mode: bool,
    /// 有效显示标签：中文模式取方案 icon_label（如 "拼"/"五"），无则 "中"；
    /// 英文小写为 "英"，大写锁定为 "A"（由协调器预计算后填入）。
    pub icon_label: String,
    pub caps_lock: bool,
    pub full_width: bool,
    pub chinese_punct: bool,
    /// 简繁转换当前是否启用（格内显示 "繁" 并高亮）
    pub s2t_enabled: bool,
    /// 是否显示简繁格（默认 false；用户开启简繁功能后显示）
    pub s2t_shown: bool,
    /// 软键盘面板是否开着（格子据此高亮）。
    pub soft_keyboard_on: bool,
    /// 当前打不出中文（密码框 / 焦点不在可编辑控件里 / 系统级禁用）：仅影响**呈现**
    /// （模式格显 "英" 且不高亮）。取值来自协调器的 `effective_input_block()`——
    /// 语言栏图标读的是**同一个**判定，两者不会再各说各话。
    ///
    /// 独立于 `icon_label` 而非直接改写它：后者是「当前方案标签」的单一语义，且会经
    /// StatusUpdate 下发写入 TSF 的 `_inputTypeLabel`（持久值）。把这种随焦点来去的
    /// 临时态烧进标签，离开时就得指望下一次状态推送把它改回来，漏一次图标即长期卡 "英"。
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
            s2t_shown: false,
            soft_keyboard_on: false,
            input_blocked: false,
        }
    }
}
