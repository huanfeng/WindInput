#include "CaretEditSession.h"
#include "TextService.h"
#include "Globals.h"

CCaretEditSession::CCaretEditSession(ITfContext* pContext)
    : _refCount(1)
    , _pContext(pContext)
    , _pComposition(nullptr)
    , _hasCompositionStart(FALSE)
    , _succeeded(FALSE)
{
    if (_pContext)
    {
        _pContext->AddRef();
    }
    ZeroMemory(&_caretRect, sizeof(_caretRect));
    ZeroMemory(&_compositionStartRect, sizeof(_compositionStartRect));
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
        WIND_LOG_ERROR(L"CaretEditSession: Context is null\n");
        return E_FAIL;
    }

    // Get the active view
    ITfContextView* pContextView = nullptr;
    HRESULT hr = _pContext->GetActiveView(&pContextView);
    if (FAILED(hr) || pContextView == nullptr)
    {
        WIND_LOG_ERROR(L"CaretEditSession: Failed to get active view\n");
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

            WIND_LOG_DEBUG_FMT(L"CaretEditSession: Got caret rect (%ld, %ld, %ld, %ld) clipped=%d\n",
                      _caretRect.left, _caretRect.top, _caretRect.right, _caretRect.bottom, clipped);
        }
        else
        {
            WIND_LOG_ERROR_FMT(L"CaretEditSession: GetTextExt failed hr=0x%08X\n", hr);
        }

        sel[0].range->Release();

        // If a composition is set, also get the start position of the composition range
        if (_succeeded && _pComposition != nullptr)
        {
            ITfRange* pCompRange = nullptr;
            hr = _pComposition->GetRange(&pCompRange);
            if (SUCCEEDED(hr) && pCompRange != nullptr)
            {
                // Clone the range and collapse to the start
                ITfRange* pStartRange = nullptr;
                hr = pCompRange->Clone(&pStartRange);
                if (SUCCEEDED(hr) && pStartRange != nullptr)
                {
                    pStartRange->Collapse(ec, TF_ANCHOR_START);
                    BOOL clippedComp = FALSE;
                    hr = pContextView->GetTextExt(ec, pStartRange, &_compositionStartRect, &clippedComp);
                    if (SUCCEEDED(hr))
                    {
                        _hasCompositionStart = TRUE;
                        WIND_LOG_DEBUG_FMT(L"CaretEditSession: Composition start (%ld, %ld)\n",
                                  _compositionStartRect.left, _compositionStartRect.bottom);
                    }
                    pStartRange->Release();
                }
                pCompRange->Release();
            }
        }
    }
    else
    {
        // No selection, try to get the end of the document or use insertion point
        WIND_LOG_DEBUG(L"CaretEditSession: No selection available\n");

        // Try to get screen extent as fallback
        hr = pContextView->GetScreenExt(&_caretRect);
        if (SUCCEEDED(hr))
        {
            // Use the top-left of the screen extent as a fallback
            _caretRect.right = _caretRect.left + 2;
            _caretRect.bottom = _caretRect.top + 20;
            _succeeded = TRUE;
            WIND_LOG_DEBUG(L"CaretEditSession: Using screen extent as fallback\n");
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

BOOL CCaretEditSession::GetCompositionStartResult(RECT* prc)
{
    if (_hasCompositionStart && prc)
    {
        *prc = _compositionStartRect;
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
        WIND_LOG_ERROR_FMT(L"RequestEditSession failed hr=0x%08X, hrSession=0x%08X\n", hr, hrSession);
    }

    pEditSession->Release();

    return result;
}

// Static method to get both caret rect and composition start rect
BOOL CCaretEditSession::GetCaretAndCompositionStartRect(ITfContext* pContext, ITfComposition* pComposition,
                                                         RECT* pCaretRect, RECT* pCompStartRect, BOOL* pHasCompStart)
{
    if (pContext == nullptr || pCaretRect == nullptr)
    {
        return FALSE;
    }

    CCaretEditSession* pEditSession = new CCaretEditSession(pContext);
    if (pEditSession == nullptr)
    {
        return FALSE;
    }

    // Set composition so DoEditSession will also query its start position
    pEditSession->SetComposition(pComposition);

    HRESULT hrSession = S_OK;
    HRESULT hr = pContext->RequestEditSession(
        TF_INVALID_COOKIE,
        pEditSession,
        TF_ES_SYNC | TF_ES_READ,
        &hrSession
    );

    BOOL result = FALSE;
    if (SUCCEEDED(hr) && SUCCEEDED(hrSession))
    {
        result = pEditSession->GetResult(pCaretRect);
        if (pCompStartRect && pHasCompStart)
        {
            *pHasCompStart = pEditSession->GetCompositionStartResult(pCompStartRect);
        }
    }
    else
    {
        WIND_LOG_ERROR_FMT(L"RequestEditSession failed hr=0x%08X, hrSession=0x%08X\n", hr, hrSession);
    }

    pEditSession->Release();
    return result;
}
