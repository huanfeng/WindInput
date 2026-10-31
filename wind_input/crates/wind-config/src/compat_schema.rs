//! 兼容规则的字段元数据：设置端「应用兼容性」页的表单渲染与帮助页的**单一数据源**。
//!
//! 每个 `compat.toml` 字段在这里登记一次：控件类型、分组、名称、一句话功能、解决的问题、
//! 已知宿主、联动条件、是否协议字段。设置端按它通用渲染，不逐字段手写界面。
//!
//! ⛔ 新增兼容字段时**必须同时在这里补一条**：`tests` 里的守护测试扫描 `app_compat.rs`
//! 的结构体源码，漏登记即红。
//! 文案写给不懂 TSF 的用户看：说现象与后果，不说内部机制名。
//!
//! 设计见 `docs/design/compat-settings-ui.md`。

use crate::app_compat::{STATUS_FALLBACK_POSITIONS, STATUS_POSITION_MODES};
use serde::Serialize;
use serde_json::{Value, json};

/// 控件类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// 普通开关（未配 = 关）。
    Bool,
    /// 三态：跟随全局 / 开 / 关。
    TriBool,
    /// 枚举（未配 = 跟随全局 / 不干预）。
    Enum,
    /// 整数。
    Int,
    /// 单行文本。
    Text,
    /// 文本列表。
    TextList,
}

/// 联动条件：仅当 `key` 的值等于 `value` 时本字段才有意义（否则界面置灰）。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Dep {
    pub key: &'static str,
    pub value: &'static str,
}

/// 一个字段的元数据。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldMeta {
    /// 所属段：`apps` / `initial_mode_scope` / `commit_newline`。
    pub section: &'static str,
    pub key: &'static str,
    pub kind: Kind,
    /// 分组 id，见 [`GROUPS`]。
    pub group: &'static str,
    pub label: &'static str,
    /// 一句话说明它做什么。
    pub summary: &'static str,
    /// 它解决什么问题（帮助页主体）。
    pub problem: &'static str,
    /// 已知需要它的软件。
    pub hosts: &'static [&'static str],
    /// 枚举可选值（`Kind::Enum` 必填）。
    pub options: &'static [&'static str],
    pub depends_on: Option<Dep>,
    /// 宿主缺陷修正（描述已确认的宿主行为，不是用户偏好）。界面可据此提示「一般不需要改」。
    /// 与合并语义无关：所有字段一视同仁地逐字段继承，没有哪个字段享有特殊待遇。
    pub protocol: bool,
    /// 折叠进「高级」。
    pub advanced: bool,
}

/// 分组：`(id, 显示名)`，顺序即界面顺序。
pub const GROUPS: &[(&str, &str)] = &[
    ("position", "候选窗定位"),
    ("status", "状态提示"),
    ("initial", "初始状态"),
    ("behavior", "输入行为"),
    ("host", "宿主兼容"),
    ("scope", "初始模式作用域"),
    ("newline", "上屏换行"),
];

const FIRST_SHOW_MODES: &[&str] = &["wait", "fast", "instant"];
const INITIAL_MODES: &[&str] = &["english", "chinese"];
const SMART_METHODS: &[&str] = &["delete_replace", "hold_composition"];
const CANDIDATE_POSITION_MODES: &[&str] = &["follow_caret", "fixed"];
const NEWLINE_STYLES: &[&str] = &["keep", "cr", "lf", "crlf"];
const PLACEHOLDER_CHARS: &[&str] = &["space", "zwsp", "blank"];
const SYLLABLE_SEPARATORS: &[&str] = &["apostrophe", "none", "space", "dot"];
const MULTILINE_COMMITS: &[&str] = &["commit", "paste"];

/// 全部字段。顺序即界面内的展示顺序（同组内）。
pub static COMPAT_FIELDS: &[FieldMeta] = &[
    // ── 候选窗定位 ────────────────────────────────────────────────
    FieldMeta {
        section: "apps",
        key: "first_show_mode",
        kind: Kind::Enum,
        group: "position",
        label: "候选窗首显",
        summary: "新一轮输入的第一个键，候选窗何时出现：快速显示、等待精确坐标（较慢）、立即显示（最快，可能抖动）。",
        problem: "有的软件收到按键后要过几十毫秒才能给出准确的光标位置。太早显示会先出现在旧位置再跳过去；\
                  等得太久又觉得迟钝。表格类软件（Excel、WPS 表格）进单元格时会先在编辑栏建临时输入区，\
                  选「等待精确坐标」才能一次到位。不设置则跟随全局设置。",
        hosts: &["EXCEL.EXE", "et.exe"],
        options: FIRST_SHOW_MODES,
        depends_on: None,
        protocol: false,
        advanced: false,
    },
    FieldMeta {
        section: "apps",
        key: "candidate_position_mode",
        kind: Kind::Enum,
        group: "position",
        label: "候选窗定位",
        summary: "跟随光标，或固定在屏幕上你指定的位置。",
        problem: "有些软件报不出可靠的光标位置，候选窗会乱跑。固定到一个你拖好的位置就不再受影响。\
                  不设置则跟随全局设置。",
        hosts: &[],
        options: CANDIDATE_POSITION_MODES,
        depends_on: None,
        protocol: false,
        advanced: false,
    },
    FieldMeta {
        section: "apps",
        key: "candidate_x",
        kind: Kind::Int,
        group: "position",
        label: "固定位置 X",
        summary: "固定模式下候选窗左上角的屏幕横坐标（物理像素）。",
        problem: "每个软件各存一份：合适的位置取决于该软件窗口在屏幕上的位置，共用一份只会让第一个配置的软件正确。\
                  (0,0) 表示还没拖过，会落到屏幕默认位置。",
        hosts: &[],
        options: &[],
        depends_on: Some(Dep {
            key: "candidate_position_mode",
            value: "fixed",
        }),
        protocol: false,
        advanced: false,
    },
    FieldMeta {
        section: "apps",
        key: "candidate_y",
        kind: Kind::Int,
        group: "position",
        label: "固定位置 Y",
        summary: "固定模式下候选窗左上角的屏幕纵坐标（物理像素）。",
        problem: "与固定位置 X 配套。换显示器后位置会被自动限制在可见范围内，不会飞到屏幕外。",
        hosts: &[],
        options: &[],
        depends_on: Some(Dep {
            key: "candidate_position_mode",
            value: "fixed",
        }),
        protocol: false,
        advanced: false,
    },
    FieldMeta {
        section: "apps",
        key: "caret_offset_x",
        kind: Kind::Int,
        group: "position",
        label: "光标位置水平校正",
        summary: "把软件报告的光标横坐标整体平移（96dpi 基准像素，正数向右）。",
        problem: "少数软件（如 Windows Terminal）报告的光标位置本身就有系统性偏差，别的输入法在里面同样偏。\
                  用它把候选窗、状态提示等一起挪正。单位按显示器缩放自动换算，多屏混插也一致。",
        hosts: &["WindowsTerminal.exe"],
        options: &[],
        depends_on: None,
        protocol: false,
        advanced: true,
    },
    FieldMeta {
        section: "apps",
        key: "caret_offset_y",
        kind: Kind::Int,
        group: "position",
        label: "光标位置垂直校正",
        summary: "把软件报告的光标纵坐标整体平移（96dpi 基准像素，正数向下）。",
        problem: "与水平校正配套，用途相同。",
        hosts: &["WindowsTerminal.exe"],
        options: &[],
        depends_on: None,
        protocol: false,
        advanced: true,
    },
    FieldMeta {
        section: "apps",
        key: "caret_use_top",
        kind: Kind::Bool,
        group: "position",
        label: "按光标上沿定位",
        summary: "用光标矩形的上沿而不是下沿来放置候选窗。",
        problem: "微信这类 Web 输入框报告的光标高度会在 1 到 20 像素之间跳变，按下沿放置时候选窗会上下漂移约 20 像素，\
                  上沿则始终稳定。",
        hosts: &["Weixin.exe"],
        options: &[],
        depends_on: None,
        protocol: false,
        advanced: false,
    },
    // ── 状态提示 ──────────────────────────────────────────────────
    FieldMeta {
        section: "apps",
        key: "status_position_mode",
        kind: Kind::Enum,
        group: "status",
        label: "状态提示位置",
        summary: "中英文切换等提示气泡出现在哪里：跟随光标、固定位置，或屏幕 / 窗口的某个锚点。",
        problem: "在光标位置不可靠或提示会挡住内容的软件里，可以让气泡固定出现在你习惯的位置。\
                  不设置则跟随全局设置。",
        hosts: &[],
        options: &STATUS_POSITION_MODES,
        depends_on: None,
        protocol: false,
        advanced: false,
    },
    FieldMeta {
        section: "apps",
        key: "status_x",
        kind: Kind::Int,
        group: "status",
        label: "状态提示固定位置 X",
        summary: "固定模式下状态提示左上角的屏幕横坐标（物理像素）。",
        problem: "(0,0) 表示已选固定但还没摆过位置，会落到光标所在的屏幕。",
        hosts: &[],
        options: &[],
        depends_on: Some(Dep {
            key: "status_position_mode",
            value: "fixed",
        }),
        protocol: false,
        advanced: false,
    },
    FieldMeta {
        section: "apps",
        key: "status_y",
        kind: Kind::Int,
        group: "status",
        label: "状态提示固定位置 Y",
        summary: "固定模式下状态提示左上角的屏幕纵坐标（物理像素）。",
        problem: "与状态提示固定位置 X 配套。",
        hosts: &[],
        options: &[],
        depends_on: Some(Dep {
            key: "status_position_mode",
            value: "fixed",
        }),
        protocol: false,
        advanced: false,
    },
    FieldMeta {
        section: "apps",
        key: "status_fallback_position",
        kind: Kind::Enum,
        group: "status",
        label: "坐标不可用时的状态提示位置",
        summary: "拿不到可信的光标位置时，状态提示放在上次位置、隐藏，或某个屏幕 / 窗口锚点。",
        problem: "只在生效的状态提示位置为「跟随光标」时起作用（该应用没设定位方式、由全局决定时同样适用，所以不做联动置灰）。避免软件刚获得焦点、光标还没报出来时，\
                  气泡出现在屏幕角落或上一个软件的位置。不设置则跟随全局设置。",
        hosts: &[],
        options: &STATUS_FALLBACK_POSITIONS,
        depends_on: None,
        protocol: false,
        advanced: true,
    },
    // ── 初始状态 ──────────────────────────────────────────────────
    FieldMeta {
        section: "apps",
        key: "initial_mode",
        kind: Kind::Enum,
        group: "initial",
        label: "初始输入模式",
        summary: "切换到这个软件时，自动设为中文或英文。",
        problem: "例如进入终端、游戏、代码编辑器时希望默认英文。这只是进入时的初始值，之后你仍可随时切换，\
                  不会被锁定。不设置则沿用全局逻辑。",
        hosts: &[],
        options: INITIAL_MODES,
        depends_on: None,
        protocol: false,
        advanced: false,
    },
    FieldMeta {
        section: "apps",
        key: "initial_punct",
        kind: Kind::Enum,
        group: "initial",
        label: "初始标点模式",
        summary: "切换到这个软件时，自动设为中文标点或英文标点。",
        problem: "显式设置后优先于「标点跟随中英文」的推导；否则你配了它却恰好开着跟随时会完全无效且没有任何提示。",
        hosts: &[],
        options: INITIAL_MODES,
        depends_on: None,
        protocol: false,
        advanced: false,
    },
    FieldMeta {
        section: "apps",
        key: "schema",
        kind: Kind::Text,
        group: "initial",
        label: "方案",
        summary: "这个软件固定使用某个输入方案；填 @remember 则记住它上次用的方案。",
        problem: "例如聊天软件用拼音、写代码用五笔。在该软件里手动切换方案只影响它自己，不改全局方案。\
                  方案 id 是否存在在使用时才校验。",
        hosts: &[],
        options: &[],
        depends_on: None,
        protocol: false,
        advanced: false,
    },
    // ── 输入行为 ──────────────────────────────────────────────────
    FieldMeta {
        section: "apps",
        key: "syllable_separator",
        kind: Kind::Enum,
        group: "behavior",
        label: "拼音音节分隔符",
        summary: "拼音编码里音节之间显示的符号：撇号 '、不显示、空格或 ·。",
        problem: "有的软件（如 Unreal Engine 的资产命名框）不接受撇号，编码里带 ' 会被拒绝或弹错。\
                  选「不显示」即可，只改显示，不影响输入和切分。不设置则跟随全局设置。",
        hosts: &[],
        options: SYLLABLE_SEPARATORS,
        depends_on: None,
        protocol: false,
        advanced: false,
    },
    FieldMeta {
        section: "apps",
        key: "auto_pair",
        kind: Kind::TriBool,
        group: "behavior",
        label: "符号自动配对",
        summary: "输入左括号 / 引号时是否自动补全右半边。",
        problem: "表格类软件（Excel、WPS 表格）在输入状态下把方向键当作「确认单元格并移动」，配对后无法把光标退回两个符号之间，\
                  目前只能对它们关掉自动配对。不设置则沿用全局设置。",
        hosts: &["EXCEL.EXE", "et.exe"],
        options: &[],
        depends_on: None,
        protocol: false,
        advanced: false,
    },
    FieldMeta {
        section: "apps",
        key: "password_force_english",
        kind: Kind::TriBool,
        group: "behavior",
        label: "密码框强制英文",
        summary: "在密码框里是否强制使用英文输入。",
        problem: "有的软件把普通输入框误报成密码框，导致无法输入中文：全局保持开启保护真密码框，只对这个软件关掉。\
                  反过来也可以全局关闭、只对某个软件开启。",
        hosts: &[],
        options: &[],
        depends_on: None,
        protocol: false,
        advanced: false,
    },
    FieldMeta {
        section: "apps",
        key: "smart_method",
        kind: Kind::Enum,
        group: "behavior",
        label: "智能符号替换方式",
        summary: "delete_replace 会删改已输入的内容来替换符号；hold_composition 全程不删改。",
        problem: "在 Tabby 这类终端里，删改已输入内容会出现严重错误，改用 hold_composition 兼容性更好。\
                  不设置则沿用全局设置。",
        hosts: &["Tabby.exe"],
        options: SMART_METHODS,
        depends_on: None,
        protocol: false,
        advanced: false,
    },
    // ── 宿主兼容 ──────────────────────────────────────────────────
    FieldMeta {
        section: "apps",
        key: "stale_probe_guard",
        kind: Kind::Bool,
        group: "host",
        label: "拦截陈旧的光标位置",
        summary: "不采信输入过程中与「输入前光标位置」矛盾的坐标。",
        problem: "微信、飞书在线表格等软件在你移动光标或换单元格后再输入时，会把上一次输入的位置当作当前位置上报，\
                  候选窗先出现在旧位置，约 80 毫秒后才跳到新位置，表现为候选窗跳动。开启后改用输入前的光标位置首显。\
                  ⚠ 不同软件的正确答案相反（有的软件是新坐标才对），所以只能按软件开启，不能全局启用。",
        hosts: &["Weixin.exe", "Feishu.exe"],
        options: &[],
        depends_on: None,
        protocol: false,
        advanced: true,
    },
    FieldMeta {
        section: "apps",
        key: "composition_start_pair_guard",
        kind: Kind::TriBool,
        group: "host",
        label: "识别成对的组合帧",
        summary: "把连续两帧「组合起点」与「当前光标」识别为同一次布局采样。",
        problem: "QQ、VS Code、飞书聊天框在长句输入时会交替上报组合起点和当前光标，候选窗因此在起点与末端之间左右闪烁。\
                  开启后固定以稳定的组合起点为准。出厂已为这些软件开启；你只需要在想关闭时显式设为「关」。",
        hosts: &["QQ.exe", "Code.exe", "Feishu.exe"],
        options: &[],
        depends_on: None,
        protocol: true,
        advanced: true,
    },
    FieldMeta {
        section: "apps",
        key: "pin_anchor_when_start_drifts",
        kind: Kind::TriBool,
        group: "host",
        label: "起点漂移时钉住锚点",
        summary: "宿主报的组合起点跟着光标一起漂移时，把候选窗锚点钉在首帧位置。",
        problem: "WPS 文字、Word 里输入时起点每帧都跟着光标右移，候选窗会被越推越远（实测离输入区近 900 像素）。\
                  钉住后候选窗留在输入起点。⚠ Excel / WPS 表格的数据形态一样，但它们需要跟随单元格，钉住反而出错，\
                  所以只能按软件声明。",
        hosts: &["wps.exe", "WINWORD.EXE"],
        options: &[],
        depends_on: None,
        protocol: true,
        advanced: true,
    },
    FieldMeta {
        section: "apps",
        key: "host_render",
        kind: Kind::Bool,
        group: "host",
        label: "宿主渲染模式",
        summary: "候选窗由输入法服务绘制，再交给软件内的组件显示。",
        problem: "Win11 开始菜单、任务栏搜索这类受限进程，普通候选窗盖不住它们的窗口层级。改由服务渲染后经共享内存转交，\
                  才能正常显示。",
        hosts: &[
            "SearchHost.exe",
            "searchapp.exe",
            "startmenuexperiencehost.exe",
        ],
        options: &[],
        depends_on: None,
        protocol: false,
        advanced: true,
    },
    FieldMeta {
        section: "apps",
        key: "ignore_host_ime_close",
        kind: Kind::TriBool,
        group: "host",
        label: "宿主关闭输入法",
        summary: "软件自己要求关闭输入法时忽略它（按住 Ctrl 的系统热键仍然放行）。",
        problem: "部分 WinForms / WPF 编写的软件（如某些工具箱、游戏平台客户端）在你点一下按钮后会把输入法整体关掉，你就被切成了英文，回到文本框还不一定恢复。\
                  开启后忽略这类关闭请求。⚠ 不能全局启用：有的软件（如 gvim）依赖这个状态来保存和恢复。",
        hosts: &[],
        options: &[],
        depends_on: None,
        protocol: true,
        advanced: true,
    },
    FieldMeta {
        section: "apps",
        key: "host_drawn_candidates",
        kind: Kind::TriBool,
        group: "host",
        label: "软件自绘候选窗",
        summary: "软件把候选串读走时，视为它在自己画候选窗，并收起本输入法的候选窗。",
        problem: "新枫之谷等游戏通过系统兼容层读走候选并画出旧式候选窗，不收起就会同时出现两个框。\
                  默认关闭：多数软件（记事本、Illustrator、EverEdit）虽然也读候选串却并不自绘，误收起会导致一个框都没有。\
                  「显式关」与「未设置」不同：关掉才能挡住出厂对该软件开启的规则。",
        hosts: &["MapleStory.exe"],
        options: &[],
        depends_on: None,
        protocol: true,
        advanced: true,
    },
    FieldMeta {
        section: "apps",
        key: "composition_placeholder",
        kind: Kind::Enum,
        group: "host",
        label: "占位字符",
        summary: "编码不显示在软件里时，输入区中临时放的那个字符：空格、零宽空格或盲文空白。",
        problem: "网页里有些输入框会自动去掉首尾空格（如番茄小说的搜索框），占位的空格一被去掉，输入就被打断，一个字都打不上去。\
                  给浏览器改用零宽空格即可避开。也有网页要「看得见内容」才开始编辑（如钉钉在线表格选中单元格直接打字），\
                  零宽空格在那里被当成没有内容，可改用盲文空白：它占一个字宽、又不会被当成空格去掉。\
                  不设置则用空格：WPS 等软件靠它确定光标位置，不要随意更改。",
        hosts: &["msedge.exe", "chrome.exe", "firefox.exe"],
        options: PLACEHOLDER_CHARS,
        depends_on: None,
        protocol: true,
        advanced: true,
    },
    FieldMeta {
        section: "apps",
        key: "multiline_commit",
        kind: Kind::Enum,
        group: "host",
        label: "多行文本上屏方式",
        summary: "含换行的文本怎么交给软件：直接上屏，或经剪贴板粘贴。",
        problem: "新版微信收到含换行的上屏文本时，会把输入的编码残留在输入框里。改用粘贴上屏即可避开。\
                  粘贴会覆盖剪贴板原有内容且不恢复（不进剪贴板历史），只影响含换行的上屏。终端等 Ctrl+V 不是粘贴的软件不要开启。\
                  不设置则直接上屏。",
        hosts: &["Weixin.exe"],
        options: MULTILINE_COMMITS,
        depends_on: None,
        protocol: true,
        advanced: true,
    },
    // ── 初始模式作用域 ────────────────────────────────────────────
    FieldMeta {
        section: "initial_mode_scope",
        key: "classes",
        kind: Kind::TextList,
        group: "scope",
        label: "允许重算的窗口类",
        summary: "该进程下，只有这些顶层窗口类才会重新应用「初始输入模式」。",
        problem: "资源管理器这类进程里有很多不同用途的窗口（文件窗口、任务栏、搜索框）。只让指定的窗口类触发初始状态，\
                  其余窗口保持现状，避免在无关窗口里被切来切去。列表为空表示任何窗口都不重算。",
        hosts: &["explorer.exe"],
        options: &[],
        depends_on: None,
        protocol: false,
        advanced: false,
    },
    // ── 上屏换行 ──────────────────────────────────────────────────
    FieldMeta {
        section: "commit_newline",
        key: "style",
        kind: Kind::Enum,
        group: "newline",
        label: "上屏换行形式",
        summary: "上屏文本里的换行用哪种字符表达：keep 原样、cr、lf、crlf。",
        problem: "Word 这类富文本软件的文档里，段落分隔符就是 CR；直接送入 LF 它不认作换行，会在每个换行处显示成一段类似 Tab 的空白。\
                  为这类软件指定 cr，多行文本上屏才会正确分段。VS Code、终端、浏览器输入框则是写什么存什么，\
                  不要给它们转换，否则会悄悄改写你的数据。不设置则跟随全局设置。",
        hosts: &["WINWORD.EXE"],
        options: NEWLINE_STYLES,
        depends_on: None,
        protocol: false,
        advanced: false,
    },
];

/// 某一段登记的字段名。
pub fn known_keys(section: &str) -> Vec<&'static str> {
    COMPAT_FIELDS
        .iter()
        .filter(|f| f.section == section)
        .map(|f| f.key)
        .collect()
}

/// 枚举可选值的中文显示名（界面只显示中文，取值本身写在配置文件里、不翻译）。
///
/// 按取值 id 全局统一：同一个 id 在各字段里含义一致（`english` / `chinese` 在初始模式与初始标点里
/// 都是英文 / 中文）。新增枚举取值必须在这里登记，[`tests::every_enum_option_has_a_chinese_label`]
/// 守着——漏登记界面就会露出裸英文 id。
pub const OPTION_LABELS: &[(&str, &str)] = &[
    ("fast", "快速显示"),
    ("wait", "等待精确坐标（较慢）"),
    ("instant", "立即显示（最快，可能抖动）"),
    ("follow_caret", "跟随光标"),
    ("fixed", "固定位置"),
    ("screen_center", "屏幕中央"),
    ("screen_top_left", "屏幕左上角"),
    ("screen_top_right", "屏幕右上角"),
    ("screen_bottom_left", "屏幕左下角"),
    ("screen_bottom_right", "屏幕右下角"),
    ("window_center", "窗口中央"),
    ("window_bottom_left", "窗口左下角"),
    ("last", "上次位置"),
    ("hide", "不显示"),
    ("english", "英文"),
    ("chinese", "中文"),
    ("delete_replace", "删除后替换"),
    ("hold_composition", "保持组合串"),
    ("keep", "保持原样"),
    ("cr", "回车（CR）"),
    ("lf", "换行（LF）"),
    ("crlf", "回车换行（CRLF）"),
    ("space", "空格"),
    ("zwsp", "零宽空格"),
    ("blank", "盲文空白（有宽度、不算空格）"),
    ("apostrophe", "撇号 '"),
    ("none", "不显示"),
    ("dot", "间隔点 ·"),
    ("commit", "直接上屏"),
    ("paste", "粘贴上屏"),
];

/// 个别字段里同一个取值 id 的说法与全局登记不同时的覆盖：`(字段键, 取值, 显示名)`。
/// 与右键菜单的用词保持一致（菜单里「状态提示位置」的 `fixed` 叫「固定（取当前位置）」，
/// 而「候选窗定位」的 `fixed` 叫「固定位置」）。
const FIELD_OPTION_LABELS: &[(&str, &str, &str)] =
    &[("status_position_mode", "fixed", "固定（取当前位置）")];

/// 三态开关三档（跟随 / 真 / 假）的显示名，默认「跟随全局 / 开 / 关」；个别字段的菜单用词不同
/// （例如「宿主关闭输入法」是「忽略 / 采纳」，与字段的 true/false 语义方向一致但说法不同）。
const TRI_LABELS: &[(&str, [&str; 3])] = &[
    ("auto_pair", ["跟随全局", "启用", "禁用"]),
    ("ignore_host_ime_close", ["跟随内置规则", "忽略", "采纳"]),
];

/// 三态开关的三档显示名（下标 0 = 跟随，1 = true，2 = false）。
pub fn tri_labels(key: &str) -> [&'static str; 3] {
    TRI_LABELS
        .iter()
        .find(|(k, _)| *k == key)
        .map_or(["跟随全局", "开", "关"], |(_, l)| *l)
}

/// 某字段某取值的显示名：先看该字段的覆盖，再落到全局登记。
pub fn option_label_for(key: &str, id: &str) -> Option<&'static str> {
    FIELD_OPTION_LABELS
        .iter()
        .find(|(k, o, _)| *k == key && *o == id)
        .map(|(_, _, l)| *l)
        .or_else(|| option_label(id))
}

/// 取值 id 的中文名；没登记返回 `None`。
pub fn option_label(id: &str) -> Option<&'static str> {
    OPTION_LABELS
        .iter()
        .find(|(k, _)| *k == id)
        .map(|(_, v)| *v)
}

/// `compat.schema` 的响应体。
pub fn schema_json() -> Value {
    // 编译期固定的数据，序列化不可能失败；出错就该响亮地报，别悄悄变成空 schema。
    let mut fields = serde_json::to_value(COMPAT_FIELDS).expect("COMPAT_FIELDS 序列化");
    if let Some(list) = fields.as_array_mut() {
        for (f, meta) in list.iter_mut().zip(COMPAT_FIELDS) {
            // `optionLabels` 与 `options` 同序同长：界面按下标取，缺失的退回原 id。
            if !meta.options.is_empty() {
                f["optionLabels"] = json!(
                    meta.options
                        .iter()
                        .map(|o| option_label_for(meta.key, o).unwrap_or(o))
                        .collect::<Vec<_>>()
                );
            }
            // 三态开关的三档说法（跟随 / 真 / 假）。
            if meta.kind == Kind::TriBool {
                f["triLabels"] = json!(tri_labels(meta.key));
            }
        }
    }
    json!({
        "groups": GROUPS
            .iter()
            .map(|(id, label)| json!({ "id": id, "label": label }))
            .collect::<Vec<_>>(),
        "fields": fields,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 抽出某个 `pub struct <name> {` 的字段名。取自源码而不是类型系统：Rust 没有字段反射，
    /// 而「漏登记」正是本测试要防的。
    fn struct_fields(src: &str, name: &str) -> Vec<String> {
        let start = src
            .find(&format!("pub struct {name} {{"))
            .unwrap_or_else(|| panic!("找不到结构体 {name}"));
        let body = &src[start..];
        let end = body.find("\n}").expect("找不到结构体结尾");
        body[..end]
            .lines()
            .filter_map(|l| {
                l.trim()
                    .strip_prefix("pub ")
                    .and_then(|r| r.split_once(':'))
                    .map(|(n, _)| n.trim().to_string())
            })
            .collect()
    }

    fn check(section: &str, struct_name: &str) {
        let src = include_str!("app_compat.rs");
        let mut fields = struct_fields(src, struct_name);
        fields.retain(|f| !crate::compat_overlay::META_KEYS.contains(&f.as_str()));
        assert!(!fields.is_empty(), "{struct_name} 字段表截获失灵");
        let registered = known_keys(section);
        for f in &fields {
            assert!(
                registered.contains(&f.as_str()),
                "{struct_name}.{f} 没登记进 COMPAT_FIELDS（新增兼容字段必须同时补元数据）"
            );
        }
        for k in &registered {
            assert!(
                fields.iter().any(|f| f == k),
                "COMPAT_FIELDS 登记了不存在的字段 {section}.{k}"
            );
        }
    }

    #[test]
    fn every_apps_field_has_metadata() {
        check("apps", "AppCompatRule");
    }

    #[test]
    fn every_scope_field_has_metadata() {
        check("initial_mode_scope", "InitialModeScopeRule");
    }

    #[test]
    fn every_newline_field_has_metadata() {
        check("commit_newline", "CommitNewlineRule");
    }

    #[test]
    fn every_field_explains_itself() {
        for f in COMPAT_FIELDS {
            assert!(
                !f.label.is_empty() && !f.summary.is_empty() && !f.problem.is_empty(),
                "{}.{} 的 label/summary/problem 不得为空",
                f.section,
                f.key
            );
        }
    }

    #[test]
    fn field_specific_labels_override_and_tri_fields_carry_three_labels() {
        assert_eq!(
            option_label_for("status_position_mode", "fixed"),
            Some("固定（取当前位置）")
        );
        assert_eq!(
            option_label_for("candidate_position_mode", "fixed"),
            Some("固定位置")
        );
        let v = schema_json();
        for f in v["fields"].as_array().unwrap() {
            if f["kind"] == "tri_bool" {
                let l = f["triLabels"].as_array().expect("三态开关必须带 triLabels");
                assert_eq!(l.len(), 3, "{}", f["key"]);
            } else {
                assert!(f.get("triLabels").is_none(), "{}", f["key"]);
            }
        }
    }

    #[test]
    fn every_enum_option_has_a_chinese_label() {
        for f in COMPAT_FIELDS {
            for o in f.options {
                let zh = option_label(o)
                    .unwrap_or_else(|| panic!("{}.{} 的取值 {o} 没有登记中文名", f.section, f.key));
                assert!(!zh.is_ascii(), "{o} 的显示名 {zh:?} 里没有中文");
            }
        }
        let v = schema_json();
        let f = v["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["key"] == "first_show_mode")
            .unwrap();
        assert_eq!(
            f["options"].as_array().unwrap().len(),
            f["optionLabels"].as_array().unwrap().len()
        );
    }

    #[test]
    fn enum_fields_list_options_and_others_do_not() {
        for f in COMPAT_FIELDS {
            if f.kind == Kind::Enum {
                assert!(!f.options.is_empty(), "{}.{} 缺 options", f.section, f.key);
            } else {
                assert!(
                    f.options.is_empty(),
                    "{}.{} 不是枚举却带了 options",
                    f.section,
                    f.key
                );
            }
        }
    }

    #[test]
    fn depends_on_points_at_a_real_enum_option() {
        for f in COMPAT_FIELDS {
            let Some(dep) = f.depends_on else { continue };
            let target = COMPAT_FIELDS
                .iter()
                .find(|t| t.section == f.section && t.key == dep.key)
                .unwrap_or_else(|| panic!("{}.depends_on 指向不存在的字段 {}", f.key, dep.key));
            assert!(
                target.options.contains(&dep.value),
                "{}.depends_on 的值 {} 不在 {} 的 options 里",
                f.key,
                dep.value,
                dep.key
            );
        }
    }

    #[test]
    fn every_group_id_is_declared() {
        for f in COMPAT_FIELDS {
            assert!(
                GROUPS.iter().any(|(id, _)| *id == f.group),
                "{}.{} 的分组 {} 未在 GROUPS 里声明",
                f.section,
                f.key,
                f.group
            );
        }
    }

    #[test]
    fn keys_are_unique_within_a_section() {
        let mut seen = std::collections::HashSet::new();
        for f in COMPAT_FIELDS {
            assert!(
                seen.insert((f.section, f.key)),
                "重复登记 {}.{}",
                f.section,
                f.key
            );
        }
    }

    #[test]
    fn schema_json_has_groups_and_all_fields() {
        let v = schema_json();
        assert_eq!(v["fields"].as_array().unwrap().len(), COMPAT_FIELDS.len());
        assert_eq!(v["groups"].as_array().unwrap().len(), GROUPS.len());
        let first = &v["fields"][0];
        for k in [
            "section", "key", "kind", "group", "label", "summary", "problem", "options", "protocol",
        ] {
            assert!(first.get(k).is_some(), "字段元数据缺少键 {k}");
        }
    }
}
