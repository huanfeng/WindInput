//! 热键编译器
//!
//! 与 Go 版本 `wind_input/internal/hotkey/compiler.go` 对齐。
//! 将配置中的热键字符串（如 "Ctrl+Space"、"lshift"）编译为 key_hash，
//! 用于按键事件中的热键匹配。

use crate::config::Config;
use tracing::{debug, warn};

/// `keys.key_actions` 里组合键条目的策略表：动词 → `(分发端 action, 策略位)`；
/// 解析不出 `BoundAction` 的动词返回 `None`。
///
/// 值域就是 [`crate::BoundAction`]，与单键、修饰键两条通路同一张表；`add_word` 一类要激活
/// C++ 全键转发的动词不在值域内，自然落 `None`。策略位按动词分（见 [`hotkey_policy_for`]），
/// 且须与 `Compiler::compile` 里固定字段那几段逐条一致，否则同一功能经两条路进表行为不同：
///
/// | 动词 | 策略位 | 理由 |
/// |---|---|---|
/// | `toggle_schema:` / `switch_schema:` / `toggle_mode` / `switch_engine` | 无 | 用来离开/返回英文态，带 `CHINESE_ONLY` 就成了单程票 |
/// | `toggle_full_width` / `toggle_toolbar` / `open_settings` / `open_dictionary` / `take_screenshot` | 无 | 两模式下都该生效 |
/// | `toggle_punct` / `toggle_s2t` / `toggle_t2s` / `single_char` | `CHINESE_ONLY` | 只在中文态有意义；不带 `GLOBAL`，免得抢占宿主快捷键 |
/// | 进 overlay 的（`temp_pinyin` / `temp_english` / `aux_code` / `rare_char` / `reverse` / `mix:` / `special:`） | `CHINESE_ONLY \| GLOBAL` | `GLOBAL` 让 TSF 用 `RegisterHotKey` 抢占，穿透 Chromium 宿主的同名加速键 |
/// | `command:<表达式>` | 无 | 与中英态无关，不改输入状态 |
/// | `softkeyboard[:<id>]` | `GLOBAL` | 与中英态无关（C++ 侧有软键盘总闸 `IsSoftKeyboard()`）；带 `CHINESE_ONLY` 英文态开不出来 |
///
/// 动词原样进表（只 trim，不规范化），分派端同样用 `BoundAction::parse` 认串；别拿表里的
/// `action` 跟字面量直接比，大写写法会对不上。值域与沿革见 docs/design/schema-key-actions.md。
pub(crate) fn hotkey_action_entry(action: &str) -> Option<(String, u32)> {
    let action = action.trim();
    let policy = hotkey_policy_for(&crate::config::BoundAction::parse(action))?;
    Some((action.to_string(), policy))
}

/// 组合键动词的策略位。`BoundAction::None`（含解析不出的未知动词）返回 `None` ⇒ 不进表。
///
/// - `CHINESE_ONLY`：复用 [`crate::BoundAction::only_in_chinese_mode`]，与分派端守卫同源，别重写。
/// - `GLOBAL`：只给会被 Chromium 类宿主同名加速键抢走的（进 overlay 的一族与软键盘）。槽位上限
///   16（`GLOBAL_HOTKEY_SLOTS`），给位要克制。
fn hotkey_policy_for(action: &crate::config::BoundAction) -> Option<u32> {
    use crate::config::BoundAction as BA;
    if matches!(action, BA::None) {
        return None;
    }
    let mut policy = 0;
    if action.only_in_chinese_mode() {
        policy |= HOTKEY_POLICY_CHINESE_ONLY;
    }
    // 穷举 `match` 而非 `matches!`：新增变体时编译不过，逼着决定要不要 GLOBAL（漏给只在
    // Chromium 宿主里按不出来，极难归因）。
    let needs_global = match action {
        BA::TempPinyin
        | BA::TempEnglish
        | BA::AuxCode
        | BA::RareChar
        | BA::Reverse
        | BA::Mix(_)
        | BA::Special(_)
        | BA::SoftKeyboard(_) => true,
        // 不抢占：不进 overlay，被宿主加速键抢走代价小，而 GLOBAL 槽位有限。
        BA::None
        | BA::SingleChar(_)
        | BA::CommentState(_)
        | BA::ToggleSchema(_)
        | BA::SwitchSchema(_)
        | BA::Action(_)
        | BA::Command(_) => false,
    };
    if needs_global {
        policy |= HOTKEY_POLICY_GLOBAL;
    }
    Some(policy)
}

/// `keys.key_actions` 的一条条目该走哪条通路。由键的形态决定，不由动词决定。
///
/// 三条通路各有各的到达条件，判据见 docs/design/schema-key-actions.md §4.1 与 §4.4：
///
/// | 形态 | 通路 | 为什么 |
/// |---|---|---|
/// | 组合键（带 Ctrl/Alt/Shift/Win） | key_down 热键 → `dispatch_hotkey` | 不与输入争键，可全局拦截 |
/// | 纯修饰键（`rshift`） | key_up 轻敲 | keydown 不能吃（宿主要看到修饰键），只能在干净单击的 keyup 上判 |
/// | 单个有字符的键（`backtick`） | keydown 引导键链 | 英文模式下必须让它出字，故排在分水岭之后 |
/// | 裸 F1–F12 | key_down 热键 | 不产出字符，空闲时 TSF 只有靠热键表才会转发 |
///
/// 单个有字符的键绝不能编译进 key_down 热键表：TSF 会把它当热键吞掉，该符号就打不出来了。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyActionRoute {
    /// 组合键：编译进 key_down 热键表。
    Hotkey,
    /// 纯修饰键：编译进 key_up 转发集。
    ModifierKeyUp,
    /// 单个有字符的键：不进任何热键表，由引导键链查表消费。
    LeadingKey,
}

/// 按键名判定通路。无法解析的键名返回 `None`（调用方 warn 后忽略）。
pub fn route_of_key_action(key: &str) -> Option<KeyActionRoute> {
    let raw = parse_hotkey(key)?;
    let has_modifier = (raw >> 16) & MOD_GENERIC_MASK != 0;
    if has_modifier {
        return Some(KeyActionRoute::Hotkey);
    }
    let vk = raw & 0xFFFF;
    if (VK_LSHIFT..=VK_RCONTROL).contains(&vk) {
        return Some(KeyActionRoute::ModifierKeyUp);
    }
    // 裸 F1–F12 不产出字符，进热键表的代价只是「中文模式下该键归输入法」（策略位
    // `CHINESE_ONLY`），不会像字符键那样打不出字；不进表的话 TSF 在空闲时根本不转发它。
    if (VK_F1..=VK_F12).contains(&vk) {
        return Some(KeyActionRoute::Hotkey);
    }
    Some(KeyActionRoute::LeadingKey)
}

/// 不带修饰键的 F1–F12。
pub fn is_bare_function_key(vk: u32, modifiers: u32) -> bool {
    modifiers & MOD_GENERIC_MASK == 0 && (VK_F1..=VK_F12).contains(&vk)
}

/// 裸 F1–F12（不带修饰键）绑的动作是否只在中文模式吃键。
///
/// 这类键没有字符产出，宿主自己常用它（F1 帮助、F5 刷新、F11 全屏）；英文模式下输入法不该
/// 夺走。唯一的例外是「离开 / 返回英文态」那几类动词（`toggle_mode` / `switch_engine` /
/// `toggle_schema:` / `switch_schema:`）：带上 `CHINESE_ONLY` 就成了英文态切不回来的单程票。
/// 编译端（策略位）与分派端（服务端守卫）共用本判据，两处不会分叉。
pub fn bare_function_key_needs_chinese(vk: u32, modifiers: u32, action: &str) -> bool {
    use crate::config::BoundAction as BA;
    if !is_bare_function_key(vk, modifiers) {
        return false;
    }
    !matches!(
        BA::parse(action.trim()),
        BA::ToggleSchema(_) | BA::SwitchSchema(_)
    ) && !matches!(
        BA::parse(action.trim()),
        BA::Action(a) if matches!(a.as_str(), "toggle_mode" | "switch_engine")
    )
}

/// 修饰键常量（与 wind-ipc MOD_* / Go ipc.Mod* 对齐）
const MOD_SHIFT: u32 = 0x0001;
const MOD_CTRL: u32 = 0x0002;
const MOD_ALT: u32 = 0x0004;
const MOD_WIN: u32 = 0x0008;
const MOD_LSHIFT: u32 = 0x0010;
const MOD_RSHIFT: u32 = 0x0020;
const MOD_LCTRL: u32 = 0x0040;
const MOD_RCTRL: u32 = 0x0080;
const MOD_CAPSLOCK: u32 = 0x0100;

/// 通用修饰位掩码（ctrl/shift/alt/win），用于规范化匹配
pub const MOD_GENERIC_MASK: u32 = MOD_SHIFT | MOD_CTRL | MOD_ALT | MOD_WIN;

/// 热键策略位（高位，发给 TSF；与 Go ipc.HotkeyPolicy* / TSF HOTKEY_POLICY_* 对齐）
const HOTKEY_POLICY_CHINESE_ONLY: u32 = 0x40000000;
const HOTKEY_POLICY_SESSION: u32 = 0x80000000;
/// 全局拦截位（可与 CHINESE_ONLY 叠加）：TSF 在「中文 + 文本框」时用 `RegisterHotKey` 注册成
/// 系统热键，规避 Chromium 类宿主无视 `pfEaten` 的加速键双处理。
const HOTKEY_POLICY_GLOBAL: u32 = 0x20000000;
/// 「仅注册转发」标记：翻页 / 选词键组这类 action 为空的登记项，有会话时转发给引擎，无会话时
/// 放行给 TSF 下游按普通标点处理。
///
/// 真动作热键绝不能带此位：TSF 的「无 Ctrl/Alt 且无会话就不吃」闸门只认它，带上的动作热键
/// （如 `shift+space`）在空缓冲时会被放行而无人接手，严格 TSF 宿主下功能失效。
const HOTKEY_POLICY_FORWARD_ONLY: u32 = 0x10000000;

/// Windows 虚拟键码（toggle / select / page 用）
const VK_F1: u32 = 0x70;
const VK_F12: u32 = 0x7B;
const VK_LSHIFT: u32 = 0xA0;
const VK_RSHIFT: u32 = 0xA1;
const VK_LCONTROL: u32 = 0xA2;
const VK_RCONTROL: u32 = 0xA3;
const VK_CAPITAL: u32 = 0x14;
const VK_TAB: u32 = 0x09;
const VK_SPACE: u32 = 0x20;
const VK_PRIOR: u32 = 0x21;
const VK_NEXT: u32 = 0x22;
const VK_OEM_1: u32 = 0xBA; // ;
const VK_OEM_7: u32 = 0xDE; // '
const VK_OEM_COMMA: u32 = 0xBC; // ,
const VK_OEM_PERIOD: u32 = 0xBE; // .
const VK_OEM_MINUS: u32 = 0xBD; // -
const VK_OEM_PLUS: u32 = 0xBB; // =
const VK_OEM_4: u32 = 0xDB; // [
const VK_OEM_6: u32 = 0xDD; // ]
const VK_END: u32 = 0x23;
const VK_HOME: u32 = 0x24;
const VK_LEFT: u32 = 0x25;
const VK_UP: u32 = 0x26;
const VK_RIGHT: u32 = 0x27;
const VK_DOWN: u32 = 0x28;
const VK_OEM_2: u32 = 0xBF; // /
const VK_OEM_3: u32 = 0xC0; // `
const VK_OEM_5: u32 = 0xDC; // \

/// `keys.session_actions` 里 keyup 类绑定的 action 名。
///
/// 必须与 `toggle_mode` 区分开：`is_toggle_mode_keycode` 按 action 过滤，复用的话把 CapsLock
/// 配成翻页键的用户空闲敲它会莫名切中英文。每次往 `key_up` 加东西都要重查这条。
pub const SESSION_ACTION: &str = "session_action";

/// `f1`..`f12`（小写、无前导零）。
fn is_function_key_name(name: &str) -> bool {
    name.strip_prefix('f')
        .and_then(|n| n.parse::<u32>().ok())
        .is_some_and(|n| (1..=12).contains(&n) && name.len() == 1 + n.to_string().len())
}

/// 注释切换键（Ctrl 系）在 `key_up` 表里的 action 名。不得复用 `toggle_mode`，理由同
/// [`SESSION_ACTION`]。
pub const COMMENT_SWITCH_ACTION: &str = "comment_switch";

/// 注释切换键要登记的 keyup hash：注释开着、切换方式不是 `off`、切换键是 Ctrl 系时才有。
/// Alt 系不走 TSF 白名单（mac / Linux 组字中本就转发 Alt）；F1 走会话按键表（keydown）。
fn comment_switch_key_up_hashes(c: &crate::comment_lib::UiCommentConfig) -> Vec<u32> {
    use crate::comment_lib::SwitchMode;
    if !c.enabled || c.switch_mode == SwitchMode::Off || !c.switch_key.is_ctrl() {
        return Vec::new();
    }
    [VK_LCONTROL, VK_RCONTROL]
        .into_iter()
        .filter(|&vk| c.switch_key.matches_vk(vk))
        .filter_map(compile_modifier_key_up_hash)
        .collect()
}

/// 会话态键名 → (VK, 是否需 Shift)。支持单个 `shift+` 前缀。
///
/// 这是 `wind_keys::keymap::session_key_name_to_vk` 的第二份实现：`wind-config` 依赖
/// `wind-keys` 会成环。两份表的一致性靠 `wind-coordinator` 的
/// `session_key_tables_agree_across_crates` 守门。
pub fn session_key_to_vk(name: &str) -> Option<(u32, bool)> {
    let raw = name.trim().to_lowercase();
    let (shift, base) = match raw.strip_prefix("shift+") {
        Some(rest) => (true, rest.trim()),
        None => (false, raw.as_str()),
    };
    let vk = match base {
        "tab" => VK_TAB,
        "pageup" | "pgup" | "prior" => VK_PRIOR,
        "pagedown" | "pgdn" | "next" => VK_NEXT,
        "up" => VK_UP,
        "down" => VK_DOWN,
        "left" => VK_LEFT,
        "right" => VK_RIGHT,
        "home" => VK_HOME,
        "end" => VK_END,
        "capslock" | "caps" => VK_CAPITAL,
        "lshift" => VK_LSHIFT,
        "rshift" => VK_RSHIFT,
        "lctrl" | "lcontrol" => VK_LCONTROL,
        "rctrl" | "rcontrol" => VK_RCONTROL,
        // F1–F12：宿主另有原义（帮助 / 重命名…）但组合进行中几乎不会用到，且不产出字符。
        // 键名表另一半在 `wind_keys::keymap::SESSION_FUNCTION_KEYS`，两处同步。
        f if is_function_key_name(f) => 0x70 + f[1..].parse::<u32>().ok()? - 1,
        "semicolon" | ";" => VK_OEM_1,
        "quote" | "'" => VK_OEM_7,
        "comma" | "," => VK_OEM_COMMA,
        "period" | "." => VK_OEM_PERIOD,
        "minus" | "-" => VK_OEM_MINUS,
        "equal" | "equals" | "=" => VK_OEM_PLUS,
        "lbracket" | "[" => VK_OEM_4,
        "rbracket" | "]" => VK_OEM_6,
        "slash" | "/" => VK_OEM_2,
        "backtick" | "grave" | "`" => VK_OEM_3,
        "backslash" | "\\" => VK_OEM_5,
        // 字母只收 z：会话态查表排在字母臂之前，收进来的字母在有候选时会被夺走。z 在多数码表里
        // 最边缘、用户也习惯它兼职功能键；别放开成全部字母，那等于静默废掉活码前缀。无会话时
        // 不受影响（导航类动词带 `requires_candidates` 守卫）。
        "z" => 0x5A,
        _ => return None,
    };
    Some((vk, shift))
}

/// keyup-only 键（CapsLock / 四个纯修饰键）的 keyup hash；其余键返回 `None`。
///
/// 这批键只能走 keyup 轻敲，判据同 `is_key_up_only_vk`。CapsLock 与四个修饰键不连号，
/// 用区间判定会漏掉它。
fn session_key_up_hash(vk: u32) -> Option<u32> {
    if vk == VK_CAPITAL {
        return Some(key_hash(MOD_CAPSLOCK, VK_CAPITAL));
    }
    compile_modifier_key_up_hash(vk)
}

/// 单个编译后的热键条目
#[derive(Debug, Clone)]
pub struct HotkeyEntry {
    /// 发给 TSF 的 hash（含 policy 高位），用于白名单匹配/转发决策
    pub tsf_hash: u32,
    /// 服务端匹配用的 hash（不含 policy、修饰位为通用位），与规范化后的入站事件比对
    pub match_hash: u32,
    /// 动作名称（用于 dispatch；空串表示仅注册转发、由常规按键逻辑处理）
    pub action: String,
}

/// 编译后的热键集合
#[derive(Debug, Clone, Default)]
pub struct CompiledHotkeys {
    pub key_down: Vec<HotkeyEntry>,
    pub key_up: Vec<HotkeyEntry>,
}

impl CompiledHotkeys {
    /// 发给 TSF 的 key_down hash 列表（含 policy 位）
    pub fn key_down_tsf_hashes(&self) -> Vec<u32> {
        self.key_down.iter().map(|e| e.tsf_hash).collect()
    }
    /// 发给 TSF 的 key_up hash 列表
    pub fn key_up_tsf_hashes(&self) -> Vec<u32> {
        self.key_up.iter().map(|e| e.tsf_hash).collect()
    }
    /// 在 key_down 集合中按规范化 hash 查找动作
    pub fn match_key_down(&self, normalized_hash: u32) -> Option<&str> {
        self.key_down
            .iter()
            .find(|e| e.match_hash == normalized_hash)
            .map(|e| e.action.as_str())
    }

    /// 在 key_down 集合中按规范化 hash 查吃键策略。供 `Coordinator::should_handle_key` 用；
    /// 策略位在本文件定义，解码也放这里。
    pub fn key_down_policy(&self, normalized_hash: u32) -> Option<KeyDownPolicy> {
        self.key_down
            .iter()
            .find(|e| e.match_hash == normalized_hash)
            .map(|e| KeyDownPolicy::from_tsf_hash(e.tsf_hash))
    }
}

/// key_down 白名单命中后的吃键策略（`tsf_hash` 高位编码的语义）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyDownPolicy {
    /// 中英两模式都吃
    Always,
    /// 仅中文模式吃；英文模式放行给宿主（吃掉 Ctrl+= 会让宿主的放大失效）
    ChineseOnly,
    /// 仅中文模式 + 有会话时吃（置顶/删词，组合键见 [`number_template_mods`]；
    /// 无会话时宿主可能另有用途）。
    ///
    /// TSF 侧 `_RegisterCandidateHotkeys` 会在候选可见期间把这些条目 `RegisterHotKey` 成系统
    /// 热键，那才是实际生效的通路。改本策略位的产出前先读
    /// `docs/design/key-resolver-unification.md` §2.2。
    Session,
    /// 仅注册转发（翻页键组 `-=`、选词键组 `;'`）：无会话时放行并继续按常规按键逻辑判定，
    /// 中文模式下它们要当标点处理。
    ForwardOnly,
}

impl KeyDownPolicy {
    fn from_tsf_hash(tsf_hash: u32) -> Self {
        if tsf_hash & HOTKEY_POLICY_SESSION != 0 {
            Self::Session
        } else if tsf_hash & HOTKEY_POLICY_CHINESE_ONLY != 0 {
            Self::ChineseOnly
        } else if tsf_hash & HOTKEY_POLICY_FORWARD_ONLY != 0 {
            Self::ForwardOnly
        } else {
            Self::Always
        }
    }
}

/// 计算 key_hash（与 wind-ipc::protocol::calc_key_hash 对齐）
fn key_hash(modifiers: u32, key_code: u32) -> u32 {
    (modifiers << 16) | (key_code & 0xFFFF)
}

/// 把一条会话态键名编译成 TSF 转发条目。返回 `(该进 key_up 表吗, 条目)`。
///
/// 全局 `keys.session_actions` 与方案级 `[session_actions]` 共用本函数，是分流规则的单一来源。
/// 按键的形态分两条路：
/// - keyup-only 键（CapsLock / 纯修饰键）→ `key_up`，带 `SESSION` 位，让 C++ 区分
///   「toggle 语义」（恒吃 keydown）与「会话语义」（仅有会话时吃）；
/// - 其余（功能键 + 可打印符号键）→ `key_down`，带 `FORWARD_ONLY` 位。
///
/// `FORWARD_ONLY` 必须保留「无会话时放行给下游」：这里收的键含可打印符号，吃掉就是丢键
/// （见 `docs/design/key-resolver-unification.md` §8 注意点 5）。
pub fn compile_session_key(name: &str) -> Option<(bool, HotkeyEntry)> {
    let (vk, shift) = session_key_to_vk(name)?;
    if let Some(hash) = session_key_up_hash(vk) {
        Some((
            true,
            HotkeyEntry {
                tsf_hash: hash | HOTKEY_POLICY_SESSION,
                match_hash: hash,
                action: SESSION_ACTION.to_string(),
            },
        ))
    } else {
        let raw = key_hash(if shift { MOD_SHIFT } else { 0 }, vk);
        Some((
            false,
            HotkeyEntry {
                tsf_hash: raw | HOTKEY_POLICY_FORWARD_ONLY,
                match_hash: raw,
                action: String::new(),
            },
        ))
    }
}

/// 热键编译器
pub struct Compiler {
    config: Config,
}

impl Compiler {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    /// 编译配置中的热键为 CompiledHotkeys（对齐 Go compiler.go::Compile）
    pub fn compile(&self) -> CompiledHotkeys {
        let mut result = CompiledHotkeys::default();
        let h = &self.config.keys;

        // ── KeyDown：两模式都吃（无 policy 位） ──
        for (name, value) in [
            ("switch_engine", &h.switch_engine),
            ("toggle_full_width", &h.toggle_full_width),
            ("toggle_toolbar", &h.toggle_toolbar),
            ("open_settings", &h.open_settings),
            ("open_dictionary", &h.open_dictionary),
            ("take_screenshot", &h.take_screenshot),
        ] {
            if let Some(raw) = parse_hotkey(value) {
                result.key_down.push(HotkeyEntry {
                    tsf_hash: raw,
                    match_hash: raw,
                    action: name.to_string(),
                });
            }
        }

        // ── KeyDown：仅中文模式吃（HOTKEY_POLICY_CHINESE_ONLY） ──
        for (name, value) in [
            ("toggle_punct", &h.toggle_punct),
            ("add_word", &h.add_word),
            ("open_add_word_dialog", &h.open_add_word_dialog),
            ("toggle_s2t", &h.toggle_s2t),
            ("toggle_t2s", &h.toggle_t2s),
        ] {
            if let Some(raw) = parse_hotkey(value) {
                // 加词类热键额外叠加 GLOBAL 位以穿透 Chromium 类宿主；其余 chinese-only 键不抢占。
                let policy = if matches!(name, "add_word" | "open_add_word_dialog") {
                    HOTKEY_POLICY_CHINESE_ONLY | HOTKEY_POLICY_GLOBAL
                } else {
                    HOTKEY_POLICY_CHINESE_ONLY
                };
                result.key_down.push(HotkeyEntry {
                    tsf_hash: raw | policy,
                    match_hash: raw,
                    action: name.to_string(),
                });
            }
        }

        // ── KeyDown：软键盘开关（GLOBAL，不带 CHINESE_ONLY） ──
        // 策略位与 `key_actions` 里 `softkeyboard` 动词逐位相同（理由见 `hotkey_action_entry`），
        // action 用动词原文，与那条通路落到同一个分派臂。两条通路可能同时登记同一个键：
        // `match_key_down` 先注册者赢，本分支在前，行为一致、不重复分派。
        if let Some(raw) = parse_hotkey(&h.softkeyboard) {
            result.key_down.push(HotkeyEntry {
                tsf_hash: raw | HOTKEY_POLICY_GLOBAL,
                match_hash: raw,
                action: "softkeyboard".to_string(),
            });
        }

        // ── KeyDown：临拼直达热键（CHINESE_ONLY | GLOBAL，与加词键同策略） ──
        // 与引导键共存：热键路径进入时组合区不写引导符（分发点传 key_code=0）。action 用动词原文，
        // 与 `key_actions` 里写 `temp_pinyin` 落到同一个分派臂。特殊模式的直达热键已收编进
        // `keys.key_actions`（`special:<方案id>`），不在这里。
        let mode_policy = HOTKEY_POLICY_CHINESE_ONLY | HOTKEY_POLICY_GLOBAL;
        if let Some(raw) = parse_hotkey(&self.config.input.temp_pinyin.hotkey) {
            result.key_down.push(HotkeyEntry {
                tsf_hash: raw | mode_policy,
                match_hash: raw,
                action: "temp_pinyin".to_string(),
            });
        }

        // 方案直达热键没有独立编译段：`keys.schema_hotkeys` 已废弃（加载期由
        // `Config::warn_legacy_schema_hotkeys` 告警后清空），改写进 `keys.key_actions` 的
        // `switch_schema:<id>`。别在这里加回来：独立段会抢先注册而让 key_actions 同键静默失效，
        // 且不问键形态，单字符键进表会被 TSF 吞掉。

        // ── KeyDown：按键功能表（keys.key_actions）──
        // 策略位逐动词取自 `hotkey_action_entry`，本段不自定。BTreeMap 遍历有序，撞键时的胜者在
        // 任何进程里都一致。
        for (key, action) in &h.key_actions {
            let action = action.trim();
            if key.is_empty() || action.is_empty() {
                continue;
            }
            let Some(route) = route_of_key_action(key) else {
                warn!("keys.key_actions: 键 {key:?} 解析失败，忽略");
                continue;
            };
            let raw = match parse_hotkey(key) {
                Some(r) => r,
                None => continue, // route_of_key_action 已解析成功，此处不可达
            };
            match route {
                KeyActionRoute::Hotkey => {
                    // 显式 `none` 用来压制更低层（出厂 / 定制版）的同键绑定：层叠合并表达不了删除。
                    // 必须在取策略位之前静默跳过：它与写错的动词在 `BoundAction::parse` 眼里都是 `None`，
                    // 落到下面会被误报成配置错误。
                    if action.eq_ignore_ascii_case("none") {
                        debug!("keys.key_actions: {key:?} 显式禁用（none），不进热键表");
                        continue;
                    }
                    let Some((dispatch_action, policy)) = hotkey_action_entry(action) else {
                        warn!(
                            "keys.key_actions: 认不得动词 {action:?}（键 {key:?}），忽略。\
                             组合键与单键同一值域（BoundAction），多半是拼错了"
                        );
                        continue;
                    };
                    // 裸 F 键：英文模式归宿主（见 `bare_function_key_needs_chinese`）。
                    let policy = if bare_function_key_needs_chinese(
                        raw & 0xFFFF,
                        (raw >> 16) & MOD_GENERIC_MASK,
                        action,
                    ) {
                        policy | HOTKEY_POLICY_CHINESE_ONLY
                    } else {
                        policy
                    };
                    result.key_down.push(HotkeyEntry {
                        // match_hash 恒不含策略位：策略位只给 TSF 看，服务端匹配只认裸 hash。
                        tsf_hash: raw | policy,
                        match_hash: raw,
                        action: dispatch_action,
                    });
                }
                // 修饰键：只登记转发，动作由服务端按 `BoundAction` 裁决。
                // action 用 `schema_bound` 而非动词本身——`is_toggle_mode_keycode` 按 action
                // 过滤，塞进动词会让它认不出来（那条判据只认 `toggle_mode`）。
                KeyActionRoute::ModifierKeyUp => {
                    if let Some(hash) = compile_modifier_key_up_hash(raw & 0xFFFF) {
                        result.key_up.push(HotkeyEntry {
                            tsf_hash: hash,
                            match_hash: hash,
                            action: "schema_bound".to_string(),
                        });
                    }
                }
                // 单个有字符的键不进任何热键表（进了会被 TSF 吞掉），由引导键链（`bound_action_for`）消费。
                KeyActionRoute::LeadingKey => {}
            }
        }

        // ── KeyDown：数字模板展开（PinCandidate / DeleteCandidate，session policy） ──
        for tmpl in [&h.pin_candidate, &h.delete_candidate] {
            for entry in compile_number_hotkey(tmpl) {
                result.key_down.push(entry);
            }
        }

        // ── KeyDown：上屏注释 / 拼音（`input.alt_commit`，SESSION policy）──
        // Alt+0..9 与 Alt+Space 只带 SESSION 位：TSF 只在「中文 + 有会话」时吃，其余时候归宿主
        // （菜单加速键、窗口系统菜单）。action 为空＝仅转发，语义在协调器 `try_alt_commit`。
        // 必须排在 key_actions 与数字模板之后，且撞键就不登记：TSF 白名单按策略位分桶，同一 hash
        // 再进 SESSION 桶会改变用户绑定的吃键条件。
        if crate::config::AltCommit::effective(&self.config) != crate::config::AltCommit::Off {
            for vk in std::iter::once(VK_SPACE).chain(0x30..=0x39) {
                let raw = key_hash(MOD_ALT, vk);
                if result.key_down.iter().any(|e| e.match_hash == raw) {
                    debug!("input.alt_commit: Alt+0x{vk:02X} 已被其它绑定占用，让位");
                    continue;
                }
                result.key_down.push(HotkeyEntry {
                    tsf_hash: raw | HOTKEY_POLICY_SESSION,
                    match_hash: raw,
                    action: String::new(),
                });
            }
        }

        // ── 会话态按键功能表（keys.session_actions）──
        // 数据源是 `effective_session_actions()`（键组展开 ⊕ `session_actions`），不能直接读
        // `config.keys.session_actions`，否则漏掉键组展开的键。分流见 `compile_session_key`。
        // 功能键（Tab / PgUp / 方向键）C++ 侧有会话时本就转发，仍全部登记，不改跨进程行为。
        for (name, verb) in &self.config.effective_session_actions() {
            if !crate::config::SessionAction::parse(verb).is_enabled() {
                continue;
            }
            let Some((to_key_up, entry)) = compile_session_key(name) else {
                continue;
            };
            if to_key_up {
                result.key_up.push(entry);
            } else {
                result.key_down.push(entry);
            }
        }

        // ── Ctrl+←/→：组码中整音节跳（rime 同款），仅在有会话时转发 ──
        // 会话键表不带 Ctrl，故单独登记；协调器侧的 Ctrl 分支与这里同看
        // `session_actions` 里的 `ctrl+left` / `ctrl+right = "none"`（写了就既不转发也不处理）。
        for (name, vk) in [("ctrl+left", VK_LEFT), ("ctrl+right", VK_RIGHT)] {
            if self
                .config
                .keys
                .session_actions
                .get(name)
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("none"))
            {
                continue;
            }
            let raw = key_hash(MOD_CTRL, vk);
            result.key_down.push(HotkeyEntry {
                tsf_hash: raw | HOTKEY_POLICY_FORWARD_ONLY,
                match_hash: raw,
                action: String::new(),
            });
        }

        // ── KeyUp：toggle 模式键（Shift/Ctrl/CapsLock） ──
        // 关键：必须带通用位+具体位，与 Go compileToggleModeKey 一致，
        // 因为 C++ GetCurrentModifiers() 对修饰键同时返回通用与具体位。
        for key in &h.toggle_mode_keys {
            if let Some(hash) = compile_toggle_mode_key(key) {
                result.key_up.push(HotkeyEntry {
                    tsf_hash: hash,
                    match_hash: hash,
                    action: "toggle_mode".to_string(),
                });
            }
        }

        // ── KeyUp：注释切换键（`ui.comment.switch_key` 选 Ctrl 系）──
        // TSF 判无会话、服务端却有候选时的退路：不登记就收不到 Ctrl 的松开。无候选时服务端回
        // PassThrough。已被上面登记过的键不再追加。
        for hash in comment_switch_key_up_hashes(&self.config.ui.comment) {
            if result.key_up.iter().any(|e| e.match_hash == hash) {
                continue;
            }
            result.key_up.push(HotkeyEntry {
                tsf_hash: hash | HOTKEY_POLICY_SESSION,
                match_hash: hash,
                action: COMMENT_SWITCH_ACTION.to_string(),
            });
        }

        // 同一个键可能既是切换键又是选词键（同 hash 两条登记）：TSF 白名单是集合、重复无害；
        // 服务端切换只认 action=="toggle_mode"，别用「key_up 里有这个 key_code」当切换判据。

        warn_global_hotkey_overflow(&result.key_down);

        debug!(
            "Compiled hotkeys: {} key_down, {} key_up",
            result.key_down.len(),
            result.key_up.len()
        );
        result
    }
}

/// Win32 `RegisterHotKey` 的槽位上限：带 `GLOBAL` 位的条目超过这么多就有一部分注册不上。
///
/// 与 `wind_tsf/src/TextService.cpp` 的 `kHotkeyIdAddWordBase + 16` 对齐；那边超出后静默
/// `break`，且翻不回键名。
const GLOBAL_HOTKEY_SLOTS: usize = 16;

/// 带 `GLOBAL` 位的条目超过槽位上限时告警，并点名会失效的那几个。
///
/// 这里是唯一既知道数量、又拿着用户键名的地方。触发真实可达：`keys.key_actions` 每绑一个
/// 进 overlay 的动词就多一条 GLOBAL。
fn warn_global_hotkey_overflow(key_down: &[HotkeyEntry]) {
    let (total, dropped) = global_hotkey_overflow(key_down);
    if dropped.is_empty() {
        return;
    }
    warn!(
        "全局拦截热键共 {total} 个，超过系统槽位上限 {GLOBAL_HOTKEY_SLOTS}：{dropped:?} \
         这几个注册不上，在 QQNT / Tabby 等宿主里会被同名加速键抢走（普通转发仍然有效）"
    );
}

/// 超限判定本体（抽出来以便测试）。返回 `(带 GLOBAL 的总数, 注册不上的那些的动词)`；
/// 超出的是靠后的那些，TSF 按 `GlobalHotkeys()` 的顺序注册，填满即 break。
fn global_hotkey_overflow(key_down: &[HotkeyEntry]) -> (usize, Vec<&str>) {
    let globals: Vec<&HotkeyEntry> = key_down
        .iter()
        .filter(|e| e.tsf_hash & HOTKEY_POLICY_GLOBAL != 0)
        .collect();
    let dropped = globals
        .get(GLOBAL_HOTKEY_SLOTS..)
        .map(|rest| rest.iter().map(|e| e.action.as_str()).collect())
        .unwrap_or_default();
    (globals.len(), dropped)
}

/// 编译 toggle 模式键（含通用位+具体位），对齐 Go compileToggleModeKey
fn compile_toggle_mode_key(key: &str) -> Option<u32> {
    match key.trim().to_lowercase().as_str() {
        "lshift" => Some(key_hash(MOD_SHIFT | MOD_LSHIFT, VK_LSHIFT)),
        "rshift" => Some(key_hash(MOD_SHIFT | MOD_RSHIFT, VK_RSHIFT)),
        "lctrl" | "lcontrol" => Some(key_hash(MOD_CTRL | MOD_LCTRL, VK_LCONTROL)),
        "rctrl" | "rcontrol" => Some(key_hash(MOD_CTRL | MOD_RCTRL, VK_RCONTROL)),
        "capslock" | "caps" => Some(key_hash(MOD_CAPSLOCK, VK_CAPITAL)),
        _ => None,
    }
}

/// 纯修饰键 VK → keyup hash（含通用位+具体位）。方案级 `[key_actions]` 绑修饰键时，
/// 用它把该键登记进 `key_up` 转发集——不登记 TSF 就不发这个 keyup，绑定形同虚设。
///
/// 与 [`compile_toggle_mode_key`] 同格式，但入参是调用方已解析好的 VK，免得多一层拼写契约。
pub fn compile_modifier_key_up_hash(vk: u32) -> Option<u32> {
    match vk {
        VK_LSHIFT => Some(key_hash(MOD_SHIFT | MOD_LSHIFT, VK_LSHIFT)),
        VK_RSHIFT => Some(key_hash(MOD_SHIFT | MOD_RSHIFT, VK_RSHIFT)),
        VK_LCONTROL => Some(key_hash(MOD_CTRL | MOD_LCTRL, VK_LCONTROL)),
        VK_RCONTROL => Some(key_hash(MOD_CTRL | MOD_RCTRL, VK_RCONTROL)),
        _ => None,
    }
}

/// 候选操作热键模板（`keys.pin_candidate` / `keys.delete_candidate`）→ 期望的修饰位集合。
///
/// 值是模板：`number` 是数字键组 0–9 的占位符，一条展开成 10 个键。值域故意收窄成下面几项
/// （`alt+number` 会撞宿主菜单加速键）；其它值返回 `None` ＝不绑定。
///
/// 编译端（[`compile_number_hotkey`]）与消费端（`Coordinator::match_candidate_action_key`）的
/// 唯一真相源。返回值必须与实际修饰位做相等比较：`ctrl+number` 是 `ctrl+alt+number` 的子集。
pub fn number_template_mods(template: &str) -> Option<u32> {
    match template.trim().to_lowercase().as_str() {
        "ctrl+number" => Some(MOD_CTRL),
        "ctrl+shift+number" => Some(MOD_CTRL | MOD_SHIFT),
        // Ctrl+Alt+数字：给「置顶」与「删除」各留一个不与出厂值冲突的备选。欧洲布局的 AltGr
        // ＝ Ctrl+Alt 会与字符输入撞车，有意不处理（目标用户是 US/CN 布局）。
        "ctrl+alt+number" => Some(MOD_CTRL | MOD_ALT),
        _ => None,
    }
}

/// 展开候选操作热键模板为 0-9 共 10 个 session 热键。值域见 [`number_template_mods`]。
fn compile_number_hotkey(template: &str) -> Vec<HotkeyEntry> {
    let Some(mods) = number_template_mods(template) else {
        return Vec::new();
    };
    (0u32..=9)
        .map(|d| {
            let raw = key_hash(mods, 0x30 + d);
            HotkeyEntry {
                tsf_hash: raw | HOTKEY_POLICY_SESSION,
                match_hash: raw,
                action: String::new(),
            }
        })
        .collect()
}

/// 计算 key_hash（与 wind-ipc::protocol::calc_key_hash 对齐）
fn calc_key_hash(modifiers: u32, key_code: u32) -> u32 {
    (modifiers << 16) | (key_code & 0xFFFF)
}

/// 解析热键字符串为 key_hash
///
/// 支持格式：
/// - "Ctrl+Space"、"Ctrl+Shift+E"
/// - "lshift"、"rshift"
/// - "Shift+Space"
/// - "Ctrl+."、"Ctrl+Equal"
pub fn parse_hotkey(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    let mut modifiers: u32 = 0;
    let mut key_code: Option<u32> = None;

    for part in s.split('+') {
        let part = part.trim().to_lowercase();
        match part.as_str() {
            "ctrl" | "control" => modifiers |= MOD_CTRL,
            "alt" => modifiers |= MOD_ALT,
            "shift" => modifiers |= MOD_SHIFT,
            "win" | "super" => modifiers |= MOD_WIN,
            _ => {
                if key_code.is_some() {
                    // 已经有一个主键了，不支持多个主键
                    return None;
                }
                key_code = Some(parse_key_name(&part)?);
            }
        }
    }

    key_code.map(|kc| calc_key_hash(modifiers, kc))
}

/// 将键名解析为 Windows 虚拟键码
fn parse_key_name(name: &str) -> Option<u32> {
    match name {
        // 修饰键本身（当作为主键时，如 "lshift"）
        "lshift" => Some(0xA0),
        "rshift" => Some(0xA1),
        "lctrl" | "lcontrol" => Some(0xA2),
        "rctrl" | "rcontrol" => Some(0xA3),
        "lalt" | "lmenu" => Some(0xA4),
        "ralt" | "rmenu" => Some(0xA5),

        // 特殊键
        "space" => Some(0x20),
        "return" | "enter" => Some(0x0D),
        "escape" | "esc" => Some(0x1B),
        "backspace" | "back" => Some(0x08),
        "tab" => Some(0x09),
        "delete" | "del" => Some(0x2E),
        "insert" | "ins" => Some(0x2D),
        "home" => Some(0x24),
        "end" => Some(0x23),
        "pageup" | "pgup" => Some(0x21),
        "pagedown" | "pgdn" => Some(0x22),
        "up" => Some(0x26),
        "down" => Some(0x28),
        "left" => Some(0x25),
        "right" => Some(0x27),

        // 标点/符号键
        "." | "period" => Some(0xBE),
        "," | "comma" => Some(0xBC),
        ";" | "semicolon" => Some(0xBA),
        "'" | "quote" => Some(0xDE),
        "/" | "slash" => Some(0xBF),
        "\\" | "backslash" => Some(0xDC),
        "[" | "lbracket" => Some(0xDB),
        "]" | "rbracket" => Some(0xDD),
        "-" | "minus" | "hyphen" => Some(0xBD),
        "=" | "equal" | "equals" => Some(0xBB),
        "`" | "backtick" | "grave" => Some(0xC0),

        // 功能键
        "f1" => Some(0x70),
        "f2" => Some(0x71),
        "f3" => Some(0x72),
        "f4" => Some(0x73),
        "f5" => Some(0x74),
        "f6" => Some(0x75),
        "f7" => Some(0x76),
        "f8" => Some(0x77),
        "f9" => Some(0x78),
        "f10" => Some(0x79),
        "f11" => Some(0x7A),
        "f12" => Some(0x7B),

        // 数字键
        "0" => Some(0x30),
        "1" => Some(0x31),
        "2" => Some(0x32),
        "3" => Some(0x33),
        "4" => Some(0x34),
        "5" => Some(0x35),
        "6" => Some(0x36),
        "7" => Some(0x37),
        "8" => Some(0x38),
        "9" => Some(0x39),

        // 单个字母
        _ if name.len() == 1 => {
            let ch = name.as_bytes()[0];
            if ch.is_ascii_alphabetic() {
                Some((ch.to_ascii_uppercase() - b'A' + 0x41) as u32)
            } else {
                None
            }
        }

        // 十六进制键码（如 "0x41"）
        _ if name.starts_with("0x") => u32::from_str_radix(&name[2..], 16).ok(),

        _ => None,
    }
}

#[cfg(test)]
#[path = "hotkey_tests.rs"]
mod tests;
