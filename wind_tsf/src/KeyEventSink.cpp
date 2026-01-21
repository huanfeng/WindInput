#include "KeyEventSink.h"
#include "TextService.h"
#include "IPCClient.h"
#include <cctype>

CKeyEventSink::CKeyEventSink(CTextService* pTextService)
    : _refCount(1)
    , _pTextService(pTextService)
    , _dwKeySinkCookie(TF_INVALID_COOKIE)
    , _isComposing(FALSE)
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
    *pfEaten = _IsKeyWeShouldHandle(wParam);
    return S_OK;
}

STDAPI CKeyEventSink::OnKeyDown(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten)
{
    *pfEaten = FALSE;

    if (!_IsKeyWeShouldHandle(wParam))
    {
        return S_OK;
    }

    WCHAR debug[128];
    wsprintfW(debug, L"[WindInput] OnKeyDown: wParam=%d (0x%X)\n", (int)wParam, (int)wParam);
    OutputDebugStringW(debug);

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
    *pfEaten = FALSE;
    return S_OK;
}

STDAPI CKeyEventSink::OnKeyUp(ITfContext* pContext, WPARAM wParam, LPARAM lParam, BOOL* pfEaten)
{
    *pfEaten = FALSE;
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
    // Get current modifier state
    int modifiers = _GetModifierState();

    // Shift key for Chinese/English toggle
    // Only handle if no other modifier keys are pressed
    if (wParam == VK_SHIFT)
    {
        // If Ctrl or Alt is also pressed, this is a system shortcut (e.g., Shift+Alt)
        // Let the system handle it
        if (modifiers & (KEY_MOD_CTRL | KEY_MOD_ALT))
        {
            OutputDebugStringW(L"[WindInput] Shift with Ctrl/Alt, passing to system\n");
            return FALSE;
        }
        return TRUE;
    }

    // If Ctrl or Alt is pressed with any key, don't intercept
    // This allows Ctrl+C, Ctrl+V, Alt+Tab, etc. to work
    if (modifiers & (KEY_MOD_CTRL | KEY_MOD_ALT))
    {
        return FALSE;
    }

    // Always handle when composing
    if (_isComposing)
    {
        // Handle letters, numbers, backspace, enter, escape, space
        if ((wParam >= 'A' && wParam <= 'Z') ||
            (wParam >= '1' && wParam <= '9') ||
            wParam == VK_BACK ||
            wParam == VK_RETURN ||
            wParam == VK_ESCAPE ||
            wParam == VK_SPACE)
        {
            return TRUE;
        }
    }
    else
    {
        // When not composing, only handle letters to start composition
        if (wParam >= 'A' && wParam <= 'Z')
        {
            return TRUE;
        }
    }

    return FALSE;
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

    if (wParam >= 'A' && wParam <= 'Z')
    {
        // Letter key - convert to lowercase
        key = (wchar_t)towlower((wint_t)wParam);

        // If starting composition, update caret position first
        if (!_isComposing)
        {
            _pTextService->SendCaretPositionUpdate();
        }
        _isComposing = TRUE;
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
    else
    {
        return FALSE;
    }

    // Send key event to Go Service with actual modifier state
    int modifiers = _GetModifierState();
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
        OutputDebugStringW(L"[WindInput] Received ACK from service\n");
        break;

    case ResponseType::InsertText:
        {
            OutputDebugStringW(L"[WindInput] Received InsertText from service\n");
            _isComposing = FALSE;

            // Insert text into application
            if (!response.text.empty())
            {
                _pTextService->InsertText(response.text);
            }
        }
        break;

    case ResponseType::UpdateComposition:
        OutputDebugStringW(L"[WindInput] Received UpdateComposition from service\n");
        // TODO: Update composition text display
        break;

    case ResponseType::ClearComposition:
        OutputDebugStringW(L"[WindInput] Received ClearComposition from service\n");
        _isComposing = FALSE;
        break;

    case ResponseType::ModeChanged:
        OutputDebugStringW(L"[WindInput] Received ModeChanged from service\n");
        // Update local mode state and language bar icon
        _pTextService->SetInputMode(response.chineseMode);
        break;

    default:
        OutputDebugStringW(L"[WindInput] Unknown response type from service\n");
        break;
    }
}
