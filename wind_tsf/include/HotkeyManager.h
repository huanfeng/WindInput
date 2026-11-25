#pragma once

#include "Globals.h"
#include "BinaryProtocol.h"
#include <vector>
#include <unordered_set>

// Hotkey type (what action the key triggers)
enum class HotkeyType {
    None,           // Not a hotkey
    ToggleMode,     // Toggle Chinese/English mode (KeyUp triggered)
    Hotkey,         // Generic hotkey (KeyDown triggered)
    Letter,         // Letter input
    Number,         // Number for candidate selection
    Punctuation,    // Punctuation input
    Backspace,
    Enter,
    Escape,
    Space,
    Tab,
    PageKey,        // Page up/down
    CursorKey,      // Cursor movement (Left/Right/Home/End)
    SelectKey,      // Select candidate 2/3
};

class CHotkeyManager
{
public:
    CHotkeyManager();
    ~CHotkeyManager();

    // Update hotkey whitelist from the service (binary protocol)
    void UpdateHotkeys(const std::vector<uint32_t>& keyDownHotkeys,
                       const std::vector<uint32_t>& keyUpHotkeys);

    // Check if a KeyDown should be intercepted (O(1) lookup)
    // Returns true if the key matches a KeyDown hotkey in the whitelist
    // 语义：两模式都吃（始终 pfEaten=TRUE）
    BOOL IsKeyDownHotkey(uint32_t keyHash) const;

    // 仅中文模式吃。命中后中文 → 吃；英文 → 透传。
    BOOL IsKeyDownChineseOnlyHotkey(uint32_t keyHash) const;

    // 仅中文模式 + 有 composition / 候选时吃。其它情形透传。
    BOOL IsKeyDownSessionHotkey(uint32_t keyHash) const;

    // 「仅注册转发」的键（翻页键组 / 选词键组），与 _keyDownHotkeys 叠加的正交标记。
    // 命中它的键在无会话时必须放行并继续往下走（不是 return），交给 ClassifyInputKey 按普通
    // 标点处理。真动作热键不带此标记，任何时候都吃。
    BOOL IsKeyDownForwardOnlyHotkey(uint32_t keyHash) const;

    // Check if a KeyUp should be intercepted (O(1) lookup)
    // Returns true if the key matches a KeyUp hotkey in the whitelist
    BOOL IsKeyUpHotkey(uint32_t keyHash) const;

    // 该 keyup 键只有会话语义（keys.session_actions 里的绑定），没有 toggle 语义。
    // 唯一用途是决定 CapsLock 的 keydown 该不该吃：配成 toggle_mode_keys 时恒吃；只配会话态
    // 绑定时无会话必须放行，否则大小写锁定永远切不动。两者都配时按 toggle（恒吃），否则
    // toggle 会静默失效。
    BOOL IsKeyUpSessionOnlyHotkey(uint32_t keyHash) const;

    // Check if a virtual key is a toggle mode key (Shift/Ctrl for mode switch)
    // This is a fallback that works even without hotkey whitelist sync
    static BOOL IsToggleModeKeyByVK(WPARAM vk);

    // Check if any hotkeys are configured
    BOOL HasHotkeys() const { return !_keyDownHotkeys.empty() || !_keyUpHotkeys.empty(); }

    // Check if a key is a basic input key (letter, number, punctuation)
    // These don't need hotkey lookup, just basic classification
    static HotkeyType ClassifyInputKey(WPARAM vk, uint32_t modifiers);

    // Check if key is punctuation
    static BOOL IsPunctuationKey(WPARAM vk);

    // Convert virtual key to punctuation character
    static wchar_t VirtualKeyToPunctuation(WPARAM vk, BOOL shiftPressed);

    // Calculate key hash for lookup
    static uint32_t CalcKeyHash(uint32_t modifiers, uint32_t keyCode);

    // Get current modifier state
    static uint32_t GetCurrentModifiers();

    // Normalize modifiers for function hotkey matching
    // This strips specific left/right modifiers, keeping only generic modifiers
    // E.g., (ModCtrl | ModLCtrl) -> ModCtrl
    static uint32_t NormalizeModifiers(uint32_t modifiers);

    // Log current configuration (for debugging)
    void LogConfig() const;

    // Hotkey policy bits（与服务端协议的热键策略位对齐）。
    // 服务端在 keyDown 哈希高位编码该热键的「何时吃」策略；C++ 收到后剥离 policy 位、
    // 按位把哈希分流到 _keyDownHotkeys / _keyDownChineseOnly / _keyDownSession 等集合。
    static constexpr uint32_t HOTKEY_POLICY_CHINESE_ONLY = 0x40000000;
    static constexpr uint32_t HOTKEY_POLICY_SESSION      = 0x80000000;
    // 全局拦截位（正交，与 CHINESE_ONLY 叠加）：TSF 在中文+文本框时用 RegisterHotKey
    // 把这些键注册为系统级热键，规避 Chromium 类宿主的加速键双处理。
    static constexpr uint32_t HOTKEY_POLICY_GLOBAL       = 0x20000000;
    // 仅注册转发位（正交，与上面三选一叠加）：翻页/选词键组这类无动作的登记项。
    static constexpr uint32_t HOTKEY_POLICY_FORWARD_ONLY = 0x10000000;
    static constexpr uint32_t HOTKEY_POLICY_MASK         = HOTKEY_POLICY_CHINESE_ONLY | HOTKEY_POLICY_SESSION
                                                         | HOTKEY_POLICY_GLOBAL | HOTKEY_POLICY_FORWARD_ONLY;

    // 需全局拦截的热键 raw hash（GLOBAL 位命中，已剥 policy）。供 RegisterHotKey 反解 (mods,vk)。
    const std::unordered_set<uint32_t>& GlobalHotkeys() const { return _globalHotkeys; }

    // SESSION 策略的 keyDown 热键 raw hash（已剥 policy）。供候选热键的 RegisterHotKey
    // 反解 (mods, vk)：SESSION 即「只在有会话时吃」，候选热键正是候选可见时注册、消失时卸载
    // （见 CTextService::NotifyCandidatesVisibilityChanged）。C++ 侧别再硬编码候选热键的修饰键
    // 组合，配置改了组合就会漏注册。
    const std::unordered_set<uint32_t>& SessionHotkeys() const { return _keyDownSession; }

private:
    // Hotkey whitelist (KeyDown triggered) — 两模式都吃
    std::unordered_set<uint32_t> _keyDownHotkeys;

    // 仅中文模式吃
    std::unordered_set<uint32_t> _keyDownChineseOnly;

    // 仅中文模式 + 有 session 吃
    std::unordered_set<uint32_t> _keyDownSession;

    // 需全局拦截（RegisterHotKey）的热键 raw hash，与 _keyDownChineseOnly 叠加（正交标记）
    std::unordered_set<uint32_t> _globalHotkeys;

    // 仅注册转发的键 raw hash（翻页/选词键组），与 _keyDownHotkeys 叠加（正交标记）
    std::unordered_set<uint32_t> _keyDownForwardOnly;

    // Hotkey whitelist (KeyUp triggered - for toggle mode keys)
    std::unordered_set<uint32_t> _keyUpHotkeys;

    // keyup 登记按语义再分两个正交子集（都已剥 policy 位，与 _keyUpHotkeys 同 key）：
    // 带 SESSION 位的进 _keyUpSession（keys.session_actions 的绑定），其余进 _keyUpNonSession
    // （toggle_mode / select_candidate / schema_bound）。同一个键可能两边都有。
    std::unordered_set<uint32_t> _keyUpSession;
    std::unordered_set<uint32_t> _keyUpNonSession;
};
