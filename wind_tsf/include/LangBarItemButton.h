#pragma once

#include <msctf.h>
#include <ctfutb.h>
#include <string>

class CTextService;

// Language bar button for showing Chinese/English mode
class CLangBarItemButton : public ITfLangBarItemButton,
                           public ITfSource
{
public:
    CLangBarItemButton(CTextService* pTextService);
    ~CLangBarItemButton();

    // IUnknown
    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj);
    STDMETHODIMP_(ULONG) AddRef();
    STDMETHODIMP_(ULONG) Release();

    // ITfLangBarItem
    STDMETHODIMP GetInfo(TF_LANGBARITEMINFO* pInfo);
    STDMETHODIMP GetStatus(DWORD* pdwStatus);
    STDMETHODIMP Show(BOOL fShow);
    STDMETHODIMP GetTooltipString(BSTR* pbstrToolTip);

    // ITfLangBarItemButton
    STDMETHODIMP OnClick(TfLBIClick click, POINT pt, const RECT* prcArea);
    STDMETHODIMP InitMenu(ITfMenu* pMenu);
    STDMETHODIMP OnMenuSelect(UINT wID);
    STDMETHODIMP GetIcon(HICON* phIcon);
    STDMETHODIMP GetText(BSTR* pbstrText);

    // ITfSource
    STDMETHODIMP AdviseSink(REFIID riid, IUnknown* punk, DWORD* pdwCookie);
    STDMETHODIMP UnadviseSink(DWORD dwCookie);

    // Initialization
    BOOL Initialize();
    void Uninitialize();

    // Update the button when mode changes
    void UpdateLangBarButton(BOOL bChineseMode);

private:
    LONG _refCount;
    CTextService* _pTextService;
    ITfLangBarItemSink* _pLangBarItemSink;
    DWORD _dwCookie;
    BOOL _bChineseMode;

    // GUID for this language bar item
    static const GUID _guidLangBarItemButton;
};
