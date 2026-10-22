//! 触发键名 ↔ 虚拟键码（VK）统一映射。
//!
//! 所有独占模式（临时拼音 / 快捷输入 / 特殊模式）共用此表，避免各写各的键名解析
//! 导致「某模式认得 backslash、某模式不认」的不一致（曾因此使特殊模式无法触发）。
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

/// 会话态功能键名 → VK。**不含符号键**（那些走 [`KEY_TABLE`]，且 `printable = true`）。
///
/// 只收「有会话时确实需要重新绑定」的键。刻意**不收** `enter` / `space` / `backspace`：
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
    // ⚠️ 别名须与 `wind_config::hotkey::session_key_to_vk` 逐字一致——两份表漂移会让绑定
    // 「进了 TSF 转发白名单却查不到动作」，无任何报错。一期漏了这四个，三期补上。
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
/// 支持单个 `shift+` 前缀（`shift+tab`）。**刻意不认 `ctrl+` / `alt+`**：带这两个修饰的
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
    // ★ 字母里**只收 z**，且 `printable: true`（它确实产出字符，文本模式下要让位）。
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

/// 该 VK 是否**只有 keyup 到得了服务端**（纯修饰键与 CapsLock）。
///
/// 这批键绑任何功能都只能走 keyup 轻敲：keydown 不能吃（吃掉会让 AutoCAD 看不到修饰键）、
/// keydown 上判定会让 `Ctrl+A` 的第一下误触发、宿主对按住的键重复发 keydown 会连续触发。
/// 详见 docs/design/schema-key-actions.md §4.4。
///
/// ⚠️ CapsLock 与那四个修饰键**不连号**，故不能用 `VK_LSHIFT..=VK_RCONTROL` 区间判定
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
/// # 为什么对动作类型泛型
///
/// 动作值域（`SessionAction`）住在 `wind-config`，而本 crate 是它的**下游**——
/// `wind-config` 经 `wind-cmdbar` 反向依赖本 crate，写死类型就得加依赖，那会成环。
/// 泛型让本表只管「键名 → 命中」这件事，动作是什么由调用方（协调器，唯一同时看得见
/// 两个 crate 的地方）决定。
///
/// 一期只有导航动词时这里写死的是 `NavAction`，二期加 `cancel` 时立刻不够用了——
/// ★ 判据：**一张「键 → 动作」的表，动作类型就不该由表来规定**。
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
    /// ★ **匹配规则的单一来源**：[`KeyBinds::classify`] 的 `.find()` 谓词走本方法，方案级
    /// `[session_actions]` 按键名逐条比对时也走本方法。三个条件（VK、shift、可打印键在
    /// 文本模式下让位）分散成两份的表现是「同一个键在方案级和全局下行为不一致」，而那种
    /// 不一致只在同时配了两层的用户那里复现。
    pub fn matches(&self, key_code: u32, shift: bool, include_printable: bool) -> bool {
        self.vk == key_code && self.shift == shift && (include_printable || !self.printable)
    }
}

impl<A: Clone> KeyBinds<A> {
    /// 从 (键名, 动作) 对编译。键名解析走 [`session_key_name_to_vk`]。
    ///
    /// 数据源是 `keys.session_actions`（旧的 `page_keys` / `highlight_keys` 组名已在
    /// `Config::normalize` 里折算进那张表）。本函数只认**已解析好的动作**，不解析动词
    /// 字符串——动词值域住在 `wind-config` 的 `SessionAction`，而本 crate 是它的下游；
    /// 在这里再写一份解析就是两处慢慢漂移。映射由协调器做，那里同时依赖两个 crate。
    ///
    /// ★ **顺序即优先级**：[`classify`](Self::classify) 用 `.find()` 取第一个匹配，同一个
    /// (键, shift) 被声明两次时**先来的赢**。旧实现按「page 组全部 push 完再 push highlight
    /// 组」建表，于是 `tab` 两组都配时 page 赢——调用方的折算必须复现这条，否则用户会
    /// 遇到「一直用的 Tab 突然从翻页变成移高亮」。
    ///
    /// 不认的键名**静默跳过**：本函数无日志依赖，告警由调用方在加载期发（那里才分得清
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
/// **不再供引导键使用**——引导键（临拼 / 特殊模式 / 临时 mix 的 `trigger_keys`）一律只认
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

/// 全部符号键的「规范名 + 组合区字符」。规范名取 [`KEY_TABLE`] 每行的**首个**别名。
///
/// 供跨仓边界回答「这个键在某方案里是不是码元」——设置页只认键名，而码元集是字符集，
/// 两边需要一张对照表。放在这里而不是让设置页自己拼：`KEY_TABLE` 增删时，那边不会
/// 跟着改，表现为新键的冲突提示永远不出（跨仓契约无编译期约束，本仓已栽过）。
pub fn symbol_keys() -> impl Iterator<Item = (&'static str, char)> {
    KEY_TABLE.iter().filter_map(|d| {
        let name = d.names.first()?;
        Some((*name, d.prefix))
    })
}

/// 纯修饰键名 → VK（`lshift` / `rshift` / `lctrl` / `rctrl`，别名见实现）。
/// 大小写与首尾空白不敏感；非修饰键返回 None。
///
/// # 为什么单开一张表，不并进 [`KEY_TABLE`]
///
/// `KEY_TABLE` 是**引导键**的配置解析入口（`trigger_keys` 五处、`special_trigger_vk`），
/// 而引导键走 keydown。修饰键在 keydown 上根本不工作——它必须走 keyup 轻敲
/// （keydown 不能吃、`Ctrl+A` 的第一下会误触发、按住会连触发，见
/// `KeyEventSink.cpp::_IsPureModifierKey`）。并进去就等于让引导键的设置项接受一个
/// 「配得上、永远不触发」的值，与临拼触发键里那个失效的 `z` 选项同型（已修，勿再造）。
///
/// 另一半理由是 `KeyDef.prefix`：修饰键没有字符，填什么都是假的，而 `vk_to_prefix_char`
/// 拿它写组合区。
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

/// VK → 组合区前缀字符。无映射时返回 None（调用方自定默认）。
pub fn vk_to_prefix_char(vk: u32) -> Option<char> {
    KEY_TABLE.iter().find(|d| d.vk == vk).map(|d| d.prefix)
}

/// 同 [`vk_to_prefix_char`]，但字母 VK 返回其小写字母。
///
/// 与 [`key_name_to_vk`] 拒绝字母**不矛盾**——两者方向与职责不同：
/// - `key_name_to_vk`（名字 → VK）是**配置解析**，必须严格。认了字母就等于允许把编码键
///   配成引导键，而全局配置无从表达「这张码表里它是死码」。
/// - `vk_to_prefix_char*`（VK → 字符）是**呈现**，字母有天然合法的显示形态。z 经方案级
///   `z_key_action` 进模式后，组合区要显示用户按下的那个 `z`；用不带字母的版本会得到
///   空前缀，用户看不到自己按了什么。
pub fn vk_to_prefix_char_with_letters(vk: u32) -> Option<char> {
    if let Some(c) = vk_to_prefix_char(vk) {
        return Some(c);
    }
    (VK_A..=VK_Z)
        .contains(&vk)
        .then(|| (b'a' + (vk - VK_A) as u8) as char)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_aliases_resolve() {
        assert_eq!(key_name_to_vk("backtick"), Some(VK_BACKTICK));
        assert_eq!(key_name_to_vk("grave"), Some(VK_BACKTICK));
        assert_eq!(key_name_to_vk("`"), Some(VK_BACKTICK));
        // 所有模式现在都认得 backslash（曾经的特殊模式不触发根因）。
        assert_eq!(key_name_to_vk("backslash"), Some(VK_BACKSLASH));
        assert_eq!(key_name_to_vk("\\"), Some(VK_BACKSLASH));
        assert_eq!(key_name_to_vk("EQUALS"), Some(VK_EQUAL)); // 大小写不敏感
        assert_eq!(key_name_to_vk(" semicolon "), Some(VK_SEMICOLON)); // 去空白
    }

    #[test]
    fn letters_only_with_letter_variant() {
        assert_eq!(key_name_to_vk("z"), None);
        assert_eq!(key_name_to_vk_with_letters("z"), Some(0x5A));
        assert_eq!(key_name_to_vk_with_letters("a"), Some(0x41));
        assert_eq!(key_name_to_vk_with_letters("backslash"), Some(VK_BACKSLASH));
    }

    /// 两个方向的严格度刻意不同：名字→VK 拒绝字母（配置解析，见
    /// `Coordinator::special_trigger_vk`），VK→字符接受字母（呈现，组合区要显示按下的 z）。
    #[test]
    fn prefix_char_accepts_letters_while_key_name_does_not() {
        assert_eq!(vk_to_prefix_char(VK_Z), None);
        assert_eq!(vk_to_prefix_char_with_letters(VK_Z), Some('z'));
        assert_eq!(vk_to_prefix_char_with_letters(VK_A), Some('a'));
        // 符号仍走 KEY_TABLE，两个版本一致。
        assert_eq!(vk_to_prefix_char_with_letters(VK_BACKTICK), Some('`'));
        assert_eq!(vk_to_prefix_char_with_letters(VK_SEMICOLON), Some(';'));
        // 非字母非符号（如数字键）两个版本都没有映射。
        assert_eq!(vk_to_prefix_char_with_letters(0x31), None);
    }

    /// 出厂默认折算后的绑定集（`pageupdown` + `minus_equal` / `arrows` + `tab`）。
    /// 顺序须与 `Config::migrate_nav_keys_into_session_actions` 的折算序一致：page 先、
    /// highlight 后——`classify` 用 `.find()`，顺序即优先级。
    fn default_nav_keys() -> NavKeys {
        NavKeys::from_binds([
            ("pageup", NavAction::PagePrev),
            ("pagedown", NavAction::PageNext),
            ("minus", NavAction::PagePrev),
            ("equal", NavAction::PageNext),
            ("up", NavAction::HighlightUp),
            ("down", NavAction::HighlightDown),
            ("shift+tab", NavAction::HighlightUp),
            ("tab", NavAction::HighlightDown),
        ])
    }

    #[test]
    fn nav_classify_config_driven() {
        let nk = default_nav_keys();
        // 专用导航键恒生效
        assert_eq!(
            nk.classify(VK_PRIOR, false, false),
            Some(NavAction::PagePrev)
        );
        assert_eq!(
            nk.classify(VK_NEXT, false, false),
            Some(NavAction::PageNext)
        );
        assert_eq!(
            nk.classify(VK_UP, false, false),
            Some(NavAction::HighlightUp)
        );
        assert_eq!(
            nk.classify(VK_DOWN, false, false),
            Some(NavAction::HighlightDown)
        );
        // tab=下移、shift+tab=上移
        assert_eq!(
            nk.classify(VK_TAB, false, false),
            Some(NavAction::HighlightDown)
        );
        assert_eq!(
            nk.classify(VK_TAB, true, false),
            Some(NavAction::HighlightUp)
        );
        // -/= 仅在 include_printable 时作翻页（码表模式 true，文本模式 false）
        assert_eq!(
            nk.classify(VK_MINUS, false, true),
            Some(NavAction::PagePrev)
        );
        assert_eq!(
            nk.classify(VK_EQUAL, false, true),
            Some(NavAction::PageNext)
        );
        assert_eq!(nk.classify(VK_MINUS, false, false), None);
        assert_eq!(nk.classify(VK_EQUAL, false, false), None);
    }

    #[test]
    fn comma_period_pages() {
        // 设置页提供 comma_period 选项，但组名曾未被识别（走 `_ => {}` 静默忽略）→
        // 无翻页绑定，逗号/句号落到标点臂直接上屏。
        let nk = NavKeys::from_binds([
            ("comma", NavAction::PagePrev),
            ("period", NavAction::PageNext),
        ]);
        assert_eq!(
            nk.classify(VK_COMMA, false, true),
            Some(NavAction::PagePrev)
        );
        assert_eq!(
            nk.classify(VK_PERIOD, false, true),
            Some(NavAction::PageNext)
        );
        // 可打印键：文本/表达式模式（临英/快捷输入）里仍作输入字符，不夺为翻页。
        assert_eq!(nk.classify(VK_COMMA, false, false), None);
        assert_eq!(nk.classify(VK_PERIOD, false, false), None);
    }

    /// 用户诉求一：Tab 向下翻页、Shift+Tab 向上翻页（把 Tab 从高亮组改到翻页组）。
    /// Tab 是功能键（`printable = false`），故在临英 / 快捷输入里同样生效——这与
    /// `-`/`=` 那类可打印键刻意不同。
    #[test]
    fn tab_can_be_rebound_to_paging() {
        let nk = NavKeys::from_binds([
            ("shift+tab", NavAction::PagePrev),
            ("tab", NavAction::PageNext),
        ]);
        assert_eq!(nk.classify(VK_TAB, false, false), Some(NavAction::PageNext));
        assert_eq!(nk.classify(VK_TAB, true, false), Some(NavAction::PagePrev));
    }

    /// 用户诉求二：CapsLock 向上翻页。解析层必须认得它，且必须被标成「只有 keyup
    /// 到得了服务端」——协调器据此把它送进 keyup 分支而非 keydown 链。
    #[test]
    fn capslock_resolves_as_key_up_only() {
        let k = session_key_name_to_vk("capslock").expect("capslock 应可解析");
        assert_eq!(k.vk, VK_CAPITAL);
        assert!(!k.printable, "CapsLock 不产出字符");
        assert!(
            is_key_up_only_vk(k.vk),
            "CapsLock 只有 keyup 到得了服务端；漏判会让绑定挂在永不触发的 keydown 上"
        );
        // ⚠️ 回归保护：CapsLock 与四个纯修饰键**不连号**，用区间判定会把它漏掉。
        assert!(!(VK_LSHIFT..=VK_RCONTROL).contains(&VK_CAPITAL));
    }

    /// `shift+` 前缀只认这一个修饰。带 ctrl/alt 的组合键归 `keys.key_actions`
    /// （key_down 热键表）——它们无会话时也要能触发，两张表的语义不同。
    #[test]
    fn session_key_names_reject_ctrl_alt_combos() {
        assert!(session_key_name_to_vk("ctrl+tab").is_none());
        assert!(session_key_name_to_vk("alt+tab").is_none());
        assert!(session_key_name_to_vk("ctrl+shift+tab").is_none());
        // 拼错的名字返回 None（由调用方在加载期告警），不静默变成别的键。
        assert!(session_key_name_to_vk("pgeup").is_none());
    }

    /// 符号键复用 `KEY_TABLE`，且必须带 `printable`——丢了这个标志，临英里就打不出减号。
    #[test]
    fn symbol_session_keys_are_printable() {
        for (name, vk) in [
            ("minus", VK_MINUS),
            ("equal", VK_EQUAL),
            ("lbracket", VK_LBRACKET),
            ("comma", VK_COMMA),
        ] {
            let k = session_key_name_to_vk(name).expect("符号键应可解析");
            assert_eq!(k.vk, vk);
            assert!(
                k.printable,
                "{name} 是可打印键，须在文本模式里回落为输入字符"
            );
        }
    }

    /// 同一个 (键, shift) 被声明两次时**先来的赢**——`classify` 用 `.find()`。
    /// 配置折算依赖这条来复现「page 组优先于 highlight 组」的历史行为。
    #[test]
    fn first_bind_wins_for_duplicate_key() {
        let nk = NavKeys::from_binds([
            ("tab", NavAction::PageNext),
            ("tab", NavAction::HighlightDown),
        ]);
        assert_eq!(nk.classify(VK_TAB, false, false), Some(NavAction::PageNext));
    }

    #[test]
    fn prefix_char_roundtrip() {
        assert_eq!(vk_to_prefix_char(VK_BACKTICK), Some('`'));
        assert_eq!(vk_to_prefix_char(VK_SLASH), Some('/'));
        assert_eq!(vk_to_prefix_char(VK_BACKSLASH), Some('\\'));
        assert_eq!(vk_to_prefix_char(0x41), None); // 字母无前缀定义
    }

    /// 修饰键走**独立**的解析入口，不进 `KEY_TABLE`——后者是引导键（keydown）的配置
    /// 解析口，而修饰键只在 keyup 轻敲上工作。混在一起就会让引导键设置项接受一个
    /// 配得上却永不触发的值（临拼触发键里那个失效的 `z` 选项就是这么来的，已修）。
    #[test]
    fn modifier_names_resolve_only_via_dedicated_entry() {
        assert_eq!(modifier_name_to_vk("rshift"), Some(VK_RSHIFT));
        assert_eq!(modifier_name_to_vk("RShift"), Some(VK_RSHIFT)); // 大小写不敏感
        assert_eq!(modifier_name_to_vk(" lctrl "), Some(VK_LCONTROL)); // 首尾空白不敏感
        assert_eq!(modifier_name_to_vk("rcontrol"), Some(VK_RCONTROL)); // 别名
        assert_eq!(modifier_name_to_vk("backslash"), None); // 符号键不归它管
        // 反向：引导键的解析口**不认**修饰键，否则 trigger_keys 里能配 rshift 却永不触发。
        assert_eq!(key_name_to_vk("rshift"), None);
        assert_eq!(key_name_to_vk_with_letters("rshift"), None);
    }

    /// 纯修饰键判定覆盖四个连号，且不误伤相邻 VK。
    #[test]
    fn pure_modifier_vk_range_is_exact() {
        for vk in [VK_LSHIFT, VK_RSHIFT, VK_LCONTROL, VK_RCONTROL] {
            assert!(is_pure_modifier_vk(vk), "0x{vk:02X} 应是纯修饰键");
        }
        assert!(!is_pure_modifier_vk(0x9F)); // 区间下界之前
        assert!(!is_pure_modifier_vk(0xA4)); // VK_LMENU，区间上界之后
        assert!(!is_pure_modifier_vk(VK_Z));
    }

    /// `symbol_keys()` 的键名是**跨仓契约**：设置页按这些名字匹配自己的下拉选项
    /// （`TRIGGER_KEY_OPTIONS`），对不上就表现为「提示永远不出」——静默降级，没人会发现。
    ///
    /// 快照式断言而非「非空即可」：改 `KEY_TABLE` 某行的**首个**别名不会破坏本仓任何
    /// 功能（其余别名仍能解析），却会静默切断那条契约。这里红一下，提醒去同步设置页。
    #[test]
    fn symbol_key_names_are_a_cross_repo_contract() {
        let names: Vec<&str> = symbol_keys().map(|(n, _)| n).collect();
        assert_eq!(
            names,
            vec![
                "backtick",
                "semicolon",
                "quote",
                "comma",
                "period",
                "slash",
                "lbracket",
                "rbracket",
                "backslash",
                "minus",
                "equal",
            ],
            "改动此列表须同步 wind-setting 的 TRIGGER_KEY_OPTIONS / key_options"
        );
        // 字符侧也一并锁住：core 用它判「该键是不是码元」，错一个就报错冲突。
        let by_name = |n: &str| symbol_keys().find(|(k, _)| *k == n).map(|(_, c)| c);
        assert_eq!(by_name("backtick"), Some('`'));
        assert_eq!(by_name("backslash"), Some('\\'));
        assert_eq!(by_name("semicolon"), Some(';'));
    }
}
