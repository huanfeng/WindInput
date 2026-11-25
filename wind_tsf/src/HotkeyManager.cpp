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
    _keyDownChineseOnly.clear();
    _keyDownSession.clear();
    _globalHotkeys.clear();
    _keyDownForwardOnly.clear();
    _keyUpHotkeys.clear();
    _keyUpSession.clear();
    _keyUpNonSession.clear();

    // Add KeyDown hotkeys — 按 policy 位分流到三个 set
    for (uint32_t hash : keyDownHotkeys)
    {
        uint32_t rawHash = hash & ~HOTKEY_POLICY_MASK;
        if (hash & HOTKEY_POLICY_SESSION)
        {
            _keyDownSession.insert(rawHash);
        }
        else if (hash & HOTKEY_POLICY_CHINESE_ONLY)
        {
            _keyDownChineseOnly.insert(rawHash);
        }
        else
        {
            _keyDownHotkeys.insert(rawHash);
        }
        // GLOBAL 是正交标记（与上面三选一叠加）：额外收进 _globalHotkeys 供 RegisterHotKey。
        if (hash & HOTKEY_POLICY_GLOBAL)
        {
            _globalHotkeys.insert(rawHash);
        }
        // FORWARD_ONLY 同为正交标记：标记「这不是动作热键，只是让 TSF 认得的转发登记」。
        if (hash & HOTKEY_POLICY_FORWARD_ONLY)
        {
            _keyDownForwardOnly.insert(rawHash);
        }
    }

    // Add KeyUp hotkeys (for toggle mode keys like Shift, Ctrl, CapsLock)
    // 必须剥 policy 位：查询侧（IsKeyUpHotkey）传的是裸 hash，`keys.session_actions` 的绑定
    // 带 SESSION 位，不剥会静默查不到。
    for (uint32_t hash : keyUpHotkeys)
    {
        uint32_t rawHash = hash & ~HOTKEY_POLICY_MASK;
        _keyUpHotkeys.insert(rawHash);
        if (hash & HOTKEY_POLICY_SESSION)
        {
            _keyUpSession.insert(rawHash);
        }
        else
        {
            _keyUpNonSession.insert(rawHash);
        }
    }

    LogConfig();
}

BOOL CHotkeyManager::IsKeyDownHotkey(uint32_t keyHash) const
{
    return _keyDownHotkeys.find(keyHash) != _keyDownHotkeys.end();
}

BOOL CHotkeyManager::IsKeyDownChineseOnlyHotkey(uint32_t keyHash) const
{
    return _keyDownChineseOnly.find(keyHash) != _keyDownChineseOnly.end();
}

BOOL CHotkeyManager::IsKeyDownSessionHotkey(uint32_t keyHash) const
{
    return _keyDownSession.find(keyHash) != _keyDownSession.end();
}

BOOL CHotkeyManager::IsKeyDownForwardOnlyHotkey(uint32_t keyHash) const
{
    return _keyDownForwardOnly.find(keyHash) != _keyDownForwardOnly.end();
}

BOOL CHotkeyManager::IsKeyUpHotkey(uint32_t keyHash) const
{
    return _keyUpHotkeys.find(keyHash) != _keyUpHotkeys.end();
}

BOOL CHotkeyManager::IsKeyUpSessionOnlyHotkey(uint32_t keyHash) const
{
    // 「只有会话语义」＝ 在 session 子集里，且不在 non-session 子集里。
    // 两者都有时返回 FALSE（按 toggle 语义恒吃），理由见头文件声明处。
    return _keyUpSession.find(keyHash) != _keyUpSession.end()
        && _keyUpNonSession.find(keyHash) == _keyUpNonSession.end();
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
        // allowing the service to convert to Chinese punctuation.
        if (modifiers & KEYMOD_SHIFT)
        {
            return HotkeyType::Punctuation;
        }
        return HotkeyType::Number;
    }

    // 小键盘数字与运算符（+,-,*,/,.）归为 Number：有输入会话时送服务端（如快捷输入的计算），
    // 无会话时照常透传。
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
// GetAsyncKeyState 与 GetKeyState 并用：前者可能落后于注入的按键（如 AHK SendInput），
// 后者与线程消息队列同步，处理排队按键时才准。
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

    // 不并入 CapsLock 锁定态：服务端编译的热键 hash 不含此位，混进来会让大写锁定期间所有热键失配。
    // 「CapsLock 作切换键」走显式 CalcKeyHash(KEYMOD_CAPSLOCK, VK_CAPITAL)（OnKeyUp 的 VK_CAPITAL 分支）。

    return modifiers;
}

// Static method: Normalize modifiers for function hotkey matching
// This keeps only generic modifiers (SHIFT, CTRL, ALT) and strips specific ones (LSHIFT, RSHIFT, etc.)
// This is used for matching function hotkeys like Ctrl+` where we don't care about left/right Ctrl
uint32_t CHotkeyManager::NormalizeModifiers(uint32_t modifiers)
{
    // Keep only generic modifiers（CapsLock 锁定态不参与热键匹配，见 GetCurrentModifiers 注释）
    return modifiers & (KEYMOD_SHIFT | KEYMOD_CTRL | KEYMOD_ALT);
}

void CHotkeyManager::LogConfig() const
{
    WIND_LOG_DEBUG_FMT(L"HotkeyManager: keyDownHotkeys=%d, chineseOnly=%d, session=%d, forwardOnly=%d, keyUpHotkeys=%d\n",
              (int)_keyDownHotkeys.size(), (int)_keyDownChineseOnly.size(),
              (int)_keyDownSession.size(), (int)_keyDownForwardOnly.size(), (int)_keyUpHotkeys.size());

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
