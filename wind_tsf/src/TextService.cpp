#include "TextService.h"
#include "KeyEventSink.h"
#include "IPCClient.h"
#include "LangBarItemButton.h"
#include "CaretEditSession.h"

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

    // Notify Go service that IME is being deactivated (before disconnecting)
    // This allows the service to hide the toolbar immediately
    if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
    {
        OutputDebugStringW(L"[WindInput] Sending ime_deactivated to service\n");
        if (_pIPCClient->SendIMEDeactivated())
        {
            // Receive response to complete the protocol
            ServiceResponse response;
            _pIPCClient->ReceiveResponse(response);
        }
    }

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

    // If gaining focus (pDocMgrFocus is not null)
    if (pDocMgrFocus != nullptr)
    {
        OutputDebugStringW(L"[WindInput] Focus gained\n");

        // Force refresh the language bar button to ensure it's visible
        if (_pLangBarItemButton != nullptr)
        {
            _pLangBarItemButton->ForceRefresh();
        }

        // Get caret position for toolbar placement
        LONG caretX = 0, caretY = 0, caretHeight = 0;
        GetCaretPosition(&caretX, &caretY, &caretHeight);

        // Send focus_gained to service (for toolbar display)
        // Will try to connect if not already connected
        if (_pIPCClient != nullptr)
        {
            if (_pIPCClient->SendFocusGained(caretX, caretY, caretHeight))
            {
                // Receive response - may be StatusUpdate with current state
                ServiceResponse response;
                if (_pIPCClient->ReceiveResponse(response))
                {
                    // If we got a status update, sync our state with the service
                    if (response.type == ResponseType::StatusUpdate)
                    {
                        _bChineseMode = response.chineseMode;
                        OutputDebugStringW(L"[WindInput] Synced state from focus_gained response\n");

                        // Update language bar button
                        if (_pLangBarItemButton != nullptr)
                        {
                            _pLangBarItemButton->UpdateLangBarButton(_bChineseMode);
                        }
                    }
                }
            }
        }
    }

    // If losing focus (pDocMgrFocus is null)
    if (pDocMgrFocus == nullptr)
    {
        OutputDebugStringW(L"[WindInput] Focus lost, notifying service\n");

        // Send focus_lost to service
        if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
        {
            _pIPCClient->SendFocusLost();

            // Receive response to keep protocol in sync
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

// Static variables to track last known good caret position
static LONG s_lastCaretX = 0;
static LONG s_lastCaretY = 0;
static LONG s_lastCaretHeight = 20;
static BOOL s_hasLastCaretPos = FALSE;

// Get caret position using TSF APIs (for browsers and modern apps)
BOOL CTextService::GetCaretPositionFromTSF(LONG* px, LONG* py, LONG* pHeight)
{
    if (_pThreadMgr == nullptr)
    {
        return FALSE;
    }

    // Get current document manager
    ITfDocumentMgr* pDocMgr = nullptr;
    HRESULT hr = _pThreadMgr->GetFocus(&pDocMgr);
    if (FAILED(hr) || pDocMgr == nullptr)
    {
        return FALSE;
    }

    // Get top context
    ITfContext* pContext = nullptr;
    hr = pDocMgr->GetTop(&pContext);
    pDocMgr->Release();

    if (FAILED(hr) || pContext == nullptr)
    {
        return FALSE;
    }

    // Use EditSession to get caret position
    RECT rc = {};
    BOOL result = CCaretEditSession::GetCaretRect(pContext, &rc);
    pContext->Release();

    if (result)
    {
        // rc contains screen coordinates
        *px = rc.left;
        *py = rc.bottom;  // Position below the caret
        *pHeight = rc.bottom - rc.top;

        if (*pHeight <= 0)
            *pHeight = 20;

        // Save as last known good position
        s_lastCaretX = *px;
        s_lastCaretY = *py;
        s_lastCaretHeight = *pHeight;
        s_hasLastCaretPos = TRUE;

        OutputDebugStringW(L"[WindInput] GetCaretPositionFromTSF: Success\n");
        return TRUE;
    }

    return FALSE;
}

BOOL CTextService::GetCaretPosition(LONG* px, LONG* py, LONG* pHeight)
{
    // Method 1 (NEW): Try TSF APIs first - this is the most reliable for browsers
    // ITfContextView::GetTextExt provides accurate caret position in Chrome, Edge, etc.
    if (GetCaretPositionFromTSF(px, py, pHeight))
    {
        return TRUE;
    }

    // Method 2: Try to get caret position from the GUI thread info
    // This works well for traditional Win32 applications
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

            // Validate position (not at origin, which usually means failure)
            if (caretPos.x > 0 || caretPos.y > 0)
            {
                *px = caretPos.x;
                *py = caretPos.y;
                *pHeight = guiInfo.rcCaret.bottom - guiInfo.rcCaret.top;

                if (*pHeight <= 0)
                    *pHeight = 20;  // Default caret height

                // Save as last known good position
                s_lastCaretX = *px;
                s_lastCaretY = *py;
                s_lastCaretHeight = *pHeight;
                s_hasLastCaretPos = TRUE;

                return TRUE;
            }
        }
    }

    // Method 3: Fallback to GetCaretPos
    POINT pt;
    if (GetCaretPos(&pt))
    {
        // Get the foreground window to convert coordinates
        HWND hwnd = GetForegroundWindow();
        if (hwnd != nullptr)
        {
            ClientToScreen(hwnd, &pt);

            // Validate position
            if (pt.x > 0 || pt.y > 0)
            {
                *px = pt.x;
                *py = pt.y + 20;  // Estimate caret height
                *pHeight = 20;

                // Save as last known good position
                s_lastCaretX = *px;
                s_lastCaretY = *py;
                s_lastCaretHeight = *pHeight;
                s_hasLastCaretPos = TRUE;

                return TRUE;
            }
        }
    }

    // Method 4: For browsers/WebView2, try to get focus window position
    // Browsers often don't expose caret position properly, so we use the focus window
    HWND hwndFocus = GetForegroundWindow();
    if (hwndFocus != nullptr)
    {
        RECT rc;
        if (GetWindowRect(hwndFocus, &rc))
        {
            // If we have a last known position within this window, use it
            if (s_hasLastCaretPos &&
                s_lastCaretX >= rc.left && s_lastCaretX <= rc.right &&
                s_lastCaretY >= rc.top && s_lastCaretY <= rc.bottom)
            {
                *px = s_lastCaretX;
                *py = s_lastCaretY;
                *pHeight = s_lastCaretHeight;
                return TRUE;
            }

            // Otherwise, position near the center-left of the window
            // This is a fallback for browsers that don't report caret position
            *px = rc.left + 100;  // Some offset from left edge
            *py = rc.top + (rc.bottom - rc.top) / 2;  // Vertical center
            *pHeight = 20;

            OutputDebugStringW(L"[WindInput] GetCaretPosition: Using window position fallback\n");
            return TRUE;
        }
    }

    // Method 5: Use last known good position if available
    if (s_hasLastCaretPos)
    {
        *px = s_lastCaretX;
        *py = s_lastCaretY;
        *pHeight = s_lastCaretHeight;
        OutputDebugStringW(L"[WindInput] GetCaretPosition: Using last known position\n");
        return TRUE;
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

void CTextService::SendMenuCommand(const char* command)
{
    OutputDebugStringW(L"[WindInput] SendMenuCommand called\n");

    if (_pIPCClient == nullptr)
    {
        OutputDebugStringW(L"[WindInput] SendMenuCommand: IPC client is null\n");
        return;
    }

    if (!_pIPCClient->IsConnected() && !_pIPCClient->Connect())
    {
        OutputDebugStringW(L"[WindInput] SendMenuCommand: Failed to connect\n");
        return;
    }

    if (_pIPCClient->SendMenuCommand(command))
    {
        ServiceResponse response;
        if (_pIPCClient->ReceiveResponse(response))
        {
            // Handle response based on type
            if (response.type == ResponseType::ModeChanged)
            {
                _bChineseMode = response.chineseMode;
                if (_pLangBarItemButton != nullptr)
                {
                    _pLangBarItemButton->UpdateLangBarButton(_bChineseMode);
                }
            }
            else if (response.type == ResponseType::StatusUpdate)
            {
                // Update all status fields
                _bChineseMode = response.chineseMode;
                if (_pLangBarItemButton != nullptr)
                {
                    _pLangBarItemButton->UpdateFullStatus(
                        response.chineseMode,
                        response.fullWidth,
                        response.chinesePunct,
                        response.toolbarVisible,
                        (GetKeyState(VK_CAPITAL) & 0x0001) != 0  // Get current Caps Lock state
                    );
                }
            }
        }
    }
}

void CTextService::UpdateFullStatus(BOOL bChineseMode, BOOL bFullWidth, BOOL bChinesePunct, BOOL bToolbarVisible, BOOL bCapsLock)
{
    _bChineseMode = bChineseMode;

    if (_pLangBarItemButton != nullptr)
    {
        _pLangBarItemButton->UpdateFullStatus(bChineseMode, bFullWidth, bChinesePunct, bToolbarVisible, bCapsLock);
    }

    WCHAR debug[256];
    wsprintfW(debug, L"[WindInput] UpdateFullStatus: mode=%d, width=%d, punct=%d, toolbar=%d, caps=%d\n",
              bChineseMode, bFullWidth, bChinesePunct, bToolbarVisible, bCapsLock);
    OutputDebugStringW(debug);
}
