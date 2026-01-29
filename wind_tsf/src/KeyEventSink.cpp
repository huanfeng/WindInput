#include "KeyEventSink.h"
#include "TextService.h"
#include "IPCClient.h"
#include "HotkeyManager.h"
#include <cctype>

CKeyEventSink::CKeyEventSink(CTextService* pTextService)
    : _refCount(1)
    , _pTextService(pTextService)
    , _dwKeySinkCookie(TF_INVALID_COOKIE)
    , _isComposing(FALSE)
    , _hasCandidates(FALSE)
    , _shiftPending(FALSE)
    , _pendingToggleKey(0)
{
    _pTextService->AddRef();
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
    OutputDebugStringW(L"[WindInput] KeyEventSink::OnSetFocus\n");
    return S_OK;
}

STDAPI CKeyEventSink::OnTestKeyDown(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten)
{
    *pfEaten = FALSE;

    // First check if the context is read-only (browser non-editable area)
    // This is critical for allowing browser shortcuts (Space to scroll, etc.)
    if (_IsContextReadOnly(pContext))
    {
        // Read-only context, don't intercept any keys
        return S_OK;
    }

    // Normal key handling
    *pfEaten = _IsKeyWeShouldHandle(wParam);

    // Debug: log key handling decision for letter keys
    if (wParam >= 'A' && wParam <= 'Z')
    {
        WCHAR debug[128];
        wsprintfW(debug, L"[WindInput] OnTestKeyDown: key=%c, eaten=%d, composing=%d, chineseMode=%d\n",
                  (wchar_t)wParam, *pfEaten, _isComposing, _pTextService->IsChineseMode());
        OutputDebugStringW(debug);
    }

    return S_OK;
}

STDAPI CKeyEventSink::OnKeyDown(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten)
{
    *pfEaten = FALSE;

    CHotkeyManager* pHotkeyMgr = _pTextService->GetHotkeyManager();
    int modifiers = _GetModifierState();

    // Check if this is a toggle mode key (Shift, Ctrl, CapsLock depending on config)
    if (pHotkeyMgr != nullptr && pHotkeyMgr->IsToggleModeKey(wParam))
    {
        // Check if this is a key repeat (bit 30 of lParam)
        if (lParam & 0x40000000)
        {
            // Key repeat, ignore
            *pfEaten = TRUE;
            return S_OK;
        }

        // Check if other modifiers are pressed (e.g., Ctrl+Shift is a system shortcut)
        // Only consider it a toggle if the key is pressed alone
        BOOL hasOtherModifier = FALSE;
        if (wParam == VK_SHIFT || wParam == VK_LSHIFT || wParam == VK_RSHIFT)
        {
            hasOtherModifier = (GetAsyncKeyState(VK_CONTROL) & 0x8000) || (GetAsyncKeyState(VK_MENU) & 0x8000);
        }
        else if (wParam == VK_CONTROL || wParam == VK_LCONTROL || wParam == VK_RCONTROL)
        {
            hasOtherModifier = (GetAsyncKeyState(VK_SHIFT) & 0x8000) || (GetAsyncKeyState(VK_MENU) & 0x8000);
        }
        // CapsLock doesn't need modifier check

        if (hasOtherModifier)
        {
            _shiftPending = FALSE;
            _pendingToggleKey = 0;
            return S_OK;  // Let system handle it
        }

        // Mark toggle key as pending (will toggle mode on release if no other key pressed)
        _shiftPending = TRUE;
        _pendingToggleKey = wParam;
        *pfEaten = TRUE;
        return S_OK;
    }

    // Any other key cancels toggle pending
    _shiftPending = FALSE;
    _pendingToggleKey = 0;

    // Check if context is read-only (browser non-editable area)
    // Don't intercept keys in read-only areas to allow browser shortcuts
    if (_IsContextReadOnly(pContext))
    {
        return S_OK;
    }

    if (!_IsKeyWeShouldHandle(wParam))
    {
        return S_OK;
    }

    // Send key to Go Service
    if (!_SendKeyToService(wParam))
    {
        OutputDebugStringW(L"[WindInput] Failed to send key to service, passing through\n");

        // Service not available - pass through letters directly
        if (wParam >= 'A' && wParam <= 'Z')
        {
            std::wstring ch;
            ch = (wchar_t)towlower((wint_t)wParam);
            _pTextService->InsertText(ch);
            *pfEaten = TRUE;
        }
        return S_OK;
    }

    // Handle response from service
    _HandleServiceResponse();

    *pfEaten = TRUE;
    return S_OK;
}

STDAPI CKeyEventSink::OnTestKeyUp(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten)
{
    CHotkeyManager* pHotkeyMgr = _pTextService->GetHotkeyManager();

    // Handle pending toggle key release
    if (_shiftPending && _pendingToggleKey != 0)
    {
        // Check if this is the same key that was pressed
        BOOL isMatchingKey = (wParam == _pendingToggleKey);
        // Also check for generic VK_SHIFT matching VK_LSHIFT/VK_RSHIFT
        if (!isMatchingKey && (wParam == VK_SHIFT || wParam == VK_LSHIFT || wParam == VK_RSHIFT))
        {
            isMatchingKey = (_pendingToggleKey == VK_SHIFT || _pendingToggleKey == VK_LSHIFT || _pendingToggleKey == VK_RSHIFT);
        }
        if (!isMatchingKey && (wParam == VK_CONTROL || wParam == VK_LCONTROL || wParam == VK_RCONTROL))
        {
            isMatchingKey = (_pendingToggleKey == VK_CONTROL || _pendingToggleKey == VK_LCONTROL || _pendingToggleKey == VK_RCONTROL);
        }

        if (isMatchingKey)
        {
            *pfEaten = TRUE;
            return S_OK;
        }
    }

    // Also handle Caps Lock for indicator (always, regardless of config)
    if (wParam == VK_CAPITAL)
    {
        *pfEaten = TRUE;
        return S_OK;
    }

    *pfEaten = FALSE;
    return S_OK;
}

STDAPI CKeyEventSink::OnKeyUp(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten)
{
    *pfEaten = FALSE;

    CHotkeyManager* pHotkeyMgr = _pTextService->GetHotkeyManager();

    // Handle toggle key release for mode toggle
    if (_shiftPending && _pendingToggleKey != 0)
    {
        // Check if this is the same key that was pressed
        BOOL isMatchingKey = (wParam == _pendingToggleKey);
        // Also check for generic VK_SHIFT matching VK_LSHIFT/VK_RSHIFT
        if (!isMatchingKey && (wParam == VK_SHIFT || wParam == VK_LSHIFT || wParam == VK_RSHIFT))
        {
            isMatchingKey = (_pendingToggleKey == VK_SHIFT || _pendingToggleKey == VK_LSHIFT || _pendingToggleKey == VK_RSHIFT);
        }
        if (!isMatchingKey && (wParam == VK_CONTROL || wParam == VK_LCONTROL || wParam == VK_RCONTROL))
        {
            isMatchingKey = (_pendingToggleKey == VK_CONTROL || _pendingToggleKey == VK_LCONTROL || _pendingToggleKey == VK_RCONTROL);
        }

        if (isMatchingKey)
        {
            _shiftPending = FALSE;
            _pendingToggleKey = 0;

            // Toggle mode (handles CapsLock specially - it toggles on key down, not release)
            if (wParam != VK_CAPITAL)
            {
                _pTextService->ToggleInputMode();
            }

            *pfEaten = TRUE;
            return S_OK;
        }
    }

    // Handle Caps Lock key release - show A/a indicator and update language bar
    // CapsLock toggles on release, not on pending
    if (wParam == VK_CAPITAL)
    {
        // If CapsLock is configured as toggle key and was pending, toggle mode
        if (pHotkeyMgr != nullptr && pHotkeyMgr->IsToggleModeKey(VK_CAPITAL))
        {
            // CapsLock toggles mode
            _pTextService->ToggleInputMode();
        }

        // Get current Caps Lock state (after the key was processed)
        BOOL capsLockOn = (GetKeyState(VK_CAPITAL) & 0x0001) != 0;

        // Update language bar to show Caps Lock state (A/a in English mode)
        _pTextService->UpdateCapsLockState(capsLockOn);

        // Send to Go service for popup indicator
        CIPCClient* pIPCClient = _pTextService->GetIPCClient();
        if (pIPCClient != nullptr && pIPCClient->IsConnected())
        {
            pIPCClient->SendCapsLockState(capsLockOn);

            // Receive and discard response
            ServiceResponse response;
            pIPCClient->ReceiveResponse(response);
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
    OutputDebugStringW(L"[WindInput] KeyEventSink::Initialize\n");

    ITfThreadMgr* pThreadMgr = _pTextService->GetThreadMgr();
    if (pThreadMgr == nullptr)
    {
        OutputDebugStringW(L"[WindInput] ThreadMgr is null!\n");
        return FALSE;
    }

    ITfKeystrokeMgr* pKeystrokeMgr = nullptr;
    HRESULT hr = pThreadMgr->QueryInterface(IID_ITfKeystrokeMgr, (void**)&pKeystrokeMgr);

    if (FAILED(hr) || pKeystrokeMgr == nullptr)
    {
        OutputDebugStringW(L"[WindInput] Failed to get ITfKeystrokeMgr\n");
        return FALSE;
    }

    hr = pKeystrokeMgr->AdviseKeyEventSink(_pTextService->GetClientId(), this, TRUE);
    pKeystrokeMgr->Release();

    if (FAILED(hr))
    {
        OutputDebugStringW(L"[WindInput] AdviseKeyEventSink failed\n");
        return FALSE;
    }

    OutputDebugStringW(L"[WindInput] KeyEventSink initialized successfully\n");
    return TRUE;
}

void CKeyEventSink::Uninitialize()
{
    OutputDebugStringW(L"[WindInput] KeyEventSink::Uninitialize\n");

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

int CKeyEventSink::_GetModifierState()
{
    int modifiers = 0;

    // Use GetAsyncKeyState to detect modifier key states
    if (GetAsyncKeyState(VK_SHIFT) & 0x8000)
        modifiers |= KEY_MOD_SHIFT;
    if (GetAsyncKeyState(VK_CONTROL) & 0x8000)
        modifiers |= KEY_MOD_CTRL;
    if (GetAsyncKeyState(VK_MENU) & 0x8000)  // VK_MENU = Alt
        modifiers |= KEY_MOD_ALT;

    return modifiers;
}

BOOL CKeyEventSink::_IsKeyWeShouldHandle(WPARAM wParam)
{
    CHotkeyManager* pHotkeyMgr = _pTextService->GetHotkeyManager();
    if (pHotkeyMgr == nullptr)
    {
        OutputDebugStringW(L"[WindInput] HotkeyManager is null, using fallback logic\n");
        // Fallback to basic handling
        int modifiers = _GetModifierState();
        if (modifiers & (KEY_MOD_CTRL | KEY_MOD_ALT))
            return FALSE;
        if (wParam >= 'A' && wParam <= 'Z')
            return TRUE;
        return FALSE;
    }

    int modifiers = _GetModifierState();
    BOOL isChineseMode = _pTextService->IsChineseMode();

    return pHotkeyMgr->ShouldInterceptKey(wParam, modifiers, _isComposing, _hasCandidates, isChineseMode);
}

// Check if a key is a punctuation key we should handle
BOOL CKeyEventSink::_IsPunctuationKey(WPARAM wParam)
{
    CHotkeyManager* pHotkeyMgr = _pTextService->GetHotkeyManager();
    if (pHotkeyMgr != nullptr)
    {
        return pHotkeyMgr->IsPunctuationKey(wParam);
    }
    // Fallback
    switch (wParam)
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

// Convert punctuation virtual key to character
wchar_t CKeyEventSink::_VirtualKeyToPunctuation(WPARAM wParam, BOOL shiftPressed)
{
    CHotkeyManager* pHotkeyMgr = _pTextService->GetHotkeyManager();
    if (pHotkeyMgr != nullptr)
    {
        return pHotkeyMgr->VirtualKeyToPunctuation(wParam, shiftPressed);
    }
    // Fallback
    switch (wParam)
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

BOOL CKeyEventSink::_SendKeyToService(WPARAM wParam)
{
    CIPCClient* pIPCClient = _pTextService->GetIPCClient();
    if (pIPCClient == nullptr)
    {
        OutputDebugStringW(L"[WindInput] IPCClient is null!\n");
        return FALSE;
    }

    // Convert virtual key to character/key name
    std::wstring key;
    int keyCode = (int)wParam;
    BOOL needCaret = FALSE;  // Whether to include caret position in request

    // Get modifier state
    int modifiers = _GetModifierState();
    BOOL shiftPressed = (modifiers & KEY_MOD_SHIFT) != 0;

    if (wParam >= 'A' && wParam <= 'Z')
    {
        // Letter key - determine case based on Caps Lock and Shift state
        BOOL capsLock = (GetKeyState(VK_CAPITAL) & 0x0001) != 0;

        // XOR logic: Caps Lock and Shift cancel each other
        BOOL shouldBeUppercase = capsLock ^ shiftPressed;

        if (_pTextService->IsChineseMode())
        {
            // Chinese mode: always use lowercase for pinyin
            key = (wchar_t)towlower((wint_t)wParam);
            _isComposing = TRUE;
        }
        else
        {
            // English mode: respect Caps Lock and Shift
            if (shouldBeUppercase)
            {
                key = (wchar_t)wParam;  // Already uppercase
            }
            else
            {
                key = (wchar_t)towlower((wint_t)wParam);
            }
        }

        // Always include caret with letter keys (both Chinese and English mode need it)
        // This replaces the separate caret_update call
        needCaret = TRUE;
    }
    else if (wParam >= '1' && wParam <= '9')
    {
        // Number key
        key = (wchar_t)wParam;
    }
    else if (wParam == VK_BACK)
    {
        key = L"backspace";
    }
    else if (wParam == VK_RETURN)
    {
        key = L"enter";
    }
    else if (wParam == VK_ESCAPE)
    {
        key = L"escape";
        _isComposing = FALSE;
    }
    else if (wParam == VK_SPACE)
    {
        key = L"space";
    }
    else if (wParam == VK_SHIFT)
    {
        key = L"shift";
    }
    else if (wParam == VK_OEM_3)  // ` key (backtick/tilde)
    {
        key = L"`";
    }
    else if (wParam == VK_OEM_MINUS)  // - key for page up
    {
        key = L"page_up";
    }
    else if (wParam == VK_OEM_PLUS)  // = key for page down
    {
        key = L"page_down";
    }
    else if (_IsPunctuationKey(wParam))
    {
        // Punctuation key
        wchar_t punct = _VirtualKeyToPunctuation(wParam, shiftPressed);
        if (punct != 0)
        {
            key = punct;
        }
        else
        {
            return FALSE;
        }
    }
    else
    {
        return FALSE;
    }

    // Include caret position in the same request if needed
    if (needCaret)
    {
        LONG x, y, height;
        if (_pTextService->GetCaretPosition(&x, &y, &height))
        {
            return pIPCClient->SendKeyEvent(key, keyCode, modifiers, &x, &y, &height);
        }
    }

    return pIPCClient->SendKeyEvent(key, keyCode, modifiers);
}

void CKeyEventSink::_HandleServiceResponse()
{
    CIPCClient* pIPCClient = _pTextService->GetIPCClient();
    if (pIPCClient == nullptr)
        return;

    ServiceResponse response;
    if (!pIPCClient->ReceiveResponse(response))
    {
        OutputDebugStringW(L"[WindInput] Failed to receive response from service\n");
        return;
    }

    switch (response.type)
    {
    case ResponseType::Ack:
        // ACK is common, no need to log
        break;

    case ResponseType::InsertText:
        {
            OutputDebugStringW(L"[WindInput] Processing InsertText response\n");
            _isComposing = FALSE;
            _hasCandidates = FALSE;

            // First, end composition to clear the preedit text
            // This will remove the underlined composition text
            _pTextService->EndComposition();
            OutputDebugStringW(L"[WindInput] EndComposition completed\n");

            // Then insert the final text
            if (!response.text.empty())
            {
                WCHAR debug[256];
                wsprintfW(debug, L"[WindInput] Inserting text: %s\n", response.text.c_str());
                OutputDebugStringW(debug);
                _pTextService->InsertText(response.text);
                OutputDebugStringW(L"[WindInput] InsertText completed\n");
            }

            // Handle mode change if present (CommitOnSwitch feature)
            if (response.modeChanged)
            {
                OutputDebugStringW(L"[WindInput] InsertText with mode change - updating language bar\n");
                _pTextService->SetInputMode(response.chineseMode);
            }
        }
        break;

    case ResponseType::UpdateComposition:
        OutputDebugStringW(L"[WindInput] Received UpdateComposition from service\n");
        _isComposing = TRUE;   // Ensure composing state is set
        _hasCandidates = TRUE; // Assume there are candidates when composition is updated
        _pTextService->UpdateComposition(response.composition, response.caretPos);
        break;

    case ResponseType::ClearComposition:
        OutputDebugStringW(L"[WindInput] Received ClearComposition from service\n");
        _isComposing = FALSE;
        _hasCandidates = FALSE;
        _pTextService->EndComposition();
        break;

    case ResponseType::ModeChanged:
        OutputDebugStringW(L"[WindInput] Received ModeChanged from service\n");
        // Clear composing state and end any active composition when mode changes
        _isComposing = FALSE;
        _hasCandidates = FALSE;
        _pTextService->EndComposition();
        // Update local mode state and language bar icon
        _pTextService->SetInputMode(response.chineseMode);
        break;

    case ResponseType::Consumed:
        // Key was consumed by a hotkey (e.g., Shift+Space, Ctrl+.)
        // Don't output anything, just consume the key
        OutputDebugStringW(L"[WindInput] Key consumed by hotkey\n");
        break;

    default:
        OutputDebugStringW(L"[WindInput] Unknown response type from service\n");
        break;
    }
}

// Check if the current context is read-only (e.g., browser non-editable area)
BOOL CKeyEventSink::_IsContextReadOnly(ITfContext* pContext)
{
    if (!pContext)
    {
        return TRUE;  // No context, treat as read-only for safety
    }

    TF_STATUS tfStatus = {};
    HRESULT hr = pContext->GetStatus(&tfStatus);

    if (SUCCEEDED(hr))
    {
        // Check if the dynamic flags indicate read-only
        if (tfStatus.dwDynamicFlags & TF_SD_READONLY)
        {
            OutputDebugStringW(L"[WindInput] Context is read-only (TF_SD_READONLY)\n");
            return TRUE;
        }

        // Also check for loading state (some apps set this temporarily)
        if (tfStatus.dwDynamicFlags & TF_SD_LOADING)
        {
            OutputDebugStringW(L"[WindInput] Context is loading (TF_SD_LOADING)\n");
            return TRUE;
        }
    }

    return FALSE;  // Context is writable
}

// Check if the current process is a browser (Chrome, Edge, Firefox, etc.)
BOOL CKeyEventSink::_IsCurrentProcessBrowser()
{
    static BOOL s_checked = FALSE;
    static BOOL s_isBrowser = FALSE;

    // Cache the result since process name doesn't change
    if (s_checked)
    {
        return s_isBrowser;
    }

    s_checked = TRUE;
    s_isBrowser = FALSE;

    // Get current process executable name
    wchar_t exePath[MAX_PATH] = {};
    if (GetModuleFileNameW(nullptr, exePath, MAX_PATH) > 0)
    {
        // Extract filename from path
        wchar_t* fileName = wcsrchr(exePath, L'\\');
        if (fileName)
        {
            fileName++;  // Skip the backslash

            // Check against known browser process names (case-insensitive)
            if (_wcsicmp(fileName, L"chrome.exe") == 0 ||
                _wcsicmp(fileName, L"msedge.exe") == 0 ||
                _wcsicmp(fileName, L"firefox.exe") == 0 ||
                _wcsicmp(fileName, L"brave.exe") == 0 ||
                _wcsicmp(fileName, L"opera.exe") == 0 ||
                _wcsicmp(fileName, L"vivaldi.exe") == 0 ||
                _wcsicmp(fileName, L"iexplore.exe") == 0)
            {
                s_isBrowser = TRUE;
                OutputDebugStringW(L"[WindInput] Running in browser process\n");
            }
        }
    }

    return s_isBrowser;
}
