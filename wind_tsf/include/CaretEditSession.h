#pragma once

#include "Globals.h"

class CTextService;

// EditSession for getting caret position using TSF APIs
// This is required to call ITfContextView::GetTextExt which needs an edit cookie
class CCaretEditSession : public ITfEditSession
{
public:
    CCaretEditSession(ITfContext* pContext);
    ~CCaretEditSession();

    // IUnknown
    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj);
    STDMETHODIMP_(ULONG) AddRef();
    STDMETHODIMP_(ULONG) Release();

    // ITfEditSession
    STDMETHODIMP DoEditSession(TfEditCookie ec);

    // Execute the session and get caret position
    // Returns TRUE if successful, FALSE otherwise
    static BOOL GetCaretRect(ITfContext* pContext, RECT* prc);

    // Get the result after DoEditSession is called
    BOOL GetResult(RECT* prc);

private:
    LONG _refCount;
    ITfContext* _pContext;
    RECT _caretRect;
    BOOL _succeeded;
};
