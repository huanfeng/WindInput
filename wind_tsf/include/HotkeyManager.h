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
    BOOL IsKeyDownHotkey(uint32_t keyHash) const;

    // Check if a KeyUp should be intercepted (O(1) lookup)
    // Returns true if the key matches a KeyUp hotkey in the whitelist
    BOOL IsKeyUpHotkey(uint32_t keyHash) const;

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

    // Log current configuration (for debugging)
    void LogConfig() const;

private:
    // Hotkey whitelist (KeyDown triggered)
    std::unordered_set<uint32_t> _keyDownHotkeys;

    // Hotkey whitelist (KeyUp triggered - for toggle mode keys)
    std::unordered_set<uint32_t> _keyUpHotkeys;
};
