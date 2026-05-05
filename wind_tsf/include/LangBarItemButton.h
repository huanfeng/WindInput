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

    // Update the button when keyboard disabled state changes
    void UpdateKeyboardDisabled(BOOL bDisabled);

    // Update both mode and Caps Lock state
    void UpdateState(BOOL bChineseMode, BOOL bCapsLock);

    // Update full status (called when receiving status_update from Go service)
    // iconLabel: display text from Go service (e.g., "中", "英", "A", "拼", "五")
    void UpdateFullStatus(BOOL bChineseMode, BOOL bFullWidth, BOOL bChinesePunct, BOOL bToolbarVisible, BOOL bCapsLock, const wchar_t* iconLabel = nullptr);

    // Thread-safe update from async thread (posts message to UI thread)
    void PostUpdateFullStatus(BOOL bChineseMode, BOOL bFullWidth, BOOL bChinesePunct, BOOL bToolbarVisible, BOOL bCapsLock, const wchar_t* iconLabel = nullptr);

    // Thread-safe commit text from async thread (posts message to UI thread)
    // This ensures EndComposition is called before InsertText on the correct thread
    void PostCommitText(const std::wstring& text);

    // Thread-safe clear composition from async thread (posts message to UI thread)
    // Used when mode is toggled via menu while there's an active composition
    void PostClearComposition();

    // Thread-safe update composition from async thread (posts message to UI thread)
    // Used for mouse click partial confirm in pinyin mode
    void PostUpdateComposition(const std::wstring& text, int caretPos);

    // Thread-safe service-ready notification from async reader thread.
    // Triggers _DoFullStateSync() on the TSF thread so the toolbar appears
    // after service restart without waiting for a focus/key event.
    void PostServiceReady();

    // Schedule a 50ms fallback caret retry on the TSF thread.
    // Used as a safety net when an app does not fire OnLayoutChange promptly.
    void PostDelayedCaretPositionUpdate();

    // Cancel a pending delayed caret retry (called when OnLayoutChange fires).
    void CancelDelayedCaretPositionUpdate();

    // Force refresh the language bar icon (used when focus is gained)
    void ForceRefresh();

    // Set the input method type label displayed in Chinese mode
    // label: "中"(default), "拼"(Pinyin), "五"(Wubi), "双"(Shuangpin), etc.
    void SetInputTypeLabel(const wchar_t* label);

private:
    // Message window for cross-thread updates
    HWND _hMsgWnd;
    static LRESULT CALLBACK _MsgWndProc(HWND hwnd, UINT msg, WPARAM wParam, LPARAM lParam);
    static const UINT WM_UPDATE_STATUS;
    static const UINT WM_COMMIT_TEXT;
    static const UINT WM_CLEAR_COMPOSITION;
    static const UINT WM_UPDATE_COMPOSITION;
    static const UINT WM_SERVICE_READY;

    // Packed status for message passing
    struct StatusUpdateData {
        BOOL bChineseMode;
        BOOL bFullWidth;
        BOOL bChinesePunct;
        BOOL bToolbarVisible;
        BOOL bCapsLock;
        wchar_t iconLabel[8];  // Icon label from Go service (e.g., "中", "英", "拼")
    };

    // Data for commit text message
    struct CommitTextData {
        std::wstring text;
    };

    // Data for update composition message
    struct UpdateCompositionData {
        std::wstring text;
        int caretPos;
    };

    // Show popup menu manually (Windows 11 workaround)
    void _ShowPopupMenu(POINT pt);

    LONG _refCount;
    CTextService* _pTextService;
    ITfLangBarItemSink* _pLangBarItemSink;
    DWORD _dwCookie;
    BOOL _bChineseMode;
    BOOL _bCapsLock;           // Caps Lock state
    BOOL _bFullWidth;          // Full-width mode (全角)
    BOOL _bChinesePunct;       // Chinese punctuation mode (中文标点)
    BOOL _bToolbarVisible;     // Toolbar visibility
    BOOL _bKeyboardDisabled;   // Keyboard disabled by system
    BOOL _bDarkMode;           // System dark mode state (cached, updated on status change)

    // Input method type label for Chinese mode display
    // Default: "中", future values: "拼"(Pinyin), "五"(Wubi), "双"(Shuangpin)
    wchar_t _inputTypeLabel[4];

    // GUID for this language bar item
    static const GUID _guidLangBarItemButton;
};
