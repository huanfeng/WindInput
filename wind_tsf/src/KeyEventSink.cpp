#include "KeyEventSink.h"
#include "TextService.h"
#include "IPCClient.h"
#include "HotkeyManager.h"
#include "BinaryProtocol.h"
#include <cctype>

CKeyEventSink::CKeyEventSink(CTextService* pTextService)
    : _refCount(1)
    , _pTextService(pTextService)
    , _dwKeySinkCookie(TF_INVALID_COOKIE)
    , _isComposing(FALSE)
    , _hasCandidates(FALSE)
    , _pendingKeyUpKey(0)
    , _pendingKeyUpModifiers(0)
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

    // Debug: Log ALL key presses to see what TSF sends us (v2 - with version marker)
    {
        WCHAR debug[128];
        wsprintfW(debug, L"[WindInput-v2] OnTestKeyDown: wParam=0x%02X\n", (uint32_t)wParam);
        OutputDebugStringW(debug);
    }

    // First check if the context is read-only (browser non-editable area)
    if (_IsContextReadOnly(pContext))
    {
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
        WCHAR debug[128];
        wsprintfW(debug, L"[WindInput] KeyDown hotkey matched: vk=0x%02X, hash=0x%08X\n",
                  (uint32_t)wParam, normalizedKeyHash);
        OutputDebugStringW(debug);
        *pfEaten = TRUE;
        return S_OK;
    }

    // Debug: log when Tab key is not matched (to diagnose Tab/Shift+Tab issues)
    if (wParam == VK_TAB)
    {
        WCHAR debug[256];
        wsprintfW(debug, L"[WindInput] Tab not in KeyDown hotkeys: mods=0x%04X, normHash=0x%08X, hasHotkeys=%d\n",
                  modifiers, normalizedKeyHash, (pHotkeyMgr != nullptr && pHotkeyMgr->HasHotkeys()) ? 1 : 0);
        OutputDebugStringW(debug);
    }

    // Check for KeyUp triggered keys (toggle mode keys) - we still need to intercept KeyDown
    // First try hash-based lookup, then fallback to VK-based detection
    BOOL isToggleModeKey = FALSE;

    if (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyUpHotkey(keyHash))
    {
        isToggleModeKey = TRUE;
    }
    else if (CHotkeyManager::IsToggleModeKeyByVK(wParam))
    {
        // Fallback: detect toggle mode keys even without hash whitelist sync
        // This ensures Shift/Ctrl toggle works even if IPC fails
        isToggleModeKey = TRUE;
    }

    // Debug: Log toggle mode key detection
    if (CHotkeyManager::IsToggleModeKeyByVK(wParam))
    {
        WCHAR debug[256];
        wsprintfW(debug, L"[WindInput] OnTestKeyDown: Toggle key wParam=0x%X, mods=0x%X, hash=0x%08X, isToggle=%d\n",
                  (uint32_t)wParam, modifiers, keyHash, isToggleModeKey);
        OutputDebugStringW(debug);
    }

    if (isToggleModeKey)
    {
        *pfEaten = TRUE;
        return S_OK;
    }

    // Check basic input keys based on current state
    if (_isComposing || _hasCandidates || _pTextService->IsChineseMode())
    {
        HotkeyType keyType = CHotkeyManager::ClassifyInputKey(wParam, modifiers);
        if (keyType != HotkeyType::None)
        {
            *pfEaten = TRUE;
            return S_OK;
        }
    }

    return S_OK;
}

STDAPI CKeyEventSink::OnKeyDown(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten)
{
    *pfEaten = FALSE;

    uint32_t modifiers = CHotkeyManager::GetCurrentModifiers();
    uint32_t keyHash = CHotkeyManager::CalcKeyHash(modifiers, (uint32_t)wParam);

    // For function hotkeys (like Ctrl+`), use normalized modifiers (no left/right distinction)
    uint32_t normalizedMods = CHotkeyManager::NormalizeModifiers(modifiers);
    uint32_t normalizedKeyHash = CHotkeyManager::CalcKeyHash(normalizedMods, (uint32_t)wParam);

    CHotkeyManager* pHotkeyMgr = _pTextService->GetHotkeyManager();

    // Check if this is a KeyUp triggered key (toggle mode keys like Shift, Ctrl, CapsLock)
    // Use hash-based lookup first, then fallback to VK-based detection
    BOOL isToggleModeKey = FALSE;
    if (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyUpHotkey(keyHash))
    {
        isToggleModeKey = TRUE;
    }
    else if (CHotkeyManager::IsToggleModeKeyByVK(wParam))
    {
        // Fallback: detect toggle mode keys even without hash whitelist sync
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
        _pendingKeyUpKey = (uint32_t)wParam;
        _pendingKeyUpModifiers = modifiers;

        OutputDebugStringW(L"[WindInput] OnKeyDown: Toggle mode key pending for KeyUp\n");

        *pfEaten = TRUE;
        return S_OK;
    }

    // Any other key cancels pending toggle
    _pendingKeyUpKey = 0;
    _pendingKeyUpModifiers = 0;

    // Check if context is read-only
    if (_IsContextReadOnly(pContext))
    {
        return S_OK;
    }

    // Check if this is a KeyDown hotkey from whitelist
    // Use normalized hash for function hotkeys (Ctrl+`, Shift+Space, etc.)
    BOOL isKeyDownHotkey = (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyDownHotkey(normalizedKeyHash));

    // Check for basic input keys
    BOOL isInputKey = FALSE;
    if (_isComposing || _hasCandidates || _pTextService->IsChineseMode())
    {
        HotkeyType keyType = CHotkeyManager::ClassifyInputKey(wParam, modifiers);
        isInputKey = (keyType != HotkeyType::None);
    }

    if (!isKeyDownHotkey && !isInputKey)
    {
        return S_OK;
    }

    // Update caret position before sending key event
    // This ensures Go has accurate position for candidate window placement
    _pTextService->SendCaretPositionUpdate();

    // Send key to Go Service using binary protocol
    if (!_SendKeyToService((uint32_t)wParam, modifiers, KEY_EVENT_DOWN))
    {
        OutputDebugStringW(L"[WindInput] Failed to send key to service, passing through\n");

        // Service not available - pass through letters directly
        if (wParam >= 'A' && wParam <= 'Z' && !(modifiers & (KEYMOD_CTRL | KEYMOD_ALT)))
        {
            std::wstring ch;
            ch = (wchar_t)towlower((wint_t)wParam);
            _pTextService->InsertText(ch);
            *pfEaten = TRUE;
        }
        return S_OK;
    }

    // Handle response from service - returns TRUE if key was handled
    *pfEaten = _HandleServiceResponse();
    return S_OK;
}

STDAPI CKeyEventSink::OnTestKeyUp(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten)
{
    *pfEaten = FALSE;

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

    // Handle toggle key release for mode toggle
    if (_pendingKeyUpKey != 0)
    {
        if (_IsMatchingKeyUp(wParam, _pendingKeyUpKey))
        {
            uint32_t pendingKey = _pendingKeyUpKey;
            uint32_t pendingMods = _pendingKeyUpModifiers;
            _pendingKeyUpKey = 0;
            _pendingKeyUpModifiers = 0;

            // Send KeyUp event to Go Service for toggle processing
            if (pendingKey != VK_CAPITAL)
            {
                // For Shift/Ctrl, send KeyUp event with the saved modifiers
                // The modifiers were captured when KeyDown was pressed (when the key was still held)
                if (_SendKeyToService(pendingKey, pendingMods, KEY_EVENT_UP))
                {
                    _HandleServiceResponse();
                }
                else
                {
                    // Fallback: toggle locally if service unavailable
                    _pTextService->ToggleInputMode();
                }
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

        if (_SendKeyToService(VK_CAPITAL, mods, KEY_EVENT_UP))
        {
            _HandleServiceResponse();
        }

        // Update language bar
        _pTextService->UpdateCapsLockState(capsLockOn);

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

// Helper: Check if wParam matches the pending KeyUp key (handles VK_SHIFT -> VK_LSHIFT/RSHIFT mapping)
BOOL CKeyEventSink::_IsMatchingKeyUp(WPARAM wParam, uint32_t pendingKey)
{
    if (wParam == pendingKey)
        return TRUE;

    // Handle generic VK_SHIFT matching VK_LSHIFT/VK_RSHIFT
    if ((wParam == VK_SHIFT || wParam == VK_LSHIFT || wParam == VK_RSHIFT) &&
        (pendingKey == VK_SHIFT || pendingKey == VK_LSHIFT || pendingKey == VK_RSHIFT))
    {
        return TRUE;
    }

    // Handle generic VK_CONTROL matching VK_LCONTROL/VK_RCONTROL
    if ((wParam == VK_CONTROL || wParam == VK_LCONTROL || wParam == VK_RCONTROL) &&
        (pendingKey == VK_CONTROL || pendingKey == VK_LCONTROL || pendingKey == VK_RCONTROL))
    {
        return TRUE;
    }

    return FALSE;
}

// Send key to Go Service using binary protocol
BOOL CKeyEventSink::_SendKeyToService(uint32_t keyCode, uint32_t modifiers, uint8_t eventType)
{
    CIPCClient* pIPCClient = _pTextService->GetIPCClient();
    if (pIPCClient == nullptr)
    {
        OutputDebugStringW(L"[WindInput] IPCClient is null!\n");
        return FALSE;
    }

    // Get scan code from virtual key (optional, set to 0 if not needed)
    uint32_t scanCode = MapVirtualKeyW(keyCode, MAPVK_VK_TO_VSC);

    return pIPCClient->SendKeyEvent(keyCode, scanCode, modifiers, eventType);
}

BOOL CKeyEventSink::_HandleServiceResponse()
{
    CIPCClient* pIPCClient = _pTextService->GetIPCClient();
    if (pIPCClient == nullptr)
        return TRUE; // Default to eating the key if no IPC

    ServiceResponse response;
    if (!pIPCClient->ReceiveResponse(response))
    {
        OutputDebugStringW(L"[WindInput] Failed to receive response from service\n");
        return TRUE; // Default to eating the key on error
    }

    switch (response.type)
    {
    case ResponseType::Ack:
        // ACK means key was handled (consumed without output)
        return TRUE;

    case ResponseType::PassThrough:
        // PassThrough means key was NOT handled, pass to system
        OutputDebugStringW(L"[WindInput] PassThrough: key not handled, passing to system\n");
        return FALSE;

    case ResponseType::CommitText:
        {
            OutputDebugStringW(L"[WindInput] Processing CommitText response\n");

            // Handle new composition if present (top code commit feature)
            if (!response.newComposition.empty())
            {
                WCHAR debug[256];
                wsprintfW(debug, L"[WindInput] CommitText with new composition: text='%s', newComp='%s'\n",
                          response.text.c_str(), response.newComposition.c_str());
                OutputDebugStringW(debug);

                _pTextService->InsertTextAndStartComposition(response.text, response.newComposition);
                _isComposing = TRUE;
                _hasCandidates = TRUE;
            }
            else
            {
                // No new composition, just insert text normally
                _pTextService->EndComposition();

                if (!response.text.empty())
                {
                    _pTextService->InsertText(response.text);
                }
                _isComposing = FALSE;
                _hasCandidates = FALSE;
            }

            // Handle mode change if present
            if (response.modeChanged)
            {
                _pTextService->SetInputMode(response.chineseMode);
            }
        }
        return TRUE;

    case ResponseType::UpdateComposition:
        OutputDebugStringW(L"[WindInput] Received UpdateComposition from service\n");
        _isComposing = TRUE;
        _hasCandidates = TRUE;
        _pTextService->UpdateComposition(response.composition, response.caretPos);
        return TRUE;

    case ResponseType::ClearComposition:
        OutputDebugStringW(L"[WindInput] Received ClearComposition from service\n");
        _isComposing = FALSE;
        _hasCandidates = FALSE;
        _pTextService->EndComposition();
        return TRUE;

    case ResponseType::ModeChanged:
        OutputDebugStringW(L"[WindInput] Received ModeChanged from service\n");
        _isComposing = FALSE;
        _hasCandidates = FALSE;
        _pTextService->EndComposition();
        _pTextService->SetInputMode(response.chineseMode);
        return TRUE;

    case ResponseType::StatusUpdate:
        {
            OutputDebugStringW(L"[WindInput] Received StatusUpdate from service\n");

            // Update input mode
            _pTextService->SetInputMode(response.IsChineseMode());

            // Update hotkey whitelist
            CHotkeyManager* pHotkeyMgr = _pTextService->GetHotkeyManager();
            if (pHotkeyMgr != nullptr && response.HasHotkeys())
            {
                pHotkeyMgr->UpdateHotkeys(response.keyDownHotkeys, response.keyUpHotkeys);
            }
        }
        return TRUE;

    case ResponseType::Consumed:
        // Key was consumed by a hotkey
        OutputDebugStringW(L"[WindInput] Key consumed by hotkey\n");
        return TRUE;

    default:
        OutputDebugStringW(L"[WindInput] Unknown response type from service\n");
        return TRUE;
    }

    return TRUE; // Default: key was handled
}

// Check if the current context is read-only
BOOL CKeyEventSink::_IsContextReadOnly(ITfContext* pContext)
{
    if (!pContext)
    {
        return TRUE;
    }

    TF_STATUS tfStatus = {};
    HRESULT hr = pContext->GetStatus(&tfStatus);

    if (SUCCEEDED(hr))
    {
        if (tfStatus.dwDynamicFlags & TF_SD_READONLY)
        {
            return TRUE;
        }

        if (tfStatus.dwDynamicFlags & TF_SD_LOADING)
        {
            return TRUE;
        }
    }

    return FALSE;
}
