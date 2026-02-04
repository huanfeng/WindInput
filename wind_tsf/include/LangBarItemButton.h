#pragma once

#include <msctf.h>
#include <ctfutb.h>
#include <string>

class CTextService;

// Menu item IDs for language bar right-click menu
#define MENU_ID_TOGGLE_MODE      1
#define MENU_ID_TOGGLE_WIDTH     2
#define MENU_ID_TOGGLE_PUNCT     3
#define MENU_ID_TOGGLE_TOOLBAR   4
#define MENU_ID_OPEN_SETTINGS    5
#define MENU_ID_DICTIONARY       6
#define MENU_ID_ABOUT            7
#define MENU_ID_EXIT             8

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

    // Update the button when Caps Lock state changes
    void UpdateCapsLockState(BOOL bCapsLock);

    // Update both mode and Caps Lock state
    void UpdateState(BOOL bChineseMode, BOOL bCapsLock);

    // Update full status (called when receiving status_update from Go service)
    void UpdateFullStatus(BOOL bChineseMode, BOOL bFullWidth, BOOL bChinesePunct, BOOL bToolbarVisible, BOOL bCapsLock);

    // Thread-safe update from async thread (posts message to UI thread)
    void PostUpdateFullStatus(BOOL bChineseMode, BOOL bFullWidth, BOOL bChinesePunct, BOOL bToolbarVisible, BOOL bCapsLock);

    // Thread-safe commit text from async thread (posts message to UI thread)
    // This ensures EndComposition is called before InsertText on the correct thread
    void PostCommitText(const std::wstring& text);

    // Force refresh the language bar icon (used when focus is gained)
    void ForceRefresh();

private:
    // Message window for cross-thread updates
    HWND _hMsgWnd;
    static LRESULT CALLBACK _MsgWndProc(HWND hwnd, UINT msg, WPARAM wParam, LPARAM lParam);
    static const UINT WM_UPDATE_STATUS;
    static const UINT WM_COMMIT_TEXT;

    // Packed status for message passing
    struct StatusUpdateData {
        BOOL bChineseMode;
        BOOL bFullWidth;
        BOOL bChinesePunct;
        BOOL bToolbarVisible;
        BOOL bCapsLock;
    };

    // Data for commit text message
    struct CommitTextData {
        std::wstring text;
    };

    // Show popup menu manually (Windows 11 workaround)
    void _ShowPopupMenu(POINT pt);

    LONG _refCount;
    CTextService* _pTextService;
    ITfLangBarItemSink* _pLangBarItemSink;
    DWORD _dwCookie;
    BOOL _bChineseMode;
    BOOL _bCapsLock;        // Caps Lock state
    BOOL _bFullWidth;       // Full-width mode (全角)
    BOOL _bChinesePunct;    // Chinese punctuation mode (中文标点)
    BOOL _bToolbarVisible;  // Toolbar visibility

    // GUID for this language bar item
    static const GUID _guidLangBarItemButton;
};
