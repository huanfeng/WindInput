#pragma once

#include "Globals.h"
#include <string>
#include <vector>
#include <set>

// Toggle mode key flags
enum ToggleModeKey {
    TOGGLE_KEY_NONE     = 0,
    TOGGLE_KEY_LSHIFT   = 0x01,
    TOGGLE_KEY_RSHIFT   = 0x02,
    TOGGLE_KEY_LCTRL    = 0x04,
    TOGGLE_KEY_RCTRL    = 0x08,
    TOGGLE_KEY_CAPSLOCK = 0x10,
};

// Select key group flags
enum SelectKeyGroup {
    SELECT_GROUP_NONE           = 0,
    SELECT_GROUP_SEMICOLON_QUOTE = 0x01,  // ; '
    SELECT_GROUP_COMMA_PERIOD   = 0x02,   // , .
    SELECT_GROUP_LRSHIFT        = 0x04,   // LShift RShift
    SELECT_GROUP_LRCTRL         = 0x08,   // LCtrl RCtrl
};

// Page key group flags
enum PageKeyGroup {
    PAGE_KEY_NONE       = 0,
    PAGE_KEY_PAGEUPDOWN = 0x01,  // PgUp PgDn
    PAGE_KEY_MINUS_EQUAL = 0x02, // - =
    PAGE_KEY_BRACKETS   = 0x04,  // [ ]
    PAGE_KEY_SHIFT_TAB  = 0x08,  // Shift+Tab Tab
};

// Hotkey type (what action the key triggers)
enum class HotkeyType {
    None,           // Not a hotkey
    ToggleMode,     // Toggle Chinese/English mode
    SwitchEngine,   // Switch between Pinyin/Wubi
    ToggleFullWidth, // Toggle full-width/half-width
    TogglePunct,    // Toggle Chinese/English punctuation
    SelectCandidate2, // Select 2nd candidate
    SelectCandidate3, // Select 3rd candidate
    PageUp,         // Page up in candidates
    PageDown,       // Page down in candidates
    Letter,         // Letter input
    Number,         // Number for candidate selection
    Punctuation,    // Punctuation input
    Backspace,
    Enter,
    Escape,
    Space,
};

// Parsed hotkey (modifier + key)
struct ParsedHotkey {
    BOOL needCtrl;
    BOOL needShift;
    BOOL needAlt;
    int keyCode;  // Virtual key code

    ParsedHotkey() : needCtrl(FALSE), needShift(FALSE), needAlt(FALSE), keyCode(0) {}
};

class CHotkeyManager
{
public:
    CHotkeyManager();
    ~CHotkeyManager();

    // Update configuration from Go service (called when receiving status_update with hotkeys)
    void UpdateConfig(
        const std::vector<std::wstring>& toggleModeKeys,
        const std::wstring& switchEngine,
        const std::wstring& toggleFullWidth,
        const std::wstring& togglePunct,
        const std::vector<std::wstring>& selectKeyGroups,
        const std::vector<std::wstring>& pageKeys
    );

    // Check if a key should be intercepted given current state
    // isComposing: whether there's active composition (input buffer)
    // hasCandidates: whether there are candidates to select
    BOOL ShouldInterceptKey(WPARAM vk, int modifiers, BOOL isComposing, BOOL hasCandidates, BOOL isChineseMode);

    // Get the hotkey type for a given key
    HotkeyType GetHotkeyType(WPARAM vk, int modifiers, BOOL isComposing, BOOL hasCandidates, BOOL isChineseMode);

    // Check if a specific toggle mode key is configured
    BOOL IsToggleModeKey(WPARAM vk);

    // Check if key is punctuation
    BOOL IsPunctuationKey(WPARAM vk);

    // Convert virtual key to punctuation character
    wchar_t VirtualKeyToPunctuation(WPARAM vk, BOOL shiftPressed);

    // Log current configuration (for debugging)
    void LogConfig();

private:
    // Toggle mode keys (bitmask)
    DWORD _toggleModeKeys;

    // Function hotkeys (parsed)
    ParsedHotkey _switchEngineHotkey;
    ParsedHotkey _toggleFullWidthHotkey;
    ParsedHotkey _togglePunctHotkey;

    // Select key groups (bitmask)
    DWORD _selectKeyGroups;

    // Page key groups (bitmask)
    DWORD _pageKeyGroups;

    // Helper to parse hotkey string like "ctrl+`", "shift+space"
    ParsedHotkey _ParseHotkeyString(const std::wstring& hotkeyStr);

    // Helper to check if a key matches a parsed hotkey
    BOOL _MatchesHotkey(const ParsedHotkey& hotkey, WPARAM vk, int modifiers);

    // Check specific key types
    BOOL _IsSelectKey2(WPARAM vk, int modifiers);
    BOOL _IsSelectKey3(WPARAM vk, int modifiers);
    BOOL _IsPageUpKey(WPARAM vk, int modifiers);
    BOOL _IsPageDownKey(WPARAM vk, int modifiers);
};
