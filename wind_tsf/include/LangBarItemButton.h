#pragma once

#include <msctf.h>
#include <ctfutb.h>
#include <string>

#include "IconShmReader.h"

class CTextService;
struct ServiceResponse;

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

    // Thread-safe replace-backward from async thread (undo commit push):
    // delete `count` chars before caret then insert text on the UI thread
    void PostReplaceBackward(int count, const std::wstring& text);

    // Thread-safe pair commit from async thread (直通 ime.pair)：
    // 在 UI 线程上屏 text、左移 moveLeft 格并记一层待跳出深度
    void PostPairCommit(const std::wstring& text, uint32_t moveLeft);

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
    // 按键 / 焦点路径已建立服务端状态时调（TSF 线程）：停掉 SERVICE_READY 的待发同步，
    // 免得它晚到一步再发 IME_ACTIVATED、把用户刚切的中英状态重置。
    void CancelServiceReadyRetry();
    // focus_gained 发不出去时调（TSF 线程）：有限次定时补发 focus_gained，不必等用户按键。
    // 与 SERVICE_READY 重试分开：它只补焦点，不发 IME_ACTIVATED。
    void ScheduleFocusRecoveryRetry();

    // Thread-safe activation status from async reader thread.
    // 触发时机：Go 收到异步化后的 CmdIMEActivated / CmdFocusGained 完成 handler 后通过
    // push pipe 推送的 CMD_ACTIVATION_STATUS_PUSH。TSF 线程上调用 TextService 的
    // ApplyActivationStatusResponse, 等价于原同步 ReceiveResponse 路径的
    // _SyncStateFromResponse + _EnsureHostRenderSetup。
    void PostActivationStatus(const ServiceResponse& response);

    // Thread-safe icon-refresh request from async reader thread (CMD_REFRESH_ICON)。
    // 只发 OnUpdate(TF_LBI_ICON) 让系统重取图标，不碰任何状态字段——服务端换的是
    // 共享内存里的位图，DLL 这边没有任何东西需要跟着变。
    void PostRefreshIcon();

    // Thread-safe「标题采集开关刚由关变开」通知（async reader 线程调用）。TSF 线程上让
    // TextService 在仍有焦点、且上一条 focus_gained 没带标题时补发一次（带类名与标题）。
    void PostTitleMatchResync();

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
    // WM_SERVICE_READY 与其 TIMER_ID_SERVICE_READY 重试共用的同步判决（TSF 线程）。
    void _RunServiceReadySync(HWND hwnd);
    // TIMER_ID_FOCUS_RECOVERY 到点（TSF 线程）。
    void _RunFocusRecoveryRetry(HWND hwnd);
    static const UINT WM_UPDATE_STATUS;
    static const UINT WM_COMMIT_TEXT;
    static const UINT WM_CLEAR_COMPOSITION;
    static const UINT WM_UPDATE_COMPOSITION;
    static const UINT WM_SERVICE_READY;
    static const UINT WM_ACTIVATION_STATUS;
    static const UINT WM_REPLACE_BACKWARD;
    static const UINT WM_PAIR_COMMIT;
    static const UINT WM_REFRESH_ICON;
    static const UINT WM_TITLE_MATCH_RESYNC;

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

    // Data for replace-backward message (undo commit)
    struct ReplaceBackwardData {
        int count;
        std::wstring text;
    };

    // Data for pair-commit message (直通 ime.pair)
    struct PairCommitData {
        std::wstring text;
        uint32_t moveLeft;
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
    BOOL _bKeyboardDisabled;   // Keyboard disabled by system (线程级 compartment)
    BOOL _bDarkMode;           // System dark mode state (cached, updated on status change)
    // 最近一次 GetIcon 给的是加载中图标（SHM 不可用）。初值 TRUE：首个 GetIcon 之前
    // 没有任何服务端图标可言。tooltip 据此与图标保持一致。
    BOOL _bIconLoading;
    // 「进入加载中」那条日志是否记过；见 GetIcon。
    BOOL _bLoggedLoading;
    // SERVICE_READY 后 IME_ACTIVATED 已连续发送失败的次数（0 = 没有在重试）。
    // 组合态推迟不计入；新的 WM_SERVICE_READY、成功、无焦点时清零。
    UINT _serviceReadyFailures;
    // focus_gained 补发已失败的次数（0 = 没有在补发）。
    UINT _focusRecoveryFailures;

    // 模式主字。中文态取方案的 icon_label（"中"/"拼"/"五"/"双"），非中文态取
    // [ui.labels]（默认 "英"/"A"，用户可配，最多 2 个字符）。
    //
    // ⚠️ 容量不是"2 个字符 = 2 个 wchar + NUL = 3"那么算的：Rust 侧的上限是 2 个
    // **Unicode 标量值**，而一个标量值在 UTF-16 里可能占 2 个 wchar（surrogate pair，
    // 如 emoji）。最坏情况 2 char = 4 wchar + NUL = 5，原来的 [4] 装不下。
    // 取 8 与 STATUS_UPDATE_DATA::iconLabel 对齐，两个缓冲同进同出。
    wchar_t _inputTypeLabel[8];

    // 服务端预渲染图标的读端。取不到时 GetIcon 显示加载中图标（LoadingIcon.h），
    // 故本对象不可用**不是**错误状态（服务未启动时就是这样）。
    CIconShmReader _iconShm;

    // GUID for this language bar item
    static const GUID _guidLangBarItemButton;
};
