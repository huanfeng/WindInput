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

    // First check if the context is read-only (browser non-editable area)
    if (_IsContextReadOnly(pContext))
    {
        return S_OK;
    }

    // Get current modifiers and calculate key hash
    uint32_t modifiers = CHotkeyManager::GetCurrentModifiers();
    uint32_t keyHash = CHotkeyManager::CalcKeyHash(modifiers, (uint32_t)wParam);

    CHotkeyManager* pHotkeyMgr = _pTextService->GetHotkeyManager();

    // Check if this is a KeyDown hotkey from the whitelist
    if (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyDownHotkey(keyHash))
    {
        *pfEaten = TRUE;
        return S_OK;
    }

    // Check for KeyUp triggered keys (toggle mode keys) - we still need to intercept KeyDown
    if (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyUpHotkey(keyHash))
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

    CHotkeyManager* pHotkeyMgr = _pTextService->GetHotkeyManager();

    // Check if this is a KeyUp triggered key (toggle mode keys like Shift, Ctrl, CapsLock)
    if (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyUpHotkey(keyHash))
    {
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

        // Mark key as pending for KeyUp toggle
        _pendingKeyUpKey = (uint32_t)wParam;
        _pendingKeyUpModifiers = modifiers;

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
    BOOL isKeyDownHotkey = (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyDownHotkey(keyHash));

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

    // Handle response from service
    _HandleServiceResponse();

    *pfEaten = TRUE;
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
            _pendingKeyUpKey = 0;
            _pendingKeyUpModifiers = 0;

            // Send KeyUp event to Go Service for toggle processing
            if (pendingKey != VK_CAPITAL)
            {
                // For Shift/Ctrl, send KeyUp event
                if (_SendKeyToService(pendingKey, 0, KEY_EVENT_UP))
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

        // Check if CapsLock is configured as toggle key
        if (pHotkeyMgr != nullptr && pHotkeyMgr->IsKeyUpHotkey(keyHash))
        {
            // Send KeyUp event to Go Service
            if (_SendKeyToService(VK_CAPITAL, KEYMOD_CAPSLOCK, KEY_EVENT_UP))
            {
                _HandleServiceResponse();
            }
        }

        // Get current Caps Lock state and update language bar
        BOOL capsLockOn = (GetKeyState(VK_CAPITAL) & 0x0001) != 0;
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
        // ACK is common, no action needed
        break;

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
        break;

    case ResponseType::UpdateComposition:
        OutputDebugStringW(L"[WindInput] Received UpdateComposition from service\n");
        _isComposing = TRUE;
        _hasCandidates = TRUE;
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
        _isComposing = FALSE;
        _hasCandidates = FALSE;
        _pTextService->EndComposition();
        _pTextService->SetInputMode(response.chineseMode);
        break;

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
        break;

    case ResponseType::Consumed:
        // Key was consumed by a hotkey
        OutputDebugStringW(L"[WindInput] Key consumed by hotkey\n");
        break;

    default:
        OutputDebugStringW(L"[WindInput] Unknown response type from service\n");
        break;
    }
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
