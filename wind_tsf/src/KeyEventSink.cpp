#include "KeyEventSink.h"
#include "TextService.h"
#include "IPCClient.h"
#include "HotkeyManager.h"
#include "BinaryProtocol.h"
#include <cctype>
#include <cstdio>  // for swprintf

namespace
{
    const wchar_t* _HotkeyTypeName(HotkeyType type)
    {
        switch (type)
        {
        case HotkeyType::None: return L"none";
        case HotkeyType::ToggleMode: return L"toggle_mode";
        case HotkeyType::Hotkey: return L"hotkey";
        case HotkeyType::Letter: return L"letter";
        case HotkeyType::Number: return L"number";
        case HotkeyType::Punctuation: return L"punctuation";
        case HotkeyType::Backspace: return L"backspace";
        case HotkeyType::Enter: return L"enter";
        case HotkeyType::Escape: return L"escape";
        case HotkeyType::Space: return L"space";
        case HotkeyType::Tab: return L"tab";
        case HotkeyType::PageKey: return L"page_key";
        case HotkeyType::CursorKey: return L"cursor_key";
        case HotkeyType::SelectKey: return L"select_key";
        }

        return L"unknown";
    }

    void _LogKeyDecision(const wchar_t* phase, ULONGLONG focusSessionId, WPARAM keyCode, uint32_t modifiers,
                         HotkeyType keyType, BOOL chineseMode, BOOL hasComposition, BOOL hasCandidates,
                         BOOL hasInputSession, BOOL eaten, const wchar_t* decision)
    {
        WindLog::OutputFmt(
            5,
            L"compat.key phase=%ls focusSession=%llu vk=0x%02X mods=0x%04X keyType=%ls chinese=%d composing=%d candidates=%d inputSession=%d eaten=%d decision=%ls",
            phase,
            focusSessionId,
            (uint32_t)keyCode,
            modifiers,
            _HotkeyTypeName(keyType),
            chineseMode ? 1 : 0,
            hasComposition ? 1 : 0,
            hasCandidates ? 1 : 0,
            hasInputSession ? 1 : 0,
            eaten ? 1 : 0,
            decision ? decision : L"-"
        );
    }

    // Map VK code + shift state to the actual character for English auto-pair
    wchar_t _MapVkToEnglishPairChar(WPARAM vk, bool hasShift)
    {
        if (hasShift)
        {
            switch (vk)
            {
            case '9':          return L'(';
            case '0':          return L')';
            case VK_OEM_4:     return L'{';  // [ key + Shift = {
            case VK_OEM_6:     return L'}';  // ] key + Shift = }
            case VK_OEM_COMMA: return L'<';  // , key + Shift = <
            case VK_OEM_PERIOD:return L'>';  // . key + Shift = >
            case VK_OEM_7:     return L'"';  // ' key + Shift = "
            }
        }
        else
        {
            switch (vk)
            {
            case VK_OEM_4:     return L'[';
            case VK_OEM_6:     return L']';
            case VK_OEM_7:     return L'\''; // ' key
            }
        }
        return 0;
    }
}

CKeyEventSink::CKeyEventSink(CTextService* pTextService)
    : _refCount(1)
    , _pTextService(pTextService)
    , _dwKeySinkCookie(TF_INVALID_COOKIE)
    , _isComposing(FALSE)
    , _hasCandidates(FALSE)
    , _lastPassthroughDigit(0)
    , _pendingKeyUpKey(0)
    , _pendingKeyUpModifiers(0)
    , _pendingKeyDownTime(0)
    , _lastToggleExecuteTime(0)
    , _anyKeyAfterToggle(TRUE)
    , _modsState(0)
    , _eventSeq(0)
    , _nextBarrierSeq(1)
    , _pendingCommit{0, L"", 0, false}
{
    _pTextService->AddRef();

    // Initialize modifier state from current keyboard state
    // This ensures consistency if IME starts while keys are held
    _modsState = GetCurrentModifiers();
}

CKeyEventSink::~CKeyEventSink()
{
    SafeRelease(_pTextService);
}

STDAPI CKeyEventSink::QueryInterface(REFIID riid, void** ppvObj)
{
    if (ppvObj == nullptr)
        return E_INVALIDARG;

    *ppvObj = nullptr;

    if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfKeyEventSink))
    {
        *ppvObj = (ITfKeyEventSink*)this;
    }

    if (*ppvObj)
    {
        AddRef();
        return S_OK;
    }

    return E_NOINTERFACE;
}

STDAPI_(ULONG) CKeyEventSink::AddRef()
{
    return InterlockedIncrement(&_refCount);
}

STDAPI_(ULONG) CKeyEventSink::Release()
{
    LONG cr = InterlockedDecrement(&_refCount);

    if (cr == 0)
    {
        delete this;
    }

    return cr;
}

STDAPI CKeyEventSink::OnSetFocus(BOOL fForeground)
{
    WIND_LOG_INFO(L"KeyEventSink::OnSetFocus\n");
    return S_OK;
}

STDAPI CKeyEventSink::OnTestKeyDown(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten)
{
    *pfEaten = FALSE;

    // Auto-pair: bypass IME for self-generated SendInput keys (VK_LEFT/RIGHT/DELETE/BACK)
    if (_TryConsumeSkipKey(wParam))
    {
        *pfEaten = FALSE; // Let it pass directly to the app
        return S_OK;
    }

    // Ctrl+Shift+F12: Dump TSF ring buffer logs to clipboard (works in AppContainer)
    if (wParam == VK_F12 && (GetKeyState(VK_CONTROL) & 0x8000)
        && (GetKeyState(VK_SHIFT) & 0x8000) && !(GetKeyState(VK_MENU) & 0x8000))
    {
        *pfEaten = TRUE;
        return S_OK;
    }

    // Trace: Log ALL key presses (very high frequency)
    WIND_LOG_TRACE_FMT(L"OnTestKeyDown: wParam=0x%02X\n", (uint32_t)wParam);

    // Keyboard disabled by system: pass through all keys
    if (_pTextService->IsKeyboardDisabled())
        return S_OK;

    // First check if the context is read-only (browser non-editable area)
    if (_IsContextReadOnly(pContext))
    {
        _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, 0, HotkeyType::None,
                        _pTextService->IsChineseMode(), _pTextService->HasActiveComposition(), _hasCandidates,
                        _pTextService->HasActiveComposition() || _hasCandidates, FALSE, L"context_readonly");
        return S_OK;
    }

    // Get current modifiers and calculate key hash
    uint32_t modifiers = CHotkeyManager::GetCurrentModifiers();
    uint32_t keyHash = CHotkeyManager::CalcKeyHash(modifiers, (uint32_t)wParam);

    // For function hotkeys (like Ctrl+`), use normalized modifiers (no left/right distinction)
    uint32_t normalizedMods = CHotkeyManager::NormalizeModifiers(modifiers);
    uint32_t normalizedKeyHash = CHotkeyManager::CalcKeyHash(normalizedMods, (uint32_t)wParam);

    CHotkeyManager* pHotkeyMgr = _pTextService->GetHotkeyManager();

    // Check if this is a KeyDown hotkey from the whitelist
    // Use normalized hash for function hotkeys (Ctrl+`, Shift+Space, etc.)
    if (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyDownHotkey(normalizedKeyHash))
    {
        // For keys without Ctrl/Alt modifiers (page keys like -=, select keys like ;'),
        // only intercept in Chinese mode or when there's an active input session.
        // Otherwise these keys get swallowed in English mode on some applications (e.g., WindTerm)
        // where OnTestKeyDown(pfEaten=TRUE) + OnKeyDown(pfEaten=FALSE) doesn't properly pass through.
        BOOL shouldEatHotkey = TRUE;
        if (!(modifiers & (KEYMOD_CTRL | KEYMOD_ALT)))
        {
            // Page keys (-=) and select keys (;') without modifiers should only be
            // intercepted when there's an active input session (candidates showing).
            // Without input session, Go would return PassThrough for page keys,
            // and some apps (e.g., WindTerm) don't handle OnTestKeyDown(TRUE) +
            // OnKeyDown(FALSE) correctly, causing the key to be swallowed.
            // The key will still be caught by ClassifyInputKey below as Punctuation
            // in Chinese mode, which correctly handles it.
            BOOL hasComp = _pTextService->HasActiveComposition();
            BOOL hasSession = hasComp || _hasCandidates;
            if (!hasSession)
            {
                WIND_LOG_DEBUG_FMT(L"OnTestKeyDown hotkey skipped (no input session): vk=0x%02X, hash=0x%08X\n",
                    (uint32_t)wParam, normalizedKeyHash);
                shouldEatHotkey = FALSE;
            }
        }

        if (shouldEatHotkey)
        {
            WIND_LOG_DEBUG_FMT(L"KeyDown hotkey matched: vk=0x%02X, hash=0x%08X\n",
                         (uint32_t)wParam, normalizedKeyHash);
            *pfEaten = TRUE;
            _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::Hotkey,
                            _pTextService->IsChineseMode(), _pTextService->HasActiveComposition(), _hasCandidates,
                            _pTextService->HasActiveComposition() || _hasCandidates, TRUE, L"keydown_hotkey");
            return S_OK;
        }
    }

    // Check for KeyUp triggered keys (toggle mode keys) - we still need to intercept KeyDown
    // First try hash-based lookup, then fallback to VK-based detection
    BOOL isToggleModeKey = FALSE;

    // TSF sends generic VK_SHIFT/VK_CONTROL as wParam, but the hotkey whitelist
    // registers specific VK_LSHIFT/VK_RSHIFT/VK_LCONTROL/VK_RCONTROL.
    // Resolve the generic VK to specific left/right variant for proper hash matching.
    uint32_t resolvedVK = (uint32_t)wParam;
    if (wParam == VK_SHIFT)
    {
        if (GetAsyncKeyState(VK_LSHIFT) & 0x8000)
            resolvedVK = VK_LSHIFT;
        else if (GetAsyncKeyState(VK_RSHIFT) & 0x8000)
            resolvedVK = VK_RSHIFT;
    }
    else if (wParam == VK_CONTROL)
    {
        if (GetAsyncKeyState(VK_LCONTROL) & 0x8000)
            resolvedVK = VK_LCONTROL;
        else if (GetAsyncKeyState(VK_RCONTROL) & 0x8000)
            resolvedVK = VK_RCONTROL;
    }
    uint32_t keyUpHash = CHotkeyManager::CalcKeyHash(modifiers, resolvedVK);

    if (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyUpHotkey(keyUpHash))
    {
        isToggleModeKey = TRUE;
    }
    else if ((pHotkeyMgr == nullptr || !pHotkeyMgr->HasHotkeys()) && CHotkeyManager::IsToggleModeKeyByVK(wParam))
    {
        // Fallback: detect toggle mode keys ONLY when hotkey whitelist hasn't been loaded yet.
        // Once the whitelist is loaded, trust it — if a key isn't in the whitelist,
        // it shouldn't be treated as a toggle key. Without this guard, Ctrl/Shift are
        // unconditionally intercepted even when not configured as toggle keys,
        // breaking modifier key usage in apps like Fusion 360.
        isToggleModeKey = TRUE;
    }

    if (isToggleModeKey)
    {
        *pfEaten = TRUE;
        _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::ToggleMode,
                        _pTextService->IsChineseMode(), _pTextService->HasActiveComposition(), _hasCandidates,
                        _pTextService->HasActiveComposition() || _hasCandidates, TRUE, L"toggle_mode_key");
        return S_OK;
    }

    // Any non-toggle-mode key cancels pending toggle.
    // IMPORTANT: Must clear here because OnKeyDown may NOT be called
    // if this key is not eaten (e.g., Shift+Enter in English mode).
    // TSF only calls OnKeyDown when OnTestKeyDown sets pfEaten=TRUE.
    if (_pendingKeyUpKey != 0)
    {
        WIND_LOG_DEBUG_FMT(L"OnTestKeyDown: Non-toggle key vk=0x%02X cancels pending toggle\n", (uint32_t)wParam);
        _pendingKeyUpKey = 0;
        _pendingKeyUpModifiers = 0;
    }
    _anyKeyAfterToggle = TRUE;

    // Check basic input keys based on current state
    // Different handling based on key type:
    // - Letter/number/punctuation keys: intercept in Chinese mode
    // - Backspace/Enter/Escape: only intercept when there's an active composition or input session
    BOOL isChineseMode = _pTextService->IsChineseMode();
    // Use TextService's composition state - this is the source of truth in async architecture
    BOOL hasComposition = _pTextService->HasActiveComposition();
    // Also check _hasCandidates for cases where InlinePreedit is disabled
    // (Go sends UpdateComposition with empty text, _hasCandidates is TRUE but HasActiveComposition is FALSE)
    BOOL hasInputSession = hasComposition || _hasCandidates;

    // English auto-pair: intercept bracket keys in English mode
    if (!isChineseMode && _englishPairEngine.IsEnabled())
    {
        bool hasShift = (modifiers & KEYMOD_SHIFT) != 0;
        wchar_t pairChar = _MapVkToEnglishPairChar(wParam, hasShift);
        if (pairChar != 0 && (_englishPairEngine.IsLeft(pairChar) || _englishPairEngine.IsRight(pairChar)))
        {
            *pfEaten = TRUE;
            _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::None,
                            isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"english_autopair");
            return S_OK;
        }
    }

    // Clear English pair stack when non-pair key is pressed in English mode
    if (!isChineseMode && !_englishPairEngine.IsEmpty())
    {
        // If we reach here, the key was NOT a pair key (would have returned above)
        _englishPairEngine.Clear();
    }

    if (hasInputSession || isChineseMode)
    {
        // Ctrl/Alt combos during active input session: intercept so OnKeyDown can
        // send to Go for state cleanup, then pass through to the host application.
        // This prevents dangling composition state when user presses Ctrl+S, Ctrl+C, etc.
        // Note: registered hotkeys (Ctrl+`, Shift+Space) are already caught above.
        // IMPORTANT: Exclude modifier keys themselves (VK_CONTROL, VK_MENU, etc.) —
        // pressing Ctrl alone should NOT trigger cleanup, otherwise Ctrl+number (pin)
        // and Ctrl+Shift+number (delete) candidate shortcuts break because the
        // composition is cleared before the number key arrives.
        bool isModifierKeyItself = (wParam == VK_CONTROL || wParam == VK_LCONTROL || wParam == VK_RCONTROL ||
                                    wParam == VK_MENU    || wParam == VK_LMENU    || wParam == VK_RMENU ||
                                    wParam == VK_SHIFT   || wParam == VK_LSHIFT   || wParam == VK_RSHIFT);
        if (hasInputSession && (modifiers & (KEYMOD_CTRL | KEYMOD_ALT)) && !isModifierKeyItself)
        {
            WIND_LOG_DEBUG_FMT(L"OnTestKeyDown: Ctrl/Alt during session, eating for cleanup: vk=0x%02X\n",
                         (uint32_t)wParam);
            *pfEaten = TRUE;
            _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::Hotkey,
                            isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"ctrl_alt_cleanup");
            return S_OK;
        }

        HotkeyType keyType = CHotkeyManager::ClassifyInputKey(wParam, modifiers);

        if (keyType == HotkeyType::Backspace || keyType == HotkeyType::Enter ||
            keyType == HotkeyType::Escape || keyType == HotkeyType::Space ||
            keyType == HotkeyType::CursorKey)
        {
            // Only intercept if we have composition or active input session
            // These keys should pass through when there's no active input
            if (hasInputSession)
            {
                *pfEaten = TRUE;
                _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, keyType,
                                isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"session_key");
                return S_OK;
            }
        }
        else if (keyType == HotkeyType::Number || keyType == HotkeyType::Tab ||
                 keyType == HotkeyType::PageKey || keyType == HotkeyType::SelectKey)
        {
            // Session-only keys: Go returns PassThrough without active input,
            // and some apps (WindTerm) don't handle the OnTestKeyDown(TRUE) +
            // OnKeyDown(FALSE) flip correctly, causing the key to be swallowed.
            if (hasInputSession)
            {
                *pfEaten = TRUE;
                _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, keyType,
                                isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"session_select_or_page");
                return S_OK;
            }
        }
        else if (keyType == HotkeyType::Letter)
        {
            // Letters: always eat in Chinese mode (they start composition)
            *pfEaten = TRUE;
            _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, keyType,
                            isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"chinese_letter");
            return S_OK;
        }
        else if (keyType == HotkeyType::Punctuation)
        {
            // Punctuation: always eat in Chinese mode.
            // Go always handles punctuation (returns InsertText), so the
            // OnTestKeyDown(TRUE) + OnKeyDown(TRUE) path is safe.
            *pfEaten = TRUE;
            _LogKeyDecision(L"test_down", _pTextService->GetFocusSessionId(), wParam, modifiers, keyType,
                            isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"chinese_punctuation");
            return S_OK;
        }
    }
    // else: not in Chinese mode and no input session — pass through

    // Track digit pass-through for smart punctuation fallback.
    // When digits pass through without reaching Go (no input session),
    // record them so the next punctuation key sent to Go carries this info via prevChar.
    // This handles editors (e.g., EverEdit) where ITfTextEditSink can't read text.
    if (*pfEaten == FALSE)
    {
        if (wParam >= '0' && wParam <= '9')
        {
            _lastPassthroughDigit = (WCHAR)wParam;
        }
        else
        {
            _lastPassthroughDigit = 0;
        }
    }

    return S_OK;
}

STDAPI CKeyEventSink::OnKeyDown(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten)
{
    *pfEaten = FALSE;

    // Ctrl+Shift+F12: Dump TSF ring buffer logs to clipboard (debug aid for AppContainer)
    if (wParam == VK_F12 && (GetKeyState(VK_CONTROL) & 0x8000)
        && (GetKeyState(VK_SHIFT) & 0x8000) && !(GetKeyState(VK_MENU) & 0x8000))
    {
        *pfEaten = TRUE;
        std::wstring logs = CFileLogger::Instance().DumpRingBuffer();
        if (!logs.empty() && OpenClipboard(nullptr))
        {
            EmptyClipboard();
            size_t cbSize = (logs.size() + 1) * sizeof(wchar_t);
            HGLOBAL hMem = GlobalAlloc(GMEM_MOVEABLE, cbSize);
            if (hMem)
            {
                wchar_t* pDst = (wchar_t*)GlobalLock(hMem);
                if (pDst)
                {
                    memcpy(pDst, logs.c_str(), cbSize);
                    GlobalUnlock(hMem);
                    SetClipboardData(CF_UNICODETEXT, hMem);
                }
            }
            CloseClipboard();
            // Brief notification via SendInput so user knows it worked
            _pTextService->InsertText(L"[WindInput TSF Log to clipboard]");
        }
        return S_OK;
    }

    // Update modifier state machine for this KeyDown event
    _UpdateModsOnKeyDown(wParam);

    // Check barrier timeout
    _CheckBarrierTimeout();

    // English auto-pair handling (before toggle key detection and Go IPC)
    if (!_pTextService->IsChineseMode() && _englishPairEngine.IsEnabled())
    {
        uint32_t mods = CHotkeyManager::GetCurrentModifiers();
        bool hasShift = (mods & KEYMOD_SHIFT) != 0;
        wchar_t pairChar = _MapVkToEnglishPairChar(wParam, hasShift);

        if (pairChar != 0)
        {
            // Smart skip: right bracket matches stack top
            if (_englishPairEngine.IsRight(pairChar))
            {
                PairEngine::Entry entry;
                if (_englishPairEngine.Peek(entry) && entry.right == pairChar)
                {
                    _englishPairEngine.Pop(entry);
                    WIND_LOG_DEBUG_FMT(L"English auto-pair: smart skip '%c'\n", pairChar);
                    _SimulatePairKey(VK_RIGHT);
                    *pfEaten = TRUE;
                    return S_OK;
                }
                // Stack doesn't match — clear and let the char through
                _englishPairEngine.Clear();
                // Fall through to insert the right bracket normally
            }

            // Auto-pair: left bracket
            if (_englishPairEngine.IsLeft(pairChar))
            {
                wchar_t rightChar = _englishPairEngine.GetRight(pairChar);
                if (rightChar != 0)
                {
                    std::wstring pairText;
                    pairText += pairChar;
                    pairText += rightChar;

                    WIND_LOG_DEBUG_FMT(L"English auto-pair: insert pair '%c%c'\n", pairChar, rightChar);
                    _pTextService->CommitText(pairText);
                    _SimulatePairKey(VK_LEFT);
                    _englishPairEngine.Push(pairChar, rightChar);

                    *pfEaten = TRUE;
                    return S_OK;
                }
            }

            // Right bracket with no stack match: insert the character normally
            if (_englishPairEngine.IsRight(pairChar))
            {
                std::wstring ch(1, pairChar);
                _pTextService->InsertText(ch);
                *pfEaten = TRUE;
                return S_OK;
            }
        }
    }

    uint32_t modifiers = CHotkeyManager::GetCurrentModifiers();
    uint32_t keyHash = CHotkeyManager::CalcKeyHash(modifiers, (uint32_t)wParam);

    // For function hotkeys (like Ctrl+`), use normalized modifiers (no left/right distinction)
    uint32_t normalizedMods = CHotkeyManager::NormalizeModifiers(modifiers);
    uint32_t normalizedKeyHash = CHotkeyManager::CalcKeyHash(normalizedMods, (uint32_t)wParam);

    CHotkeyManager* pHotkeyMgr = _pTextService->GetHotkeyManager();

    // Check if this is a KeyUp triggered key (toggle mode keys like Shift, Ctrl, CapsLock)
    // Use hash-based lookup first, then fallback to VK-based detection
    //
    // TSF sends generic VK_SHIFT/VK_CONTROL as wParam, but the hotkey whitelist
    // registers specific VK_LSHIFT/VK_RSHIFT/VK_LCONTROL/VK_RCONTROL.
    // Resolve the generic VK to specific left/right variant for proper hash matching.
    BOOL isToggleModeKey = FALSE;
    uint32_t resolvedVK = (uint32_t)wParam;
    if (wParam == VK_SHIFT)
    {
        if (GetAsyncKeyState(VK_LSHIFT) & 0x8000)
            resolvedVK = VK_LSHIFT;
        else if (GetAsyncKeyState(VK_RSHIFT) & 0x8000)
            resolvedVK = VK_RSHIFT;
    }
    else if (wParam == VK_CONTROL)
    {
        if (GetAsyncKeyState(VK_LCONTROL) & 0x8000)
            resolvedVK = VK_LCONTROL;
        else if (GetAsyncKeyState(VK_RCONTROL) & 0x8000)
            resolvedVK = VK_RCONTROL;
    }
    uint32_t keyUpHash = CHotkeyManager::CalcKeyHash(modifiers, resolvedVK);

    if (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyUpHotkey(keyUpHash))
    {
        isToggleModeKey = TRUE;
    }
    else if ((pHotkeyMgr == nullptr || !pHotkeyMgr->HasHotkeys()) && CHotkeyManager::IsToggleModeKeyByVK(wParam))
    {
        // Fallback: only use VK-based detection when hotkey whitelist hasn't been loaded yet
        isToggleModeKey = TRUE;
    }

    if (isToggleModeKey)
    {
        // CapsLock has its own special handling in OnKeyUp, don't set pending here
        if (wParam == VK_CAPITAL)
        {
            // Just consume the KeyDown, let OnKeyUp handle it
            *pfEaten = TRUE;
            return S_OK;
        }

        // Check if this is a key repeat (bit 30 of lParam)
        if (lParam & 0x40000000)
        {
            // Key repeat, ignore
            *pfEaten = TRUE;
            return S_OK;
        }

        // Check if other modifiers are pressed (e.g., Ctrl+Shift is a system shortcut)
        BOOL hasOtherModifier = FALSE;
        if (wParam == VK_SHIFT || wParam == VK_LSHIFT || wParam == VK_RSHIFT)
        {
            hasOtherModifier = (GetAsyncKeyState(VK_CONTROL) & 0x8000) || (GetAsyncKeyState(VK_MENU) & 0x8000);
        }
        else if (wParam == VK_CONTROL || wParam == VK_LCONTROL || wParam == VK_RCONTROL)
        {
            hasOtherModifier = (GetAsyncKeyState(VK_SHIFT) & 0x8000) || (GetAsyncKeyState(VK_MENU) & 0x8000);
        }

        if (hasOtherModifier)
        {
            _pendingKeyUpKey = 0;
            _pendingKeyUpModifiers = 0;
            return S_OK;  // Let system handle it
        }

        // Mark key as pending for KeyUp toggle (Shift/Ctrl only, not CapsLock)
        // IMPORTANT: Determine the specific left/right key for proper config matching
        // wParam might be generic VK_SHIFT, but we need to know if it's LShift or RShift
        uint32_t specificKey = (uint32_t)wParam;
        if (wParam == VK_SHIFT)
        {
            // Determine which shift is actually pressed using GetAsyncKeyState
            if (GetAsyncKeyState(VK_LSHIFT) & 0x8000)
            {
                specificKey = VK_LSHIFT;
            }
            else if (GetAsyncKeyState(VK_RSHIFT) & 0x8000)
            {
                specificKey = VK_RSHIFT;
            }
        }
        else if (wParam == VK_CONTROL)
        {
            if (GetAsyncKeyState(VK_LCONTROL) & 0x8000)
            {
                specificKey = VK_LCONTROL;
            }
            else if (GetAsyncKeyState(VK_RCONTROL) & 0x8000)
            {
                specificKey = VK_RCONTROL;
            }
        }
        _pendingKeyUpKey = specificKey;
        _pendingKeyUpModifiers = modifiers;
        _pendingKeyDownTime = GetTickCount();

        WIND_LOG_DEBUG(L"OnKeyDown: Toggle mode key pending for KeyUp\n");

        *pfEaten = TRUE;
        return S_OK;
    }

    // Any other key cancels pending toggle
    _pendingKeyUpKey = 0;
    _pendingKeyUpModifiers = 0;

    // Mark that a non-modifier key was pressed (for toggle guard)
    _anyKeyAfterToggle = TRUE;

    // Check if context is read-only
    if (_IsContextReadOnly(pContext))
    {
        _LogKeyDecision(L"down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::None,
                        _pTextService->IsChineseMode(), _pTextService->HasActiveComposition(), _hasCandidates,
                        _pTextService->HasActiveComposition() || _hasCandidates, FALSE, L"context_readonly");
        return S_OK;
    }

    // Check if this is a KeyDown hotkey from whitelist
    // Use normalized hash for function hotkeys (Ctrl+`, Shift+Space, etc.)
    BOOL isKeyDownHotkey = (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyDownHotkey(normalizedKeyHash));

    // Check for basic input keys
    // IMPORTANT: Different handling based on key type:
    // - Letter/number/punctuation keys: intercept in Chinese mode (start new composition)
    // - Backspace/Enter/Escape: only intercept when there's an active composition or input session
    //   (otherwise, pass through to application)
    BOOL isInputKey = FALSE;
    BOOL isChineseMode = _pTextService->IsChineseMode();
    // Use TextService's composition state - this is the source of truth in async architecture
    BOOL hasComposition = _pTextService->HasActiveComposition();
    // Also check _hasCandidates for cases where InlinePreedit is disabled
    BOOL hasInputSession = hasComposition || _hasCandidates;

    // Track whether this is a Ctrl/Alt combo that needs cleanup-then-passthrough
    BOOL isCtrlAltCleanup = FALSE;

    if (hasInputSession || isChineseMode)
    {
        // Ctrl/Alt combos during active input session: mark as input key so we can
        // send to Go for state cleanup. After response, we'll override pfEaten=FALSE.
        // Note: registered hotkeys are already caught by isKeyDownHotkey above.
        // IMPORTANT: Exclude modifier keys themselves — pressing Ctrl/Alt alone should
        // not trigger cleanup, to preserve Ctrl+number and Ctrl+Shift+number shortcuts.
        bool isModifierKeyItself = (wParam == VK_CONTROL || wParam == VK_LCONTROL || wParam == VK_RCONTROL ||
                                    wParam == VK_MENU    || wParam == VK_LMENU    || wParam == VK_RMENU ||
                                    wParam == VK_SHIFT   || wParam == VK_LSHIFT   || wParam == VK_RSHIFT);
        if (hasInputSession && (modifiers & (KEYMOD_CTRL | KEYMOD_ALT)) && !isKeyDownHotkey && !isModifierKeyItself)
        {
            isInputKey = TRUE;
            isCtrlAltCleanup = TRUE;
            WIND_LOG_DEBUG_FMT(L"OnKeyDown: Ctrl/Alt during session, sending to Go for cleanup: vk=0x%02X\n",
                         (uint32_t)wParam);
        }
        else
        {
            HotkeyType keyType = CHotkeyManager::ClassifyInputKey(wParam, modifiers);

            // Backspace, Enter, Escape, CursorKey should only be intercepted when there's an active composition or input session
            // Otherwise they should pass through to the application
            if (keyType == HotkeyType::Backspace || keyType == HotkeyType::Enter ||
                keyType == HotkeyType::Escape || keyType == HotkeyType::CursorKey)
            {
                isInputKey = hasInputSession;  // Only intercept if we have composition or input session
            }
            else
            {
                isInputKey = (keyType != HotkeyType::None);
            }
        }
    }

    if (!isKeyDownHotkey && !isInputKey)
    {
        // CRITICAL FIX: If OnTestKeyDown decided to eat this key (based on the state
        // at that time), but now the state has changed (e.g., _isComposing became FALSE
        // after a commit), we STILL need to consume the key to maintain consistency.
        // Otherwise, the key will be passed to the application unexpectedly.
        //
        // This can happen during fast typing: "d<space>d" where:
        // 1. OnTestKeyDown('d') sees _isComposing=TRUE, returns pfEaten=TRUE
        // 2. Space key IPC returns, sets _isComposing=FALSE
        // 3. OnKeyDown('d') now sees _isComposing=FALSE, but must still consume 'd'
        //
        // We detect this by checking if we're in Chinese mode and this is a letter key.
        if (isChineseMode && wParam >= 'A' && wParam <= 'Z' && !(modifiers & (KEYMOD_CTRL | KEYMOD_ALT)))
        {
            // Letter key in Chinese mode slipped through due to state change - consume it
            *pfEaten = TRUE;
            _LogKeyDecision(L"down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::Letter,
                            isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"state_change_letter_consume");
        }
        else
        {
            _LogKeyDecision(L"down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::None,
                            isChineseMode, hasComposition, _hasCandidates, hasInputSession, FALSE, L"passthrough_not_handled");
        }
        return S_OK;
    }

    // Update caret position before sending key event
    // This ensures the candidate window appears at the correct position
    _pTextService->SendCaretPositionUpdate();

    // Send key to Go Service using binary protocol (SYNC mode)
    if (!_SendKeyToService((uint32_t)wParam, modifiers, KEY_EVENT_DOWN))
    {
        WIND_LOG_ERROR(L"Failed to send key to service");
        WIND_LOG_DEBUG_FMT(
            L"compat.ipc_send_failed focusSession=%llu vk=0x%02X mods=0x%04X chinese=%d composing=%d candidates=%d",
            _pTextService->GetFocusSessionId(), (uint32_t)wParam, modifiers,
            isChineseMode ? 1 : 0, hasComposition ? 1 : 0, _hasCandidates ? 1 : 0
        );
        WindLogForegroundProcessInfo(4, L"compat.ipc_send_failed.foreground_host");

        // Service not available - pass through letters directly
        if (wParam >= 'A' && wParam <= 'Z' && !(modifiers & (KEYMOD_CTRL | KEYMOD_ALT)))
        {
            std::wstring ch;
            if (modifiers & KEYMOD_SHIFT)
                ch = (wchar_t)wParam;                      // Shift held: uppercase
            else
                ch = (wchar_t)towlower((wint_t)wParam);    // No Shift: lowercase
            _pTextService->InsertText(ch);
            *pfEaten = TRUE;
            _LogKeyDecision(L"down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::Letter,
                            isChineseMode, hasComposition, _hasCandidates, hasInputSession, TRUE, L"ipc_failed_fallback_insert");
        }
        else
        {
            // 非字母按键（符号、标点等）：放行给应用程序处理
            *pfEaten = FALSE;
            _LogKeyDecision(L"down", _pTextService->GetFocusSessionId(), wParam, modifiers, HotkeyType::None,
                            isChineseMode, hasComposition, _hasCandidates, hasInputSession, FALSE, L"ipc_failed_passthrough");
        }
        return S_OK;
    }

    // SYNC: Wait for response and handle it directly
    // This is simpler and matches Weasel's architecture
    *pfEaten = _HandleServiceResponse();

    // Ctrl/Alt combo during active session: decide pass-through based on Go's response.
    // If Go handled the key as a candidate operation (pin/delete) and the composition
    // is still active, respect Go's decision and eat the key. Only override to FALSE
    // when Go actually cleared the composition (e.g., Ctrl+S cleanup).
    if (isCtrlAltCleanup && *pfEaten)
    {
        if (_hasCandidates || _isComposing)
        {
            // Go handled it as a candidate action (e.g., Ctrl+number pin/delete),
            // composition still active — keep pfEaten=TRUE to prevent app from seeing the key.
            WIND_LOG_DEBUG(L"OnKeyDown: Ctrl/Alt key handled by Go (session still active), eating key\n");
        }
        else
        {
            // Go cleared composition (cleanup) — pass key through to the host application.
            WIND_LOG_DEBUG(L"OnKeyDown: Ctrl/Alt cleanup done, overriding to pass-through\n");
            *pfEaten = FALSE;
        }
    }

    _LogKeyDecision(L"down", _pTextService->GetFocusSessionId(), wParam, modifiers,
                    isKeyDownHotkey ? HotkeyType::Hotkey : CHotkeyManager::ClassifyInputKey(wParam, modifiers),
                    isChineseMode, hasComposition, _hasCandidates, hasInputSession, *pfEaten,
                    isCtrlAltCleanup && !*pfEaten ? L"ctrl_alt_cleanup_passthrough" : L"service_response");

    return S_OK;
}

STDAPI CKeyEventSink::OnTestKeyUp(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten)
{
    *pfEaten = FALSE;

    // Auto-pair: bypass IME for self-generated SendInput key releases
    if (_TryConsumeSkipKey(wParam))
    {
        *pfEaten = FALSE;
        return S_OK;
    }

    // Keyboard disabled by system: pass through all keys
    if (_pTextService->IsKeyboardDisabled())
        return S_OK;

    // Intercept modifier release if we have a pending auto-pair action
    if (_pendingPairAction.active)
    {
        if (wParam == VK_SHIFT || wParam == VK_LSHIFT || wParam == VK_RSHIFT ||
            wParam == VK_CONTROL || wParam == VK_LCONTROL || wParam == VK_RCONTROL ||
            wParam == VK_MENU || wParam == VK_LMENU || wParam == VK_RMENU)
        {
            *pfEaten = TRUE;
            return S_OK;
        }
    }

    // Handle pending toggle key release
    if (_pendingKeyUpKey != 0)
    {
        // Check if this matches the pending key
        if (_IsMatchingKeyUp(wParam, _pendingKeyUpKey))
        {
            *pfEaten = TRUE;
            return S_OK;
        }
    }

    // Also handle Caps Lock for indicator
    if (wParam == VK_CAPITAL)
    {
        *pfEaten = TRUE;
        return S_OK;
    }

    return S_OK;
}

STDAPI CKeyEventSink::OnKeyUp(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten)
{
    *pfEaten = FALSE;

    // Update modifier state machine for this KeyUp event
    _UpdateModsOnKeyUp(wParam);

    // Execute pending auto-pair action when all modifiers are released
    if (_pendingPairAction.active && !_AreModifiersHeld())
    {
        WIND_LOG_DEBUG_FMT(L"Auto-pair: executing deferred vk=0x%02X x%d (modifiers released)\n",
            (WORD)_pendingPairAction.vk, _pendingPairAction.count);
        for (int i = 0; i < _pendingPairAction.count; i++)
        {
            _PushSkipKey(_pendingPairAction.vk);
            INPUT inputs[2] = {};
            inputs[0].type = INPUT_KEYBOARD;
            inputs[0].ki.wVk = _pendingPairAction.vk;
            inputs[1].type = INPUT_KEYBOARD;
            inputs[1].ki.wVk = _pendingPairAction.vk;
            inputs[1].ki.dwFlags = KEYEVENTF_KEYUP;
            SendInput(2, inputs, sizeof(INPUT));
        }
        _pendingPairAction = {};
        // Consume the modifier key-up to prevent mode toggle.
        // The user pressed Shift for a shifted character (e.g., parenthesis),
        // not for toggling input mode.
        *pfEaten = TRUE;
        return S_OK;
    }

    // Handle toggle key release for mode toggle
    if (_pendingKeyUpKey != 0)
    {
        if (_IsMatchingKeyUp(wParam, _pendingKeyUpKey))
        {
            uint32_t pendingKey = _pendingKeyUpKey;
            DWORD pressDuration = GetTickCount() - _pendingKeyDownTime;
            _pendingKeyUpKey = 0;
            _pendingKeyUpModifiers = 0;
            _pendingKeyDownTime = 0;

            // Long press should NOT trigger mode toggle - only short taps count
            if (pressDuration > TOGGLE_TAP_THRESHOLD_MS)
            {
                WIND_LOG_DEBUG_FMT(L"OnKeyUp: Toggle key held too long (%lu ms > %lu ms), ignoring\n",
                    pressDuration, TOGGLE_TAP_THRESHOLD_MS);
                *pfEaten = TRUE;
                return S_OK;
            }

            // Guard against accidental re-toggle when modifier is used as key combination
            // If no non-modifier key was pressed since the last toggle AND we're within
            // the guard window, this is likely an accidental re-toggle
            // (e.g., Shift tap to English, then immediately Shift+A for uppercase)
            if (!_anyKeyAfterToggle && _lastToggleExecuteTime != 0 &&
                (GetTickCount() - _lastToggleExecuteTime) < TOGGLE_GUARD_MS)
            {
                WIND_LOG_DEBUG_FMT(L"OnKeyUp: Toggle guard active (no key typed since last toggle, %lu ms ago)\n",
                    GetTickCount() - _lastToggleExecuteTime);
                *pfEaten = TRUE;
                return S_OK;
            }

            // For Shift/Ctrl toggle: Send KeyUp event to Go service
            // Go side will check config (e.g., only LShift vs both L/R Shift)
            // and return ModeChanged response if the key is configured as toggle key
            if (pendingKey != VK_CAPITAL)
            {
                WIND_LOG_DEBUG_FMT(L"Sending toggle key KeyUp to Go: vk=0x%02X\n", pendingKey);

                // Build modifiers for the specific key being released
                // This helps Go identify exactly which key was released
                uint32_t mods = 0;
                if (pendingKey == VK_LSHIFT)
                {
                    mods = KEYMOD_SHIFT | KEYMOD_LSHIFT;
                }
                else if (pendingKey == VK_RSHIFT)
                {
                    mods = KEYMOD_SHIFT | KEYMOD_RSHIFT;
                }
                else if (pendingKey == VK_LCONTROL)
                {
                    mods = KEYMOD_CTRL | KEYMOD_LCTRL;
                }
                else if (pendingKey == VK_RCONTROL)
                {
                    mods = KEYMOD_CTRL | KEYMOD_RCTRL;
                }

                // Update caret position before sending toggle key
                // This ensures status indicators appear at the correct position
                _pTextService->SendCaretPositionUpdate();

                // Send KeyUp event to Go service (SYNC mode, wait for response)
                // Go will check config and return ModeChanged if key is configured as toggle
                // All state changes go through Go service - no local fallback
                if (_SendKeyToService(pendingKey, mods, KEY_EVENT_UP))
                {
                    // Handle response - may include mode change
                    _HandleServiceResponse();
                }
                else
                {
                    // IPC failed - don't toggle locally to keep state consistent with Go
                    WIND_LOG_ERROR(L"IPC failed for toggle key, not toggling locally");
                }

                // Update toggle guard state: ignore subsequent toggles until a key is typed
                _lastToggleExecuteTime = GetTickCount();
                _anyKeyAfterToggle = FALSE;
            }

            *pfEaten = TRUE;
            return S_OK;
        }
    }

    // Handle Caps Lock key release
    if (wParam == VK_CAPITAL)
    {
        CHotkeyManager* pHotkeyMgr = _pTextService->GetHotkeyManager();

        // Calculate hash for CapsLock
        uint32_t keyHash = CHotkeyManager::CalcKeyHash(KEYMOD_CAPSLOCK, VK_CAPITAL);

        // Check if CapsLock is configured as toggle key (for Chinese/English switching)
        BOOL isConfiguredAsToggle = (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyUpHotkey(keyHash));

        // Get current Caps Lock state
        BOOL capsLockOn = (GetKeyState(VK_CAPITAL) & 0x0001) != 0;

        // Always send CapsLock event to Go service for:
        // 1. Mode toggle (if configured)
        // 2. CapsLock indicator display (A/a prompt)
        // 3. Toolbar state update
        // Use a special modifier to indicate whether this is for mode toggle
        uint32_t mods = KEYMOD_CAPSLOCK;
        if (!isConfiguredAsToggle)
        {
            // Add a marker to indicate this is just for CapsLock state notification, not mode toggle
            // Go side will check this to decide whether to toggle mode
            mods |= 0x8000; // High bit as "state notification only" marker
        }

        // Update caret position before sending CapsLock event
        _pTextService->SendCaretPositionUpdate();

        // SYNC: Send key event and wait for response
        // Go service will push state update followed by CMD_CONSUMED response
        // _HandleServiceResponse will process both and update the language bar
        if (_SendKeyToService(VK_CAPITAL, mods, KEY_EVENT_UP))
        {
            _HandleServiceResponse();
        }
        else
        {
            // IPC failed, fall back to local update
            WIND_LOG_ERROR(L"IPC failed for CapsLock, updating locally");
            _pTextService->UpdateCapsLockState(capsLockOn);
        }

        *pfEaten = TRUE;
        return S_OK;
    }

    return S_OK;
}

STDAPI CKeyEventSink::OnPreservedKey(ITfContext* pContext, REFGUID rguid, BOOL* pfEaten)
{
    *pfEaten = FALSE;
    return S_OK;
}

BOOL CKeyEventSink::Initialize()
{
    WIND_LOG_INFO(L"KeyEventSink::Initialize\n");

    ITfThreadMgr* pThreadMgr = _pTextService->GetThreadMgr();
    if (pThreadMgr == nullptr)
    {
        WIND_LOG_ERROR(L"ThreadMgr is null");
        return FALSE;
    }

    ITfKeystrokeMgr* pKeystrokeMgr = nullptr;
    HRESULT hr = pThreadMgr->QueryInterface(IID_ITfKeystrokeMgr, (void**)&pKeystrokeMgr);

    if (FAILED(hr) || pKeystrokeMgr == nullptr)
    {
        WIND_LOG_ERROR(L"Failed to get ITfKeystrokeMgr");
        return FALSE;
    }

    hr = pKeystrokeMgr->AdviseKeyEventSink(_pTextService->GetClientId(), this, TRUE);
    pKeystrokeMgr->Release();

    if (FAILED(hr))
    {
        WIND_LOG_ERROR(L"AdviseKeyEventSink failed");
        return FALSE;
    }

    WIND_LOG_INFO(L"KeyEventSink initialized successfully\n");
    return TRUE;
}

void CKeyEventSink::Uninitialize()
{
    WIND_LOG_INFO(L"KeyEventSink::Uninitialize\n");

    ITfThreadMgr* pThreadMgr = _pTextService->GetThreadMgr();
    if (pThreadMgr == nullptr)
        return;

    ITfKeystrokeMgr* pKeystrokeMgr = nullptr;
    if (SUCCEEDED(pThreadMgr->QueryInterface(IID_ITfKeystrokeMgr, (void**)&pKeystrokeMgr)))
    {
        pKeystrokeMgr->UnadviseKeyEventSink(_pTextService->GetClientId());
        pKeystrokeMgr->Release();
    }
}

// Helper: Check if wParam matches the pending KeyUp key
// IMPORTANT: We now store specific keys (VK_LSHIFT vs VK_RSHIFT) at KeyDown time,
// so we need to match the specific key that was pressed, not any Shift/Ctrl.
// When KeyUp comes with generic VK_SHIFT, we use GetAsyncKeyState to determine which one.
BOOL CKeyEventSink::_IsMatchingKeyUp(WPARAM wParam, uint32_t pendingKey)
{
    if (pendingKey == 0)
        return FALSE;

    // Direct match
    if (wParam == pendingKey)
        return TRUE;

    // Handle generic VK_SHIFT -> need to check if the pending specific key was released
    if (wParam == VK_SHIFT)
    {
        // pendingKey is specific (VK_LSHIFT or VK_RSHIFT)
        // Check if that specific key is no longer pressed
        if (pendingKey == VK_LSHIFT && !(GetAsyncKeyState(VK_LSHIFT) & 0x8000))
        {
            return TRUE;
        }
        if (pendingKey == VK_RSHIFT && !(GetAsyncKeyState(VK_RSHIFT) & 0x8000))
        {
            return TRUE;
        }
        return FALSE;
    }

    // Handle generic VK_CONTROL -> need to check if the pending specific key was released
    if (wParam == VK_CONTROL)
    {
        if (pendingKey == VK_LCONTROL && !(GetAsyncKeyState(VK_LCONTROL) & 0x8000))
        {
            return TRUE;
        }
        if (pendingKey == VK_RCONTROL && !(GetAsyncKeyState(VK_RCONTROL) & 0x8000))
        {
            return TRUE;
        }
        return FALSE;
    }

    // Handle specific VK matching specific pending
    // E.g., if pendingKey is VK_LSHIFT and wParam is VK_LSHIFT -> already matched above
    // But if pendingKey is VK_LSHIFT and wParam is VK_RSHIFT -> don't match (different keys)

    return FALSE;
}

// Send key to Go Service using binary protocol
BOOL CKeyEventSink::_SendKeyToService(uint32_t keyCode, uint32_t modifiers, uint8_t eventType)
{
    DWORD startTime = GetTickCount();

    CIPCClient* pIPCClient = _pTextService->GetIPCClient();
    if (pIPCClient == nullptr)
    {
        WIND_LOG_ERROR(L"IPCClient is null");
        return FALSE;
    }

    // If a new connection was established (e.g., service started after TSF loaded),
    // perform a full state sync before processing key events.
    // This covers the edge case where service becomes available between focus events.
    if (pIPCClient->NeedsStateSync())
    {
        if (!pIPCClient->IsConnected() && !pIPCClient->Connect())
        {
            WIND_LOG_WARN(L"State sync needed but reconnect failed before key send");
            return FALSE;
        }

        _pTextService->_DoFullStateSync();

        // Re-send caret position after reconnection/state sync so the Go side has
        // a valid anchor before it processes the first post-restart key event.
        _pTextService->SendCaretPositionUpdate();
    }

    _pTextService->TryRecoverFocusState();

    // Get scan code from virtual key (optional, set to 0 if not needed)
    uint32_t scanCode = MapVirtualKeyW(keyCode, MAPVK_VK_TO_VSC);

    // Get toggles and event sequence
    uint8_t toggles = _GetTogglesSnapshot();
    uint16_t eventSeq = _GetNextEventSeq();

    // IMPORTANT: Always use the passed-in modifiers from CHotkeyManager::GetCurrentModifiers()
    // which calls GetAsyncKeyState(). The _modsState state machine can get out of sync
    // when we pass keys through to the system (e.g., Ctrl+S for save).
    // Using stale _modsState causes all subsequent keys to appear as having Ctrl held.

    // Get character before caret for smart punctuation:
    // 1. Prefer ITfTextEditSink::OnEndEdit cache (works in Notepad, browsers, etc.)
    //    Consume (clear) to prevent stale values in apps where OnEndEdit fires late (e.g., WeChat)
    // 2. Fallback to digit pass-through tracking (for editors like EverEdit where TSF text access fails)
    uint16_t prevChar = (uint16_t)_pTextService->ConsumeCachedPrevChar();
    // Digit pass-through fallback: only apply for period/comma keys (smart punct targets).
    if (prevChar == 0 && _lastPassthroughDigit != 0 &&
        (keyCode == VK_OEM_PERIOD || keyCode == VK_OEM_COMMA))
    {
        prevChar = (uint16_t)_lastPassthroughDigit;
        _lastPassthroughDigit = 0;  // 已消费，清除以避免后续标点误判
    }
    // Clear stale digit passthrough when any non-smart-punct key is sent to Go.
    // Without this, _lastPassthroughDigit persists through eaten keys (composition,
    // candidate selection, etc.), causing e.g. "58的。" to incorrectly use digit
    // fallback and output "." instead of "。" in non-TSF apps.
    else if (_lastPassthroughDigit != 0 &&
             keyCode != VK_OEM_PERIOD && keyCode != VK_OEM_COMMA)
    {
        _lastPassthroughDigit = 0;
    }

    BOOL result = pIPCClient->SendKeyEvent(keyCode, scanCode, modifiers, eventType, toggles, eventSeq, prevChar);

    WIND_LOG_DEBUG_FMT(L"_SendKeyToService: vk=0x%02X, mods=0x%04X, elapsed=%dms\n",
                 keyCode, modifiers, GetTickCount() - startTime);

    return result;
}

BOOL CKeyEventSink::_HandleServiceResponse()
{
    LARGE_INTEGER startTime, midTime, freq;
    QueryPerformanceCounter(&startTime);
    QueryPerformanceFrequency(&freq);

    CIPCClient* pIPCClient = _pTextService->GetIPCClient();
    if (pIPCClient == nullptr)
        return TRUE; // Default to eating the key if no IPC

    ServiceResponse response;

    // Loop to handle any StatusUpdate (state push) messages that may precede the actual response
    // This is necessary because Go service may push state updates before the operation response
    while (true)
    {
        if (!pIPCClient->ReceiveResponse(response))
        {
            WIND_LOG_ERROR(L"Failed to receive response from service");
            return TRUE; // Default to eating the key on error
        }

        // If this is a StatusUpdate (state push), process it and continue reading
        if (response.type == ResponseType::StatusUpdate)
        {
            WIND_LOG_DEBUG(L"Received StatusUpdate (state push), processing and reading next response\n");

            // Update input mode from state push (with icon label from Go service)
            _pTextService->UpdateFullStatus(
                response.IsChineseMode(),
                response.IsFullWidth(),
                response.IsChinesePunct(),
                response.IsToolbarVisible(),
                response.IsCapsLock(),
                response.iconLabel.empty() ? nullptr : response.iconLabel.c_str()
            );

            // Update hotkey whitelist if present
            CHotkeyManager* pHotkeyMgr = _pTextService->GetHotkeyManager();
            if (pHotkeyMgr != nullptr && response.HasHotkeys())
            {
                pHotkeyMgr->UpdateHotkeys(response.keyDownHotkeys, response.keyUpHotkeys);
            }

            // Continue reading to get the actual operation response
            continue;
        }

        // Got a non-StatusUpdate response, break out of loop to process it
        break;
    }

    QueryPerformanceCounter(&midTime);
    int ipcMs = (int)((midTime.QuadPart - startTime.QuadPart) * 1000 / freq.QuadPart);
    WIND_LOG_DEBUG_FMT(L"_HandleServiceResponse: IPC receive took %dms, responseType=%d\n",
                 ipcMs, (int)response.type);

    switch (response.type)
    {
    case ResponseType::Ack:
        // ACK means key was handled (consumed without output)
        return TRUE;

    case ResponseType::PassThrough:
        // PassThrough means key was NOT handled, pass to system
        WIND_LOG_DEBUG(L"PassThrough: key not handled, passing to system\n");
        return FALSE;

    case ResponseType::CommitText:
        {
            LARGE_INTEGER ctStart, ctMid1, ctEnd;
            QueryPerformanceCounter(&ctStart);

            WIND_LOG_DEBUG(L"Processing CommitText response\n");

            // Handle new composition if present (top code commit feature)
            if (!response.newComposition.empty())
            {
                WIND_LOG_TRACE_FMT(L"CommitText with new composition: textLen=%zu, newCompLen=%zu\n",
                             response.text.length(), response.newComposition.length());

                _pTextService->InsertTextAndStartComposition(response.text, response.newComposition);
                _isComposing = TRUE;
                _hasCandidates = TRUE;

                // Re-send caret position after composition change
                _pTextService->SendCaretPositionUpdate();
            }
            else
            {
                // No new composition, commit text atomically (end composition + insert in one EditSession)
                _pTextService->CommitText(response.text);
                QueryPerformanceCounter(&ctMid1);

                _isComposing = FALSE;
                _hasCandidates = FALSE;

                int commitMs = (int)((ctMid1.QuadPart - ctStart.QuadPart) * 1000 / freq.QuadPart);
                WIND_LOG_TRACE_FMT(L"CommitText: atomic commit=%dms\n", commitMs);
            }

            // Handle mode change if present
            if (response.modeChanged)
            {
                _pTextService->SetInputMode(response.chineseMode);
            }

            QueryPerformanceCounter(&ctEnd);
            int ctMs = (int)((ctEnd.QuadPart - ctStart.QuadPart) * 1000 / freq.QuadPart);
            WIND_LOG_DEBUG_FMT(L"CommitText total took %dms\n", ctMs);
        }
        return TRUE;

    case ResponseType::UpdateComposition:
        {
            LARGE_INTEGER ucStart, ucEnd;
            QueryPerformanceCounter(&ucStart);

            WIND_LOG_TRACE(L"Received UpdateComposition from service\n");
            _isComposing = TRUE;
            _hasCandidates = TRUE;
            _pTextService->UpdateComposition(response.composition, response.caretPos);

            // Re-send caret position after composition update so Go can
            // reposition the candidate window with the up-to-date coordinates.
            _pTextService->SendCaretPositionUpdate();

            QueryPerformanceCounter(&ucEnd);
            int ucMs = (int)((ucEnd.QuadPart - ucStart.QuadPart) * 1000 / freq.QuadPart);
            WIND_LOG_DEBUG_FMT(L"UpdateComposition total took %dms\n", ucMs);
        }
        return TRUE;

    case ResponseType::ClearComposition:
        WIND_LOG_DEBUG(L"Received ClearComposition from service\n");
        _isComposing = FALSE;
        _hasCandidates = FALSE;
        _pTextService->EndComposition();
        return TRUE;

    case ResponseType::ModeChanged:
        WIND_LOG_DEBUG(L"Received ModeChanged from service\n");
        _isComposing = FALSE;
        _hasCandidates = FALSE;
        _pTextService->EndComposition();
        _pTextService->SetInputMode(response.chineseMode);
        return TRUE;

    case ResponseType::StatusUpdate:
        // StatusUpdate is normally handled in the loop above, but if we get here
        // it means we received a StatusUpdate as the final response (e.g., from FocusGained)
        WIND_LOG_DEBUG(L"Received StatusUpdate as final response\n");
        _pTextService->UpdateFullStatus(
            response.IsChineseMode(),
            response.IsFullWidth(),
            response.IsChinesePunct(),
            response.IsToolbarVisible(),
            response.IsCapsLock(),
            response.iconLabel.empty() ? nullptr : response.iconLabel.c_str()
        );
        return TRUE;

    case ResponseType::Consumed:
        // Key was consumed by a hotkey
        WIND_LOG_DEBUG(L"Key consumed by hotkey\n");
        return TRUE;

    case ResponseType::InsertTextWithCursor:
        {
            WIND_LOG_DEBUG(L"Processing InsertTextWithCursor response\n");
            _pTextService->CommitText(response.text);
            _isComposing = FALSE;
            _hasCandidates = FALSE;
            for (int i = 0; i < response.cursorOffset; i++)
                _SimulatePairKey(VK_LEFT);
        }
        return TRUE;

    case ResponseType::MoveCursorRight:
        {
            WIND_LOG_DEBUG(L"Processing MoveCursorRight response (smart skip)\n");
            _SimulatePairKey(VK_RIGHT);
        }
        return TRUE;

    case ResponseType::DeletePair:
        {
            WIND_LOG_DEBUG(L"Processing DeletePair response (smart delete)\n");
            _SimulatePairKey(VK_DELETE);
            _SimulatePairKey(VK_BACK);
        }
        return TRUE;

    default:
        WIND_LOG_ERROR(L"Unknown response type from service");
        return TRUE;
    }

    return TRUE; // Default: key was handled
}

// Check if the current context is read-only
BOOL CKeyEventSink::_IsContextReadOnly(ITfContext* pContext)
{
    if (!pContext)
    {
        WIND_LOG_DEBUG_FMT(L"compat.context_status focusSession=%llu context=null", _pTextService->GetFocusSessionId());
        return TRUE;
    }

    TF_STATUS tfStatus = {};
    HRESULT hr = pContext->GetStatus(&tfStatus);

    if (SUCCEEDED(hr))
    {
        if (tfStatus.dwDynamicFlags & TF_SD_READONLY)
        {
            WIND_LOG_DEBUG_FMT(
                L"compat.context_status focusSession=%llu flags=0x%08X readonly=1 loading=0",
                _pTextService->GetFocusSessionId(), tfStatus.dwDynamicFlags
            );
            return TRUE;
        }

        if (tfStatus.dwDynamicFlags & TF_SD_LOADING)
        {
            WIND_LOG_DEBUG_FMT(
                L"compat.context_status focusSession=%llu flags=0x%08X readonly=0 loading=1",
                _pTextService->GetFocusSessionId(), tfStatus.dwDynamicFlags
            );
            return TRUE;
        }

        WIND_LOG_TRACE_FMT(
            L"compat.context_status focusSession=%llu flags=0x%08X readonly=0 loading=0",
            _pTextService->GetFocusSessionId(), tfStatus.dwDynamicFlags
        );
    }
    else
    {
        WIND_LOG_WARN_FMT(
            L"compat.context_status focusSession=%llu get_status_failed hr=0x%08X",
            _pTextService->GetFocusSessionId(), hr
        );
    }

    return FALSE;
}

// Called when composition is unexpectedly terminated by the application
// This typically happens when:
// 1. Fast typing: new composition starts before previous InsertText completes
// 2. User clicks in input field to change cursor position
// 3. Application forcefully terminates composition
void CKeyEventSink::OnCompositionUnexpectedlyTerminated()
{
    WIND_LOG_INFO(L"OnCompositionUnexpectedlyTerminated: Resetting state and notifying Go service\n");

    // Reset local state
    _isComposing = FALSE;
    _hasCandidates = FALSE;

    // Notify Go service to clear input buffer and hide candidate window
    // Use CompositionTerminated instead of FocusLost so that the toolbar stays visible
    // (FocusLost would hide toolbar, but composition termination should not)
    CIPCClient* pIPCClient = _pTextService->GetIPCClient();
    if (pIPCClient != nullptr && pIPCClient->IsConnected())
    {
        pIPCClient->SendCompositionTerminated();
        WIND_LOG_DEBUG(L"OnCompositionUnexpectedlyTerminated: Sent CompositionTerminated to Go service\n");
    }
}

// ============================================================================
// Modifier key state machine implementation
// ============================================================================

void CKeyEventSink::_UpdateModsOnKeyDown(WPARAM vk)
{
    switch (vk)
    {
    case VK_SHIFT:
        // Generic shift - set generic flag, actual L/R determined by GetAsyncKeyState
        _modsState |= KEYMOD_SHIFT;
        if (GetAsyncKeyState(VK_LSHIFT) & 0x8000) _modsState |= KEYMOD_LSHIFT;
        if (GetAsyncKeyState(VK_RSHIFT) & 0x8000) _modsState |= KEYMOD_RSHIFT;
        break;
    case VK_LSHIFT:
        _modsState |= (KEYMOD_SHIFT | KEYMOD_LSHIFT);
        break;
    case VK_RSHIFT:
        _modsState |= (KEYMOD_SHIFT | KEYMOD_RSHIFT);
        break;

    case VK_CONTROL:
        _modsState |= KEYMOD_CTRL;
        if (GetAsyncKeyState(VK_LCONTROL) & 0x8000) _modsState |= KEYMOD_LCTRL;
        if (GetAsyncKeyState(VK_RCONTROL) & 0x8000) _modsState |= KEYMOD_RCTRL;
        break;
    case VK_LCONTROL:
        _modsState |= (KEYMOD_CTRL | KEYMOD_LCTRL);
        break;
    case VK_RCONTROL:
        _modsState |= (KEYMOD_CTRL | KEYMOD_RCTRL);
        break;

    case VK_MENU:
    case VK_LMENU:
    case VK_RMENU:
        _modsState |= KEYMOD_ALT;
        break;

    case VK_LWIN:
    case VK_RWIN:
        _modsState |= KEYMOD_WIN;
        break;
    }
}

void CKeyEventSink::_UpdateModsOnKeyUp(WPARAM vk)
{
    switch (vk)
    {
    case VK_SHIFT:
        // Clear all shift flags when generic VK_SHIFT is released
        _modsState &= ~(KEYMOD_SHIFT | KEYMOD_LSHIFT | KEYMOD_RSHIFT);
        break;
    case VK_LSHIFT:
        _modsState &= ~KEYMOD_LSHIFT;
        // Only clear generic shift if right shift is also not held
        if (!(_modsState & KEYMOD_RSHIFT))
            _modsState &= ~KEYMOD_SHIFT;
        break;
    case VK_RSHIFT:
        _modsState &= ~KEYMOD_RSHIFT;
        if (!(_modsState & KEYMOD_LSHIFT))
            _modsState &= ~KEYMOD_SHIFT;
        break;

    case VK_CONTROL:
        _modsState &= ~(KEYMOD_CTRL | KEYMOD_LCTRL | KEYMOD_RCTRL);
        break;
    case VK_LCONTROL:
        _modsState &= ~KEYMOD_LCTRL;
        if (!(_modsState & KEYMOD_RCTRL))
            _modsState &= ~KEYMOD_CTRL;
        break;
    case VK_RCONTROL:
        _modsState &= ~KEYMOD_RCTRL;
        if (!(_modsState & KEYMOD_LCTRL))
            _modsState &= ~KEYMOD_CTRL;
        break;

    case VK_MENU:
    case VK_LMENU:
    case VK_RMENU:
        _modsState &= ~KEYMOD_ALT;
        break;

    case VK_LWIN:
    case VK_RWIN:
        _modsState &= ~KEYMOD_WIN;
        break;
    }
}

uint8_t CKeyEventSink::_GetTogglesSnapshot() const
{
    uint8_t toggles = 0;
    if (GetKeyState(VK_CAPITAL) & 0x01) toggles |= TOGGLE_CAPSLOCK;
    if (GetKeyState(VK_NUMLOCK) & 0x01) toggles |= TOGGLE_NUMLOCK;
    if (GetKeyState(VK_SCROLL) & 0x01)  toggles |= TOGGLE_SCROLLLOCK;
    return toggles;
}

void CKeyEventSink::_SyncStateFromResponse(uint32_t statusFlags)
{
    // Sync mode from Go response
    bool chineseMode = (statusFlags & STATUS_CHINESE_MODE) != 0;
    _pTextService->SetInputMode(chineseMode);
}

// ============================================================================
// Config sync handler
// ============================================================================

void CKeyEventSink::OnSyncConfig(const std::string& key, const std::vector<uint8_t>& value)
{
    if (key == CONFIG_KEY_ENGLISH_PAIRS)
    {
        if (value.size() < 2) return;
        bool enabled = value[0] != 0;
        uint8_t count = value[1];

        std::vector<std::pair<wchar_t, wchar_t>> pairs;
        for (size_t i = 0; i < count && (2 + i * 4 + 4) <= value.size(); i++)
        {
            uint16_t left = *reinterpret_cast<const uint16_t*>(value.data() + 2 + i * 4);
            uint16_t right = *reinterpret_cast<const uint16_t*>(value.data() + 2 + i * 4 + 2);
            pairs.push_back({(wchar_t)left, (wchar_t)right});
        }

        _englishPairEngine.SetPairs(pairs);
        _englishPairEngine.SetEnabled(enabled);

        WIND_LOG_INFO_FMT(L"English pair config updated: enabled=%d, pairs=%d\n", enabled, (int)pairs.size());
    }
}

// ============================================================================
// Barrier mechanism implementation
// ============================================================================

BOOL CKeyEventSink::_SendCommitRequest(uint16_t barrierSeq, uint16_t triggerKey, uint32_t mods, const std::string& inputBuffer)
{
    CIPCClient* pIPCClient = _pTextService->GetIPCClient();
    if (pIPCClient == nullptr || !pIPCClient->IsConnected())
    {
        return FALSE;
    }

    // Build CommitRequestPayload
    size_t payloadSize = sizeof(CommitRequestPayload) - sizeof(uint32_t) + 4 + inputBuffer.size();
    std::vector<uint8_t> payload(12 + inputBuffer.size());

    // Header fields
    payload[0] = barrierSeq & 0xFF;
    payload[1] = (barrierSeq >> 8) & 0xFF;
    payload[2] = triggerKey & 0xFF;
    payload[3] = (triggerKey >> 8) & 0xFF;
    payload[4] = mods & 0xFF;
    payload[5] = (mods >> 8) & 0xFF;
    payload[6] = (mods >> 16) & 0xFF;
    payload[7] = (mods >> 24) & 0xFF;
    uint32_t inputLen = (uint32_t)inputBuffer.size();
    payload[8] = inputLen & 0xFF;
    payload[9] = (inputLen >> 8) & 0xFF;
    payload[10] = (inputLen >> 16) & 0xFF;
    payload[11] = (inputLen >> 24) & 0xFF;

    // Copy input buffer
    if (!inputBuffer.empty())
    {
        memcpy(payload.data() + 12, inputBuffer.data(), inputBuffer.size());
    }

    return pIPCClient->SendCommitRequest(payload.data(), (uint32_t)payload.size());
}

void CKeyEventSink::_HandleCommitResult(uint16_t barrierSeq, const std::wstring& text, const std::wstring& newComp, bool modeChanged, bool chineseMode)
{
    if (!_pendingCommit.waiting || _pendingCommit.barrierSeq != barrierSeq)
    {
        // Barrier mismatch, log warning
        WIND_LOG_TRACE(L"CommitResult barrier mismatch, ignoring\n");
        return;
    }

    // Clear pending state
    _pendingCommit.waiting = false;

    // Commit the text and handle composition atomically
    if (!newComp.empty())
    {
        // Has new composition: use InsertTextAndStartComposition (now handles end old composition internally)
        _pTextService->InsertTextAndStartComposition(text, newComp);
        _isComposing = TRUE;
    }
    else
    {
        // No new composition: atomic commit (end composition + insert text)
        _pTextService->CommitText(text);
        _isComposing = FALSE;
        _hasCandidates = FALSE;
    }

    // Handle mode change
    if (modeChanged)
    {
        _pTextService->SetInputMode(chineseMode);
    }
}

void CKeyEventSink::_CheckBarrierTimeout()
{
    if (!_pendingCommit.waiting)
        return;

    DWORD elapsed = GetTickCount() - _pendingCommit.requestTime;
    if (elapsed > BARRIER_TIMEOUT_MS)
    {
        WIND_LOG_ERROR(L"Barrier timeout, falling back to local handling");

        // Timeout - clear pending state and try to recover
        _pendingCommit.waiting = false;

        // Fallback: just clear the composition
        _pTextService->EndComposition();
        _isComposing = FALSE;
        _hasCandidates = FALSE;
    }
}

// ============================================================================
// Auto-pair key simulation (deferred + skip list approach)
//
// When modifiers are held (e.g., Shift for "("), we defer the cursor key
// until modifiers are released. This avoids the fundamental flaw of the
// "release modifiers via SendInput" approach: releasing and restoring Shift
// via SendInput causes the OS to generate additional Shift key-down events
// (with repeat bit 0), which re-arms _pendingKeyUpKey and triggers mode
// toggle when the physical Shift is released.
// ============================================================================

void CKeyEventSink::_SimulatePairKey(WORD vk)
{
    if (_AreModifiersHeld())
    {
        // Defer: save action, execute when modifiers released
        if (!_pendingPairAction.active)
        {
            _pendingPairAction.vk = vk;
            _pendingPairAction.count = 1;
            _pendingPairAction.active = true;
        }
        else if (_pendingPairAction.vk == vk)
        {
            // Same key deferred again (e.g., Shift+< pressed multiple times)
            // Only the last pair's cursor positioning matters, keep count = 1
        }
        else
        {
            // Different key — replace pending action
            _pendingPairAction.vk = vk;
            _pendingPairAction.count = 1;
        }
        WIND_LOG_DEBUG_FMT(L"Auto-pair: deferred vk=0x%02X x%d (modifiers held)\n",
            (WORD)vk, _pendingPairAction.count);
        return;
    }

    // No modifiers: execute immediately via skip list
    _PushSkipKey(vk);

    INPUT inputs[2] = {};
    inputs[0].type = INPUT_KEYBOARD;
    inputs[0].ki.wVk = vk;
    inputs[1].type = INPUT_KEYBOARD;
    inputs[1].ki.wVk = vk;
    inputs[1].ki.dwFlags = KEYEVENTF_KEYUP;
    SendInput(2, inputs, sizeof(INPUT));
}

bool CKeyEventSink::_AreModifiersHeld()
{
    return (GetAsyncKeyState(VK_SHIFT) & 0x8000) != 0 ||
           (GetAsyncKeyState(VK_CONTROL) & 0x8000) != 0 ||
           (GetAsyncKeyState(VK_MENU) & 0x8000) != 0;
}

void CKeyEventSink::_PushSkipKey(WORD vk)
{
    if (_skipKeyCount < MAX_SKIP_KEYS)
    {
        _skipKeys[_skipKeyCount++] = vk;
    }
}

BOOL CKeyEventSink::_TryConsumeSkipKey(WPARAM wParam)
{
    if (_skipKeyCount > 0 && _skipKeys[0] == (WORD)wParam)
    {
        // Shift remaining entries left
        for (int i = 1; i < _skipKeyCount; i++)
            _skipKeys[i - 1] = _skipKeys[i];
        _skipKeyCount--;
        WIND_LOG_DEBUG_FMT(L"Auto-pair: skip key 0x%02X bypassed IME, remaining=%d\n", (WORD)wParam, _skipKeyCount);
        return TRUE;
    }
    return FALSE;
}

