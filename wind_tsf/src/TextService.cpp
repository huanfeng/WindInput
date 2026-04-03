#include "TextService.h"
#include "KeyEventSink.h"
#include "IPCClient.h"
#include "LangBarItemButton.h"
#include "CaretEditSession.h"
#include "DisplayAttributeInfo.h"
#include "HotkeyManager.h"
#include "HostWindow.h"
#include <vector>

// EditSession for ending composition
// NOTE: This class takes ownership of the composition pointer passed to it.
// The composition will be ended and released when DoEditSession is called,
// or in the destructor if the edit session request fails.
class CEndCompositionEditSession : public ITfEditSession
{
public:
    // pComposition ownership is transferred to this object
    CEndCompositionEditSession(CTextService* pTextService, ITfComposition* pComposition)
        : _refCount(1), _pTextService(pTextService), _pComposition(pComposition)
    {
        _pTextService->AddRef();
        // Note: pComposition ownership is transferred, no AddRef needed
    }

    ~CEndCompositionEditSession()
    {
        _pTextService->Release();
        // If DoEditSession was never called (request failed), release the composition
        if (_pComposition != nullptr)
        {
            WIND_LOG_DEBUG(L"~CEndCompositionEditSession: Releasing orphaned composition\n");
            _pComposition->Release();
            _pComposition = nullptr;
        }
    }

    // IUnknown
    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj)
    {
        if (ppvObj == nullptr) return E_INVALIDARG;
        *ppvObj = nullptr;
        if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfEditSession))
        {
            *ppvObj = (ITfEditSession*)this;
            AddRef();
            return S_OK;
        }
        return E_NOINTERFACE;
    }

    STDMETHODIMP_(ULONG) AddRef()
    {
        return InterlockedIncrement(&_refCount);
    }

    STDMETHODIMP_(ULONG) Release()
    {
        LONG cr = InterlockedDecrement(&_refCount);
        if (cr == 0) delete this;
        return cr;
    }

    // ITfEditSession
    STDMETHODIMP DoEditSession(TfEditCookie ec)
    {
        if (_pComposition != nullptr)
        {
            // Get the composition range and clear the text before ending
            // This prevents the composition text from being committed
            ITfRange* pRange = nullptr;
            if (SUCCEEDED(_pComposition->GetRange(&pRange)))
            {
                // Clear the composition text (set to empty string)
                pRange->SetText(ec, 0, L"", 0);
                pRange->Release();
            }

            _pComposition->EndComposition(ec);

            // Release the composition
            _pComposition->Release();
            _pComposition = nullptr;
            WIND_LOG_DEBUG(L"DoEditSession: Composition ended and released\n");
        }
        return S_OK;
    }

private:
    LONG _refCount;
    CTextService* _pTextService;
    ITfComposition* _pComposition;  // Owned composition pointer
};

// EditSession for updating composition
class CUpdateCompositionEditSession : public ITfEditSession
{
public:
    CUpdateCompositionEditSession(CTextService* pTextService, ITfContext* pContext, const std::wstring& text, int caretPos = -1)
        : _refCount(1), _pTextService(pTextService), _pContext(pContext), _text(text), _caretPos(caretPos)
    {
        _pTextService->AddRef();
        _pContext->AddRef();
    }

    ~CUpdateCompositionEditSession()
    {
        _pTextService->Release();
        _pContext->Release();
    }

    // IUnknown
    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj)
    {
        if (ppvObj == nullptr) return E_INVALIDARG;
        *ppvObj = nullptr;
        if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfEditSession))
        {
            *ppvObj = (ITfEditSession*)this;
            AddRef();
            return S_OK;
        }
        return E_NOINTERFACE;
    }

    STDMETHODIMP_(ULONG) AddRef()
    {
        return InterlockedIncrement(&_refCount);
    }

    STDMETHODIMP_(ULONG) Release()
    {
        LONG cr = InterlockedDecrement(&_refCount);
        if (cr == 0) delete this;
        return cr;
    }

    // ITfEditSession
    STDMETHODIMP DoEditSession(TfEditCookie ec)
    {
        HRESULT hr = S_OK;

        // 1. If no composition exists, start one
        if (_pTextService->_pComposition == nullptr)
        {
            // Get current selection (cursor position) to start composition there
            TF_SELECTION tfSelection;
            ULONG cFetched;
            if (FAILED(_pContext->GetSelection(ec, TF_DEFAULT_SELECTION, 1, &tfSelection, &cFetched)) || cFetched != 1)
            {
                return E_FAIL;
            }

            ITfContextComposition* pContextComp = nullptr;
            if (FAILED(_pContext->QueryInterface(IID_ITfContextComposition, (void**)&pContextComp)))
            {
                tfSelection.range->Release();
                return E_FAIL;
            }

            // Start composition
            hr = pContextComp->StartComposition(
                ec,
                tfSelection.range,
                (ITfCompositionSink*)_pTextService,
                &_pTextService->_pComposition);

            pContextComp->Release();
            tfSelection.range->Release();

            if (FAILED(hr) || _pTextService->_pComposition == nullptr)
            {
                WIND_LOG_ERROR(L"StartComposition failed\n");
                return E_FAIL;
            }
            WIND_LOG_DEBUG(L"StartComposition succeeded\n");
        }

        // 2. Get range from composition
        ITfRange* pRange = nullptr;
        if (FAILED(_pTextService->_pComposition->GetRange(&pRange)))
        {
            return E_FAIL;
        }

        // 3. Set text
        hr = pRange->SetText(ec, TF_ST_CORRECTION, _text.c_str(), (LONG)_text.length());

        if (SUCCEEDED(hr))
        {
            // 4. Apply display attribute to show underline
            _SetDisplayAttribute(ec, pRange);

            // 5. Position cursor within composition
            // If _caretPos is valid and less than text length, position cursor there;
            // otherwise collapse to end (default behavior).
            ITfRange* pRangeForSel = nullptr;
            if (SUCCEEDED(_pTextService->_pComposition->GetRange(&pRangeForSel)))
            {
                if (_caretPos >= 0 && _caretPos < (int)_text.length())
                {
                    // Move the range start to the caret position, then collapse to start
                    // This positions the cursor at the specified offset within the composition
                    LONG shifted = 0;
                    pRangeForSel->Collapse(ec, TF_ANCHOR_START);
                    pRangeForSel->ShiftEnd(ec, (LONG)_caretPos, &shifted, nullptr);
                    pRangeForSel->ShiftStart(ec, (LONG)_caretPos, &shifted, nullptr);
                }
                else
                {
                    // Default: cursor at end of composition
                    pRangeForSel->Collapse(ec, TF_ANCHOR_END);
                }

                TF_SELECTION sel = {};
                sel.range = pRangeForSel;
                sel.style.ase = TF_AE_NONE;
                sel.style.fInterimChar = FALSE;
                _pContext->SetSelection(ec, 1, &sel);

                pRangeForSel->Release();
            }
        }

        pRange->Release();

        // Cache caret position from within this valid edit session.
        // This is critical for WebView apps where a separate CCaretEditSession
        // with TF_INVALID_COOKIE would be rejected.
        if (SUCCEEDED(hr))
        {
            _CacheCaretPosition(ec);
        }

        return hr;
    }

private:
    int _caretPos;  // Cursor position within composition (-1 = at end)

    void _CacheCaretPosition(TfEditCookie ec)
    {
        ITfContextView* pContextView = nullptr;
        if (FAILED(_pContext->GetActiveView(&pContextView)) || pContextView == nullptr)
            return;

        // Get current caret position (selection)
        TF_SELECTION sel[1];
        ULONG fetched = 0;
        if (SUCCEEDED(_pContext->GetSelection(ec, TF_DEFAULT_SELECTION, 1, sel, &fetched)) && fetched > 0 && sel[0].range != nullptr)
        {
            RECT caretRect = {};
            BOOL clipped = FALSE;
            if (SUCCEEDED(pContextView->GetTextExt(ec, sel[0].range, &caretRect, &clipped)))
            {
                _pTextService->_cachedCaretRect = caretRect;
                _pTextService->_hasCachedCaretPos = TRUE;
            }
            sel[0].range->Release();
        }

        // Get composition start position
        if (_pTextService->_pComposition != nullptr)
        {
            ITfRange* pCompRange = nullptr;
            if (SUCCEEDED(_pTextService->_pComposition->GetRange(&pCompRange)) && pCompRange != nullptr)
            {
                ITfRange* pStartRange = nullptr;
                if (SUCCEEDED(pCompRange->Clone(&pStartRange)) && pStartRange != nullptr)
                {
                    pStartRange->Collapse(ec, TF_ANCHOR_START);
                    RECT compStartRect = {};
                    BOOL clipped = FALSE;
                    if (SUCCEEDED(pContextView->GetTextExt(ec, pStartRange, &compStartRect, &clipped)))
                    {
                        _pTextService->_cachedCompStartRect = compStartRect;
                        _pTextService->_hasCachedCompStartPos = TRUE;
                    }
                    pStartRange->Release();
                }
                pCompRange->Release();
            }
        }

        pContextView->Release();
    }

    void _SetDisplayAttribute(TfEditCookie ec, ITfRange* pRange)
    {
        // Get the display attribute atom from TextService
        TfGuidAtom gaDisplayAttr = _pTextService->GetDisplayAttributeInputAtom();
        if (gaDisplayAttr == TF_INVALID_GUIDATOM)
        {
            WIND_LOG_DEBUG(L"Display attribute not initialized\n");
            return;
        }

        // Get ITfProperty for display attribute
        ITfProperty* pDisplayAttrProp = nullptr;
        if (FAILED(_pContext->GetProperty(GUID_PROP_ATTRIBUTE, &pDisplayAttrProp)))
        {
            WIND_LOG_DEBUG(L"Failed to get GUID_PROP_ATTRIBUTE property\n");
            return;
        }

        // Set the display attribute on the composition range
        VARIANT var;
        var.vt = VT_I4;
        var.lVal = gaDisplayAttr;

        HRESULT hr = pDisplayAttrProp->SetValue(ec, pRange, &var);
        if (FAILED(hr))
        {
            WIND_LOG_DEBUG(L"Failed to set display attribute\n");
        }
        else
        {
            WIND_LOG_DEBUG(L"Display attribute set successfully\n");
        }

        pDisplayAttrProp->Release();
    }

private:
    LONG _refCount;
    CTextService* _pTextService;
    ITfContext* _pContext;
    std::wstring _text;
};

// EditSession for inserting text and starting new composition (for top code commit)
class CInsertAndComposeEditSession : public ITfEditSession
{
public:
    CInsertAndComposeEditSession(CTextService* pTextService, ITfContext* pContext,
                                  const std::wstring& insertText, const std::wstring& newComposition)
        : _refCount(1), _pTextService(pTextService), _pContext(pContext),
          _insertText(insertText), _newComposition(newComposition)
    {
        _pTextService->AddRef();
        _pContext->AddRef();
    }

    ~CInsertAndComposeEditSession()
    {
        _pTextService->Release();
        _pContext->Release();
    }

    // IUnknown
    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj)
    {
        if (ppvObj == nullptr) return E_INVALIDARG;
        *ppvObj = nullptr;
        if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfEditSession))
        {
            *ppvObj = (ITfEditSession*)this;
            AddRef();
            return S_OK;
        }
        return E_NOINTERFACE;
    }

    STDMETHODIMP_(ULONG) AddRef() { return InterlockedIncrement(&_refCount); }
    STDMETHODIMP_(ULONG) Release()
    {
        LONG cr = InterlockedDecrement(&_refCount);
        if (cr == 0) delete this;
        return cr;
    }

    // ITfEditSession
    STDMETHODIMP DoEditSession(TfEditCookie ec)
    {
        HRESULT hr = S_OK;

        WIND_LOG_DEBUG_FMT(L"InsertAndCompose: insert='%s', newComp='%s'\n",
                     _insertText.c_str(), _newComposition.c_str());

        // 1. Get current selection to insert text there
        TF_SELECTION tfSelection;
        ULONG cFetched;
        if (FAILED(_pContext->GetSelection(ec, TF_DEFAULT_SELECTION, 1, &tfSelection, &cFetched)) || cFetched != 1)
        {
            WIND_LOG_DEBUG(L"InsertAndCompose: Failed to get selection\n");
            return E_FAIL;
        }

        // 2. Insert the final text at current position
        if (!_insertText.empty())
        {
            hr = tfSelection.range->SetText(ec, 0, _insertText.c_str(), (LONG)_insertText.length());
            if (FAILED(hr))
            {
                WIND_LOG_DEBUG(L"InsertAndCompose: Failed to insert text\n");
                tfSelection.range->Release();
                return hr;
            }
            WIND_LOG_DEBUG(L"InsertAndCompose: Text inserted successfully\n");

            // Collapse range to end (after inserted text)
            tfSelection.range->Collapse(ec, TF_ANCHOR_END);
        }

        // 3. Now start a new composition for the new input
        if (!_newComposition.empty())
        {
            ITfContextComposition* pContextComp = nullptr;
            if (FAILED(_pContext->QueryInterface(IID_ITfContextComposition, (void**)&pContextComp)))
            {
                WIND_LOG_DEBUG(L"InsertAndCompose: Failed to get ITfContextComposition\n");
                tfSelection.range->Release();
                return E_FAIL;
            }

            // Start new composition at current position (after inserted text)
            hr = pContextComp->StartComposition(
                ec,
                tfSelection.range,
                (ITfCompositionSink*)_pTextService,
                &_pTextService->_pComposition);

            pContextComp->Release();

            if (FAILED(hr) || _pTextService->_pComposition == nullptr)
            {
                WIND_LOG_DEBUG(L"InsertAndCompose: Failed to start new composition\n");
                tfSelection.range->Release();
                return E_FAIL;
            }

            WIND_LOG_DEBUG(L"InsertAndCompose: New composition started\n");

            // 4. Set the composition text
            ITfRange* pCompRange = nullptr;
            if (SUCCEEDED(_pTextService->_pComposition->GetRange(&pCompRange)))
            {
                hr = pCompRange->SetText(ec, TF_ST_CORRECTION, _newComposition.c_str(), (LONG)_newComposition.length());
                if (SUCCEEDED(hr))
                {
                    // Apply display attribute
                    _SetDisplayAttribute(ec, pCompRange);

                    // Set cursor at end of composition
                    ITfRange* pRangeForSel = nullptr;
                    if (SUCCEEDED(_pTextService->_pComposition->GetRange(&pRangeForSel)))
                    {
                        pRangeForSel->Collapse(ec, TF_ANCHOR_END);
                        TF_SELECTION sel = {};
                        sel.range = pRangeForSel;
                        sel.style.ase = TF_AE_NONE;
                        sel.style.fInterimChar = FALSE;
                        _pContext->SetSelection(ec, 1, &sel);
                        pRangeForSel->Release();
                    }
                    WIND_LOG_DEBUG(L"InsertAndCompose: Composition text set\n");
                }
                pCompRange->Release();
            }
        }

        tfSelection.range->Release();
        return S_OK;
    }

private:
    void _SetDisplayAttribute(TfEditCookie ec, ITfRange* pRange)
    {
        TfGuidAtom gaDisplayAttr = _pTextService->GetDisplayAttributeInputAtom();
        if (gaDisplayAttr == TF_INVALID_GUIDATOM) return;

        ITfProperty* pDisplayAttrProp = nullptr;
        if (FAILED(_pContext->GetProperty(GUID_PROP_ATTRIBUTE, &pDisplayAttrProp))) return;

        VARIANT var;
        var.vt = VT_I4;
        var.lVal = gaDisplayAttr;
        pDisplayAttrProp->SetValue(ec, pRange, &var);
        pDisplayAttrProp->Release();
    }

private:
    LONG _refCount;
    CTextService* _pTextService;
    ITfContext* _pContext;
    std::wstring _insertText;
    std::wstring _newComposition;
};

static const LONG DEFAULT_CARET_HEIGHT = 20;

CTextService::CTextService()
    : _refCount(1)
    , _pThreadMgr(nullptr)
    , _tfClientId(TF_CLIENTID_NULL)
    , _dwThreadMgrEventSinkCookie(TF_INVALID_COOKIE)
    , _activateFlags(0)
    , _pKeyEventSink(nullptr)
    , _pIPCClient(nullptr)
    , _pLangBarItemButton(nullptr)
    , _pHotkeyManager(nullptr)
    , _pHostWindow(nullptr)
    , _bChineseMode(TRUE)
    , _focusSessionId(0)
    , _pComposition(nullptr)
    , _hasCachedCaretPos(FALSE)
    , _hasCachedCompStartPos(FALSE)
    , _needsFocusRecovery(FALSE)
    , _lastFocusCaretX(0)
    , _lastFocusCaretY(0)
    , _lastFocusCaretHeight(DEFAULT_CARET_HEIGHT)
    , _hasLastKnownCaretPos(FALSE)
    , _lastKnownCaretX(0)
    , _lastKnownCaretY(0)
    , _lastKnownCaretHeight(DEFAULT_CARET_HEIGHT)
    , _gaDisplayAttributeInput(TF_INVALID_GUIDATOM)
    , _dwLayoutSinkCookie(TF_INVALID_COOKIE)
    , _pLayoutSinkContext(nullptr)
{
    ZeroMemory(&_cachedCaretRect, sizeof(_cachedCaretRect));
    ZeroMemory(&_cachedCompStartRect, sizeof(_cachedCompStartRect));
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
    else if (IsEqualIID(riid, IID_ITfTextInputProcessorEx))
    {
        *ppvObj = (ITfTextInputProcessorEx*)this;
    }
    else if (IsEqualIID(riid, IID_ITfThreadMgrEventSink))
    {
        *ppvObj = (ITfThreadMgrEventSink*)this;
    }
    else if (IsEqualIID(riid, IID_ITfCompositionSink))
    {
        *ppvObj = (ITfCompositionSink*)this;
    }
    else if (IsEqualIID(riid, IID_ITfDisplayAttributeProvider))
    {
        *ppvObj = (ITfDisplayAttributeProvider*)this;
    }
    else if (IsEqualIID(riid, IID_ITfTextLayoutSink))
    {
        *ppvObj = (ITfTextLayoutSink*)this;
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
    return ActivateEx(pThreadMgr, tfClientId, 0);
}

STDAPI CTextService::ActivateEx(ITfThreadMgr* pThreadMgr, TfClientId tfClientId, DWORD dwFlags)
{
    WIND_LOG_INFO_FMT(L"TextService::ActivateEx called tfClientId=0x%08X dwFlags=0x%08X", tfClientId, dwFlags);

    _activateFlags = dwFlags;

    WindHostProcessInfo currentHost;
    if (WindQueryCurrentProcessInfo(&currentHost))
        WindLogHostProcessInfo(4, L"compat.activate.current_host", currentHost);
    else
        WIND_LOG_WARN(L"compat.activate.current_host query failed");

    _pThreadMgr = pThreadMgr;
    _pThreadMgr->AddRef();

    _tfClientId = tfClientId;

    // Initialize thread manager event sink
    if (!_InitThreadMgrEventSink())
    {
        WIND_LOG_ERROR(L"_InitThreadMgrEventSink failed\n");
        Deactivate();
        return E_FAIL;
    }
    WIND_LOG_INFO(L"ThreadMgrEventSink initialized\n");

    // Initialize IPC client
    if (!_InitIPCClient())
    {
        WIND_LOG_ERROR(L"_InitIPCClient failed\n");
        Deactivate();
        return E_FAIL;
    }
    WIND_LOG_INFO(L"IPCClient initialized\n");

    // Initialize hotkey manager with default config
    _pHotkeyManager = new CHotkeyManager();
    WIND_LOG_INFO(L"HotkeyManager initialized\n");

    // Initialize key event sink
    if (!_InitKeyEventSink())
    {
        WIND_LOG_ERROR(L"_InitKeyEventSink failed\n");
        Deactivate();
        return E_FAIL;
    }
    WIND_LOG_INFO(L"KeyEventSink initialized\n");

    // Initialize display attribute
    if (!_InitDisplayAttribute())
    {
        WIND_LOG_WARN(L"_InitDisplayAttribute failed (non-fatal)\n");
        // Not fatal, continue without display attribute
    }
    else
    {
        WIND_LOG_INFO(L"DisplayAttribute initialized\n");
    }

    // Initialize language bar button
    if (!_InitLangBarButton())
    {
        WIND_LOG_WARN(L"_InitLangBarButton failed (non-fatal)\n");
        // Not fatal, continue without language bar button
    }
    else
    {
        WIND_LOG_INFO(L"LangBarButton initialized\n");
    }

    // Update caret position before notifying activation
    // This ensures status indicators appear at the correct position immediately
    SendCaretPositionUpdate();

    // Notify Go service that IME is activated and sync full state.
    // Uses _DoFullStateSync which also handles lazy connect (service may
    // still be starting after first install).
    _DoFullStateSync();

    // NOTE: Using synchronous IPC mode (no reader thread)
    // Reference: Weasel uses sync IPC with librime and it works well
    // The reader thread is not started - responses are received synchronously in OnKeyDown

    WIND_LOG_INFO(L"TextService::Activate completed successfully (sync IPC mode)\n");
    return S_OK;
}

STDAPI CTextService::Deactivate()
{
    WIND_LOG_INFO(L"TextService::Deactivate called\n");

    // End any active composition before deactivating
    EndComposition();

    // Unregister layout sink
    _UnadviseTextLayoutSink();

    // Release language bar button
    _UninitLangBarButton();

    // Release display attribute
    _UninitDisplayAttribute();

    // Release key event sink
    _UninitKeyEventSink();

    // Notify Go service that IME is being deactivated (before disconnecting)
    // This allows the service to hide the toolbar immediately
    if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
    {
        WIND_LOG_DEBUG(L"Sending ime_deactivated to service\n");
        // SendIMEDeactivated is async (fire-and-forget), no response expected
        _pIPCClient->SendIMEDeactivated();
    }

    // Release host window (before IPC client, so shared memory is still valid during shutdown)
    if (_pHostWindow != nullptr)
    {
        _pHostWindow->Uninitialize();
        delete _pHostWindow;
        _pHostWindow = nullptr;
    }

    // Release IPC client
    _UninitIPCClient();

    // Release hotkey manager
    if (_pHotkeyManager != nullptr)
    {
        delete _pHotkeyManager;
        _pHotkeyManager = nullptr;
    }

    // Release thread manager event sink
    _UninitThreadMgrEventSink();

    // Release thread manager
    SafeRelease(_pThreadMgr);

    _tfClientId = TF_CLIENTID_NULL;

    WIND_LOG_INFO(L"TextService::Deactivate completed\n");
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
    WIND_LOG_DEBUG_FMT(L"OnSetFocus called focus=0x%p prev=0x%p", pDocMgrFocus, pDocMgrPrevFocus);

    // If gaining focus (pDocMgrFocus is not null)
    if (pDocMgrFocus != nullptr)
    {
        _focusSessionId++;
        WIND_LOG_DEBUG_FMT(L"Focus gained focusSession=%llu", _focusSessionId);

        // Register ITfTextLayoutSink on the new context to receive
        // layout change notifications (for accurate candidate window positioning)
        ITfContext* pContext = nullptr;
        if (SUCCEEDED(pDocMgrFocus->GetTop(&pContext)) && pContext != nullptr)
        {
            _AdviseTextLayoutSink(pContext);
            pContext->Release();
        }

        WindHostProcessInfo currentHost;
        if (WindQueryCurrentProcessInfo(&currentHost))
            WindLogHostProcessInfo(4, L"compat.focus.current_host", currentHost);

        WindLogForegroundProcessInfo(4, L"compat.focus.foreground_host");

        // Force refresh the language bar button to ensure it's visible
        if (_pLangBarItemButton != nullptr)
        {
            _pLangBarItemButton->ForceRefresh();
        }

        // Reset composing state on focus gained to ensure clean state
        // This prevents stale composition state from affecting new input
        if (_pKeyEventSink != nullptr)
        {
            _pKeyEventSink->ResetComposingState();
        }

        // Get caret position for toolbar placement
        LONG caretX = 0, caretY = 0, caretHeight = 0;
        if (!GetCaretPosition(&caretX, &caretY, &caretHeight) && _hasLastKnownCaretPos)
        {
            caretX = _lastKnownCaretX;
            caretY = _lastKnownCaretY;
            caretHeight = _lastKnownCaretHeight;
            WIND_LOG_INFO_FMT(L"OnSetFocus: using last known caret position x=%ld y=%ld h=%ld", caretX, caretY, caretHeight);
        }
        WIND_LOG_DEBUG_FMT(
            L"compat.focus.caret focusSession=%llu x=%ld y=%ld height=%ld",
            _focusSessionId, caretX, caretY, caretHeight
        );
        _lastFocusCaretX = caretX;
        _lastFocusCaretY = caretY;
        _lastFocusCaretHeight = caretHeight > 0 ? caretHeight : DEFAULT_CARET_HEIGHT;

        // Send focus_gained to service and receive response synchronously.
        // This ensures state is properly synced before user starts typing.
        // Note: SendFocusGained does lazy connect internally, so this also
        // handles the case where the service started after TSF was loaded.
        if (_pIPCClient != nullptr)
        {
            if (_pIPCClient->SendFocusGained(caretX, caretY, caretHeight))
            {
                ServiceResponse response;
                if (_pIPCClient->ReceiveResponse(response))
                {
                    BOOL needsStateSync = _pIPCClient->NeedsStateSync();
                    WIND_LOG_DEBUG_FMT(L"FocusGained response received focusSession=%llu", _focusSessionId);
                    _SyncStateFromResponse(response);
                    _EnsureHostRenderSetup(response, needsStateSync);
                    _needsFocusRecovery = FALSE;
                    _pIPCClient->ClearNeedsSyncFlag();
                }
                else
                {
                    WIND_LOG_WARN_FMT(L"FocusGained response missing focusSession=%llu", _focusSessionId);
                }
            }
            else
            {
                WIND_LOG_WARN_FMT(L"FocusGained IPC send failed focusSession=%llu", _focusSessionId);
                _needsFocusRecovery = TRUE;
            }
        }
    }

    // If losing focus (pDocMgrFocus is null)
    if (pDocMgrFocus == nullptr)
    {
        WIND_LOG_DEBUG_FMT(L"Focus lost focusSession=%llu, notifying service", _focusSessionId);

        // End any active composition before sending focus_lost
        EndComposition();

        // Send focus_lost to service (async, no response expected)
        if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
        {
            _pIPCClient->SendFocusLost();
        }

        // Reset composing state
        if (_pKeyEventSink != nullptr)
        {
            _pKeyEventSink->ResetComposingState();
        }

        // Unregister layout sink when losing focus
        _UnadviseTextLayoutSink();

        _needsFocusRecovery = FALSE;
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

// ============================================================================
// State sync helpers
// ============================================================================

void CTextService::_SyncStateFromResponse(const ServiceResponse& response)
{
    if (response.type != ResponseType::StatusUpdate)
        return;

    _bChineseMode = response.IsChineseMode();

    // Sync full status to LangBarItemButton
    if (_pLangBarItemButton != nullptr)
    {
        BOOL bCapsLock = (GetKeyState(VK_CAPITAL) & 0x0001) != 0;
        _pLangBarItemButton->UpdateFullStatus(
            response.IsChineseMode(),
            response.IsFullWidth(),
            response.IsChinesePunct(),
            response.IsToolbarVisible(),
            bCapsLock,
            response.iconLabel.empty() ? nullptr : response.iconLabel.c_str()
        );
    }

    // Update hotkey whitelist if present
    if (response.HasHotkeys() && _pHotkeyManager != nullptr)
    {
        WIND_LOG_DEBUG(L"Updating hotkey whitelist from state sync\n");
        _pHotkeyManager->UpdateHotkeys(
            response.keyDownHotkeys,
            response.keyUpHotkeys
        );
    }

    WIND_LOG_INFO_FMT(L"State synced: mode=%d, width=%d, punct=%d, toolbar=%d, hostRender=%d\n",
        response.IsChineseMode(), response.IsFullWidth(),
        response.IsChinesePunct(), response.IsToolbarVisible(), response.IsHostRenderAvailable());
}

void CTextService::_EnsureHostRenderSetup(const ServiceResponse& response, BOOL forceRefresh)
{
    if (_pIPCClient == nullptr || !_pIPCClient->IsConnected())
        return;

    BOOL hadHostWindow = (_pHostWindow != nullptr);
    BOOL hostRenderAvailable = response.IsHostRenderAvailable();
    BOOL shouldRetryExistingHost = forceRefresh && hadHostWindow && !hostRenderAvailable;

    if (!hostRenderAvailable && !shouldRetryExistingHost)
    {
        if (forceRefresh && _pHostWindow != nullptr)
        {
            WIND_LOG_INFO(L"Host render unavailable after refresh, disabling existing host window\n");
            _pHostWindow->Uninitialize();
            delete _pHostWindow;
            _pHostWindow = nullptr;
        }
        return;
    }

    if (shouldRetryExistingHost)
    {
        WIND_LOG_WARN(L"Host render flag missing after reconnect, retrying setup because host window was previously active\n");
    }

    if (_pHostWindow != nullptr && !forceRefresh)
        return;

    if (_pHostWindow != nullptr)
    {
        WIND_LOG_INFO(L"Refreshing host render window after service reconnection\n");
        _pHostWindow->Uninitialize();
        delete _pHostWindow;
        _pHostWindow = nullptr;
    }

    WIND_LOG_INFO(L"Host render available, requesting setup\n");

    ServiceResponse hrResponse;
    if (_pIPCClient->SendHostRenderRequest(hrResponse) &&
        hrResponse.type == ResponseType::HostRenderSetup &&
        !hrResponse.shmName.empty() && !hrResponse.eventName.empty())
    {
        _pHostWindow = new CHostWindow();
        if (!_pHostWindow->Initialize(
            hrResponse.shmName.c_str(),
            hrResponse.eventName.c_str(),
            hrResponse.maxBufferSize))
        {
            WIND_LOG_WARN(L"Host window initialization failed, falling back to Go window\n");
            delete _pHostWindow;
            _pHostWindow = nullptr;
        }
        else
        {
            WIND_LOG_INFO(L"Host window initialized successfully\n");
        }
    }
    else
    {
        WIND_LOG_WARN(L"Host render setup request failed, falling back to Go window\n");
    }
}

void CTextService::_DoFullStateSync()
{
    if (_pIPCClient == nullptr || !_pIPCClient->IsConnected())
        return;

    WIND_LOG_INFO(L"Performing full state sync with Go service\n");

    if (_pIPCClient->SendIMEActivated())
    {
        ServiceResponse response;
        if (_pIPCClient->ReceiveResponse(response))
        {
            _SyncStateFromResponse(response);
            _EnsureHostRenderSetup(response, TRUE);
        }
    }

    _pIPCClient->ClearNeedsSyncFlag();
    _needsFocusRecovery = FALSE; // Full sync covers focus recovery
}

void CTextService::TryRecoverFocusState()
{
    if (!_needsFocusRecovery || _pIPCClient == nullptr || !_pIPCClient->IsConnected())
        return;

    LONG caretX = _lastFocusCaretX;
    LONG caretY = _lastFocusCaretY;
    LONG caretHeight = _lastFocusCaretHeight > 0 ? _lastFocusCaretHeight : DEFAULT_CARET_HEIGHT;

    if (GetCaretPosition(&caretX, &caretY, &caretHeight))
    {
        _lastFocusCaretX = caretX;
        _lastFocusCaretY = caretY;
        _lastFocusCaretHeight = caretHeight > 0 ? caretHeight : DEFAULT_CARET_HEIGHT;
    }
    else if (_hasLastKnownCaretPos)
    {
        caretX = _lastKnownCaretX;
        caretY = _lastKnownCaretY;
        caretHeight = _lastKnownCaretHeight;
        WIND_LOG_INFO_FMT(L"Recovering focus state with last known caret x=%ld y=%ld h=%ld", caretX, caretY, caretHeight);
    }

    WIND_LOG_INFO_FMT(L"Attempting deferred focus recovery focusSession=%llu x=%ld y=%ld h=%ld",
        _focusSessionId, caretX, caretY, caretHeight);

    if (_pIPCClient->SendFocusGained((int)caretX, (int)caretY, (int)caretHeight))
    {
        ServiceResponse response;
        if (_pIPCClient->ReceiveResponse(response))
        {
            BOOL needsStateSync = _pIPCClient->NeedsStateSync();
            _SyncStateFromResponse(response);
            _EnsureHostRenderSetup(response, needsStateSync);
            _needsFocusRecovery = FALSE;
            _pIPCClient->ClearNeedsSyncFlag();
            SendCaretPositionUpdate();
            WIND_LOG_INFO(L"Deferred focus recovery succeeded\n");
        }
        else
        {
            WIND_LOG_WARN_FMT(L"Deferred focus recovery response missing focusSession=%llu", _focusSessionId);
            _needsFocusRecovery = FALSE;
        }
    }
    else
    {
        WIND_LOG_WARN_FMT(L"Deferred focus recovery send failed focusSession=%llu", _focusSessionId);
        _needsFocusRecovery = FALSE;
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
        WIND_LOG_WARN(L"Failed to connect to Go Service, will retry later\n");
    }

    // Set up state push callback
    CTextService* pThis = this;
    _pIPCClient->SetStatePushCallback([pThis](const ServiceResponse& response) {
        // This callback is called from the async reader thread
        // We need to update our state and notify the language bar
        WIND_LOG_INFO_FMT(L"State push received: mode=%d, fullWidth=%d, punct=%d, caps=%d\n",
                     response.IsChineseMode(), response.IsFullWidth(),
                     response.IsChinesePunct(), response.IsCapsLock());

        // Update internal state (atomic operation, thread-safe)
        pThis->_bChineseMode = response.IsChineseMode();

        // Update language bar button using thread-safe PostUpdateFullStatus
        // This posts a message to the UI thread instead of calling COM directly
        if (pThis->_pLangBarItemButton != nullptr)
        {
            pThis->_pLangBarItemButton->PostUpdateFullStatus(
                response.IsChineseMode(),
                response.IsFullWidth(),
                response.IsChinesePunct(),
                response.IsToolbarVisible(),
                response.IsCapsLock(),
                response.iconLabel.empty() ? nullptr : response.iconLabel.c_str()
            );
        }
    });

    // Set up commit text callback for mouse click on candidate
    _pIPCClient->SetCommitTextCallback([pThis](const std::wstring& text) {
        // This callback is called from the async reader thread
        WIND_LOG_DEBUG_FMT(L"Commit text received from Go, textLen=%zu\n", text.length());

        // Use PostCommitText to ensure EndComposition is called before InsertText on UI thread
        // This fixes the issue where text was inserted into composition range
        if (pThis->_pLangBarItemButton != nullptr)
        {
            pThis->_pLangBarItemButton->PostCommitText(text);
        }
        else
        {
            // Fallback: direct InsertText (composition won't be ended properly)
            pThis->InsertText(text);
        }
    });

    // Set up clear composition callback for mode toggle via menu
    _pIPCClient->SetClearCompositionCallback([pThis]() {
        // This callback is called from the async reader thread
        WIND_LOG_DEBUG(L"Clear composition received from Go service\n");

        if (pThis->_pLangBarItemButton != nullptr)
        {
            pThis->_pLangBarItemButton->PostClearComposition();
        }
        else
        {
            // Fallback: direct EndComposition
            pThis->EndComposition();
        }
    });

    // Start async reader thread for receiving state pushes from Go
    if (!_pIPCClient->StartAsyncReader())
    {
        WIND_LOG_WARN(L"Failed to start async reader thread (non-fatal)\n");
        // Non-fatal - we can still use sync IPC
    }
    else
    {
        WIND_LOG_INFO(L"Async reader thread started for state push\n");
    }

    return TRUE;
}

void CTextService::_UninitIPCClient()
{
    if (_pIPCClient != nullptr)
    {
        // Stop async reader thread first
        _pIPCClient->StopAsyncReader();
        _pIPCClient->Disconnect();
        delete _pIPCClient;
        _pIPCClient = nullptr;
    }
}

// EditSession for inserting text at current selection
class CInsertTextEditSession : public ITfEditSession
{
public:
    CInsertTextEditSession(CTextService* pTextService, ITfContext* pContext, const std::wstring& text)
        : _refCount(1), _pTextService(pTextService), _pContext(pContext), _text(text), _success(FALSE)
    {
        _pTextService->AddRef();
        _pContext->AddRef();
    }

    ~CInsertTextEditSession()
    {
        _pTextService->Release();
        _pContext->Release();
    }

    // IUnknown
    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj)
    {
        if (ppvObj == nullptr) return E_INVALIDARG;
        *ppvObj = nullptr;
        if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfEditSession))
        {
            *ppvObj = (ITfEditSession*)this;
            AddRef();
            return S_OK;
        }
        return E_NOINTERFACE;
    }

    STDMETHODIMP_(ULONG) AddRef() { return InterlockedIncrement(&_refCount); }
    STDMETHODIMP_(ULONG) Release()
    {
        LONG cr = InterlockedDecrement(&_refCount);
        if (cr == 0) delete this;
        return cr;
    }

    // ITfEditSession
    STDMETHODIMP DoEditSession(TfEditCookie ec)
    {
        // Get ITfInsertAtSelection interface
        ITfInsertAtSelection* pInsertAtSel = nullptr;
        HRESULT hr = _pContext->QueryInterface(IID_ITfInsertAtSelection, (void**)&pInsertAtSel);
        if (FAILED(hr) || pInsertAtSel == nullptr)
        {
            WIND_LOG_DEBUG(L"InsertTextEditSession: Failed to get ITfInsertAtSelection\n");
            return E_FAIL;
        }

        // Insert text at current selection
        ITfRange* pRange = nullptr;
        hr = pInsertAtSel->InsertTextAtSelection(
            ec,
            0,  // No special flags
            _text.c_str(),
            (LONG)_text.length(),
            &pRange);

        pInsertAtSel->Release();

        if (FAILED(hr))
        {
            WIND_LOG_DEBUG_FMT(L"InsertTextEditSession: InsertTextAtSelection failed hr=0x%08X\n", hr);
            return hr;
        }

        if (pRange != nullptr)
        {
            // Move selection to end of inserted text
            pRange->Collapse(ec, TF_ANCHOR_END);

            TF_SELECTION sel = {};
            sel.range = pRange;
            sel.style.ase = TF_AE_NONE;
            sel.style.fInterimChar = FALSE;
            _pContext->SetSelection(ec, 1, &sel);

            pRange->Release();
        }

        _success = TRUE;
        WIND_LOG_DEBUG_FMT(L"InsertTextEditSession: Successfully inserted '%s'\n", _text.c_str());
        return S_OK;
    }

    BOOL GetSuccess() const { return _success; }

private:
    LONG _refCount;
    CTextService* _pTextService;
    ITfContext* _pContext;
    std::wstring _text;
    BOOL _success;
};

BOOL CTextService::InsertText(const std::wstring& text)
{
    if (text.empty())
    {
        return TRUE;
    }

    // Try TSF method first (works on main thread with proper context)
    if (_pThreadMgr != nullptr)
    {
        // Get current document manager
        ITfDocumentMgr* pDocMgr = nullptr;
        HRESULT hr = _pThreadMgr->GetFocus(&pDocMgr);
        if (SUCCEEDED(hr) && pDocMgr != nullptr)
        {
            // Get top context
            ITfContext* pContext = nullptr;
            hr = pDocMgr->GetTop(&pContext);
            pDocMgr->Release();

            if (SUCCEEDED(hr) && pContext != nullptr)
            {
                // Try to insert using TSF EditSession
                CInsertTextEditSession* pEditSession = new CInsertTextEditSession(this, pContext, text);

                HRESULT hrSession;
                // Use TF_ES_SYNC to ensure synchronous execution
                hr = pContext->RequestEditSession(_tfClientId, pEditSession, TF_ES_SYNC | TF_ES_READWRITE, &hrSession);

                BOOL success = pEditSession->GetSuccess();
                pEditSession->Release();
                pContext->Release();

                if (SUCCEEDED(hr) && SUCCEEDED(hrSession) && success)
                {
                    WIND_LOG_DEBUG(L"InsertText: Successfully used TSF method\n");
                    return TRUE;
                }

                WIND_LOG_DEBUG_FMT(L"InsertText: TSF method failed (hr=0x%08X, hrSession=0x%08X), falling back to SendInput\n", hr, hrSession);
                WIND_LOG_DEBUG_FMT(
                    L"compat.insert_text_fallback focusSession=%llu textLen=%zu hr=0x%08X hrSession=0x%08X",
                    _focusSessionId, text.length(), hr, hrSession
                );
                WindLogForegroundProcessInfo(4, L"compat.insert_text_fallback.foreground_host");
            }
        }
    }

    // Fallback: Use SendInput for batch input (all characters at once)
    // This works from any thread and is used when TSF method fails
    WIND_LOG_DEBUG_FMT(L"InsertText: Using SendInput batch method for '%s'\n", text.c_str());

    // Allocate INPUT structures for all characters (2 per char: down + up)
    std::vector<INPUT> inputs;
    inputs.reserve(text.length() * 2);

    for (wchar_t ch : text)
    {
        INPUT inputDown = {};
        inputDown.type = INPUT_KEYBOARD;
        inputDown.ki.wVk = 0;
        inputDown.ki.wScan = ch;
        inputDown.ki.dwFlags = KEYEVENTF_UNICODE;
        inputs.push_back(inputDown);

        INPUT inputUp = {};
        inputUp.type = INPUT_KEYBOARD;
        inputUp.ki.wVk = 0;
        inputUp.ki.wScan = ch;
        inputUp.ki.dwFlags = KEYEVENTF_UNICODE | KEYEVENTF_KEYUP;
        inputs.push_back(inputUp);
    }

    // Send all inputs at once - this makes text appear instantly
    UINT sent = SendInput((UINT)inputs.size(), inputs.data(), sizeof(INPUT));

    if (sent != inputs.size())
    {
        WIND_LOG_WARN_FMT(L"InsertText: SendInput sent %u of %u inputs\n", sent, (UINT)inputs.size());
    }
    else
    {
        WIND_LOG_DEBUG_FMT(
            L"compat.sendinput_commit focusSession=%llu textLen=%zu inputs=%u",
            _focusSessionId, text.length(), (UINT)inputs.size()
        );
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
    BOOL result = CCaretEditSession::GetCaretRect(pContext, _tfClientId, &rc);
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

        WIND_LOG_DEBUG(L"GetCaretPositionFromTSF: Success\n");
        return TRUE;
    }

    return FALSE;
}

// Helper function to check if a window is a console/terminal window
static BOOL IsConsoleWindow(HWND hwnd)
{
    if (hwnd == nullptr)
        return FALSE;

    WCHAR className[256] = {0};
    if (GetClassNameW(hwnd, className, 256) == 0)
        return FALSE;

    // Check for known console window classes
    // ConsoleWindowClass - Traditional conhost.exe console
    // CASCADIA_HOSTING_WINDOW_CLASS - Windows Terminal
    // PseudoConsoleWindow - ConPTY pseudo console
    if (wcscmp(className, L"ConsoleWindowClass") == 0 ||
        wcscmp(className, L"CASCADIA_HOSTING_WINDOW_CLASS") == 0 ||
        wcsstr(className, L"Console") != nullptr ||
        wcsstr(className, L"Terminal") != nullptr)
    {
        return TRUE;
    }

    return FALSE;
}

// Try to get caret position for console/terminal windows
static BOOL GetConsoleCaretPosition(HWND hwndConsole, LONG* px, LONG* py, LONG* pHeight)
{
    if (hwndConsole == nullptr)
        return FALSE;

    // For Windows Terminal and modern consoles, we can try to get the console buffer info
    // This requires the console to be attached to our process or accessible

    // First, try to get the console window handle and screen buffer info
    // Note: GetConsoleWindow() returns the console for the CURRENT process,
    // which may not be the foreground console. We need a different approach.

    // Get window rect for calculations
    RECT rcWindow;
    if (!GetWindowRect(hwndConsole, &rcWindow))
        return FALSE;

    // Get client rect
    RECT rcClient;
    if (!GetClientRect(hwndConsole, &rcClient))
        return FALSE;

    // Calculate client area origin in screen coordinates
    POINT clientOrigin = {0, 0};
    ClientToScreen(hwndConsole, &clientOrigin);

    // Try to use GUITHREADINFO - sometimes works for console windows
    DWORD threadId = GetWindowThreadProcessId(hwndConsole, nullptr);
    GUITHREADINFO guiInfo = { sizeof(GUITHREADINFO) };

    if (GetGUIThreadInfo(threadId, &guiInfo) && guiInfo.hwndCaret != nullptr)
    {
        POINT caretPos;
        caretPos.x = guiInfo.rcCaret.left;
        caretPos.y = guiInfo.rcCaret.bottom;

        // Convert from client coordinates to screen coordinates
        ClientToScreen(guiInfo.hwndCaret, &caretPos);

        // Validate that it's within the console window area
        if (caretPos.x >= rcWindow.left && caretPos.x <= rcWindow.right &&
            caretPos.y >= rcWindow.top && caretPos.y <= rcWindow.bottom)
        {
            *px = caretPos.x;
            *py = caretPos.y;
            *pHeight = max(guiInfo.rcCaret.bottom - guiInfo.rcCaret.top, 16);

            WIND_LOG_DEBUG(L"GetConsoleCaretPosition: Got caret from GUITHREADINFO\n");
            return TRUE;
        }
    }

    // Fallback: Position the candidate window at a reasonable location
    // For consoles, we position it near the bottom of the visible area
    // This is better than the center, as typing usually happens at the bottom

    // Estimate: console typically shows text near the current cursor line
    // Position the IME window near the bottom-left of the console
    int clientWidth = rcClient.right - rcClient.left;
    int clientHeight = rcClient.bottom - rcClient.top;

    // Position at roughly 10% from left, 80% from top (near bottom where typing usually occurs)
    *px = clientOrigin.x + (clientWidth * 10 / 100);
    *py = clientOrigin.y + (clientHeight * 80 / 100);
    *pHeight = 16;  // Standard console line height approximation

    WIND_LOG_DEBUG_FMT(L"GetConsoleCaretPosition: Using console fallback position (%ld, %ld)\n", *px, *py);

    return TRUE;
}

BOOL CTextService::GetCaretPosition(LONG* px, LONG* py, LONG* pHeight)
{
    // First, check if the foreground window is a console/terminal
    HWND hwndForeground = GetForegroundWindow();
    BOOL isConsole = IsConsoleWindow(hwndForeground);

    if (isConsole)
    {
        WIND_LOG_DEBUG(L"GetCaretPosition: Detected console window\n");
    }

    // Method 1: Try TSF APIs first - this is the most reliable for browsers and modern apps
    // ITfContextView::GetTextExt provides accurate caret position in Chrome, Edge, etc.
    if (GetCaretPositionFromTSF(px, py, pHeight))
    {
        return TRUE;
    }

    // For console windows, use specialized handling
    if (isConsole)
    {
        if (GetConsoleCaretPosition(hwndForeground, px, py, pHeight))
        {
            // Save as last known good position
            s_lastCaretX = *px;
            s_lastCaretY = *py;
            s_lastCaretHeight = *pHeight;
            s_hasLastCaretPos = TRUE;
            return TRUE;
        }
    }

    // Method 3: Try to get caret position from the GUI thread info
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

    // Fallback to GetCaretPos
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

            WIND_LOG_DEBUG(L"GetCaretPosition: Using window position fallback\n");
            return TRUE;
        }
    }

    // Method 5: Use last known good position if available
    if (s_hasLastCaretPos)
    {
        *px = s_lastCaretX;
        *py = s_lastCaretY;
        *pHeight = s_lastCaretHeight;
        WIND_LOG_DEBUG(L"GetCaretPosition: Using last known position\n");
        return TRUE;
    }

    WIND_LOG_DEBUG(L"GetCaretPosition: Failed to get caret position\n");
    return FALSE;
}

void CTextService::SendCaretPositionUpdate()
{
    LONG x = 0, y = 0, height = 20;
    LONG compStartX = 0, compStartY = 0;
    BOOL hasPosition = FALSE;

    // Priority 1: Use cached position from edit session (reliable for WebView apps
    // where separate CaretEditSession with TF_INVALID_COOKIE is rejected)
    if (_hasCachedCaretPos)
    {
        x = _cachedCaretRect.left;
        y = _cachedCaretRect.bottom;
        height = _cachedCaretRect.bottom - _cachedCaretRect.top;
        if (height <= 0) height = 20;
        hasPosition = TRUE;

        if (_hasCachedCompStartPos)
        {
            compStartX = _cachedCompStartRect.left;
            compStartY = _cachedCompStartRect.bottom;
        }

        // Clear cache (one-shot: next call falls back to normal methods)
        _hasCachedCaretPos = FALSE;
        _hasCachedCompStartPos = FALSE;

        WIND_LOG_DEBUG(L"SendCaretPositionUpdate: Using cached position from edit session\n");
    }

    // Priority 2: Normal method (separate edit session + fallbacks)
    if (!hasPosition)
    {
        if (!GetCaretPosition(&x, &y, &height))
        {
            if (_hasLastKnownCaretPos)
            {
                x = _lastKnownCaretX;
                y = _lastKnownCaretY;
                height = _lastKnownCaretHeight;
                hasPosition = TRUE;
                WIND_LOG_INFO_FMT(L"SendCaretPositionUpdate: using last known caret x=%ld y=%ld h=%ld", x, y, height);
            }
            else
            {
                return; // No position available at all
            }
        }
        else
        {
            hasPosition = TRUE;
        }

        if (_pComposition != nullptr)
        {
            GetCompositionStartPosition(&compStartX, &compStartY);
        }
    }

    if (hasPosition)
    {
        _hasLastKnownCaretPos = TRUE;
        _lastKnownCaretX = x;
        _lastKnownCaretY = y;
        _lastKnownCaretHeight = height > 0 ? height : DEFAULT_CARET_HEIGHT;
    }

    // SendCaretUpdate is async (fire-and-forget), no response expected
    if (_pIPCClient != nullptr && _pIPCClient->IsConnected())
    {
        _pIPCClient->SendCaretUpdate((int)x, (int)y, (int)height, (int)compStartX, (int)compStartY);
    }
}

BOOL CTextService::GetCompositionStartPosition(LONG* px, LONG* py)
{
    if (_pComposition == nullptr || _pThreadMgr == nullptr)
    {
        return FALSE;
    }

    ITfDocumentMgr* pDocMgr = nullptr;
    HRESULT hr = _pThreadMgr->GetFocus(&pDocMgr);
    if (FAILED(hr) || pDocMgr == nullptr)
    {
        return FALSE;
    }

    ITfContext* pContext = nullptr;
    hr = pDocMgr->GetTop(&pContext);
    pDocMgr->Release();
    if (FAILED(hr) || pContext == nullptr)
    {
        return FALSE;
    }

    RECT caretRect = {}, compStartRect = {};
    BOOL hasCompStart = FALSE;
    BOOL result = CCaretEditSession::GetCaretAndCompositionStartRect(
        pContext, _tfClientId, _pComposition, &caretRect, &compStartRect, &hasCompStart);
    pContext->Release();

    if (result && hasCompStart)
    {
        *px = compStartRect.left;
        *py = compStartRect.bottom;
        WIND_LOG_DEBUG_FMT(L"GetCompositionStartPosition: x=%ld, y=%ld\n", *px, *py);
        return TRUE;
    }

    return FALSE;
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
    WIND_LOG_INFO(L"ToggleInputMode called (local fallback)\n");

    // Toggle mode locally (this is used as a fallback when Go service is unavailable)
    // The actual mode toggle is handled via KeyUp event -> Go service -> ModeChanged response
    EndComposition();
    _bChineseMode = !_bChineseMode;

    WIND_LOG_INFO_FMT(L"Switched to %s mode\n", _bChineseMode ? L"Chinese" : L"English");

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

    WIND_LOG_INFO_FMT(L"Mode set to %s (from service)\n", _bChineseMode ? L"Chinese" : L"English");

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
    WIND_LOG_INFO_FMT(L"SendMenuCommand: command=%hs\n", command);

    CIPCClient* pClient = _pIPCClient;
    CIPCClient* pTempClient = nullptr;

    // If main IPC client is null (Deactivate was called), create temporary connection
    if (pClient == nullptr)
    {
        WIND_LOG_INFO(L"SendMenuCommand: Main IPC null, creating temporary connection\n");
        pTempClient = new CIPCClient();
        if (pTempClient == nullptr)
        {
            WIND_LOG_ERROR(L"SendMenuCommand: Failed to create temporary IPC client\n");
            return;
        }
        if (!pTempClient->Connect())
        {
            WIND_LOG_WARN(L"SendMenuCommand: Temporary connection failed\n");
            delete pTempClient;
            return;
        }
        pClient = pTempClient;
        WIND_LOG_INFO(L"SendMenuCommand: Temporary connection established\n");
    }
    else if (!pClient->IsConnected())
    {
        // Main client exists but disconnected, try to reconnect
        WIND_LOG_INFO(L"SendMenuCommand: IPC disconnected, attempting reconnect\n");
        if (!pClient->Connect())
        {
            WIND_LOG_WARN(L"SendMenuCommand: Reconnect failed\n");
            return;
        }
        WIND_LOG_INFO(L"SendMenuCommand: Reconnected successfully\n");
    }

    // Send menu command via IPC (command is UTF-8 string)
    size_t commandLen = strlen(command);
    ServiceResponse response;
    if (pClient->SendSync(CMD_MENU_COMMAND, command, (uint32_t)commandLen, response))
    {
        WIND_LOG_INFO(L"SendMenuCommand: Command sent successfully\n");

        // Apply any status updates from response
        if (response.type == ResponseType::StatusUpdate)
        {
            BOOL bChineseMode = response.IsChineseMode();
            BOOL bFullWidth = response.IsFullWidth();
            BOOL bChinesePunct = response.IsChinesePunct();
            BOOL bToolbarVisible = response.IsToolbarVisible();
            BOOL bCapsLock = response.IsCapsLock();

            UpdateFullStatus(bChineseMode, bFullWidth, bChinesePunct, bToolbarVisible, bCapsLock,
                            response.iconLabel.empty() ? nullptr : response.iconLabel.c_str());
        }
    }
    else
    {
        WIND_LOG_WARN(L"SendMenuCommand: Failed to send command\n");
    }

    // Clean up temporary client if we created one
    if (pTempClient != nullptr)
    {
        pTempClient->Disconnect();
        delete pTempClient;
        WIND_LOG_DEBUG(L"SendMenuCommand: Temporary connection closed\n");
    }
}

void CTextService::SendShowContextMenu(int screenX, int screenY)
{
    WIND_LOG_INFO_FMT(L"SendShowContextMenu: x=%d, y=%d\n", screenX, screenY);

    CIPCClient* pClient = _pIPCClient;
    CIPCClient* pTempClient = nullptr;

    // If main IPC client is null (Deactivate was called), create temporary connection
    if (pClient == nullptr)
    {
        WIND_LOG_INFO(L"SendShowContextMenu: Main IPC null, creating temporary connection\n");
        pTempClient = new CIPCClient();
        if (pTempClient == nullptr)
        {
            WIND_LOG_ERROR(L"SendShowContextMenu: Failed to create temporary IPC client\n");
            return;
        }
        if (!pTempClient->Connect())
        {
            WIND_LOG_WARN(L"SendShowContextMenu: Temporary connection failed\n");
            delete pTempClient;
            return;
        }
        pClient = pTempClient;
        WIND_LOG_INFO(L"SendShowContextMenu: Temporary connection established\n");
    }
    else if (!pClient->IsConnected())
    {
        WIND_LOG_INFO(L"SendShowContextMenu: IPC disconnected, attempting reconnect\n");
        if (!pClient->Connect())
        {
            WIND_LOG_WARN(L"SendShowContextMenu: Reconnect failed\n");
            return;
        }
        WIND_LOG_INFO(L"SendShowContextMenu: Reconnected successfully\n");
    }

    // Build payload: int32 x + int32 y = 8 bytes
    struct {
        int32_t x;
        int32_t y;
    } payload;
    payload.x = (int32_t)screenX;
    payload.y = (int32_t)screenY;

    // Send async (fire-and-forget, Go side will show the menu)
    pClient->SendAsync(CMD_SHOW_CONTEXT_MENU, &payload, sizeof(payload));

    // Clean up temporary client if we created one
    if (pTempClient != nullptr)
    {
        pTempClient->Disconnect();
        delete pTempClient;
        WIND_LOG_DEBUG(L"SendShowContextMenu: Temporary connection closed\n");
    }
}

void CTextService::UpdateFullStatus(BOOL bChineseMode, BOOL bFullWidth, BOOL bChinesePunct, BOOL bToolbarVisible, BOOL bCapsLock, const wchar_t* iconLabel)
{
    _bChineseMode = bChineseMode;

    if (_pLangBarItemButton != nullptr)
    {
        _pLangBarItemButton->UpdateFullStatus(bChineseMode, bFullWidth, bChinesePunct, bToolbarVisible, bCapsLock, iconLabel);
    }

    WIND_LOG_DEBUG_FMT(L"UpdateFullStatus: mode=%d, width=%d, punct=%d, toolbar=%d, caps=%d, label=%ls\n",
                 bChineseMode, bFullWidth, bChinesePunct, bToolbarVisible, bCapsLock,
                 iconLabel ? iconLabel : L"(none)");
}

// ITfCompositionSink implementation
STDAPI CTextService::OnCompositionTerminated(TfEditCookie ecWrite, ITfComposition* pComposition)
{
    WIND_LOG_DEBUG(L"OnCompositionTerminated called\n");

    // Clear composition text cache
    _lastCompositionText.clear();
    _lastCaretPos = -1;

    // Only release if this is the same composition we're tracking
    // It may have already been released in DoEditSession
    if (_pComposition != nullptr && _pComposition == pComposition)
    {
        // CRITICAL: This is an unexpected termination (we didn't call EndComposition)
        // This can happen when:
        // 1. Fast typing: new composition starts before previous InsertText completes
        // 2. Application forcefully terminates composition
        //
        // We MUST clear the composition text to prevent it from leaking to the document
        // as plain text (which would cause the "d being committed directly" bug)
        ITfRange* pRange = nullptr;
        if (SUCCEEDED(pComposition->GetRange(&pRange)) && pRange != nullptr)
        {
            // Clear the composition text by setting it to empty
            HRESULT hr = pRange->SetText(ecWrite, 0, L"", 0);
            if (SUCCEEDED(hr))
            {
                WIND_LOG_DEBUG(L"OnCompositionTerminated: Cleared composition text (unexpected termination)\n");
            }
            else
            {
                WIND_LOG_ERROR_FMT(L"OnCompositionTerminated: SetText failed hr=0x%08X\n", hr);
            }
            pRange->Release();
        }

        WIND_LOG_DEBUG(L"OnCompositionTerminated: Releasing composition\n");
        _pComposition->Release();
        _pComposition = nullptr;

        // Notify KeyEventSink that composition was unexpectedly terminated
        // This ensures _isComposing and _hasCandidates flags are properly reset
        if (_pKeyEventSink != nullptr)
        {
            _pKeyEventSink->OnCompositionUnexpectedlyTerminated();
        }
    }
    else if (_pComposition == nullptr)
    {
        WIND_LOG_DEBUG(L"OnCompositionTerminated: Already released\n");
    }

    return S_OK;
}

// ITfTextLayoutSink - called by TSF when the text layout changes.
// This fires after the app has reflowed text (processed WM_PAINT etc.),
// so GetTextExt now returns the correct, up-to-date coordinates.
STDAPI CTextService::OnLayoutChange(ITfContext* pContext, TfLayoutCode lCode, ITfContextView* pView)
{
    if (lCode == TF_LC_CHANGE && _pComposition != nullptr)
    {
        WIND_LOG_DEBUG(L"OnLayoutChange: TF_LC_CHANGE with active composition, updating caret position\n");
        SendCaretPositionUpdate();
    }
    return S_OK;
}

void CTextService::_AdviseTextLayoutSink(ITfContext* pContext)
{
    // Unadvise previous if any
    _UnadviseTextLayoutSink();

    if (pContext == nullptr)
        return;

    ITfSource* pSource = nullptr;
    if (SUCCEEDED(pContext->QueryInterface(IID_ITfSource, (void**)&pSource)) && pSource != nullptr)
    {
        if (SUCCEEDED(pSource->AdviseSink(IID_ITfTextLayoutSink, (ITfTextLayoutSink*)this, &_dwLayoutSinkCookie)))
        {
            _pLayoutSinkContext = pContext;
            _pLayoutSinkContext->AddRef();
            WIND_LOG_DEBUG(L"TextLayoutSink advised successfully\n");
        }
        pSource->Release();
    }
}

void CTextService::_UnadviseTextLayoutSink()
{
    if (_pLayoutSinkContext != nullptr && _dwLayoutSinkCookie != TF_INVALID_COOKIE)
    {
        ITfSource* pSource = nullptr;
        if (SUCCEEDED(_pLayoutSinkContext->QueryInterface(IID_ITfSource, (void**)&pSource)) && pSource != nullptr)
        {
            pSource->UnadviseSink(_dwLayoutSinkCookie);
            pSource->Release();
        }
        _pLayoutSinkContext->Release();
        _pLayoutSinkContext = nullptr;
        _dwLayoutSinkCookie = TF_INVALID_COOKIE;
        WIND_LOG_DEBUG(L"TextLayoutSink unadvised\n");
    }
}

// Update composition text
BOOL CTextService::UpdateComposition(const std::wstring& text, int caretPos)
{
    WIND_LOG_DEBUG_FMT(L"UpdateComposition called, textLen=%zu, _pComposition=%p\n",
                 text.length(), _pComposition);

    // OPTIMIZATION: Skip if both composition text AND caret position are the same as last time
    // This avoids unnecessary TSF RequestEditSession calls which can be slow in some apps
    // Note: must compare caretPos too, otherwise cursor movement (left/right arrow) gets skipped
    if (text == _lastCompositionText && caretPos == _lastCaretPos && _pComposition != nullptr)
    {
        WIND_LOG_DEBUG(L"UpdateComposition: Skipping duplicate (same text and caret)\n");
        return TRUE;
    }

    // Need a document manager
    ITfDocumentMgr* pDocMgr = nullptr;
    if (_pThreadMgr == nullptr || FAILED(_pThreadMgr->GetFocus(&pDocMgr)) || pDocMgr == nullptr)
    {
        WIND_LOG_ERROR(L"UpdateComposition: Failed to get DocMgr\n");
        return FALSE;
    }

    ITfContext* pContext = nullptr;
    HRESULT hr = pDocMgr->GetTop(&pContext);
    pDocMgr->Release();

    if (FAILED(hr) || pContext == nullptr)
    {
        WIND_LOG_ERROR(L"UpdateComposition: Failed to get Context\n");
        return FALSE;
    }

    CUpdateCompositionEditSession* pEditSession = new CUpdateCompositionEditSession(this, pContext, text, caretPos);

    // Timing: measure RequestEditSession duration
    LARGE_INTEGER startTime, endTime, freq;
    QueryPerformanceCounter(&startTime);
    QueryPerformanceFrequency(&freq);

    HRESULT hrSession;
    hr = pContext->RequestEditSession(_tfClientId, pEditSession, TF_ES_ASYNCDONTCARE | TF_ES_READWRITE, &hrSession);

    QueryPerformanceCounter(&endTime);
    int durationMs = (int)((endTime.QuadPart - startTime.QuadPart) * 1000 / freq.QuadPart);

    // Track if this was async (Weasel optimization pattern)
    _asyncEdit = (hrSession == TF_S_ASYNC);

    WIND_LOG_DEBUG_FMT(L"UpdateComposition: RequestEditSession hr=0x%08X, hrSession=0x%08X, async=%d, duration=%dms\n",
                 hr, hrSession, _asyncEdit ? 1 : 0, durationMs);

    pEditSession->Release();
    pContext->Release();

    // Update cache on success
    if (SUCCEEDED(hr))
    {
        _lastCompositionText = text;
        _lastCaretPos = caretPos;
    }

    return SUCCEEDED(hr);
}

// End composition
// NOTE: This method is now ASYNC - it returns immediately without waiting for
// the composition to actually end. The _pComposition pointer is cleared immediately
// so that HasActiveComposition() returns FALSE and new compositions can start.
void CTextService::EndComposition()
{
    LARGE_INTEGER startTime, endTime, freq;
    QueryPerformanceCounter(&startTime);
    QueryPerformanceFrequency(&freq);

    // Clear composition text cache
    _lastCompositionText.clear();
    _lastCaretPos = -1;

    // If there's no active composition, nothing to do
    if (_pComposition == nullptr)
    {
        WIND_LOG_DEBUG(L"EndComposition: No active composition\n");
        return;
    }

    WIND_LOG_DEBUG(L"EndComposition: Ending active composition\n");

    // CRITICAL: Transfer ownership of _pComposition immediately
    // This allows new compositions to start while the old one is being ended async
    ITfComposition* pCompToEnd = _pComposition;
    _pComposition = nullptr;  // Clear immediately - HasActiveComposition() now returns FALSE

    // Need a document manager to request edit session
    ITfDocumentMgr* pDocMgr = nullptr;
    if (_pThreadMgr == nullptr || FAILED(_pThreadMgr->GetFocus(&pDocMgr)) || pDocMgr == nullptr)
    {
        // Can't get document manager, force cleanup
        WIND_LOG_DEBUG(L"EndComposition: Can't get DocMgr, forcing cleanup\n");
        pCompToEnd->Release();
        return;
    }

    ITfContext* pContext = nullptr;
    HRESULT hr = pDocMgr->GetTop(&pContext);
    pDocMgr->Release();

    if (FAILED(hr) || pContext == nullptr)
    {
        // Can't get context, force cleanup
        WIND_LOG_DEBUG(L"EndComposition: Can't get Context, forcing cleanup\n");
        pCompToEnd->Release();
        return;
    }

    // Create edit session with ownership transfer of pCompToEnd
    CEndCompositionEditSession* pEditSession = new CEndCompositionEditSession(this, pCompToEnd);

    HRESULT hrSession;
    // Use TF_ES_ASYNCDONTCARE for non-blocking operation
    // The edit session will complete asynchronously, and pCompToEnd will be
    // released in DoEditSession or in ~CEndCompositionEditSession if the request fails
    hr = pContext->RequestEditSession(_tfClientId, pEditSession, TF_ES_ASYNCDONTCARE | TF_ES_READWRITE, &hrSession);

    QueryPerformanceCounter(&endTime);
    int durationMs = (int)((endTime.QuadPart - startTime.QuadPart) * 1000 / freq.QuadPart);
    WIND_LOG_DEBUG_FMT(L"EndComposition: RequestEditSession hr=0x%08X, hrSession=0x%08X, duration=%dms\n",
                 hr, hrSession, durationMs);

    if (FAILED(hr))
    {
        // Request failed - pEditSession destructor will release pCompToEnd
        WIND_LOG_DEBUG(L"EndComposition: RequestEditSession failed\n");
    }

    pEditSession->Release();
    pContext->Release();
}

// Insert text and start new composition (for top code commit)
BOOL CTextService::InsertTextAndStartComposition(const std::wstring& insertText, const std::wstring& newComposition)
{
    WIND_LOG_DEBUG_FMT(L"InsertTextAndStartComposition: insert='%s', newComp='%s', _pComposition=%p\n",
                 insertText.c_str(), newComposition.c_str(), _pComposition);

    // First, end any existing composition
    if (_pComposition != nullptr)
    {
        EndComposition();
    }

    // Need a document manager
    ITfDocumentMgr* pDocMgr = nullptr;
    if (_pThreadMgr == nullptr || FAILED(_pThreadMgr->GetFocus(&pDocMgr)) || pDocMgr == nullptr)
    {
        WIND_LOG_ERROR(L"InsertTextAndStartComposition: Failed to get DocMgr\n");
        return FALSE;
    }

    ITfContext* pContext = nullptr;
    HRESULT hr = pDocMgr->GetTop(&pContext);
    pDocMgr->Release();

    if (FAILED(hr) || pContext == nullptr)
    {
        WIND_LOG_ERROR(L"InsertTextAndStartComposition: Failed to get Context\n");
        return FALSE;
    }

    CInsertAndComposeEditSession* pEditSession = new CInsertAndComposeEditSession(this, pContext, insertText, newComposition);

    HRESULT hrSession;
    // Use TF_ES_SYNC to ensure synchronous execution
    hr = pContext->RequestEditSession(_tfClientId, pEditSession, TF_ES_SYNC | TF_ES_READWRITE, &hrSession);

    WIND_LOG_DEBUG_FMT(L"InsertTextAndStartComposition: RequestEditSession hr=0x%08X, hrSession=0x%08X\n", hr, hrSession);

    pEditSession->Release();
    pContext->Release();

    return SUCCEEDED(hr) && SUCCEEDED(hrSession);
}

// ============================================================================
// ITfDisplayAttributeProvider implementation
// ============================================================================

STDAPI CTextService::EnumDisplayAttributeInfo(IEnumTfDisplayAttributeInfo** ppEnum)
{
    if (ppEnum == nullptr)
        return E_INVALIDARG;

    *ppEnum = new CEnumDisplayAttributeInfo();
    return (*ppEnum != nullptr) ? S_OK : E_OUTOFMEMORY;
}

STDAPI CTextService::GetDisplayAttributeInfo(REFGUID guid, ITfDisplayAttributeInfo** ppInfo)
{
    if (ppInfo == nullptr)
        return E_INVALIDARG;

    *ppInfo = nullptr;

    if (IsEqualGUID(guid, c_guidDisplayAttributeInput))
    {
        *ppInfo = new CDisplayAttributeInfoInput();
        return (*ppInfo != nullptr) ? S_OK : E_OUTOFMEMORY;
    }

    return E_INVALIDARG;
}

// ============================================================================
// Display Attribute initialization
// ============================================================================

BOOL CTextService::_InitDisplayAttribute()
{
    // Get category manager
    ITfCategoryMgr* pCategoryMgr = nullptr;
    HRESULT hr = CoCreateInstance(CLSID_TF_CategoryMgr, nullptr, CLSCTX_INPROC_SERVER,
                                   IID_ITfCategoryMgr, (void**)&pCategoryMgr);
    if (FAILED(hr) || pCategoryMgr == nullptr)
    {
        WIND_LOG_ERROR(L"Failed to create category manager\n");
        return FALSE;
    }

    // Register display attribute GUID
    hr = pCategoryMgr->RegisterGUID(c_guidDisplayAttributeInput, &_gaDisplayAttributeInput);
    if (FAILED(hr))
    {
        WIND_LOG_ERROR(L"Failed to register display attribute GUID\n");
        pCategoryMgr->Release();
        return FALSE;
    }

    WIND_LOG_DEBUG_FMT(L"Display attribute registered, atom=%lu\n", (unsigned long)_gaDisplayAttributeInput);

    pCategoryMgr->Release();
    return TRUE;
}

void CTextService::_UninitDisplayAttribute()
{
    // Reset the GUID atom
    _gaDisplayAttributeInput = TF_INVALID_GUIDATOM;
}
