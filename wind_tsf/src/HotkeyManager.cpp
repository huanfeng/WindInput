#include "HotkeyManager.h"

CHotkeyManager::CHotkeyManager()
{
}

CHotkeyManager::~CHotkeyManager()
{
}

void CHotkeyManager::UpdateHotkeys(const std::vector<uint32_t>& keyDownHotkeys,
                                   const std::vector<uint32_t>& keyUpHotkeys)
{
    WIND_LOG_INFO(L"HotkeyManager::UpdateHotkeys called\n");

    // Clear existing hotkeys
    _keyDownHotkeys.clear();
    _keyUpHotkeys.clear();

    // Add KeyDown hotkeys
    for (uint32_t hash : keyDownHotkeys)
    {
        _keyDownHotkeys.insert(hash);
    }

    // Add KeyUp hotkeys (for toggle mode keys like Shift, Ctrl, CapsLock)
    for (uint32_t hash : keyUpHotkeys)
    {
        _keyUpHotkeys.insert(hash);
    }

    LogConfig();
}

BOOL CHotkeyManager::IsKeyDownHotkey(uint32_t keyHash) const
{
    return _keyDownHotkeys.find(keyHash) != _keyDownHotkeys.end();
}

BOOL CHotkeyManager::IsKeyUpHotkey(uint32_t keyHash) const
{
    return _keyUpHotkeys.find(keyHash) != _keyUpHotkeys.end();
}

// Static method: Check if a virtual key is a toggle mode key
// This is a fallback that works even without hotkey whitelist sync
BOOL CHotkeyManager::IsToggleModeKeyByVK(WPARAM vk)
{
    switch (vk)
    {
    case VK_LSHIFT:
    case VK_RSHIFT:
    case VK_SHIFT:
    case VK_LCONTROL:
    case VK_RCONTROL:
    case VK_CONTROL:
    case VK_CAPITAL:
        return TRUE;
    default:
        return FALSE;
    }
}

// Static method: Classify basic input key type
HotkeyType CHotkeyManager::ClassifyInputKey(WPARAM vk, uint32_t modifiers)
{
    // If Ctrl or Alt is pressed, not a basic input key
    if (modifiers & KEYMOD_CTRL || modifiers & KEYMOD_ALT)
    {
        return HotkeyType::None;
    }

    // Letters A-Z
    if (vk >= 'A' && vk <= 'Z')
    {
        return HotkeyType::Letter;
    }

    // Numbers 0-9 (main keyboard)
    if (vk >= '0' && vk <= '9')
    {
        // Shift+number produces punctuation characters (!, @, #, $, etc.)
        // Classify as Punctuation so OnTestKeyDown always eats them in Chinese mode,
        // allowing Go service to convert to Chinese punctuation.
        if (modifiers & KEYMOD_SHIFT)
        {
            return HotkeyType::Punctuation;
        }
        return HotkeyType::Number;
    }

    // Numpad 0-9: classify as Number so they reach Go when there's an active
    // input session (e.g., quick input mode for number/amount/date conversion).
    // Without input session, Number type passes through — same as before.
    // Numpad operators (+,-,*,/,.) are also classified as Number so they can
    // participate in quick input calculations, but only during active sessions.
    if ((vk >= VK_NUMPAD0 && vk <= VK_NUMPAD9) ||
        vk == VK_MULTIPLY || vk == VK_ADD || vk == VK_SUBTRACT ||
        vk == VK_DECIMAL || vk == VK_DIVIDE)
    {
        return HotkeyType::Number;
    }

    // Punctuation keys
    if (IsPunctuationKey(vk))
    {
        return HotkeyType::Punctuation;
    }

    // Special keys
    switch (vk)
    {
    case VK_BACK:
        return HotkeyType::Backspace;
    case VK_RETURN:
        return HotkeyType::Enter;
    case VK_ESCAPE:
        return HotkeyType::Escape;
    case VK_SPACE:
        return HotkeyType::Space;
    case VK_TAB:
        return HotkeyType::Tab;
    case VK_PRIOR:  // Page Up
    case VK_NEXT:   // Page Down
        return HotkeyType::PageKey;
    case VK_LEFT:   // Left arrow
    case VK_RIGHT:  // Right arrow
    case VK_UP:     // Up arrow
    case VK_DOWN:   // Down arrow
    case VK_HOME:   // Home
    case VK_END:    // End
    case VK_DELETE: // Delete
    case VK_INSERT: // Insert (reserved for future Ctrl+Insert word management)
        return HotkeyType::CursorKey;
    }

    return HotkeyType::None;
}

// Static method: Check if key is punctuation
BOOL CHotkeyManager::IsPunctuationKey(WPARAM vk)
{
    switch (vk)
    {
    case VK_OEM_COMMA:    // , <
    case VK_OEM_PERIOD:   // . >
    case VK_OEM_1:        // ; :
    case VK_OEM_2:        // / ?
    case VK_OEM_3:        // ` ~
    case VK_OEM_4:        // [ {
    case VK_OEM_5:        // \ |
    case VK_OEM_6:        // ] }
    case VK_OEM_7:        // ' "
    case VK_OEM_MINUS:    // - _
    case VK_OEM_PLUS:     // = +
        return TRUE;
    default:
        return FALSE;
    }
}

// Static method: Convert virtual key to punctuation character
wchar_t CHotkeyManager::VirtualKeyToPunctuation(WPARAM vk, BOOL shiftPressed)
{
    switch (vk)
    {
    case VK_OEM_COMMA:   return shiftPressed ? L'<' : L',';
    case VK_OEM_PERIOD:  return shiftPressed ? L'>' : L'.';
    case VK_OEM_1:       return shiftPressed ? L':' : L';';
    case VK_OEM_2:       return shiftPressed ? L'?' : L'/';
    case VK_OEM_3:       return shiftPressed ? L'~' : L'`';
    case VK_OEM_4:       return shiftPressed ? L'{' : L'[';
    case VK_OEM_5:       return shiftPressed ? L'|' : L'\\';
    case VK_OEM_6:       return shiftPressed ? L'}' : L']';
    case VK_OEM_7:       return shiftPressed ? L'"' : L'\'';
    case VK_OEM_MINUS:   return shiftPressed ? L'_' : L'-';
    case VK_OEM_PLUS:    return shiftPressed ? L'+' : L'=';
    default:             return 0;
    }
}

// Static method: Calculate key hash for lookup
// Hash format: (modifiers << 16) | keyCode
uint32_t CHotkeyManager::CalcKeyHash(uint32_t modifiers, uint32_t keyCode)
{
    return (modifiers << 16) | (keyCode & 0xFFFF);
}

// Static method: Get current modifier state from system
// Uses both GetAsyncKeyState (hardware state) and GetKeyState (message queue state)
// to avoid missing modifiers from simulated input (e.g. AHK SendInput).
// GetAsyncKeyState can lag behind injected key events, while GetKeyState is
// synchronized with the thread's message queue and reflects the correct state
// when processing a queued keystroke.
uint32_t CHotkeyManager::GetCurrentModifiers()
{
    uint32_t modifiers = 0;

    // Check generic modifiers - union of async (hardware) and sync (message queue) state
    if ((GetAsyncKeyState(VK_SHIFT) & 0x8000) || (GetKeyState(VK_SHIFT) & 0x8000))
        modifiers |= KEYMOD_SHIFT;
    if ((GetAsyncKeyState(VK_CONTROL) & 0x8000) || (GetKeyState(VK_CONTROL) & 0x8000))
        modifiers |= KEYMOD_CTRL;
    if ((GetAsyncKeyState(VK_MENU) & 0x8000) || (GetKeyState(VK_MENU) & 0x8000))
        modifiers |= KEYMOD_ALT;

    // Check specific left/right modifiers
    if ((GetAsyncKeyState(VK_LSHIFT) & 0x8000) || (GetKeyState(VK_LSHIFT) & 0x8000))
        modifiers |= KEYMOD_LSHIFT;
    if ((GetAsyncKeyState(VK_RSHIFT) & 0x8000) || (GetKeyState(VK_RSHIFT) & 0x8000))
        modifiers |= KEYMOD_RSHIFT;
    if ((GetAsyncKeyState(VK_LCONTROL) & 0x8000) || (GetKeyState(VK_LCONTROL) & 0x8000))
        modifiers |= KEYMOD_LCTRL;
    if ((GetAsyncKeyState(VK_RCONTROL) & 0x8000) || (GetKeyState(VK_RCONTROL) & 0x8000))
        modifiers |= KEYMOD_RCTRL;

    // Check CapsLock state
    if (GetKeyState(VK_CAPITAL) & 0x0001)
        modifiers |= KEYMOD_CAPSLOCK;

    return modifiers;
}

// Static method: Normalize modifiers for function hotkey matching
// This keeps only generic modifiers (SHIFT, CTRL, ALT) and strips specific ones (LSHIFT, RSHIFT, etc.)
// This is used for matching function hotkeys like Ctrl+` where we don't care about left/right Ctrl
uint32_t CHotkeyManager::NormalizeModifiers(uint32_t modifiers)
{
    // Keep only generic modifiers and CapsLock
    return modifiers & (KEYMOD_SHIFT | KEYMOD_CTRL | KEYMOD_ALT | KEYMOD_CAPSLOCK);
}

void CHotkeyManager::LogConfig() const
{
    WIND_LOG_DEBUG_FMT(L"HotkeyManager: keyDownHotkeys=%d, keyUpHotkeys=%d\n",
              (int)_keyDownHotkeys.size(), (int)_keyUpHotkeys.size());

    // Log some hotkey hashes for debugging
    if (!_keyDownHotkeys.empty())
    {
        WCHAR hashBuf[256] = L"";
        int count = 0;
        for (uint32_t hash : _keyDownHotkeys)
        {
            WCHAR temp[16];
            wsprintfW(temp, L"0x%08X ", hash);
            wcscat_s(hashBuf, temp);
            if (++count >= 5)
            {
                wcscat_s(hashBuf, L"...");
                break;
            }
        }
        WIND_LOG_DEBUG_FMT(L"KeyDown hotkeys: %s\n", hashBuf);
    }

    if (!_keyUpHotkeys.empty())
    {
        WCHAR hashBuf[256] = L"";
        int count = 0;
        for (uint32_t hash : _keyUpHotkeys)
        {
            WCHAR temp[16];
            wsprintfW(temp, L"0x%08X ", hash);
            wcscat_s(hashBuf, temp);
            if (++count >= 5)
            {
                wcscat_s(hashBuf, L"...");
                break;
            }
        }
        WIND_LOG_DEBUG_FMT(L"KeyUp hotkeys: %s\n", hashBuf);
    }
}
