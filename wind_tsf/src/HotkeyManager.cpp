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
    OutputDebugStringW(L"[WindInput] HotkeyManager::UpdateHotkeys called\n");

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

    // Numbers 0-9
    if (vk >= '0' && vk <= '9')
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
uint32_t CHotkeyManager::GetCurrentModifiers()
{
    uint32_t modifiers = 0;

    // Check generic modifiers
    if (GetAsyncKeyState(VK_SHIFT) & 0x8000)
        modifiers |= KEYMOD_SHIFT;
    if (GetAsyncKeyState(VK_CONTROL) & 0x8000)
        modifiers |= KEYMOD_CTRL;
    if (GetAsyncKeyState(VK_MENU) & 0x8000)
        modifiers |= KEYMOD_ALT;

    // Check specific left/right modifiers
    if (GetAsyncKeyState(VK_LSHIFT) & 0x8000)
        modifiers |= KEYMOD_LSHIFT;
    if (GetAsyncKeyState(VK_RSHIFT) & 0x8000)
        modifiers |= KEYMOD_RSHIFT;
    if (GetAsyncKeyState(VK_LCONTROL) & 0x8000)
        modifiers |= KEYMOD_LCTRL;
    if (GetAsyncKeyState(VK_RCONTROL) & 0x8000)
        modifiers |= KEYMOD_RCTRL;

    // Check CapsLock state
    if (GetKeyState(VK_CAPITAL) & 0x0001)
        modifiers |= KEYMOD_CAPSLOCK;

    return modifiers;
}

void CHotkeyManager::LogConfig() const
{
    WCHAR debug[512];
    wsprintfW(debug, L"[WindInput] HotkeyManager: keyDownHotkeys=%d, keyUpHotkeys=%d\n",
              (int)_keyDownHotkeys.size(), (int)_keyUpHotkeys.size());
    OutputDebugStringW(debug);

    // Log some hotkey hashes for debugging
    if (!_keyDownHotkeys.empty())
    {
        OutputDebugStringW(L"[WindInput] KeyDown hotkeys: ");
        int count = 0;
        for (uint32_t hash : _keyDownHotkeys)
        {
            wsprintfW(debug, L"0x%08X ", hash);
            OutputDebugStringW(debug);
            if (++count >= 5)
            {
                OutputDebugStringW(L"...");
                break;
            }
        }
        OutputDebugStringW(L"\n");
    }

    if (!_keyUpHotkeys.empty())
    {
        OutputDebugStringW(L"[WindInput] KeyUp hotkeys: ");
        int count = 0;
        for (uint32_t hash : _keyUpHotkeys)
        {
            wsprintfW(debug, L"0x%08X ", hash);
            OutputDebugStringW(debug);
            if (++count >= 5)
            {
                OutputDebugStringW(L"...");
                break;
            }
        }
        OutputDebugStringW(L"\n");
    }
}
