#include "KeyEventSink.h"
#include "TextService.h"
#include "IPCClient.h"
#include "HotkeyManager.h"
#include "BinaryProtocol.h"
#include <cctype>
#include <cstdio>  // for swprintf

CKeyEventSink::CKeyEventSink(CTextService* pTextService)
    : _refCount(1)
    , _pTextService(pTextService)
    , _dwKeySinkCookie(TF_INVALID_COOKIE)
    , _isComposing(FALSE)
    , _hasCandidates(FALSE)
    , _pendingKeyUpKey(0)
    , _pendingKeyUpModifiers(0)
    , _pendingKeyDownTime(0)
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

    // Trace: Log ALL key presses (very high frequency)
    WIND_LOG_TRACE_FMT(L"OnTestKeyDown: wParam=0x%02X\n", (uint32_t)wParam);

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
        WIND_LOG_DEBUG_FMT(L"KeyDown hotkey matched: vk=0x%02X, hash=0x%08X\n",
                     (uint32_t)wParam, normalizedKeyHash);
        *pfEaten = TRUE;
        return S_OK;
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

    if (isToggleModeKey)
    {
        *pfEaten = TRUE;
        return S_OK;
    }

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

    if (hasInputSession || isChineseMode)
    {
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
                return S_OK;
            }
        }
        else if (keyType != HotkeyType::None)
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

    // Update modifier state machine for this KeyDown event
    _UpdateModsOnKeyDown(wParam);

    // Check barrier timeout
    _CheckBarrierTimeout();

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

    // Check if context is read-only
    if (_IsContextReadOnly(pContext))
    {
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

    if (hasInputSession || isChineseMode)
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

    // SYNC: Wait for response and handle it directly
    // This is simpler and matches Weasel's architecture
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

    // Update modifier state machine for this KeyUp event
    _UpdateModsOnKeyUp(wParam);

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
    if (pIPCClient->NeedsStateSync() && pIPCClient->IsConnected())
    {
        _pTextService->_DoFullStateSync();
    }

    // Get scan code from virtual key (optional, set to 0 if not needed)
    uint32_t scanCode = MapVirtualKeyW(keyCode, MAPVK_VK_TO_VSC);

    // Get toggles and event sequence
    uint8_t toggles = _GetTogglesSnapshot();
    uint16_t eventSeq = _GetNextEventSeq();

    // IMPORTANT: Always use the passed-in modifiers from CHotkeyManager::GetCurrentModifiers()
    // which calls GetAsyncKeyState(). The _modsState state machine can get out of sync
    // when we pass keys through to the system (e.g., Ctrl+S for save).
    // Using stale _modsState causes all subsequent keys to appear as having Ctrl held.

    BOOL result = pIPCClient->SendKeyEvent(keyCode, scanCode, modifiers, eventType, toggles, eventSeq);

    WIND_LOG_DEBUG_FMT(L"_SendKeyToService: vk=0x%02X, mods=0x%04X, elapsed=%dms\n",
                 keyCode, modifiers, GetTickCount() - startTime);

    return result;
}

BOOL CKeyEventSink::_HandleServiceResponse()
{
    LARGE_INTEGER startTime, midTime, endTime, freq;
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
            LARGE_INTEGER ctStart, ctMid1, ctMid2, ctEnd;
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
                // No new composition, just insert text normally
                _pTextService->EndComposition();
                QueryPerformanceCounter(&ctMid1);

                if (!response.text.empty())
                {
                    _pTextService->InsertText(response.text);
                }
                QueryPerformanceCounter(&ctMid2);

                _isComposing = FALSE;
                _hasCandidates = FALSE;

                // Log detailed timing (use integer ms to avoid wsprintfW %f issue)
                int endCompMs = (int)((ctMid1.QuadPart - ctStart.QuadPart) * 1000 / freq.QuadPart);
                int insertMs = (int)((ctMid2.QuadPart - ctMid1.QuadPart) * 1000 / freq.QuadPart);
                WIND_LOG_TRACE_FMT(L"CommitText: EndComposition=%dms, InsertText=%dms\n", endCompMs, insertMs);
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

    // Commit the text
    if (!text.empty())
    {
        _pTextService->InsertText(text);
    }

    // Handle new composition
    if (!newComp.empty())
    {
        _pTextService->UpdateComposition(newComp, (int)newComp.length());
        _isComposing = TRUE;
    }
    else
    {
        _pTextService->EndComposition();
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

