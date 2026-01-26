#include "CaretEditSession.h"
#include "TextService.h"

CCaretEditSession::CCaretEditSession(ITfContext* pContext)
    : _refCount(1)
    , _pContext(pContext)
    , _succeeded(FALSE)
{
    if (_pContext)
    {
        _pContext->AddRef();
    }
    ZeroMemory(&_caretRect, sizeof(_caretRect));
}

CCaretEditSession::~CCaretEditSession()
{
    SafeRelease(_pContext);
}

STDAPI CCaretEditSession::QueryInterface(REFIID riid, void** ppvObj)
{
    if (ppvObj == nullptr)
        return E_INVALIDARG;

    *ppvObj = nullptr;

    if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfEditSession))
    {
        *ppvObj = (ITfEditSession*)this;
    }

    if (*ppvObj)
    {
        AddRef();
        return S_OK;
    }

    return E_NOINTERFACE;
}

STDAPI_(ULONG) CCaretEditSession::AddRef()
{
    return InterlockedIncrement(&_refCount);
}

STDAPI_(ULONG) CCaretEditSession::Release()
{
    LONG cr = InterlockedDecrement(&_refCount);

    if (cr == 0)
    {
        delete this;
    }

    return cr;
}

STDAPI CCaretEditSession::DoEditSession(TfEditCookie ec)
{
    _succeeded = FALSE;

    if (!_pContext)
    {
        OutputDebugStringW(L"[WindInput] CaretEditSession: Context is null\n");
        return E_FAIL;
    }

    // Get the active view
    ITfContextView* pContextView = nullptr;
    HRESULT hr = _pContext->GetActiveView(&pContextView);
    if (FAILED(hr) || pContextView == nullptr)
    {
        OutputDebugStringW(L"[WindInput] CaretEditSession: Failed to get active view\n");
        return hr;
    }

    // Get the current selection
    TF_SELECTION sel[1];
    ULONG fetched = 0;
    hr = _pContext->GetSelection(ec, TF_DEFAULT_SELECTION, 1, sel, &fetched);

    if (SUCCEEDED(hr) && fetched > 0 && sel[0].range != nullptr)
    {
        // Get the text extent of the selection (caret position)
        BOOL clipped = FALSE;
        hr = pContextView->GetTextExt(ec, sel[0].range, &_caretRect, &clipped);

        if (SUCCEEDED(hr))
        {
            _succeeded = TRUE;

            WCHAR debug[256];
            wsprintfW(debug, L"[WindInput] CaretEditSession: Got caret rect (%ld, %ld, %ld, %ld) clipped=%d\n",
                      _caretRect.left, _caretRect.top, _caretRect.right, _caretRect.bottom, clipped);
            OutputDebugStringW(debug);
        }
        else
        {
            WCHAR debug[128];
            wsprintfW(debug, L"[WindInput] CaretEditSession: GetTextExt failed hr=0x%08X\n", hr);
            OutputDebugStringW(debug);
        }

        sel[0].range->Release();
    }
    else
    {
        // No selection, try to get the end of the document or use insertion point
        OutputDebugStringW(L"[WindInput] CaretEditSession: No selection available\n");

        // Try to get screen extent as fallback
        hr = pContextView->GetScreenExt(&_caretRect);
        if (SUCCEEDED(hr))
        {
            // Use the top-left of the screen extent as a fallback
            _caretRect.right = _caretRect.left + 2;
            _caretRect.bottom = _caretRect.top + 20;
            _succeeded = TRUE;
            OutputDebugStringW(L"[WindInput] CaretEditSession: Using screen extent as fallback\n");
        }
    }

    pContextView->Release();

    return _succeeded ? S_OK : E_FAIL;
}

BOOL CCaretEditSession::GetResult(RECT* prc)
{
    if (_succeeded && prc)
    {
        *prc = _caretRect;
        return TRUE;
    }
    return FALSE;
}

// Static method to execute the edit session and get caret rect
BOOL CCaretEditSession::GetCaretRect(ITfContext* pContext, RECT* prc)
{
    if (pContext == nullptr || prc == nullptr)
    {
        return FALSE;
    }

    // Create edit session
    CCaretEditSession* pEditSession = new CCaretEditSession(pContext);
    if (pEditSession == nullptr)
    {
        return FALSE;
    }

    // Request edit session with read-only access
    // TF_ES_SYNC: Execute synchronously
    // TF_ES_READ: Read-only access (we don't need to modify anything)
    HRESULT hrSession = S_OK;
    HRESULT hr = pContext->RequestEditSession(
        TF_INVALID_COOKIE,  // We don't have a client ID here, use invalid
        pEditSession,
        TF_ES_SYNC | TF_ES_READ,
        &hrSession
    );

    BOOL result = FALSE;

    if (SUCCEEDED(hr) && SUCCEEDED(hrSession))
    {
        result = pEditSession->GetResult(prc);
    }
    else
    {
        WCHAR debug[128];
        wsprintfW(debug, L"[WindInput] RequestEditSession failed hr=0x%08X, hrSession=0x%08X\n", hr, hrSession);
        OutputDebugStringW(debug);
    }

    pEditSession->Release();

    return result;
}
