//! 触发键名 ↔ 虚拟键码（VK）统一映射。
//!
//! 所有独占模式（临时拼音 / 快捷输入 / 特殊模式）共用此表，避免各写各的键名解析
//! 导致「某模式认得 backslash、某模式不认」。
//!
//! 单一真相源是 [`KEY_TABLE`]：每行用具名 VK 常量声明「VK + 组合区前缀字符 + 键名别名」，
//! [`key_name_to_vk`] 与 [`vk_to_prefix_char`] 两个方向均由它派生，新增键只改一处。

/// 符号 / OEM 虚拟键码常量（Windows Virtual-Key Codes）。
/// 统一在此定义，杜绝散落各处的 `0xBA` 之类裸十六进制字面量。
pub const VK_SEMICOLON: u32 = 0xBA; // ; :  VK_OEM_1
pub const VK_EQUAL: u32 = 0xBB; // = +  VK_OEM_PLUS
pub const VK_COMMA: u32 = 0xBC; // , <  VK_OEM_COMMA
pub const VK_MINUS: u32 = 0xBD; // - _  VK_OEM_MINUS
pub const VK_PERIOD: u32 = 0xBE; // . >  VK_OEM_PERIOD
pub const VK_SLASH: u32 = 0xBF; // / ?  VK_OEM_2
pub const VK_BACKTICK: u32 = 0xC0; // ` ~  VK_OEM_3
pub const VK_LBRACKET: u32 = 0xDB; // [ {  VK_OEM_4
pub const VK_BACKSLASH: u32 = 0xDC; // \ |  VK_OEM_5
pub const VK_RBRACKET: u32 = 0xDD; // ] }  VK_OEM_6
pub const VK_QUOTE: u32 = 0xDE; // ' "  VK_OEM_7

// 控制 / 编辑 / 导航虚拟键码（统一定义，杜绝散落的裸十六进制）。
pub const VK_BACK: u32 = 0x08; // 退格
pub const VK_TAB: u32 = 0x09;
pub const VK_RETURN: u32 = 0x0D; // 回车
pub const VK_ESCAPE: u32 = 0x1B;
pub const VK_SPACE: u32 = 0x20;
pub const VK_PRIOR: u32 = 0x21; // PageUp
pub const VK_NEXT: u32 = 0x22; // PageDown
pub const VK_END: u32 = 0x23;
pub const VK_HOME: u32 = 0x24;
pub const VK_LEFT: u32 = 0x25;
pub const VK_UP: u32 = 0x26;
pub const VK_RIGHT: u32 = 0x27;
pub const VK_DOWN: u32 = 0x28;
pub const VK_DELETE: u32 = 0x2E; // 前删（光标后一字符）
pub const VK_INSERT: u32 = 0x2D;
// 功能键 F1..=F24 与小键盘，连号。目前只有按键事件的键名表（`names`）用到。
pub const VK_F1: u32 = 0x70;
pub const VK_F24: u32 = 0x87;
pub const VK_NUMPAD0: u32 = 0x60;
pub const VK_NUMPAD9: u32 = 0x69;
pub const VK_MULTIPLY: u32 = 0x6A;
pub const VK_ADD: u32 = 0x6B;
pub const VK_SUBTRACT: u32 = 0x6D;
pub const VK_DECIMAL: u32 = 0x6E;
pub const VK_DIVIDE: u32 = 0x6F;
// 字母 / 数字区间端点（区间用 VK_A..=VK_Z / VK_0..=VK_9 表达，VK 与 ASCII 大写/数字一致）。
pub const VK_A: u32 = 0x41;
pub const VK_Z: u32 = 0x5A;
pub const VK_0: u32 = 0x30;
pub const VK_9: u32 = 0x39;
pub const VK_1: u32 = 0x31;
// 纯修饰键的左右具体键码。只在「修饰键被配成功能键」的路径出现：中英文切换键、
// 二三候选键的 lrshift/lrctrl 组——这两条都由 TSF 在 keyup 转发**具体**键码（笼统的
// VK_SHIFT/VK_CONTROL 已在那边解析成左右），故服务端只需认这四个。
// 四个连号（0xA0..=0xA3），可用 `VK_LSHIFT..=VK_RCONTROL` 表达「是不是纯修饰键」。
pub const VK_LSHIFT: u32 = 0xA0;
pub const VK_RSHIFT: u32 = 0xA1;
pub const VK_LCONTROL: u32 = 0xA2;
pub const VK_RCONTROL: u32 = 0xA3;
/// CapsLock。与四个纯修饰键同属「只有 keyup 到得了服务端」的一类，但**不连号**，
/// 故不在 `VK_LSHIFT..=VK_RCONTROL` 区间里——判「能否走 keydown」要用
/// [`is_key_up_only_vk`]，不能只判那个区间。
pub const VK_CAPITAL: u32 = 0x14;

/// 会话态键名的解析结果（[`session_key_name_to_vk`]）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SessionKey {
    pub vk: u32,
    /// 是否要求 Shift 同时按下（键名前缀 `shift+`）。
    pub shift: bool,
    /// 该键本身是否产出字符。`true` 的键在文本/表达式模式（临英 / 快捷输入）里必须
    /// 回落成输入字符，不能被夺为导航——见 [`NavKeys::classify`] 的 `include_printable`。
    pub printable: bool,
}

/// 会话态功能键名 → VK。不含符号键（那些走 [`KEY_TABLE`]，且 `printable = true`）。
///
/// 只收「有会话时确实需要重新绑定」的键。刻意不收 `enter` / `space` / `backspace`：
/// 它们各自已有专属的策略参数（`input.enter_behavior` / `input.space_on_empty_behavior` /
/// 退格粒度），那是枚举形状而非绑定形状，混进本表会形成两个真相源。理由见
/// docs/design/session-key-actions.md §6.1。
const SESSION_FUNCTION_KEYS: &[(&str, u32)] = &[
    ("tab", VK_TAB),
    ("pageup", VK_PRIOR),
    ("pgup", VK_PRIOR),
    ("prior", VK_PRIOR),
    ("pagedown", VK_NEXT),
    ("pgdn", VK_NEXT),
    ("next", VK_NEXT),
    ("up", VK_UP),
    ("down", VK_DOWN),
    ("left", VK_LEFT),
    ("right", VK_RIGHT),
    ("home", VK_HOME),
    ("end", VK_END),
    ("capslock", VK_CAPITAL),
    ("caps", VK_CAPITAL),
    // 纯修饰键：作二三候选键时走 keyup 轻敲（`select_key_groups = ["lrctrl"]` 的收编形态）。
    // 别名须与 `wind_config::hotkey::session_key_to_vk` 逐字一致——两份表漂移会让绑定
    // 「进了 TSF 转发白名单却查不到动作」，无任何报错。
    ("lshift", VK_LSHIFT),
    ("rshift", VK_RSHIFT),
    ("lctrl", VK_LCONTROL),
    ("lcontrol", VK_LCONTROL),
    ("rctrl", VK_RCONTROL),
    ("rcontrol", VK_RCONTROL),
    // F1–F12：会话态绑功能（候选中按 F1 切换…）。与 `wind_config::hotkey::session_key_to_vk` 同步。
    ("f1", 0x70),
    ("f2", 0x71),
    ("f3", 0x72),
    ("f4", 0x73),
    ("f5", 0x74),
    ("f6", 0x75),
    ("f7", 0x76),
    ("f8", 0x77),
    ("f9", 0x78),
    ("f10", 0x79),
    ("f11", 0x7A),
    ("f12", 0x7B),
];

/// 会话态键名 → [`SessionKey`]。大小写与首尾空白不敏感；不认的名字返回 `None`。
///
/// 支持单个 `shift+` 前缀（`shift+tab`）。刻意不认 `ctrl+` / `alt+`：带这两个修饰的
/// 组合键归 `keys.key_actions`（key_down 热键表）——它们无会话时同样要能触发，与本表
/// 「只在组合输入期间改写键义」的语义不同，混收会让同一个组合键有两个注册入口。
pub fn session_key_name_to_vk(name: &str) -> Option<SessionKey> {
    let raw = name.trim().to_lowercase();
    let (shift, base) = match raw.strip_prefix("shift+") {
        Some(rest) => (true, rest.trim()),
        None => (false, raw.as_str()),
    };
    if let Some((_, vk)) = SESSION_FUNCTION_KEYS.iter().find(|(n, _)| *n == base) {
        return Some(SessionKey {
            vk: *vk,
            shift,
            printable: false,
        });
    }
    // 字母里只收 z，且 `printable: true`（它确实产出字符，文本模式下要让位）。
    // 键名表另一半在 `wind_config::hotkey::session_key_to_vk`，那里写着为什么只收 z。
    // 两处漂移的表现是「进了 TSF 转发白名单却查不到动作」，靠
    // `session_key_tables_agree_across_crates` 守门。
    if base == "z" {
        return Some(SessionKey {
            vk: VK_Z,
            shift,
            printable: true,
        });
    }
    key_name_to_vk(base).map(|vk| SessionKey {
        vk,
        shift,
        printable: true,
    })
}

/// 该 VK 是否只有 keyup 到得了服务端（纯修饰键与 CapsLock）。
///
/// 这批键绑任何功能都只能走 keyup 轻敲：keydown 不能吃（吃掉会让 AutoCAD 看不到修饰键）、
/// keydown 上判定会让 `Ctrl+A` 的第一下误触发、宿主对按住的键重复发 keydown 会连续触发。
/// 详见 docs/design/schema-key-actions.md §4.4。
///
/// CapsLock 与那四个修饰键不连号，故不能用 `VK_LSHIFT..=VK_RCONTROL` 区间判定
/// ——那样写会把 CapsLock 漏成「可走 keydown」，而 C++ 侧压根不发它的 keydown。
pub fn is_key_up_only_vk(vk: u32) -> bool {
    matches!(vk, VK_LSHIFT..=VK_RCONTROL | VK_CAPITAL)
}

/// 候选导航动作（翻页 / 高亮移动）。统一分类的结果，见 [`NavKeys`]。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NavAction {
    PagePrev,
    PageNext,
    HighlightUp,
    HighlightDown,
}

/// 一个会话态按键绑定：(键码, 是否需 Shift, 动作, 该键是否为可打印字符)。
/// `printable=true`（如 `-`/`=`/`[`/`]`）在文本/表达式模式（临英/快捷输入）中作输入而非动作，
/// 由 `classify(..., include_printable=false)` 排除；专用功能键（PageUp/Down、方向键、Tab）恒生效。
#[derive(Clone, Copy)]
struct Bind<A> {
    key: u32,
    shift: bool,
    action: A,
    printable: bool,
}

/// 配置驱动的会话态按键分类器。从 `keys.session_actions` 编译一次，普通模式与所有 overlay
/// 模式共用 [`classify`](KeyBinds::classify)，消除各处硬编码的翻页/高亮/取消判断。
///
/// 对动作类型泛型：动作值域（`SessionAction`）住在 `wind-config`，而 `wind-config` 经
/// `wind-cmdbar` 反向依赖本 crate，写死类型会成环。本表只管「键名 → 命中」，动作由调用方
/// （协调器）决定。
#[derive(Clone)]
pub struct KeyBinds<A> {
    binds: Vec<Bind<A>>,
}

// 手写 Default：`derive` 会要求 `A: Default`，而动作类型没有、也不该有「默认动作」。
impl<A> Default for KeyBinds<A> {
    fn default() -> Self {
        Self { binds: Vec::new() }
    }
}

/// 只装导航动作的绑定表。**本 crate 的单测用**——生产代码走 `KeyBinds<SessionAction>`
/// （动作值域在 `wind-config`，见 [`KeyBinds`] 的泛型说明）。
pub type NavKeys = KeyBinds<NavAction>;

impl<A> Bind<A> {
    /// 还原成 [`SessionKey`]，供共用的匹配谓词使用。
    ///
    /// `Bind` 把三个字段摊平存而不是内嵌 `SessionKey`，是编译期布局的选择；匹配规则
    /// 不该跟着摊平成两份。
    fn as_session_key(&self) -> SessionKey {
        SessionKey {
            vk: self.key,
            shift: self.shift,
            printable: self.printable,
        }
    }
}

impl SessionKey {
    /// 这条键定义是否匹配一次按键事件。
    ///
    /// 匹配规则的单一来源：[`KeyBinds::classify`] 与方案级 `[session_actions]` 的逐条比对都走
    /// 本方法，分散成两份会让同一个键在方案级和全局下行为不一致。
    pub fn matches(&self, key_code: u32, shift: bool, include_printable: bool) -> bool {
        self.vk == key_code && self.shift == shift && (include_printable || !self.printable)
    }
}

impl<A: Clone> KeyBinds<A> {
    /// 从 (键名, 动作) 对编译。键名解析走 [`session_key_name_to_vk`]。
    ///
    /// 数据源是 `keys.session_actions`（`page_keys` / `highlight_keys` 组名已在 `Config::normalize`
    /// 折算进来）。只认已解析好的动作，动词解析由协调器做（理由同类型文档）。
    ///
    /// 顺序即优先级：[`classify`](Self::classify) 用 `.find()` 取第一个匹配，同一 (键, shift)
    /// 声明两次时先来的赢。调用方折算时须保持 page 组在 highlight 组之前（`tab` 两组都配时翻页）。
    ///
    /// 不认的键名静默跳过：本函数无日志依赖，告警由调用方在加载期发（那里才分得清
    /// 「拼错了」与「显式 none」）。
    pub fn from_binds<'a>(binds: impl IntoIterator<Item = (&'a str, A)>) -> Self {
        let binds = binds
            .into_iter()
            .filter_map(|(name, action)| {
                session_key_name_to_vk(name).map(|k| Bind {
                    key: k.vk,
                    shift: k.shift,
                    action,
                    printable: k.printable,
                })
            })
            .collect();
        Self { binds }
    }

    /// 分类一个键。`include_printable=false` 时排除可打印键（`-`/`=`/`[`/`]`），
    /// 供输入需要这些字符的模式（临英/快捷输入）使用，避免吞掉输入语义。
    pub fn classify(&self, key_code: u32, shift: bool, include_printable: bool) -> Option<A> {
        self.binds
            .iter()
            .find(|b| {
                b.as_session_key()
                    .matches(key_code, shift, include_printable)
            })
            .map(|b| b.action.clone())
    }
}

/// 单个键的定义：虚拟键码、组合区前缀字符、可接受的键名别名（全小写）。
struct KeyDef {
    vk: u32,
    prefix: char,
    names: &'static [&'static str],
}

/// 触发键映射的单一真相源。两个方向的查询函数均从此派生。
const KEY_TABLE: &[KeyDef] = &[
    KeyDef {
        vk: VK_BACKTICK,
        prefix: '`',
        names: &["backtick", "grave", "`"],
    },
    KeyDef {
        vk: VK_SEMICOLON,
        prefix: ';',
        names: &["semicolon", ";"],
    },
    KeyDef {
        vk: VK_QUOTE,
        prefix: '\'',
        names: &["quote", "'"],
    },
    KeyDef {
        vk: VK_COMMA,
        prefix: ',',
        names: &["comma", ","],
    },
    KeyDef {
        vk: VK_PERIOD,
        prefix: '.',
        names: &["period", "."],
    },
    KeyDef {
        vk: VK_SLASH,
        prefix: '/',
        names: &["slash", "/"],
    },
    KeyDef {
        vk: VK_LBRACKET,
        prefix: '[',
        names: &["lbracket", "["],
    },
    KeyDef {
        vk: VK_RBRACKET,
        prefix: ']',
        names: &["rbracket", "]"],
    },
    KeyDef {
        vk: VK_BACKSLASH,
        prefix: '\\',
        names: &["backslash", "\\"],
    },
    KeyDef {
        vk: VK_MINUS,
        prefix: '-',
        names: &["minus", "-"],
    },
    KeyDef {
        vk: VK_EQUAL,
        prefix: '=',
        names: &["equal", "equals", "="],
    },
];

/// 触发键名 → VK。支持规范名 / 别名 / 单字符；大小写与首尾空白不敏感。
/// 字母键由调用方按需处理（见 [`key_name_to_vk_with_letters`]）。
pub fn key_name_to_vk(name: &str) -> Option<u32> {
    let k = name.trim().to_lowercase();
    KEY_TABLE
        .iter()
        .find(|d| d.names.contains(&k.as_str()))
        .map(|d| d.vk)
}

/// 同 [`key_name_to_vk`]，但额外接受单字母 a-z。
///
/// 不供引导键使用——引导键（临拼 / 特殊模式 / 临时 mix 的 `trigger_keys`）一律只认
/// 符号键，字母的特殊能力走方案级 `schema.codetable.z_key_action`。当前唯一消费者是
/// `key_inject`（按键注入按名字找 VK，与引导键语义无关）。
pub fn key_name_to_vk_with_letters(name: &str) -> Option<u32> {
    if let Some(vk) = key_name_to_vk(name) {
        return Some(vk);
    }
    let k = name.trim().to_lowercase();
    let bytes = k.as_bytes();
    if bytes.len() == 1 && bytes[0].is_ascii_lowercase() {
        // VK for 'A'..='Z' 与 ASCII 大写一致（0x41..=0x5A）。
        return Some(0x41 + (bytes[0] - b'a') as u32);
    }
    None
}

/// 全部符号键的「规范名 + 组合区字符」。规范名取 [`KEY_TABLE`] 每行的首个别名。
///
/// 供设置页回答「这个键在某方案里是不是码元」（设置页只认键名，码元集是字符集）。
/// 放在这里而不让设置页自己拼：跨仓无编译期约束，`KEY_TABLE` 增删时那边不会跟着改。
pub fn symbol_keys() -> impl Iterator<Item = (&'static str, char)> {
    KEY_TABLE.iter().filter_map(|d| {
        let name = d.names.first()?;
        Some((*name, d.prefix))
    })
}

/// 纯修饰键名 → VK（`lshift` / `rshift` / `lctrl` / `rctrl`，别名见实现）。
/// 大小写与首尾空白不敏感；非修饰键返回 None。
///
/// 不并进 [`KEY_TABLE`]：那是走 keydown 的引导键的解析入口，而修饰键必须走 keyup 轻敲
/// （见 [`is_key_up_only_vk`]），并进去会让引导键设置项接受一个永远不触发的值；修饰键也
/// 没有可填进 `KeyDef.prefix` 的字符。
///
/// 当前唯一消费者是方案级 `[key_actions]`（见 `Coordinator::bound_action_for`）。
pub fn modifier_name_to_vk(name: &str) -> Option<u32> {
    match name.trim().to_lowercase().as_str() {
        "lshift" | "leftshift" | "left_shift" => Some(VK_LSHIFT),
        "rshift" | "rightshift" | "right_shift" => Some(VK_RSHIFT),
        "lctrl" | "lcontrol" | "leftctrl" | "left_ctrl" => Some(VK_LCONTROL),
        "rctrl" | "rcontrol" | "rightctrl" | "right_ctrl" => Some(VK_RCONTROL),
        _ => None,
    }
}

/// 该 VK 是否为纯修饰键（四个连号，见常量定义处说明）。
pub fn is_pure_modifier_vk(vk: u32) -> bool {
    (VK_LSHIFT..=VK_RCONTROL).contains(&vk)
}

/// 该 VK 是否为 Alt（左 / 右 / 不分左右）。不并进 [`is_pure_modifier_vk`]：Alt 不走修饰键轻敲那条路。
pub fn is_alt_vk(vk: u32) -> bool {
    matches!(vk, 0xA4 | 0xA5 | 0x12)
}

/// VK → 符号键的规范名（`semicolon`、`comma`…，即 [`key_name_to_vk`] 认的第一个名字）。
pub fn vk_to_symbol_key_name(vk: u32) -> Option<&'static str> {
    KEY_TABLE
        .iter()
        .find(|d| d.vk == vk)
        .and_then(|d| d.names.first().copied())
}

/// VK → 组合区前缀字符。无映射时返回 None（调用方自定默认）。
pub fn vk_to_prefix_char(vk: u32) -> Option<char> {
    KEY_TABLE.iter().find(|d| d.vk == vk).map(|d| d.prefix)
}

/// 同 [`vk_to_prefix_char`]，但字母 VK 返回其小写字母。
///
/// 与 [`key_name_to_vk`] 拒绝字母不矛盾：那是配置解析，认字母等于允许把编码键配成引导键；
/// 这是呈现，z 经 `z_key_action` 进模式后组合区要显示用户按下的那个 `z`。
pub fn vk_to_prefix_char_with_letters(vk: u32) -> Option<char> {
    if let Some(c) = vk_to_prefix_char(vk) {
        return Some(c);
    }
    (VK_A..=VK_Z)
        .contains(&vk)
        .then(|| (b'a' + (vk - VK_A) as u8) as char)
}

#[cfg(test)]
#[path = "keymap_tests.rs"]
mod tests;
