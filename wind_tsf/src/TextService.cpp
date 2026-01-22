#include "TextService.h"
#include "KeyEventSink.h"
#include "IPCClient.h"
#include "LangBarItemButton.h"

CTextService::CTextService()
    : _refCount(1)
    , _pThreadMgr(nullptr)
    , _tfClientId(TF_CLIENTID_NULL)
    , _dwThreadMgrEventSinkCookie(TF_INVALID_COOKIE)
    , _pKeyEventSink(nullptr)
    , _pIPCClient(nullptr)
    , _pLangBarItemButton(nullptr)
    , _bChineseMode(TRUE)
{
    DllAddRef();
}

CTextService::~CTextService()
{
    DllRelease();
}

STDAPI CTextService::QueryInterface(REFIID riid, void** ppvObj)
{
    if (ppvObj == nullptr)
        return E_INVALIDARG;

    *ppvObj = nullptr;

    if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfTextInputProcessor))
    {
        *ppvObj = (ITfTextInputProcessor*)this;
    }
    else if (IsEqualIID(riid, IID_ITfThreadMgrEventSink))
    {
        *ppvObj = (ITfThreadMgrEventSink*)this;
    }

    if (*ppvObj)
    {
        AddRef();
        return S_OK;
    }

    return E_NOINTERFACE;
}

STDAPI_(ULONG) CTextService::AddRef()
{
    return InterlockedIncrement(&_refCount);
}

STDAPI_(ULONG) CTextService::Release()
{
    LONG cr = InterlockedDecrement(&_refCount);

    if (cr == 0)
    {
        delete this;
    }

    return cr;
}

STDAPI CTextService::Activate(ITfThreadMgr* pThreadMgr, TfClientId tfClientId)
{
    OutputDebugStringW(L"[WindInput] TextService::Activate called\n");

    _pThreadMgr = pThreadMgr;
    _pThreadMgr->AddRef();

    _tfClientId = tfClientId;

    // Initialize thread manager event sink
    if (!_InitThreadMgrEventSink())
    {
        OutputDebugStringW(L"[WindInput] _InitThreadMgrEventSink failed\n");
        Deactivate();
        return E_FAIL;
    }
    OutputDebugStringW(L"[WindInput] ThreadMgrEventSink initialized\n");

    // Initialize IPC client
    if (!_InitIPCClient())
    {
        OutputDebugStringW(L"[WindInput] _InitIPCClient failed\n");
        Deactivate();
        return E_FAIL;
    }
    OutputDebugStringW(L"[WindInput] IPCClient initialized\n");

    // Initialize key event sink
    if (!_InitKeyEventSink())
    {
        OutputDebugStringW(L"[WindInput] _InitKeyEventSink failed\n");
        Deactivate();
        return E_FAIL;
    }
    OutputDebugStringW(L"[WindInput] KeyEventSink initialized\n");

    // Initialize language bar button
    if (!_InitLangBarButton())
    {
        OutputDebugStringW(L"[WindInput] _InitLangBarButton failed (non-fatal)\n");
        // Not fatal, continue without language bar button
    }
    else
    {
        OutputDebugStringW(L"[WindInput] LangBarButton initialized\n");
    }

    OutputDebugStringW(L"[WindInput] TextService::Activate completed successfully\n");
    return S_OK;
}

STDAPI CTextService::Deactivate()
{
    OutputDebugStringW(L"[WindInput] TextService::Deactivate called\n");

    // Release language bar button
    _UninitLangBarButton();

    // Release key event sink
    _UninitKeyEventSink();

    // Release IPC client
    _UninitIPCClient();

    // Release thread manager event sink
    _UninitThreadMgrEventSink();

    // Release thread manager
    SafeRelease(_pThreadMgr);

    _tfClientId = TF_CLIENTID_NULL;

    OutputDebugStringW(L"[WindInput] TextService::Deactivate completed\n");
    return S_OK;
}

BOOL CTextService::_InitThreadMgrEventSink()
{
    ITfSource* pSource = nullptr;
    HRESULT hr = _pThreadMgr->QueryInterface(IID_ITfSource, (void**)&pSource);

    if (SUCCEEDED(hr))
    {
        hr = pSource->AdviseSink(IID_ITfThreadMgrEventSink,
                                 (ITfThreadMgrEventSink*)this,
                                 &_dwThreadMgrEventSinkCookie);
        pSource->Release();
    }

    return SUCCEEDED(hr);
}

void CTextService::_UninitThreadMgrEventSink()
{
    if (_dwThreadMgrEventSinkCookie != TF_INVALID_COOKIE)
    {
        ITfSource* pSource = nullptr;
        if (SUCCEEDED(_pThreadMgr->QueryInterface(IID_ITfSource, (void**)&pSource)))
        {
            pSource->UnadviseSink(_dwThreadMgrEventSinkCookie);
            pSource->Release();
        }

        _dwThreadMgrEventSinkCookie = TF_INVALID_COOKIE;
    }
}

STDAPI CTextService::OnInitDocumentMgr(ITfDocumentMgr* pDocMgr)
{
    return S_OK;
}

STDAPI CTextService::OnUninitDocumentMgr(ITfDocumentMgr* pDocMgr)
{
    return S_OK;
}

STDAPI CTextService::OnSetFocus(ITfDocumentMgr* pDocMgrFocus, ITfDocumentMgr* pDocMgrPrevFocus)
{
    OutputDebugStringW(L"[WindInput] OnSetFocus called\n");

    // If losing focus (pDocMgrFocus is null or different), notify service
    if (pDocMgrFocus == nullptr || pDocMgrFocus != pDocMgrPrevFocus)
    {
        OutputDebugStringW(L"[WindInput] Focus changed, notifying service\n");

        // Send focus_lost to service
        if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
        {
            _pIPCClient->SendFocusLost();

            // Also receive response to keep protocol in sync
            ServiceResponse response;
            _pIPCClient->ReceiveResponse(response);
        }

        // Reset composing state
        if (_pKeyEventSink != nullptr)
        {
            _pKeyEventSink->ResetComposingState();
        }
    }

    return S_OK;
}

STDAPI CTextService::OnPushContext(ITfContext* pContext)
{
    return S_OK;
}

STDAPI CTextService::OnPopContext(ITfContext* pContext)
{
    return S_OK;
}

BOOL CTextService::_InitKeyEventSink()
{
    _pKeyEventSink = new CKeyEventSink(this);
    if (_pKeyEventSink == nullptr)
        return FALSE;

    return _pKeyEventSink->Initialize();
}

void CTextService::_UninitKeyEventSink()
{
    if (_pKeyEventSink != nullptr)
    {
        _pKeyEventSink->Uninitialize();
        _pKeyEventSink->Release();
        _pKeyEventSink = nullptr;
    }
}

BOOL CTextService::_InitIPCClient()
{
    _pIPCClient = new CIPCClient();
    if (_pIPCClient == nullptr)
        return FALSE;

    // Try to connect to Go Service (failure is OK, will retry later)
    if (!_pIPCClient->Connect())
    {
        OutputDebugStringW(L"[WindInput] Failed to connect to Go Service, will retry later\n");
    }

    return TRUE;
}

void CTextService::_UninitIPCClient()
{
    if (_pIPCClient != nullptr)
    {
        _pIPCClient->Disconnect();
        delete _pIPCClient;
        _pIPCClient = nullptr;
    }
}

BOOL CTextService::InsertText(const std::wstring& text)
{
    if (_pThreadMgr == nullptr)
    {
        OutputDebugStringW(L"[WindInput] ThreadMgr is null\n");
        return FALSE;
    }

    // Get current document manager
    ITfDocumentMgr* pDocMgr = nullptr;
    HRESULT hr = _pThreadMgr->GetFocus(&pDocMgr);
    if (FAILED(hr) || pDocMgr == nullptr)
    {
        OutputDebugStringW(L"[WindInput] Failed to get focus document manager\n");
        return FALSE;
    }

    // Get top context
    ITfContext* pContext = nullptr;
    hr = pDocMgr->GetTop(&pContext);
    pDocMgr->Release();

    if (FAILED(hr) || pContext == nullptr)
    {
        OutputDebugStringW(L"[WindInput] Failed to get top context\n");
        return FALSE;
    }

    // Get edit session
    ITfEditSession* pEditSession = nullptr;

    // For simplicity, use keyboard simulation to insert text
    // This is a workaround - proper implementation would use ITfInsertAtSelection
    pContext->Release();

    // Use SendInput to simulate keyboard input
    for (wchar_t ch : text)
    {
        INPUT input[2] = {};

        // Key down
        input[0].type = INPUT_KEYBOARD;
        input[0].ki.wVk = 0;
        input[0].ki.wScan = ch;
        input[0].ki.dwFlags = KEYEVENTF_UNICODE;

        // Key up
        input[1].type = INPUT_KEYBOARD;
        input[1].ki.wVk = 0;
        input[1].ki.wScan = ch;
        input[1].ki.dwFlags = KEYEVENTF_UNICODE | KEYEVENTF_KEYUP;

        SendInput(2, input, sizeof(INPUT));
    }

    return TRUE;
}

BOOL CTextService::GetCaretPosition(LONG* px, LONG* py, LONG* pHeight)
{
    // Method 1: Try to get caret position from the GUI thread info
    // This is more reliable than trying to use TSF's GetSelection which requires an edit cookie
    GUITHREADINFO guiInfo = { sizeof(GUITHREADINFO) };

    if (GetGUIThreadInfo(0, &guiInfo))
    {
        // Check if there's an active caret
        if (guiInfo.hwndCaret != nullptr)
        {
            POINT caretPos;
            caretPos.x = guiInfo.rcCaret.left;
            caretPos.y = guiInfo.rcCaret.bottom;

            // Convert from client coordinates to screen coordinates
            ClientToScreen(guiInfo.hwndCaret, &caretPos);

            *px = caretPos.x;
            *py = caretPos.y;
            *pHeight = guiInfo.rcCaret.bottom - guiInfo.rcCaret.top;

            if (*pHeight <= 0)
                *pHeight = 20;  // Default caret height

            return TRUE;
        }
    }

    // Method 2: Fallback to GetCaretPos
    POINT pt;
    if (GetCaretPos(&pt))
    {
        // Get the foreground window to convert coordinates
        HWND hwnd = GetForegroundWindow();
        if (hwnd != nullptr)
        {
            ClientToScreen(hwnd, &pt);
            *px = pt.x;
            *py = pt.y + 20;  // Estimate caret height
            *pHeight = 20;

            return TRUE;
        }
    }

    OutputDebugStringW(L"[WindInput] GetCaretPosition: Failed to get caret position\n");
    return FALSE;
}

void CTextService::SendCaretPositionUpdate()
{
    LONG x, y, height;
    if (GetCaretPosition(&x, &y, &height))
    {
        if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
        {
            _pIPCClient->SendCaretUpdate((int)x, (int)y, (int)height);

            // Receive response to keep protocol in sync
            ServiceResponse response;
            _pIPCClient->ReceiveResponse(response);
        }
    }
}

BOOL CTextService::_InitLangBarButton()
{
    _pLangBarItemButton = new CLangBarItemButton(this);
    if (_pLangBarItemButton == nullptr)
        return FALSE;

    if (!_pLangBarItemButton->Initialize())
    {
        _pLangBarItemButton->Release();
        _pLangBarItemButton = nullptr;
        return FALSE;
    }

    return TRUE;
}

void CTextService::_UninitLangBarButton()
{
    if (_pLangBarItemButton != nullptr)
    {
        _pLangBarItemButton->Uninitialize();
        _pLangBarItemButton->Release();
        _pLangBarItemButton = nullptr;
    }
}

void CTextService::ToggleInputMode()
{
    OutputDebugStringW(L"[WindInput] ToggleInputMode called\n");

    // Send toggle_mode request to Go service and get new state
    if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
    {
        if (_pIPCClient->SendToggleMode())
        {
            ServiceResponse response;
            if (_pIPCClient->ReceiveResponse(response))
            {
                if (response.type == ResponseType::ModeChanged)
                {
                    _bChineseMode = response.chineseMode;
                    OutputDebugStringW(_bChineseMode ?
                        L"[WindInput] Mode synced from service: Chinese\n" :
                        L"[WindInput] Mode synced from service: English\n");
                }
                else
                {
                    // Fallback: toggle locally if unexpected response
                    _bChineseMode = !_bChineseMode;
                    OutputDebugStringW(L"[WindInput] Unexpected response, toggling locally\n");
                }
            }
            else
            {
                // Fallback: toggle locally if receive failed
                _bChineseMode = !_bChineseMode;
                OutputDebugStringW(L"[WindInput] Failed to receive response, toggling locally\n");
            }
        }
        else
        {
            // Fallback: toggle locally if send failed
            _bChineseMode = !_bChineseMode;
            OutputDebugStringW(L"[WindInput] Failed to send toggle_mode, toggling locally\n");
        }
    }
    else
    {
        // No IPC connection, toggle locally
        _bChineseMode = !_bChineseMode;
        OutputDebugStringW(L"[WindInput] No IPC connection, toggling locally\n");
    }

    OutputDebugStringW(_bChineseMode ?
        L"[WindInput] Switched to Chinese mode\n" :
        L"[WindInput] Switched to English mode\n");

    // Update language bar button
    if (_pLangBarItemButton != nullptr)
    {
        _pLangBarItemButton->UpdateLangBarButton(_bChineseMode);
    }
}

void CTextService::SetInputMode(BOOL bChineseMode)
{
    // Set mode directly from service response (no IPC call)
    _bChineseMode = bChineseMode;

    OutputDebugStringW(_bChineseMode ?
        L"[WindInput] Mode set to Chinese (from service)\n" :
        L"[WindInput] Mode set to English (from service)\n");

    // Update language bar button
    if (_pLangBarItemButton != nullptr)
    {
        _pLangBarItemButton->UpdateLangBarButton(_bChineseMode);
    }
}

void CTextService::UpdateCapsLockState(BOOL bCapsLock)
{
    if (_pLangBarItemButton != nullptr)
    {
        _pLangBarItemButton->UpdateCapsLockState(bCapsLock);
    }
}
