//! 热键编译器
//!
//! 与 Go 版本 `wind_input/internal/hotkey/compiler.go` 对齐。
//! 将配置中的热键字符串（如 "Ctrl+Space"、"lshift"）编译为 key_hash，
//! 用于按键事件中的热键匹配。

use crate::config::Config;
use tracing::{debug, warn};

/// `keys.key_actions` 里**组合键**条目的动词表：动词 → `(分发端 action, 策略位)`；
/// 不支持的动词返回 `None`。
///
/// 白名单而非「解析得动就收」：写错的动词若静默进热键表，按下时分发端匹配不上、
/// 什么都不发生，而用户看不出是自己拼错了还是功能坏了。调用点拦下并 warn，与
/// `global_hotkeys` 对不支持动作的处理同策略。
///
/// ★ **只管组合键**。单键条目走的是引导键通路（`Coordinator::bound_action_for`），
/// 值域是完整的 [`crate::BoundAction`]，不经本函数——两条通路的分发端不同，能认的动词
/// 自然不同。用一张表管两条路的结果，是要么放行了热键分发端不认的（配了没反应），
/// 要么挡住了引导键通路完全支持的（能力凭空少一半）。
///
/// 值域语义见 docs/design/schema-key-actions.md §2。
///
/// ★★ **策略位必须按动词分**，不能一律不带：同一个位在两类机制下后果相反。
///
/// | 动词 | 策略位 | why |
/// |---|---|---|
/// | `toggle_schema:<id>` | 无 | 回程恰恰要在**非中文态**下按得动——带上 `CHINESE_ONLY` 就成了单程票，切到英文方案后回不来 |
/// | `special:<id>` | `CHINESE_ONLY \| GLOBAL` | 进 overlay 只在中文输入中途有意义；`GLOBAL` 让 TSF 用 `RegisterHotKey` 抢占，穿透 QQNT/Tabby 等 Chromium 宿主的同名加速键 |
///
/// ★ 动词形态在此做一次映射：引导键通路用 `special:<id>`（[`crate::BoundAction`] 的值域），
/// 而热键分发端认的是 `enter_special:<id>`。两条通路的分发端不同，动词形态也就不同——
/// 映射放在编译期，分发端零改动。
pub(crate) fn hotkey_action_entry(action: &str) -> Option<(String, u32)> {
    // 两个切方案动词都**不带 CHINESE_ONLY**：切方案在中英两态下都该生效——尤其
    // 「英文方案 → 中文方案」，要求恰恰是在非中文态下也能按。带上就是「切得过去、
    // 切不回来」。与 `switch_engine` 循环键同策略。
    if let Some(id) = action.strip_prefix("toggle_schema:")
        && !id.trim().is_empty()
    {
        return Some((action.to_string(), 0));
    }
    if let Some(id) = action.strip_prefix("switch_schema:")
        && !id.trim().is_empty()
    {
        return Some((action.to_string(), 0));
    }
    if let Some(id) = action.strip_prefix("special:")
        && !id.trim().is_empty()
    {
        return Some((
            format!("enter_special:{}", id.trim()),
            HOTKEY_POLICY_CHINESE_ONLY | HOTKEY_POLICY_GLOBAL,
        ));
    }
    // 生僻字模式：策略位与 `special:` 完全一致——同样是「进 overlay 只在中文输入中途有
    // 意义」，同样需要 GLOBAL 穿透 Chromium 类宿主的同名加速键。动词做一次映射
    // （`rare_char` → `enter_rare_char`），理由同 `special:`：引导键通路与热键分发端
    // 认的是两个不同的串。
    if action == "rare_char" {
        return Some((
            "enter_rare_char".to_string(),
            HOTKEY_POLICY_CHINESE_ONLY | HOTKEY_POLICY_GLOBAL,
        ));
    }
    // 软键盘：**不带 `CHINESE_ONLY`**。
    //
    // 面板画的是「键位 → 符号」的映射，跟当前是中文还是英文模式没有关系——用户在英文态
    // 想打个 ℃ 同样合理。C++ 侧为此专设了软键盘总闸（`IsSoftKeyboard()`），接管不再
    // 依附于中文模式的那条判定链。带上这个位的话，英文态连开都开不出来。
    //
    // 保留 `GLOBAL`：同 `special:`，规避 Chromium 类宿主无视 `pfEaten` 造成的双处理。
    // 它自身的注册条件含「中文 + 焦点在文本框」，英文态下自然退回普通热键链路。
    //
    // 动词原样传给分派端（不像 `special:` 那样改写成 `enter_special:`）：协调器的
    // `softkeyboard_hotkey` 认的就是这个串。
    if action == "softkeyboard" {
        return Some((action.to_string(), HOTKEY_POLICY_GLOBAL));
    }
    if let Some(id) = action.strip_prefix("softkeyboard:")
        && !id.trim().is_empty()
    {
        return Some((action.to_string(), HOTKEY_POLICY_GLOBAL));
    }
    None
}

/// `keys.key_actions` 的一条条目该走哪条通路。由**键的形态**决定，不由动词决定。
///
/// 三条通路各有各的到达条件，判据见 docs/design/schema-key-actions.md §4.1 与 §4.4：
///
/// | 形态 | 通路 | 为什么 |
/// |---|---|---|
/// | 组合键（带 Ctrl/Alt/Shift/Win） | key_down 热键 → `dispatch_hotkey` | 不与输入争键，可全局拦截 |
/// | 纯修饰键（`rshift`） | key_up 轻敲 | keydown 不能吃（宿主要看到修饰键），只能在干净单击的 keyup 上判 |
/// | 单个有字符的键（`backtick`） | keydown 引导键链 | 英文模式下必须让它出字，故排在分水岭之后 |
///
/// ⚠ 单键**绝不能**编译进 key_down 热键表：`parse_hotkey("backtick")` 返回的是无修饰位的
/// 裸 VK，进表后 TSF 会把它当热键转发并吞掉，该符号就再也打不出来了。
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
    Some(KeyActionRoute::LeadingKey)
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
/// 全局拦截位（正交标记，与 CHINESE_ONLY 叠加）：TSF 侧在「中文模式 + 焦点在文本框」
/// 时用 Win32 RegisterHotKey 把这些键注册为系统级热键，让 OS 在 WM_KEYDOWN 派发前
/// 直接消费，规避 QQNT / Tabby 等 Chromium 类宿主无视 TSF pfEaten 契约的加速键双处理。
const HOTKEY_POLICY_GLOBAL: u32 = 0x20000000;
/// 「仅注册转发」标记：翻页键组 / 选词键组这类 action 为空的登记项——它们不是动作热键，
/// 只是让 TSF 认得这些键、在有会话时转发给引擎；无会话时必须放行，由 TSF 下游的
/// ClassifyInputKey 按普通标点处理（中文模式下要出中文标点）。
///
/// ⚠ 真动作热键**绝不能**带此位。TSF 侧的「无 Ctrl/Alt 且无会话就不吃」闸门只认这个标记；
/// 早先该闸门无差别地套在所有无 Ctrl/Alt 的 keydown 热键上，把 `shift+space`
/// （toggle_full_width）一并放行了，而 Space 在下游只有「有会话」和「已是全角」两条
/// 出路，半角空缓冲时无人接手 —— 严格 TSF 宿主（EverEdit）不再回调 OnKeyDown，
/// 全半角切换彻底失效；宽松宿主（记事本/Chromium）照调 OnKeyDown 才碰巧还能用。
const HOTKEY_POLICY_FORWARD_ONLY: u32 = 0x10000000;

/// Windows 虚拟键码（toggle / select / page 用）
const VK_LSHIFT: u32 = 0xA0;
const VK_RSHIFT: u32 = 0xA1;
const VK_LCONTROL: u32 = 0xA2;
const VK_RCONTROL: u32 = 0xA3;
const VK_CAPITAL: u32 = 0x14;
const VK_TAB: u32 = 0x09;
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
/// ★ **必须与 `toggle_mode` 区分开**。`is_toggle_mode_keycode` 按 action 过滤而非按键码
/// ——若复用 `toggle_mode`，只把 CapsLock 配成翻页键的用户会在空闲敲 CapsLock 时莫名
/// 切中英文。`select_key_groups` 进 keyup 表时已经踩过一次这个坑，`schema_bound` 是
/// 第二次，本项是第三次；每次往 `key_up` 加东西都要重查这条。
pub const SESSION_ACTION: &str = "session_action";

/// 会话态键名 → (VK, 是否需 Shift)。支持单个 `shift+` 前缀。
///
/// ⚠️ **这是 `wind_keys::keymap::session_key_name_to_vk` 的第二份实现**，因为
/// `wind-config` 不能依赖 `wind-keys`（后者经 `wind-cmdbar` 反向依赖本 crate，加进去成环）。
/// 本文件早已因同样的理由自带一份 VK 常量与键组展开表，这里延续该结构。
///
/// 两份表的一致性**没有编译期约束**，靠 `wind-coordinator` 的
/// `session_key_tables_agree_across_crates` 守门（那里同时依赖两个 crate）。跨仓/跨 crate
/// 契约漂移是本仓反复栽过的一类，别指望「改的时候会记得」。
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
        // ★ 字母里**只收 z**。会话态查表（`apply_session_action`）排在大 match 的字母臂
        // 之前，故收进来的字母在**有候选时**会被夺走、打不出以它接续的编码。z 是唯一
        // 值得付这个代价的：它在多数码表里是最边缘的码元，且用户对「z 兼职功能键」已有
        // 习惯（`z_key_action` 那条路早就这么用）。
        //
        // ⛔ 别顺手放开成全部字母：其余字母在任何码表里几乎都是活码前缀，配了等于把那个
        // 字母在该方案里废掉，且没有任何提示。这与 `schema_key_actions` 的键名下拉「字母
        // 只留 z」是同一条判据。
        //
        // ⚠️ 无会话时不受影响：导航类动词带 `requires_candidates` 守卫，空缓冲按 z 照常
        // 起头组码。被夺走的只有「已出候选后再按 z」那一下（五笔的 zz 类短语）。
        "z" => 0x5A,
        _ => return None,
    };
    Some((vk, shift))
}

/// keyup-only 键（CapsLock / 四个纯修饰键）的 keyup hash；其余键返回 `None`。
///
/// 这批键绑任何功能都只能走 keyup 轻敲（keydown 不能吃、`Ctrl+A` 会误触发、按住会连发），
/// 判据同 `is_key_up_only_vk`。⚠️ CapsLock 与四个修饰键**不连号**，用区间判定会漏掉它。
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

    /// 在 key_down 集合中按规范化 hash 查**吃键策略**。
    ///
    /// 供 `Coordinator::should_handle_key` 用（吃键判定的唯一真相源）。策略位本就在
    /// 本文件定义，解码也放这里——此前只有 C++ 侧从 `tsf_hash` 高位解，
    /// 判据实现分居两地，注释里那句「判据须与服务端保持单一真相源」就是这么来的。
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
    /// ⚠️ 带本策略位的 keydown 条目在 TSF 侧还有**第二个消费者**：
    /// `CTextService::_RegisterCandidateHotkeys` 会在候选可见期间把它们逐个
    /// `RegisterHotKey` 成系统级热键——**那才是实际生效的通路**，`OnTestKeyDown`
    /// 的白名单分支只是它失败时的退路。改本策略位的产出前先读
    /// `docs/design/key-resolver-unification.md` §2.2 的那个警告框。
    Session,
    /// 仅注册转发（翻页键组 `-=`、选词键组 `;'`）：无会话时**放行并继续按常规按键
    /// 逻辑判定**，不是直接不吃——中文模式下它们要当标点处理。
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

/// 把一条**会话态**键名编译成 TSF 转发条目。返回 `(该进 key_up 表吗, 条目)`。
///
/// ★ **分流规则的单一来源**：全局 `keys.session_actions` 与方案级 `[session_actions]`
/// 共用本函数。两处各写一份的表现是「同一个键名在全局配能用、写进方案文件就不转发」——
/// 而 TSF 不转发的键在服务端根本收不到，与「配错了」完全同形。
///
/// 按**键的形态**分两条路，与 `keys.key_actions` 的三分通路同构：
/// - keyup-only 键（CapsLock / 纯修饰键）→ `key_up`，带 `SESSION` 位，让 C++ 区分
///   「toggle 语义」（恒吃 keydown）与「会话语义」（仅有会话时吃）；
/// - 其余（功能键 + 可打印符号键）→ `key_down`，带 `FORWARD_ONLY` 位。
///
/// ⚠️ `FORWARD_ONLY` 那条**必须**保留「无会话时放行给下游按标点处理」的语义：本函数收的
/// 键名含减号、方括号、分号等可打印符号，吃掉就是丢键。见
/// `docs/design/key-resolver-unification.md` §8 注意点 5。
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
        ] {
            if let Some(raw) = parse_hotkey(value) {
                // 加词类热键额外叠加 GLOBAL 位：TSF 侧在中文+文本框时 RegisterHotKey 全局拦截，
                // 规避 Chromium 类宿主（QQNT/Tabby）的加速键双处理。其余 chinese-only 键不拦截，
                // 避免不必要地抢占宿主快捷键。
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

        // ── KeyDown：临拼直达热键（CHINESE_ONLY | GLOBAL，与加词键同策略） ──
        // 与引导键共存：热键路径进入时组合区不写引导符（分发点传 key_code=0）。
        // GLOBAL 位使 TSF 在「中文 + 文本框」时 RegisterHotKey 全局拦截，穿透 QQNT/Tabby 等
        // Chromium 宿主的加速键双处理。
        //
        // 特殊模式的直达热键**不在这里**：它已收编进 `keys.key_actions`（写作
        // `"ctrl+shift+u" = "special:<方案id>"`），由上方的 `KeyActionRoute::Hotkey`
        // 分支按动词取策略位编译。原先那段遍历 `schema.special_modes[].hotkey` 的循环
        // 连同「id 为空则跳过」那条陷阱一并消失——身份现在就是方案 id，不可能为空。
        let mode_policy = HOTKEY_POLICY_CHINESE_ONLY | HOTKEY_POLICY_GLOBAL;
        if let Some(raw) = parse_hotkey(&self.config.input.temp_pinyin.hotkey) {
            result.key_down.push(HotkeyEntry {
                tsf_hash: raw | mode_policy,
                match_hash: raw,
                action: "enter_temp_pinyin".to_string(),
            });
        }

        // ── 方案直达热键在这里**没有**独立编译段 ──
        // `keys.schema_hotkeys` 已废弃，改写进下方的 `keys.key_actions`（动词
        // `switch_schema:<id>`）。**没有兼容折算**：加载期只由
        // `Config::warn_legacy_schema_hotkeys` 告警一次随后清空，残留的老配置不生效
        // （用户拍板不做向后兼容，见 044a2a1a）。
        //
        // 合表的理由之一正是本段自己造成的：它与 key_actions 编译进**同一张**
        // key_down 表，而 `match_key_down` 是 `.find()` 先注册者赢——本段排在前面，于是同一个
        // 键两处都配时 key_actions 那条静默失效。另一条是本段**不问键形态**地把解析结果塞进
        // key_down 表（下方 key_actions 走 `route_of_key_action` 三分），单字符键进表会被 TSF
        // 当热键吞掉，那个符号从此打不出来。
        //
        // ⚠ 别在这里"顺手"把它加回来：加回来这两个洞就一起回来了。

        // ── KeyDown：按键功能表（keys.key_actions）──
        // **不带 CHINESE_ONLY**，理由与上面方案直达热键同：`toggle_schema` 的回程恰恰要在
        // 非中文态下按得动（切到英文方案后带上该位就回不来了）。
        //
        // ⚠ 后续接入别的动词时**策略位必须按动词分**，不能沿用这里的"一律不带"：进 overlay
        // 的动词（enter_special / temp_pinyin 那类）只在中文输入中途有意义，需要
        // CHINESE_ONLY | GLOBAL——同一个位在两类机制下后果相反（见上方 enter_special 那段）。
        //
        // BTreeMap 遍历即有序，无需像 schema_hotkeys 那样显式排序：撞键时的胜者顺序
        // 在任何进程里都一致。
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
                    let Some((dispatch_action, policy)) = hotkey_action_entry(action) else {
                        warn!("keys.key_actions: 组合键不支持动词 {action:?}（键 {key:?}），忽略");
                        continue;
                    };
                    result.key_down.push(HotkeyEntry {
                        // match_hash 恒不含策略位——策略位是给 TSF 看的转发/抢占指示，
                        // 服务端匹配只认裸 hash（与 enter_special / add_word 同构）。
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
                // 单个有字符的键：**不进任何热键表**。进了 TSF 就会把它当热键吞掉，
                // 该符号再也打不出来。由引导键链（`bound_action_for`）查配置消费。
                KeyActionRoute::LeadingKey => {}
            }
        }

        // ── KeyDown：数字模板展开（PinCandidate / DeleteCandidate，session policy） ──
        for tmpl in [&h.pin_candidate, &h.delete_candidate] {
            for entry in compile_number_hotkey(tmpl) {
                result.key_down.push(entry);
            }
        }

        // ── 会话态按键功能表（keys.session_actions）──
        //
        // 数据源是 `effective_session_actions()`＝四组键组配置的展开结果 ⊕ `session_actions`
        // （后者优先）。⚠️ **不能**直接读 `config.keys.session_actions`：那只是用户显式配的
        // 那部分，漏掉四组键组展开的键，表现是翻页/选词键全失效。
        //
        // ★ 按**键的形态**分两条路，与五c 给 `keys.key_actions` 做的分流同构：
        //   - keyup-only 键（CapsLock / 纯修饰键）→ `key_up` 表，带 SESSION 位，让 C++ 区分
        //     「toggle 语义」（恒吃 keydown）与「会话语义」（仅有会话时吃）；
        //   - 其余（功能键 + 可打印符号键）→ `key_down` 表，带 FORWARD_ONLY 位。
        //
        // ⚠️ 功能键（Tab / PgUp / 方向键）本就在 C++ 的 `_IsSessionKey` 表里、有会话时免费
        //    转发，登记与否都能工作。这里**仍照旧全部登记**：旧的 `compile_page_key_group`
        //    就是这么做的，收编时顺手改可达性会把一次配置重构变成一次跨进程行为变更，
        //    而后者只有真机才验得了。
        for (name, verb) in &self.config.keys.effective_session_actions() {
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

        // 二三候选键（含修饰键组 lrshift / lrctrl）的登记已并入上面的 `session_actions` 段
        // ——它按键的形态自动分流：可打印键进 key_down + FORWARD_ONLY，修饰键进 key_up。
        //
        // ⚠️ 同一个键可能既是切换键又是选词键（两条登记同 hash）：TSF 侧白名单是集合，重复
        // 无害；服务端按 action 区分，切换只认 action=="toggle_mode"，故消费端**不能**用
        // 「key_up 里有这个 key_code」当切换判据（见 is_toggle_mode_keycode）。

        debug!(
            "Compiled hotkeys: {} key_down, {} key_up",
            result.key_down.len(),
            result.key_up.len()
        );
        result
    }
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
/// 与 [`compile_toggle_mode_key`] 同格式但入参是 VK 而非键名：调用方（协调器）手里
/// 已经是解析好的 VK（`keymap::modifier_name_to_vk`），再转回字符串只为了重新解析
/// 一次，中间多一层拼写契约就多一处静默失配的机会。
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
/// 这两项的值是**模板**而非普通热键串：`number` 是数字键组 0–9 的占位符，一条模板展开成
/// 10 个键。值域故意收窄成下面这几项；`none`（以及任何其它值）返回 `None` ＝不绑定。
///
/// ★ **编译端与消费端的唯一真相源**。编译端（[`compile_number_hotkey`]）拿它算 TSF 转发表，
/// 消费端（`Coordinator::match_candidate_action_key`）拿它判命中。两边各写一份白名单的话，
/// 加一个取值只改一边的表现是「TSF 转发了但没人认」或「认得但 TSF 根本不发」——两种都不报错、
/// 只是按下去毫无反应。
///
/// ⚠️ 返回值必须与实际修饰位做**相等**比较，不能按位包含：`ctrl+number` 的位集合是
/// `ctrl+alt+number` 的**子集**，用包含判据的话前者会把后者的按键恒久劫走。
pub fn number_template_mods(template: &str) -> Option<u32> {
    match template.trim().to_lowercase().as_str() {
        "ctrl+number" => Some(MOD_CTRL),
        "ctrl+shift+number" => Some(MOD_CTRL | MOD_SHIFT),
        // Ctrl+Alt+数字：给「置顶」与「删除」各留一个不与出厂值冲突的备选。
        // ⚠️ 欧洲键盘布局的 AltGr ＝ Ctrl+Alt，那些布局下本组合会与字符输入撞车；
        // TSF 侧无法可靠区分真 Ctrl+Alt 与 AltGr。**有意不处理**——本项目是中文输入法，
        // 目标用户是 US/CN 布局。想改主意的话，判据不在这里，在 TSF 的按键来源。
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

// 选词键组 / 以词定字键组的四个解析器（`compile_select_key_group`、
// `compile_select_modifier_group`、`select_key_vks`、`select_char_vks`）已随三期收编删除。
//
// 它们的职责现在由两处承担：**编译**走上面的 `session_actions` 段（按键的形态自动分流到
// key_down + FORWARD_ONLY 或 key_up），**折算**走 `Config::select_key_group_binds` /
// `select_char_group_binds`（组名 → 具体键 + 动词）。
//
// ★ 删而不是留着不用：这四个函数与 `session_actions` 是**平行的第二套真相源**，留着就是
// 「两处慢慢漂移」的种子。此前 `select_key_vks`（不含 brackets）与 `select_char_vks`
// （含 brackets）就被张冠李戴过一次，`brackets` 配置静默失效——收编后两者靠**动词**区分，
// 那类错配从结构上消失了。
//
// 修饰键为什么只能走 keyup（三条独立理由：keydown 不能吃 / `Ctrl+A` 首下会误选 / 长按
// 连发），见 `keymap::is_key_up_only_vk` 的文档。

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

// `parse_hotkey_prefix`（"ctrl+shift+number" → (modifiers, has_number)）已删除：全仓无调用者，
// 而它做的事正是 [`number_template_mods`] 的事。留着就是同一份逻辑的第三份写法——本仓已经因为
// 「两份手写清单慢慢漂移」翻过车（`hotkey_action_entry` 白名单 vs 设置页 `verb_allowed`）。
//
// ★ 它与 `number_template_mods` 有一处**语义差异**，不是等价物：它「解析得动就收」，
// `alt+number`、`win+number` 都会返回一个修饰位；而模板值域是**白名单**，只认那几项。
// 后者是有意的——放开值域会引入一批「配了但静默失效」的取值（`alt+number` 撞宿主菜单
// 加速键与小键盘 Unicode 输入），而失效点在 TSF 侧，用户这边只看到「按了没反应」。

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_key() {
        let hash = parse_hotkey("lshift").unwrap();
        assert_eq!(hash, 0xA0); // no modifiers, key=0xA0
    }

    #[test]
    fn test_parse_ctrl_space() {
        let hash = parse_hotkey("Ctrl+Space").unwrap();
        assert_eq!(hash, (MOD_CTRL << 16) | 0x20);
    }

    #[test]
    fn test_parse_ctrl_shift_e() {
        let hash = parse_hotkey("ctrl+shift+e").unwrap();
        assert_eq!(hash, ((MOD_CTRL | MOD_SHIFT) << 16) | 0x45);
    }

    #[test]
    fn test_parse_shift_space() {
        let hash = parse_hotkey("shift+space").unwrap();
        assert_eq!(hash, (MOD_SHIFT << 16) | 0x20);
    }

    #[test]
    fn test_parse_ctrl_dot() {
        let hash = parse_hotkey("ctrl+.").unwrap();
        assert_eq!(hash, (MOD_CTRL << 16) | 0xBE);
    }

    #[test]
    fn test_parse_ctrl_equal() {
        let hash = parse_hotkey("ctrl+equal").unwrap();
        assert_eq!(hash, (MOD_CTRL << 16) | 0xBB);
    }

    #[test]
    fn test_parse_empty() {
        assert!(parse_hotkey("").is_none());
    }

    #[test]
    fn test_toggle_mode_key_includes_specific_modifier() {
        // 关键回归：lshift 的 keyUp hash 必须同时含通用位(MOD_SHIFT)和具体位(MOD_LSHIFT)，
        // 否则 C++ TSF 算出的 0x1100A0 在白名单里找不到 → Shift 切换失效。
        let lshift = compile_toggle_mode_key("lshift").unwrap();
        assert_eq!(lshift, key_hash(MOD_SHIFT | MOD_LSHIFT, VK_LSHIFT));
        assert_eq!(lshift, 0x0011_00A0);

        let rshift = compile_toggle_mode_key("rshift").unwrap();
        assert_eq!(rshift, key_hash(MOD_SHIFT | MOD_RSHIFT, VK_RSHIFT));
        assert_eq!(rshift, 0x0021_00A1);
    }

    #[test]
    fn test_number_hotkey_expands_to_ten_session_keys() {
        let entries = compile_number_hotkey("ctrl+shift+number");
        assert_eq!(entries.len(), 10);
        // tsf_hash 含 session policy 位；match_hash 为 raw
        assert!(entries[0].tsf_hash & HOTKEY_POLICY_SESSION != 0);
        assert_eq!(entries[0].match_hash, key_hash(MOD_CTRL | MOD_SHIFT, 0x30));
        assert!(compile_number_hotkey("none").is_empty());
    }

    #[test]
    fn test_number_template_mods_is_whitelist_not_parser() {
        // 白名单，不是「解析得动就收」：值域外的写法一律不绑定（含已删除的
        // `parse_hotkey_prefix` 曾经会放行的 alt/win 变体）。
        assert_eq!(number_template_mods("ctrl+number"), Some(MOD_CTRL));
        assert_eq!(
            number_template_mods("Ctrl+Shift+Number"), // 大小写/空白不敏感
            Some(MOD_CTRL | MOD_SHIFT)
        );
        assert_eq!(number_template_mods(" ctrl+number "), Some(MOD_CTRL));
        assert_eq!(
            number_template_mods("ctrl+alt+number"),
            Some(MOD_CTRL | MOD_ALT)
        );
        for bad in ["none", "", "alt+number", "win+number", "ctrl+shift+e"] {
            assert_eq!(number_template_mods(bad), None, "{bad:?} 不该被接受");
        }
    }

    #[test]
    fn test_number_template_mods_are_pairwise_distinct() {
        // 回归：消费端按**相等**比较修饰位，前提是任意两项的位集合不相等。
        // （更强的性质——不存在子集关系——已不再要求：等值判据对子集免疫，
        //   这正是 `ctrl+alt+number` 能安全加进值域的原因。）
        let all = ["ctrl+number", "ctrl+shift+number", "ctrl+alt+number"];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(
                    number_template_mods(a),
                    number_template_mods(b),
                    "{a:?} 与 {b:?} 修饰位相同，等值判据无法区分"
                );
            }
        }
    }

    #[test]
    fn test_compile_switch_engine_match_hash() {
        let mut cfg = Config::default();
        cfg.keys.switch_engine = "ctrl+shift+e".to_string();
        cfg.keys.toggle_mode_keys = vec!["lshift".into(), "rshift".into()];
        let compiled = Compiler::new(cfg).compile();
        // switch_engine 无 policy 位，match_hash == tsf_hash == 0x30045
        let se = compiled
            .key_down
            .iter()
            .find(|e| e.action == "switch_engine")
            .unwrap();
        assert_eq!(se.match_hash, key_hash(MOD_CTRL | MOD_SHIFT, 0x45));
        assert_eq!(se.tsf_hash, se.match_hash);
        // 规范化匹配：带 L/R 具体位的入站事件也能命中
        assert_eq!(
            compiled.match_key_down(key_hash(MOD_CTRL | MOD_SHIFT, 0x45)),
            Some("switch_engine")
        );
        assert_eq!(compiled.key_up.len(), 2);
    }

    #[test]
    fn open_add_word_dialog_registered_chinese_only() {
        let mut cfg = Config::default();
        cfg.keys.open_add_word_dialog = "ctrl+shift+equal".to_string();
        let compiled = Compiler::new(cfg).compile();
        // action 串应出现在 key_down 组
        assert!(
            compiled
                .key_down
                .iter()
                .any(|e| e.action == "open_add_word_dialog"),
            "open_add_word_dialog 应注册进 key_down"
        );
    }

    /// 把 (键名, 动词) 对拼成 `keys.session_actions`。测试直接构造 `Compiler`、不经
    /// `Config::normalize`，故须显式写出折算后的形态。
    fn session_actions(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// 清空四组键组配置，使 `effective_session_actions()` 只剩显式表。
    ///
    /// 出厂默认里 `page_keys` / `highlight_keys` / `select_key_groups` **都非空**，
    /// 不清的话每个用例都会多出十来个折算来的键，断言「只该有这几个」必然失败。
    fn only_explicit(cfg: &mut Config) {
        cfg.keys.page_keys.clear();
        cfg.keys.highlight_keys.clear();
        cfg.keys.select_key_groups.clear();
        cfg.keys.select_char_keys.clear();
    }

    /// ★★★ 四组键组配置必须经 `effective_session_actions()` 进 TSF 转发表。
    ///
    /// 2026-08-11 回归守门：折算从 `normalize` 移到消费层视图后，本编译器若仍直接读
    /// `config.keys.session_actions`（只有用户显式配的那部分），出厂默认的翻页/选词键
    /// **一个都不会进转发表**——表现是升级后翻页键、次选键全部失效，而配置文件看着好好的。
    #[test]
    fn key_group_config_reaches_forward_set_without_explicit_table() {
        let cfg = Config::default();
        assert!(
            cfg.keys.session_actions.is_empty(),
            "前置条件：出厂默认不该有显式会话态绑定，否则本测试证明不了什么"
        );
        let compiled = Compiler::new(cfg).compile();
        // 默认 page_keys 含 pageupdown、highlight_keys 含 arrows、select_key_groups 含 semicolon_quote。
        for (vk, label) in [
            (VK_PRIOR, "PageUp（page_keys 默认）"),
            (VK_NEXT, "PageDown（page_keys 默认）"),
            (VK_UP, "↑（highlight_keys 默认）"),
            (VK_OEM_1, "分号（select_key_groups 默认）"),
        ] {
            assert!(
                compiled
                    .key_down
                    .iter()
                    .any(|e| e.match_hash & 0xFFFF == vk
                        && e.tsf_hash & HOTKEY_POLICY_FORWARD_ONLY != 0),
                "{label} 未进 FORWARD_ONLY 转发集——四组键组配置没被编译进去"
            );
        }
    }

    #[test]
    fn forward_only_bit_marks_page_and_select_keys_only() {
        let mut cfg = Config::default();
        only_explicit(&mut cfg);
        cfg.keys.toggle_full_width = "shift+space".to_string();
        // 等价于旧的 `page_keys = ["minus_equal", "shift_tab"]` +
        // `select_key_groups = ["semicolon_quote"]` 折算后的样子。
        cfg.keys.session_actions = session_actions(&[
            ("minus", "page_prev"),
            ("equal", "page_next"),
            ("shift+tab", "page_prev"),
            ("tab", "page_next"),
            ("semicolon", "select_candidate:2"),
            ("quote", "select_candidate:3"),
        ]);
        let compiled = Compiler::new(cfg).compile();

        // ⚠ 判据不能用「action 为空」：pin/delete 候选的数字热键 action 同样是空串
        //（动作由服务端按 hash 自认），它们是 session 热键、不该带 FORWARD_ONLY。
        // 只有会话态键才是仅注册转发，故按 raw hash 精确点名。
        let forward_only_raw: Vec<u32> = vec![
            key_hash(0, VK_OEM_MINUS),
            key_hash(0, VK_OEM_PLUS),
            key_hash(MOD_SHIFT, VK_TAB),
            key_hash(0, VK_TAB),
            key_hash(0, VK_OEM_1),
            key_hash(0, VK_OEM_7),
        ];
        assert_eq!(forward_only_raw.len(), 6, "样例绑定应展开出 6 个键");

        for e in &compiled.key_down {
            let expected = forward_only_raw.contains(&e.match_hash);
            assert_eq!(
                e.tsf_hash & HOTKEY_POLICY_FORWARD_ONLY != 0,
                expected,
                "hash=0x{:08X} action={:?} 的 FORWARD_ONLY 位不符预期",
                e.tsf_hash,
                e.action
            );
            // match_hash 是服务端匹配用的裸 hash，任何 policy 位都不该混进去
            assert_eq!(e.match_hash & HOTKEY_POLICY_FORWARD_ONLY, 0);
        }

        // 定桩：shift+space 无任何 policy 位，TSF 侧必须无条件吃。
        let fw = compiled
            .key_down
            .iter()
            .find(|e| e.action == "toggle_full_width")
            .expect("toggle_full_width 应在 key_down 组");
        assert_eq!(fw.tsf_hash, fw.match_hash);
        assert_eq!(fw.tsf_hash, key_hash(MOD_SHIFT, 0x20));
    }

    /// 修饰键作选词键时只进 key_up，绝不进 key_down。
    ///
    /// 回归点：曾与 `;'` 一起注册进 key_down，而 TSF 的 keydown 查表用的是「通用修饰位 +
    /// 笼统 VK_CONTROL」，两个维度都对不上这里登记的「具体位 + VK_LCONTROL」，于是这项配置
    /// 端到端从未生效过——且即便对上了也不能吃（纯修饰键 keydown 必须放行）。
    ///
    /// ⚠️ 三期收编后 action 从 `select_candidate` 改为统一的 `session_action`。这是安全的：
    /// 该 action 名对 keyup 选词**没有功能作用**（`handle_select_key_up` 只按 VK 查偏移），
    /// 唯一读 action 的是 `is_toggle_mode_keycode`，而它只认 `toggle_mode`。
    #[test]
    fn select_modifier_group_registers_on_key_up_only() {
        let mut cfg = Config::default();
        cfg.keys.toggle_mode_keys = vec!["lshift".into(), "rshift".into()];
        // 等价于旧的 `select_key_groups = ["lrctrl"]` 折算后的样子。
        cfg.keys.session_actions = session_actions(&[
            ("lctrl", "select_candidate:2"),
            ("rctrl", "select_candidate:3"),
        ]);
        let compiled = Compiler::new(cfg).compile();

        assert!(
            !compiled
                .key_down
                .iter()
                .any(|e| matches!(e.match_hash & 0xFFFF, VK_LCONTROL | VK_RCONTROL)),
            "修饰键选词键不得出现在 key_down"
        );
        let sel: Vec<&HotkeyEntry> = compiled
            .key_up
            .iter()
            .filter(|e| e.action == SESSION_ACTION)
            .collect();
        assert_eq!(sel.len(), 2, "lrctrl 应展开出左右两个 keyup 登记");
        assert_eq!(
            sel[0].match_hash,
            key_hash(MOD_CTRL | MOD_LCTRL, VK_LCONTROL)
        );
        assert_eq!(
            sel[1].match_hash,
            key_hash(MOD_CTRL | MOD_RCTRL, VK_RCONTROL)
        );
        // 与 toggle 登记同格式（通用位+具体位），否则 C++ GetCurrentModifiers 的双位哈希对不上。
        assert_eq!(
            compiled
                .key_up
                .iter()
                .filter(|e| e.action == "toggle_mode")
                .count(),
            2,
            "切换键登记不应被选词登记挤掉"
        );
    }

    /// 用户诉求二：`capslock = "page_prev"`。CapsLock 只有 keyup 到得了服务端，故必须进
    /// `key_up` 表并带 SESSION 位——C++ 靠这个位区分「toggle 语义」（恒吃 keydown）与
    /// 「会话语义」（仅有会话时吃）。
    ///
    /// ★★ 回归保护：action **不能**是 `toggle_mode`。`is_toggle_mode_keycode` 按 action
    /// 过滤，混用会让「只把 CapsLock 配成翻页键」的用户在空闲敲 CapsLock 时莫名切中英文。
    /// 这是第三次触碰该判据（前两次是 `select_key_groups` 与 `schema_bound`）。
    #[test]
    fn capslock_session_action_registers_on_key_up_with_session_policy() {
        let mut cfg = Config::default();
        cfg.keys.session_actions = session_actions(&[("capslock", "page_prev")]);
        let compiled = Compiler::new(cfg).compile();

        let e = compiled
            .key_up
            .iter()
            .find(|e| e.action == SESSION_ACTION)
            .expect("capslock 的会话态绑定应登记进 key_up");
        assert_eq!(e.match_hash, key_hash(MOD_CAPSLOCK, VK_CAPITAL));
        assert!(
            e.tsf_hash & HOTKEY_POLICY_SESSION != 0,
            "缺 SESSION 位，C++ 会把它当 toggle 键处理，keydown 恒被吃 ⇒ 大小写切换全局失效"
        );
        assert_eq!(
            e.match_hash & HOTKEY_POLICY_SESSION,
            0,
            "match_hash 是服务端匹配用的裸 hash，policy 位不该混进去"
        );
        assert_ne!(
            e.action, "toggle_mode",
            "见本测试文档：会让空闲敲 CapsLock 切中英文"
        );
        assert!(
            !compiled
                .key_down
                .iter()
                .any(|e| e.match_hash & 0xFFFF == VK_CAPITAL),
            "CapsLock 不得进 key_down —— C++ 侧根本不发它的 keydown"
        );
    }

    /// 会话态的**功能键**走 key_down + FORWARD_ONLY（与旧 `page_keys` 的登记形态一致）。
    /// 无会话时 C++ 靠 FORWARD_ONLY 闸门放行，键回落宿主的原语义（Tab 仍是制表符）。
    #[test]
    fn session_function_keys_register_on_key_down_forward_only() {
        let mut cfg = Config::default();
        cfg.keys.session_actions =
            session_actions(&[("tab", "page_next"), ("shift+tab", "page_prev")]);
        let compiled = Compiler::new(cfg).compile();

        for (want_mods, label) in [(0, "tab"), (MOD_SHIFT, "shift+tab")] {
            let raw = key_hash(want_mods, VK_TAB);
            let e = compiled
                .key_down
                .iter()
                .find(|e| e.match_hash == raw)
                .unwrap_or_else(|| panic!("{label} 应登记进 key_down"));
            assert!(
                e.tsf_hash & HOTKEY_POLICY_FORWARD_ONLY != 0,
                "{label} 缺 FORWARD_ONLY：无会话时 TSF 会把它当动作热键吞掉"
            );
            assert!(
                e.action.is_empty(),
                "会话态键是仅注册转发，动作由服务端自认"
            );
        }
    }

    /// 显式 `none` 与拼错的键名都不产出登记，且不 panic。
    #[test]
    fn session_actions_skip_disabled_and_unknown_keys() {
        let mut cfg = Config::default();
        only_explicit(&mut cfg);
        cfg.keys.session_actions =
            session_actions(&[("tab", "none"), ("pgeup", "page_prev"), ("up", "")]);
        let compiled = Compiler::new(cfg).compile();
        assert!(
            !compiled
                .key_down
                .iter()
                .any(|e| matches!(e.match_hash & 0xFFFF, VK_TAB | VK_UP)),
            "none / 空值不应登记"
        );
    }

    /// 可打印选词键的通路不变：仍在 key_down 且带 FORWARD_ONLY，不进 key_up。
    #[test]
    fn printable_select_group_stays_on_key_down() {
        let mut cfg = Config::default();
        cfg.keys.session_actions = session_actions(&[
            ("semicolon", "select_candidate:2"),
            ("quote", "select_candidate:3"),
        ]);
        let compiled = Compiler::new(cfg).compile();
        for raw in [key_hash(0, VK_OEM_1), key_hash(0, VK_OEM_7)] {
            let e = compiled
                .key_down
                .iter()
                .find(|e| e.match_hash == raw)
                .expect("可打印选词键应在 key_down");
            assert!(e.tsf_hash & HOTKEY_POLICY_FORWARD_ONLY != 0);
        }
        assert!(
            !compiled
                .key_up
                .iter()
                .any(|e| e.action == "select_candidate"),
            "可打印选词键不该跑到 key_up 去"
        );
    }

    #[test]
    fn add_word_hotkeys_carry_global_policy() {
        let mut cfg = Config::default();
        cfg.keys.add_word = "ctrl+equal".to_string();
        cfg.keys.open_add_word_dialog = "ctrl+shift+equal".to_string();
        cfg.keys.toggle_punct = "ctrl+period".to_string();
        let compiled = Compiler::new(cfg).compile();

        let find = |a: &str| {
            compiled
                .key_down
                .iter()
                .find(|e| e.action == a)
                .unwrap()
                .clone()
        };

        // 加词两键：CHINESE_ONLY + GLOBAL 叠加
        for a in ["add_word", "open_add_word_dialog"] {
            let e = find(a);
            assert!(
                e.tsf_hash & HOTKEY_POLICY_GLOBAL != 0,
                "{a} 的 tsf_hash 应带 GLOBAL 位"
            );
            assert!(
                e.tsf_hash & HOTKEY_POLICY_CHINESE_ONLY != 0,
                "{a} 的 tsf_hash 应仍带 CHINESE_ONLY 位"
            );
            // match_hash 是规范化的原始 hash，不含任何 policy 位
            assert_eq!(
                e.match_hash & (HOTKEY_POLICY_CHINESE_ONLY | HOTKEY_POLICY_GLOBAL),
                0,
                "{a} 的 match_hash 不应含 policy 位"
            );
        }

        // 其它 chinese-only 键（toggle_punct）不该被全局拦截，避免多抢宿主快捷键
        let tp = find("toggle_punct");
        assert!(
            tp.tsf_hash & HOTKEY_POLICY_GLOBAL == 0,
            "toggle_punct 不应带 GLOBAL 位"
        );
        assert!(tp.tsf_hash & HOTKEY_POLICY_CHINESE_ONLY != 0);
    }

    /// 特殊模式直达热键现在写在 `keys.key_actions` 里（`special:<方案id>`），
    /// 编译时映射成分发端认的 `enter_special:<id>`，并带 CHINESE_ONLY | GLOBAL。
    #[test]
    fn special_mode_hotkey_compiles_with_global_policy() {
        let mut cfg = Config::default();
        cfg.keys
            .key_actions
            .insert("ctrl+shift+u".to_string(), "special:rare".to_string());
        let compiled = Compiler::new(cfg).compile();
        let e = compiled
            .key_down
            .iter()
            .find(|e| e.action == "enter_special:rare")
            .expect("key_actions 的 special:<id> 应编出 enter_special:<id>");
        // 与加词键同策略：CHINESE_ONLY | GLOBAL；match_hash 不含任何 policy 位
        assert!(e.tsf_hash & HOTKEY_POLICY_GLOBAL != 0);
        assert!(e.tsf_hash & HOTKEY_POLICY_CHINESE_ONLY != 0);
        assert_eq!(
            e.match_hash & (HOTKEY_POLICY_CHINESE_ONLY | HOTKEY_POLICY_GLOBAL),
            0
        );
    }

    #[test]
    fn temp_pinyin_hotkey_compiles_with_global_policy() {
        let mut cfg = Config::default();
        cfg.input.temp_pinyin.hotkey = "ctrl+shift+p".to_string();
        let compiled = Compiler::new(cfg).compile();
        let e = compiled
            .key_down
            .iter()
            .find(|e| e.action == "enter_temp_pinyin")
            .expect("temp_pinyin.hotkey 应编出 enter_temp_pinyin");
        assert!(e.tsf_hash & HOTKEY_POLICY_GLOBAL != 0);
        assert!(e.tsf_hash & HOTKEY_POLICY_CHINESE_ONLY != 0);
        assert_eq!(
            e.match_hash & (HOTKEY_POLICY_CHINESE_ONLY | HOTKEY_POLICY_GLOBAL),
            0
        );
    }

    /// `keys.key_actions` 编出对应动词，且与方案直达热键同策略（不带 CHINESE_ONLY）。
    ///
    /// policy 位对 `toggle_schema` 比对 `switch_schema` 更要命：带上它，切到英文方案后
    /// **回程那一下**就不响应了——功能恰好废掉一半，而"切过去"仍然好用，很容易被当成
    /// 「回程没实现」而不是「策略位配错」。
    #[test]
    fn key_actions_compile_toggle_schema_without_chinese_only_policy() {
        let mut cfg = Config::default();
        cfg.keys.key_actions.insert(
            "ctrl+shift+n".to_string(),
            "toggle_schema:english".to_string(),
        );
        let compiled = Compiler::new(cfg).compile();
        let e = compiled
            .key_down
            .iter()
            .find(|e| e.action == "toggle_schema:english")
            .expect("keys.key_actions 应编出 toggle_schema:<id>");
        assert_eq!(
            e.tsf_hash & HOTKEY_POLICY_CHINESE_ONLY,
            0,
            "往返热键不得带 CHINESE_ONLY，否则从英文方案回不来"
        );
    }

    /// 不支持的动词与解析不了的键都被丢弃，不进热键表。
    ///
    /// 守的是「静默失效」：写错的动词若混进表里，按下时分发端匹配不上，表现是「按了没反应」，
    /// 与热键没注册上完全同形，用户无从分辨自己拼错了还是功能坏了。
    #[test]
    fn key_actions_drop_unknown_verbs_and_unparsable_keys() {
        let mut cfg = Config::default();
        cfg.keys.key_actions.insert(
            "ctrl+shift+n".to_string(),
            "no_such_verb:english".to_string(),
        );
        cfg.keys
            .key_actions
            .insert("ctrl+shift+m".to_string(), "toggle_schema:".to_string());
        cfg.keys.key_actions.insert(
            "这不是热键".to_string(),
            "toggle_schema:english".to_string(),
        );
        let n = Config::default();
        let base = Compiler::new(n).compile().key_down.len();
        let compiled = Compiler::new(cfg).compile();
        assert_eq!(compiled.key_down.len(), base, "三条非法项都不该进热键表");
    }

    /// `switch_schema:<id>`（单向）能编进 key_down 表，且**不带 CHINESE_ONLY**。
    ///
    /// 这个 policy 位是重点：带上它，切到英文方案后热键就不再响应，用户切得过去、切不
    /// 回来。特殊模式热键需要它（overlay 只在中文输入中途有意义），方案切换恰恰相反——
    /// 同一个位，两种机制下后果相反。
    ///
    /// ⚠️ 本契约的测试**函数体一度整个丢失**：注释与 `#[test]` 还在，函数被后加的
    /// `key_actions_compile_toggle_schema_...` 顶替，两个 `#[test]` 落在同一个 fn 上。
    /// 编译器只报一句 `duplicated attribute` 警告，而读代码的人看到注释会以为这条契约
    /// 有守门——**「注释还在」是比「没有测试」更坏的状态**。
    #[test]
    fn switch_schema_compiles_without_chinese_only_policy() {
        let mut cfg = Config::default();
        cfg.keys.key_actions.insert(
            "ctrl+shift+r".to_string(),
            "switch_schema:wubi86".to_string(),
        );
        let compiled = Compiler::new(cfg).compile();
        let e = compiled
            .key_down
            .iter()
            .find(|e| e.action == "switch_schema:wubi86")
            .expect("switch_schema 应编进 key_down 表");
        assert_eq!(
            e.tsf_hash & HOTKEY_POLICY_CHINESE_ONLY,
            0,
            "方案切换热键不得带 CHINESE_ONLY，否则英文方案下切不回中文方案"
        );
        // 反向对照：同一份编译产物里，特殊模式热键确实是带 CHINESE_ONLY 的——
        // 否则「不带」这条断言在 policy 位整体失效时也会通过。
        let mut cfg2 = Config::default();
        cfg2.keys
            .key_actions
            .insert("ctrl+shift+u".to_string(), "special:rare".to_string());
        let c2 = Compiler::new(cfg2).compile();
        let e2 = c2
            .key_down
            .iter()
            .find(|e| e.action == "enter_special:rare")
            .unwrap();
        assert!(
            e2.tsf_hash & HOTKEY_POLICY_CHINESE_ONLY != 0,
            "对照组：特殊模式热键应带 CHINESE_ONLY"
        );
    }

    /// 残留的 `keys.schema_hotkeys` **不再生效**，且不悄悄折算进 key_actions。
    ///
    /// 兼容层已删除（用户拍板：不做向后兼容，换核心代码简洁）。但它必须**可见地**失效：
    /// 告警在 `normalize` 里发出，随后清空该表——留着会让后续任何「有没有配过」的判断
    /// 读到假信号。这里钉住「不生效」与「清空」两条，告警文本本身不断言（文案会改）。
    #[test]
    fn legacy_schema_hotkeys_are_dropped_not_migrated() {
        let mut cfg = Config::default();
        cfg.keys
            .legacy_schema_hotkeys
            .insert("wubi86".to_string(), "ctrl+shift+r".to_string());
        cfg.normalize();
        assert!(
            cfg.keys.legacy_schema_hotkeys.is_empty(),
            "告警后须清空，否则后续「有没有配过」的判断会读到假信号"
        );
        assert!(
            !cfg.keys.key_actions.contains_key("ctrl+shift+r"),
            "兼容层已删除，不该再折算进 key_actions"
        );
        let compiled = Compiler::new(cfg).compile();
        assert!(
            !compiled
                .key_down
                .iter()
                .any(|e| e.action.starts_with("switch_schema:")),
            "残留旧键不得产生任何热键条目"
        );
    }

    /// 动词 id 为空（`special:`）不产生条目；`temp_pinyin.hotkey` 默认空同理。
    ///
    /// 「id 为空」原先是 `special_modes[]` 条目的一个真实陷阱（只写 schema 不写 id 的
    /// 条目会静默不注册热键）。身份收敛到方案 id 后它不可能为空，这里只剩防脏数据。
    #[test]
    fn empty_or_idless_mode_hotkey_produces_no_entry() {
        let mut cfg = Config::default();
        cfg.keys
            .key_actions
            .insert("ctrl+shift+u".to_string(), "special:".to_string());
        cfg.keys
            .key_actions
            .insert("ctrl+shift+i".to_string(), "special:   ".to_string());
        let compiled = Compiler::new(cfg).compile();
        assert!(
            !compiled
                .key_down
                .iter()
                .any(|e| e.action.starts_with("enter_special:") || e.action == "enter_temp_pinyin"),
            "空 hotkey / 空 id 不应产生直达热键条目"
        );
    }

    /// 修饰键的 keyup hash 必须带**通用位 + 具体位**：C++ `GetCurrentModifiers()` 对
    /// 修饰键同时返回两者，只带一边会匹配不上（表现为「绑了没反应」）。
    /// 与 `compile_toggle_mode_key` 同格式——两者服务于同一条 TSF keyup 通路。
    #[test]
    fn modifier_key_up_hash_matches_toggle_format() {
        assert_eq!(
            compile_modifier_key_up_hash(VK_RSHIFT),
            compile_toggle_mode_key("rshift"),
            "同一个键经两条入口应得到同一个 hash"
        );
        assert_eq!(
            compile_modifier_key_up_hash(VK_LCONTROL),
            compile_toggle_mode_key("lctrl")
        );
        // 低 16 位是 VK，供 is_pure_modifier_vk / 分派点反查。
        let h = compile_modifier_key_up_hash(VK_RSHIFT).unwrap();
        assert_eq!(h & 0xFFFF, VK_RSHIFT);
        // 非修饰键没有 keyup 形态（CapsLock 也不在此列：它走 toggle_mode_keys 那条）。
        assert_eq!(compile_modifier_key_up_hash(VK_OEM_1), None);
        assert_eq!(compile_modifier_key_up_hash(VK_CAPITAL), None);
    }

    /// `keys.key_actions` 按**键形态**分三条通路，不按动词。
    #[test]
    fn key_action_routes_split_by_key_shape() {
        use KeyActionRoute::*;
        assert_eq!(route_of_key_action("ctrl+shift+n"), Some(Hotkey));
        assert_eq!(route_of_key_action("ctrl+space"), Some(Hotkey));
        assert_eq!(route_of_key_action("rshift"), Some(ModifierKeyUp));
        assert_eq!(route_of_key_action("lctrl"), Some(ModifierKeyUp));
        assert_eq!(route_of_key_action("backtick"), Some(LeadingKey));
        assert_eq!(route_of_key_action("semicolon"), Some(LeadingKey));
        assert_eq!(route_of_key_action("z"), Some(LeadingKey));
        assert_eq!(route_of_key_action("不存在的键"), None);
    }

    /// ★★ 单个有字符的键**绝不能**进 key_down 热键表。
    ///
    /// `parse_hotkey("backtick")` 返回的是无修饰位的裸 VK（0xC0）。进表后 TSF 会把它
    /// 当热键转发并吞掉，于是 `` ` `` 这个符号在所有方案里都再也打不出来——而用户只是
    /// 想给它绑个功能。这条是本次收编最危险的一处，故单独立测。
    #[test]
    fn single_character_key_never_enters_keydown_hotkeys() {
        let mut cfg = Config::default();
        cfg.keys
            .key_actions
            .insert("backtick".into(), "temp_pinyin".into());
        cfg.keys
            .key_actions
            .insert("semicolon".into(), "mix:quick_mix".into());
        cfg.keys
            .key_actions
            .insert("rshift".into(), "toggle_mode".into());
        cfg.keys
            .key_actions
            .insert("ctrl+shift+n".into(), "toggle_schema:wubi86".into());
        // 折算后的选词绑定（本测试直接构造 Compiler、不经 normalize，故显式写出）。
        // 顺带覆盖一个真实场景：**同一个键 `;` 同时在两张表里**——无会话时是 mix 引导键，
        // 有会话时是次选键。两张表按触发态分野，本就该能共存。
        cfg.keys.session_actions = session_actions(&[
            ("semicolon", "select_candidate:2"),
            ("quote", "select_candidate:3"),
        ]);
        let compiled = Compiler::new(cfg).compile();

        // ★ 判据是「有没有产生**带这个动词的** key_down 条目」，不是「这个 VK 在不在
        // key_down 里」——`;` / `'` 同时被会话态表以 FORWARD_ONLY 登记着，按 VK 判会把
        // 那条误当成本段的产物，测了个寂寞。
        for verb in ["temp_pinyin", "mix:quick_mix"] {
            assert!(
                !compiled.key_down.iter().any(|e| e.action == verb),
                "单键条目 {verb} 不该产生 key_down 热键，实际 {:?}",
                compiled
                    .key_down
                    .iter()
                    .map(|e| &e.action)
                    .collect::<Vec<_>>()
            );
        }
        // 反向确认分流真的生效：会话态那条 `;` 仍在（action 为空的转发登记），
        // 说明上面的「没有」不是因为整段编译被跳过了。
        assert!(
            compiled
                .key_down
                .iter()
                .any(|e| (e.match_hash & 0xFFFF) == VK_OEM_1 && e.action.is_empty()),
            "会话态表里 `;` 的转发登记应不受影响"
        );

        // 组合键照常进 key_down（收编不该动这条既有通路）。
        assert!(
            compiled
                .key_down
                .iter()
                .any(|e| e.action == "toggle_schema:wubi86"),
            "组合键条目应仍走热键通路"
        );
        // 修饰键进 key_up，且 action 是 schema_bound 而非动词本身——
        // `is_toggle_mode_keycode` 按 action 过滤，塞动词进去它就认不出来了。
        let up = compiled
            .key_up
            .iter()
            .find(|e| (e.match_hash & 0xFFFF) == VK_RSHIFT)
            .expect("rshift 应进 key_up 转发集");
        assert_eq!(up.action, "schema_bound");
    }

    /// 生僻字模式直达热键：`rare_char` 编成分发端认的 `enter_rare_char`，
    /// 策略位与 `special:` 一致（CHINESE_ONLY | GLOBAL）。
    ///
    /// GLOBAL 那半不是可选的：少了它，QQNT / Tabby 这类 Chromium 宿主会用自己的同名
    /// 加速键把这个组合吃掉，用户只在部分程序里按不出来——最难归因的一类问题。
    #[test]
    fn rare_char_hotkey_compiles_with_global_policy() {
        let mut cfg = Config::default();
        cfg.keys
            .key_actions
            .insert("ctrl+shift+r".to_string(), "rare_char".to_string());
        let compiled = Compiler::new(cfg).compile();
        let e = compiled
            .key_down
            .iter()
            .find(|e| e.action == "enter_rare_char")
            .expect("key_actions 的 rare_char 应编出 enter_rare_char");
        assert!(
            e.tsf_hash & HOTKEY_POLICY_GLOBAL != 0,
            "须带 GLOBAL，否则 Chromium 类宿主会吃掉它"
        );
        assert!(
            e.tsf_hash & HOTKEY_POLICY_CHINESE_ONLY != 0,
            "进 overlay 只在中文输入中途有意义"
        );
    }
}
