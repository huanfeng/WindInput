//! 热键编译器
//!
//! 与 Go 版本 `wind_input/internal/hotkey/compiler.go` 对齐。
//! 将配置中的热键字符串（如 "Ctrl+Space"、"lshift"）编译为 key_hash，
//! 用于按键事件中的热键匹配。

use crate::config::Config;
use tracing::{debug, warn};

/// `keys.key_actions` 里**组合键**条目的策略表：动词 → `(分发端 action, 策略位)`；
/// 结构上走不通的动词返回 `None`。
///
/// ★ **值域就是 [`crate::BoundAction`]**，与单键、修饰键两条通路同一张表。本函数只回答
/// 「这个动词编进 key_down 热键表时该带哪些策略位」，不回答「组合键能不能绑它」——后者
/// 由 `BoundAction::parse` 认不认得该动词决定。
///
/// 2026-09-18 之前这里是一份**逐条追加**的六项白名单（`toggle_schema:` / `switch_schema:` /
/// `special:` / `rare_char` / `softkeyboard`），每接一个新功能往里加一行，于是组合键能绑的
/// 功能恒比单键少一大截——而组合键这条通路在按键链上位次更早（英文模式分水岭之前）、
/// 不与输入争键、可全局拦截，本该是限制**最少**的一条。那份白名单不是设计论证的产物，
/// 是演进残留；现已翻转为「解析得出 `BoundAction` 就收，减去结构上走不通的」。
///
/// 结构上走不通的只有两类，都不在 `BoundAction` 值域内、因而自然落 `None`：
/// `add_word` / `open_add_word_dialog` 要返回占位 composition 激活 C++ 转发全部按键，
/// 不符 `dispatch_hotkey` 的 `bool` 契约（分派端另有特判）。
///
/// ★★ **策略位必须按动词分**，不能一律不带：同一个位在两类机制下后果相反。分组与
/// `Compiler::compile` 里固定字段那几段**逐条一致**——同一个功能经两条路进表却带不同的位，
/// 表现是「在设置页配和在自定义按键里配，行为不一样」。
///
/// | 动词 | 策略位 | why |
/// |---|---|---|
/// | `toggle_schema:` / `switch_schema:` / `toggle_mode` / `switch_engine` | 无 | 它们正是用来**离开/返回英文态**的；带上 `CHINESE_ONLY` 就成了单程票 |
/// | `toggle_full_width` / `toggle_toolbar` / `open_settings` / `open_dictionary` / `take_screenshot` | 无 | 两模式下都该生效，与固定字段那段同档 |
/// | `toggle_punct` / `toggle_s2t` / `toggle_t2s` / `single_char` | `CHINESE_ONLY` | 只在中文态有意义；**不带 `GLOBAL`**，避免不必要地抢占宿主快捷键 |
/// | 进 overlay 的（`temp_pinyin` / `temp_english` / `aux_code` / `rare_char` / `reverse` / `mix:` / `special:`） | `CHINESE_ONLY \| GLOBAL` | 进 overlay 只在中文输入中途有意义；`GLOBAL` 让 TSF 用 `RegisterHotKey` 抢占，穿透 QQNT/Tabby 等 Chromium 宿主的同名加速键 |
/// | `command:<表达式>` | 无 | 跑的是用户的命令，与中英态无关（英文态下也该按得动）；不改输入状态，不需要抢占 |
/// | `softkeyboard[:<id>]` | `GLOBAL` | 面板画的是「键位 → 符号」，与中英文态无关（英文态想打个 ℃ 同样合理），C++ 侧为此专设了软键盘总闸 `IsSoftKeyboard()`；带上 `CHINESE_ONLY` 英文态连开都开不出来 |
///
/// ⚠️ `GLOBAL` 有**容量上限 16**（`TextService.cpp` 的 `kHotkeyIdAddWordBase + 16`，
/// `RegisterHotKey` 超出后静默 break）。给位要克制：只在确需穿透 Chromium 宿主时给，
/// 别为了「看起来更可靠」给整张表都挂上。
///
/// ★ 动词**原样**进表，不再改写成 `enter_special:` / `enter_rare_char` 那种分发端专用形态
/// ——分派端现在也用 `BoundAction::parse` 认串，两条通路认的是同一个值域，映射失去了意义。
/// （那层映射本身还制造过一类缺陷：新增动词时只在一处加，另一处忘了，症状是「配了没反应」。）
///
/// 值域语义见 docs/design/schema-key-actions.md §2。
/// ⚠️ 进表的是**用户写的原串**（只 trim，不规范化）：`BoundAction::parse` 对无载荷动词
/// 大小写不敏感，所以 `"TEMP_PINYIN"` 会以大写形态躺在 key_down 表里。消费方
/// （`dispatch_bound_action_hotkey`）同样用 `BoundAction::parse` 认串，故行为正确——
/// 但**别拿表里的 `action` 跟字面量直接比**，那种写法只在用户恰好写小写时成立。
pub(crate) fn hotkey_action_entry(action: &str) -> Option<(String, u32)> {
    let action = action.trim();
    let policy = hotkey_policy_for(&crate::config::BoundAction::parse(action))?;
    Some((action.to_string(), policy))
}

/// 组合键动词的策略位。`BoundAction::None`（含解析不出的未知动词）返回 `None` ⇒ 不进表。
///
/// 两个位各有各的判据，分开取：
///
/// - `CHINESE_ONLY` —— 复用 [`crate::BoundAction::only_in_chinese_mode`]，**不在这里重写一份**。
///   分派端的 `chinese_mode` 守卫读的是同一个方法，两处分叉的后果见那里的说明。
/// - `GLOBAL` —— 只给「会被 Chromium 类宿主（QQNT/Tabby）的同名加速键抢走」的那些：进
///   overlay 的一族与软键盘。⚠️ 它有**容量上限 16**（`TextService.cpp` 的
///   `kHotkeyIdAddWordBase + 16`，`RegisterHotKey` 超出后静默 break），给位要克制，
///   别为了「看起来更可靠」给整张表都挂上。
fn hotkey_policy_for(action: &crate::config::BoundAction) -> Option<u32> {
    use crate::config::BoundAction as BA;
    if matches!(action, BA::None) {
        return None;
    }
    let mut policy = 0;
    if action.only_in_chinese_mode() {
        policy |= HOTKEY_POLICY_CHINESE_ONLY;
    }
    // ⚠️ 穷举 `match`，不用 `matches!`：漏给 GLOBAL 的症状是「在 QQNT / Tabby 里按不出来、
    // 在记事本里正常」——本仓自述最难归因的一类。列全的话新增变体编译不过。
    let needs_global = match action {
        BA::TempPinyin
        | BA::TempEnglish
        | BA::AuxCode
        | BA::RareChar
        | BA::Reverse
        | BA::Mix(_)
        | BA::Special(_)
        | BA::SoftKeyboard(_) => true,
        // 不抢占：这些不进 overlay，被宿主同名加速键抢走的代价也小（最多是这一次没切成），
        // 而 GLOBAL 的槽位只有 16 个（见下方 `warn_global_hotkey_overflow`）。
        //
        // `Command` 同档：跑的是用户自己的命令，被宿主加速键抢走时换个组合键即可。
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

/// `keys.key_actions` 的一条条目该走哪条通路。由**键的形态**决定，不由动词决定。
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
/// ★ **必须与 `toggle_mode` 区分开**。`is_toggle_mode_keycode` 按 action 过滤而非按键码
/// ——若复用 `toggle_mode`，只把 CapsLock 配成翻页键的用户会在空闲敲 CapsLock 时莫名
/// 切中英文。`select_key_groups` 进 keyup 表时已经踩过一次这个坑，`schema_bound` 是
/// 第二次，本项是第三次；每次往 `key_up` 加东西都要重查这条。
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

        // ── KeyDown：软键盘开关（GLOBAL，**不带 CHINESE_ONLY**） ──
        //
        // 策略位与 `key_actions` 里 `softkeyboard` 动词那条**逐位相同**，理由见
        // `hotkey_action_entry` 中那段注释：面板画的是「键位 → 符号」的映射，跟当前是
        // 中文还是英文模式没关系（英文态想打个 ℃ 同样合理），C++ 侧为此专设了软键盘总闸
        // `IsSoftKeyboard()`；带上 CHINESE_ONLY 的话英文态连开都开不出来。GLOBAL 位保留，
        // 规避 Chromium 类宿主无视 `pfEaten` 造成的双处理。
        //
        // ★ action 用动词原文 `"softkeyboard"`（不像 `special:` 那样改写成 `enter_*`）：
        // 协调器的 `softkeyboard_hotkey` 认的就是这个串，本分支与 `key_actions` 那条通路
        // 落到**同一个**分派臂，分派端零改动。
        //
        // ⚠️ 两条通路可以同时命中同一个键（用户层 `key_actions` 里还留着 v0.120.0 物化
        // 灌进去的 `ctrl+shift+k = "softkeyboard"`）。`match_key_down` 是 `.find()`
        // 先注册者赢，本分支在前 ⇒ 行为一致、不重复分派；设置页只读组会把这种同键
        // 重复标橙提示用户清理。
        if let Some(raw) = parse_hotkey(&h.softkeyboard) {
            result.key_down.push(HotkeyEntry {
                tsf_hash: raw | HOTKEY_POLICY_GLOBAL,
                match_hash: raw,
                action: "softkeyboard".to_string(),
            });
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
        //
        // ★ action 用动词原文 `"temp_pinyin"`（`BoundAction` 的值域），不是早先那个分发端
        // 专用的 `enter_temp_pinyin`：分派端现已统一用 `BoundAction::parse` 认串，本条与
        // `key_actions` 里写 `"ctrl+;" = "temp_pinyin"` 落到**同一个**分派臂。
        let mode_policy = HOTKEY_POLICY_CHINESE_ONLY | HOTKEY_POLICY_GLOBAL;
        if let Some(raw) = parse_hotkey(&self.config.input.temp_pinyin.hotkey) {
            result.key_down.push(HotkeyEntry {
                tsf_hash: raw | mode_policy,
                match_hash: raw,
                action: "temp_pinyin".to_string(),
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
        // 策略位**逐动词**取自 `hotkey_action_entry`，本段自己不定位——同一个位在两类动词上
        // 后果相反（切方案带 CHINESE_ONLY 就成单程票，进 overlay 不带就会在英文态凭空触发），
        // 那张表是这批判断的单点，分组理由见其文档。
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
                    // 显式 `none` = 用户在本层表态「这个键别干活」，用来压制**更低层**
                    // 写在同一张表里的绑定（L2 出厂 / L2.5 定制版）。`merge_value` 与
                    // `merge_toml` 都只能新增/覆盖、表达不了删除，所以「禁用」只能靠
                    // 显式值承载——语义与单键、修饰键两条通路的 `BoundAction::None`
                    // 逐条对应，见 `Coordinator::bound_action_with_source_layered`。
                    //
                    // ★ 必须在取策略位**之前**静默跳过。落到下面那句 warn 的话，会把
                    // 「用户主动禁用」报成「配置写错了」——而这是一条预期内的配法。
                    // `none` 与「动词写错了」在 `BoundAction::parse` 眼里都是 `None`，
                    // 两者只能靠这里的先后顺序分开。
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

        // ── KeyDown：上屏注释 / 拼音（`input.alt_commit`，SESSION policy）──
        //
        // Alt+0..9 与 Alt+Space，只带 SESSION 位——与置顶/删除同一档：TSF 只在「中文 + 有会话」
        // 时吃（`OnTestKeyDown` / `OnKeyDown` 两道闸门都按这个位判），候选可见期间才被
        // `_RegisterCandidateHotkeys` 抢占成系统热键，其余时候 Alt+数字（宿主菜单加速键）、
        // Alt+空格（窗口系统菜单）原样归宿主。action 为空＝仅转发，语义在协调器
        // `try_alt_commit` 分派。
        //
        // ★ 必须排在 key_actions 与数字模板**之后**，且撞键就**不登记**：用户在 key_actions
        // 里绑过的 Alt 组合让位给用户。只靠 `.find()` 先到先得不够——TSF 那侧的白名单是按
        // 策略位分桶的集合，同一个 hash 同时进 SESSION 桶会改变用户绑定的吃键条件。
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
        //
        // 候选显示时 TSF 无条件如实上报 Ctrl（`EVENT_MOD_*`），用不着这条；它是 TSF 判无会话、
        // 服务端却有候选时的退路：TSF 判过单击才转发 Ctrl 的松开，不登记就收不到。TSF 对修饰键
        // keyup 不看 SESSION 位：有文本上下文就记待判、两向都不吃，无候选时服务端回 PassThrough。
        // 已被上面登记过的键不再追加——那些功能优先，键本来就会送到。
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

        // 二三候选键（含修饰键组 lrshift / lrctrl）的登记已并入上面的 `session_actions` 段
        // ——它按键的形态自动分流：可打印键进 key_down + FORWARD_ONLY，修饰键进 key_up。
        //
        // ⚠️ 同一个键可能既是切换键又是选词键（两条登记同 hash）：TSF 侧白名单是集合，重复
        // 无害；服务端按 action 区分，切换只认 action=="toggle_mode"，故消费端**不能**用
        // 「key_up 里有这个 key_code」当切换判据（见 is_toggle_mode_keycode）。

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
/// 与 `wind_tsf/src/TextService.cpp` 的 `kHotkeyIdAddWordBase + 16` 对齐。那边超出后
/// **静默 `break`**，只留一行 `registered=%d/%d` 的 debug 日志——说得出「少注册了几个」，
/// 说不出「是哪几个键」（DLL 那侧只有 rawHash，翻不回键名）。
const GLOBAL_HOTKEY_SLOTS: usize = 16;

/// 带 `GLOBAL` 位的条目超过槽位上限时告警，并点名会失效的那几个。
///
/// ★ **这里是唯一说得出人话的地方**：本函数既知道数量、又还拿着用户写的键名。
///
/// 触发条件真实可达（不是恒假的护栏）：出厂基线只有 2~4 条带 GLOBAL，但
/// `keys.key_actions` 里每绑一个进 overlay 的动词就加一条，绑满十来个组合键就会顶到上限。
/// 2026-09-18 组合键值域合流后 `temp_english` / `aux_code` / `mix:` 也进了这一族，
/// 从「结构上不可能顶到」变成了「结构上可能」。
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

/// 超限判定本体，抽出来是为了**可测**：`warn!` 只在日志里，断言不到。
///
/// 返回 `(带 GLOBAL 的总数, 注册不上的那些的动词)`。超出的是**靠后**的那些：
/// TSF 侧按 `GlobalHotkeys()` 的顺序注册，填满即 break。
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
#[path = "hotkey_tests.rs"]
mod tests;
