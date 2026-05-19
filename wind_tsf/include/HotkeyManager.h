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

    // Update hotkey whitelist from Go service (binary protocol)
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

    // Check if a KeyUp should be intercepted (O(1) lookup)
    // Returns true if the key matches a KeyUp hotkey in the whitelist
    BOOL IsKeyUpHotkey(uint32_t keyHash) const;

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

    // Hotkey policy bits (与 Go 侧 ipc.HotkeyPolicy* 对齐).
    // Go 在 keyDown 哈希高 2 位编码该热键的"何时吃"策略；C++ 收到后剥离 policy 位、
    // 按 bit 把哈希分流到 _keyDownHotkeys / _keyDownChineseOnly / _keyDownSession.
    static constexpr uint32_t HOTKEY_POLICY_CHINESE_ONLY = 0x40000000;
    static constexpr uint32_t HOTKEY_POLICY_SESSION      = 0x80000000;
    static constexpr uint32_t HOTKEY_POLICY_MASK         = HOTKEY_POLICY_CHINESE_ONLY | HOTKEY_POLICY_SESSION;

private:
    // Hotkey whitelist (KeyDown triggered) — 两模式都吃
    std::unordered_set<uint32_t> _keyDownHotkeys;

    // 仅中文模式吃
    std::unordered_set<uint32_t> _keyDownChineseOnly;

    // 仅中文模式 + 有 session 吃
    std::unordered_set<uint32_t> _keyDownSession;

    // Hotkey whitelist (KeyUp triggered - for toggle mode keys)
    std::unordered_set<uint32_t> _keyUpHotkeys;
};
