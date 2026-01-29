#include "HotkeyManager.h"
#include <algorithm>
#include <cctype>

CHotkeyManager::CHotkeyManager()
    : _toggleModeKeys(TOGGLE_KEY_LSHIFT | TOGGLE_KEY_RSHIFT)  // Default: both shift keys
    , _selectKeyGroups(SELECT_GROUP_SEMICOLON_QUOTE)          // Default: ; '
    , _pageKeyGroups(PAGE_KEY_PAGEUPDOWN | PAGE_KEY_MINUS_EQUAL) // Default: PgUp/Dn, -/=
{
    // Default function hotkeys
    _switchEngineHotkey.needCtrl = TRUE;
    _switchEngineHotkey.keyCode = VK_OEM_3;  // Ctrl+`

    _toggleFullWidthHotkey.needShift = TRUE;
    _toggleFullWidthHotkey.keyCode = VK_SPACE;  // Shift+Space

    _togglePunctHotkey.needCtrl = TRUE;
    _togglePunctHotkey.keyCode = VK_OEM_PERIOD;  // Ctrl+.
}

CHotkeyManager::~CHotkeyManager()
{
}

void CHotkeyManager::UpdateConfig(
    const std::vector<std::wstring>& toggleModeKeys,
    const std::wstring& switchEngine,
    const std::wstring& toggleFullWidth,
    const std::wstring& togglePunct,
    const std::vector<std::wstring>& selectKeyGroups,
    const std::vector<std::wstring>& pageKeys)
{
    OutputDebugStringW(L"[WindInput] HotkeyManager::UpdateConfig called\n");

    // Parse toggle mode keys
    _toggleModeKeys = TOGGLE_KEY_NONE;
    for (const auto& key : toggleModeKeys)
    {
        if (key == L"lshift") _toggleModeKeys |= TOGGLE_KEY_LSHIFT;
        else if (key == L"rshift") _toggleModeKeys |= TOGGLE_KEY_RSHIFT;
        else if (key == L"lctrl") _toggleModeKeys |= TOGGLE_KEY_LCTRL;
        else if (key == L"rctrl") _toggleModeKeys |= TOGGLE_KEY_RCTRL;
        else if (key == L"capslock") _toggleModeKeys |= TOGGLE_KEY_CAPSLOCK;
    }

    // Parse function hotkeys
    _switchEngineHotkey = _ParseHotkeyString(switchEngine);
    _toggleFullWidthHotkey = _ParseHotkeyString(toggleFullWidth);
    _togglePunctHotkey = _ParseHotkeyString(togglePunct);

    // Parse select key groups
    _selectKeyGroups = SELECT_GROUP_NONE;
    for (const auto& group : selectKeyGroups)
    {
        if (group == L"semicolon_quote") _selectKeyGroups |= SELECT_GROUP_SEMICOLON_QUOTE;
        else if (group == L"comma_period") _selectKeyGroups |= SELECT_GROUP_COMMA_PERIOD;
        else if (group == L"lrshift") _selectKeyGroups |= SELECT_GROUP_LRSHIFT;
        else if (group == L"lrctrl") _selectKeyGroups |= SELECT_GROUP_LRCTRL;
    }

    // Parse page key groups
    _pageKeyGroups = PAGE_KEY_NONE;
    for (const auto& pk : pageKeys)
    {
        if (pk == L"pageupdown") _pageKeyGroups |= PAGE_KEY_PAGEUPDOWN;
        else if (pk == L"minus_equal") _pageKeyGroups |= PAGE_KEY_MINUS_EQUAL;
        else if (pk == L"brackets") _pageKeyGroups |= PAGE_KEY_BRACKETS;
        else if (pk == L"shift_tab") _pageKeyGroups |= PAGE_KEY_SHIFT_TAB;
    }

    LogConfig();
}

ParsedHotkey CHotkeyManager::_ParseHotkeyString(const std::wstring& hotkeyStr)
{
    ParsedHotkey result;

    if (hotkeyStr.empty() || hotkeyStr == L"none")
    {
        return result;  // No hotkey
    }

    // Parse modifiers and key
    if (hotkeyStr == L"ctrl+`")
    {
        result.needCtrl = TRUE;
        result.keyCode = VK_OEM_3;  // ` key
    }
    else if (hotkeyStr == L"ctrl+shift+e")
    {
        result.needCtrl = TRUE;
        result.needShift = TRUE;
        result.keyCode = 'E';
    }
    else if (hotkeyStr == L"shift+space")
    {
        result.needShift = TRUE;
        result.keyCode = VK_SPACE;
    }
    else if (hotkeyStr == L"ctrl+shift+space")
    {
        result.needCtrl = TRUE;
        result.needShift = TRUE;
        result.keyCode = VK_SPACE;
    }
    else if (hotkeyStr == L"ctrl+.")
    {
        result.needCtrl = TRUE;
        result.keyCode = VK_OEM_PERIOD;
    }
    else if (hotkeyStr == L"ctrl+,")
    {
        result.needCtrl = TRUE;
        result.keyCode = VK_OEM_COMMA;
    }

    return result;
}

BOOL CHotkeyManager::_MatchesHotkey(const ParsedHotkey& hotkey, WPARAM vk, int modifiers)
{
    if (hotkey.keyCode == 0)
        return FALSE;  // No hotkey configured

    BOOL hasCtrl = (modifiers & KEY_MOD_CTRL) != 0;
    BOOL hasShift = (modifiers & KEY_MOD_SHIFT) != 0;
    BOOL hasAlt = (modifiers & KEY_MOD_ALT) != 0;

    return (hotkey.needCtrl == hasCtrl) &&
           (hotkey.needShift == hasShift) &&
           (hotkey.needAlt == hasAlt) &&
           (hotkey.keyCode == (int)vk);
}

BOOL CHotkeyManager::IsToggleModeKey(WPARAM vk)
{
    switch (vk)
    {
    case VK_LSHIFT:
        return (_toggleModeKeys & TOGGLE_KEY_LSHIFT) != 0;
    case VK_RSHIFT:
        return (_toggleModeKeys & TOGGLE_KEY_RSHIFT) != 0;
    case VK_SHIFT:
        // Generic shift - check if either left or right is configured
        return (_toggleModeKeys & (TOGGLE_KEY_LSHIFT | TOGGLE_KEY_RSHIFT)) != 0;
    case VK_LCONTROL:
        return (_toggleModeKeys & TOGGLE_KEY_LCTRL) != 0;
    case VK_RCONTROL:
        return (_toggleModeKeys & TOGGLE_KEY_RCTRL) != 0;
    case VK_CAPITAL:
        return (_toggleModeKeys & TOGGLE_KEY_CAPSLOCK) != 0;
    default:
        return FALSE;
    }
}

BOOL CHotkeyManager::_IsSelectKey2(WPARAM vk, int modifiers)
{
    // Only valid when no modifiers (except for shift-based selection)
    if (modifiers & KEY_MOD_CTRL || modifiers & KEY_MOD_ALT)
        return FALSE;

    if ((_selectKeyGroups & SELECT_GROUP_SEMICOLON_QUOTE) && vk == VK_OEM_1)  // ;
        return TRUE;
    if ((_selectKeyGroups & SELECT_GROUP_COMMA_PERIOD) && vk == VK_OEM_COMMA)  // ,
        return TRUE;
    if ((_selectKeyGroups & SELECT_GROUP_LRSHIFT) && vk == VK_LSHIFT)
        return TRUE;
    if ((_selectKeyGroups & SELECT_GROUP_LRCTRL) && vk == VK_LCONTROL)
        return TRUE;

    return FALSE;
}

BOOL CHotkeyManager::_IsSelectKey3(WPARAM vk, int modifiers)
{
    if (modifiers & KEY_MOD_CTRL || modifiers & KEY_MOD_ALT)
        return FALSE;

    if ((_selectKeyGroups & SELECT_GROUP_SEMICOLON_QUOTE) && vk == VK_OEM_7)  // '
        return TRUE;
    if ((_selectKeyGroups & SELECT_GROUP_COMMA_PERIOD) && vk == VK_OEM_PERIOD)  // .
        return TRUE;
    if ((_selectKeyGroups & SELECT_GROUP_LRSHIFT) && vk == VK_RSHIFT)
        return TRUE;
    if ((_selectKeyGroups & SELECT_GROUP_LRCTRL) && vk == VK_RCONTROL)
        return TRUE;

    return FALSE;
}

BOOL CHotkeyManager::_IsPageUpKey(WPARAM vk, int modifiers)
{
    if ((_pageKeyGroups & PAGE_KEY_PAGEUPDOWN) && vk == VK_PRIOR)  // Page Up
        return TRUE;
    if ((_pageKeyGroups & PAGE_KEY_MINUS_EQUAL) && vk == VK_OEM_MINUS)  // -
        return TRUE;
    if ((_pageKeyGroups & PAGE_KEY_BRACKETS) && vk == VK_OEM_4)  // [
        return TRUE;
    if ((_pageKeyGroups & PAGE_KEY_SHIFT_TAB) && vk == VK_TAB && (modifiers & KEY_MOD_SHIFT))  // Shift+Tab
        return TRUE;

    return FALSE;
}

BOOL CHotkeyManager::_IsPageDownKey(WPARAM vk, int modifiers)
{
    if ((_pageKeyGroups & PAGE_KEY_PAGEUPDOWN) && vk == VK_NEXT)  // Page Down
        return TRUE;
    if ((_pageKeyGroups & PAGE_KEY_MINUS_EQUAL) && (vk == VK_OEM_PLUS || vk == VK_OEM_NEC_EQUAL))  // =
        return TRUE;
    if ((_pageKeyGroups & PAGE_KEY_BRACKETS) && vk == VK_OEM_6)  // ]
        return TRUE;
    if ((_pageKeyGroups & PAGE_KEY_SHIFT_TAB) && vk == VK_TAB && !(modifiers & KEY_MOD_SHIFT))  // Tab (without shift)
        return TRUE;

    return FALSE;
}

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
        return TRUE;
    default:
        return FALSE;
    }
}

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
    default:             return 0;
    }
}

HotkeyType CHotkeyManager::GetHotkeyType(WPARAM vk, int modifiers, BOOL isComposing, BOOL hasCandidates, BOOL isChineseMode)
{
    // Check function hotkeys first (these work in any state)
    if (_MatchesHotkey(_switchEngineHotkey, vk, modifiers))
        return HotkeyType::SwitchEngine;
    if (_MatchesHotkey(_toggleFullWidthHotkey, vk, modifiers))
        return HotkeyType::ToggleFullWidth;
    if (_MatchesHotkey(_togglePunctHotkey, vk, modifiers))
        return HotkeyType::TogglePunct;

    // Check toggle mode keys (no modifiers required for toggle keys themselves)
    if (!(modifiers & KEY_MOD_CTRL) && !(modifiers & KEY_MOD_ALT))
    {
        if (IsToggleModeKey(vk))
            return HotkeyType::ToggleMode;
    }

    // Skip further checks if Ctrl or Alt is pressed (let system handle)
    if (modifiers & KEY_MOD_CTRL || modifiers & KEY_MOD_ALT)
        return HotkeyType::None;

    // Candidate state specific keys
    if (hasCandidates)
    {
        // Select keys
        if (_IsSelectKey2(vk, modifiers))
            return HotkeyType::SelectCandidate2;
        if (_IsSelectKey3(vk, modifiers))
            return HotkeyType::SelectCandidate3;

        // Page keys
        if (_IsPageUpKey(vk, modifiers))
            return HotkeyType::PageUp;
        if (_IsPageDownKey(vk, modifiers))
            return HotkeyType::PageDown;

        // Number keys for selection
        if (vk >= '1' && vk <= '9')
            return HotkeyType::Number;

        // Space to select first candidate
        if (vk == VK_SPACE)
            return HotkeyType::Space;
    }

    // Composing state specific keys
    if (isComposing)
    {
        if (vk == VK_BACK)
            return HotkeyType::Backspace;
        if (vk == VK_RETURN)
            return HotkeyType::Enter;
        if (vk == VK_ESCAPE)
            return HotkeyType::Escape;
        if (vk == VK_SPACE)
            return HotkeyType::Space;

        // Letters during composition
        if (vk >= 'A' && vk <= 'Z')
            return HotkeyType::Letter;

        // Punctuation during composition (may trigger punct-commit)
        if (IsPunctuationKey(vk))
            return HotkeyType::Punctuation;
    }

    // Chinese mode specific keys (start composition)
    if (isChineseMode)
    {
        // Letters start composition
        if (vk >= 'A' && vk <= 'Z')
            return HotkeyType::Letter;

        // Punctuation for direct conversion
        if (IsPunctuationKey(vk))
            return HotkeyType::Punctuation;
    }
    else
    {
        // English mode - pass letters through
        if (vk >= 'A' && vk <= 'Z')
            return HotkeyType::Letter;
    }

    return HotkeyType::None;
}

BOOL CHotkeyManager::ShouldInterceptKey(WPARAM vk, int modifiers, BOOL isComposing, BOOL hasCandidates, BOOL isChineseMode)
{
    HotkeyType type = GetHotkeyType(vk, modifiers, isComposing, hasCandidates, isChineseMode);
    return type != HotkeyType::None;
}

void CHotkeyManager::LogConfig()
{
    WCHAR debug[512];
    wsprintfW(debug, L"[WindInput] HotkeyManager config: toggleModeKeys=0x%x, selectKeyGroups=0x%x, pageKeyGroups=0x%x\n",
              _toggleModeKeys, _selectKeyGroups, _pageKeyGroups);
    OutputDebugStringW(debug);

    wsprintfW(debug, L"[WindInput] switchEngine: ctrl=%d shift=%d key=%d\n",
              _switchEngineHotkey.needCtrl, _switchEngineHotkey.needShift, _switchEngineHotkey.keyCode);
    OutputDebugStringW(debug);

    wsprintfW(debug, L"[WindInput] toggleFullWidth: ctrl=%d shift=%d key=%d\n",
              _toggleFullWidthHotkey.needCtrl, _toggleFullWidthHotkey.needShift, _toggleFullWidthHotkey.keyCode);
    OutputDebugStringW(debug);

    wsprintfW(debug, L"[WindInput] togglePunct: ctrl=%d shift=%d key=%d\n",
              _togglePunctHotkey.needCtrl, _togglePunctHotkey.needShift, _togglePunctHotkey.keyCode);
    OutputDebugStringW(debug);
}
