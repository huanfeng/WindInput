#pragma once

#include "Globals.h"
#include <string>

// Forward declarations
class CKeyEventSink;
class CIPCClient;
class CLangBarItemButton;
class CCaretEditSession;
class CDisplayAttributeProvider;
class CHotkeyManager;
class CHostWindow;
struct ServiceResponse;

class CTextService : public ITfTextInputProcessorEx,
                     public ITfThreadMgrEventSink,
                     public ITfThreadFocusSink,
                     public ITfCompositionSink,
                     public ITfDisplayAttributeProvider,
                     public ITfTextLayoutSink,
                     public ITfTextEditSink,
                     public ITfCompartmentEventSink,
                     // ITfCandidateListUIElementBehavior 已继承 ITfCandidateListUIElement (已继承 ITfUIElement)，
                     // 只列一个最派生的即可。
                     public ITfCandidateListUIElementBehavior,
                     // ITfFunctionProvider — 通过 ITfSourceSingle::AdviseSingleSink 注册自己为
                     // 该 IME 实例的 Function Provider。这是其它成熟 TSF IME 都做的事，
                     // 让 Chromium / QQNT 等宿主将我们识别为"完整 IME"，走 IME-first 调度。
                     public ITfFunctionProvider
{
    friend class CUpdateCompositionEditSession;
    friend class CEndCompositionEditSession;
    friend class CCommitTextEditSession;
    friend class CInsertAndComposeEditSession;
    friend class CInsertTextEditSession;
public:
    CTextService();
    ~CTextService();

    // IUnknown
    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj);
    STDMETHODIMP_(ULONG) AddRef();
    STDMETHODIMP_(ULONG) Release();

    // ITfTextInputProcessor
    STDMETHODIMP Activate(ITfThreadMgr* pThreadMgr, TfClientId tfClientId);
    STDMETHODIMP Deactivate();

    // ITfTextInputProcessorEx
    STDMETHODIMP ActivateEx(ITfThreadMgr* pThreadMgr, TfClientId tfClientId, DWORD dwFlags);

    // ITfThreadMgrEventSink
    STDMETHODIMP OnInitDocumentMgr(ITfDocumentMgr* pDocMgr);
    STDMETHODIMP OnUninitDocumentMgr(ITfDocumentMgr* pDocMgr);
    STDMETHODIMP OnSetFocus(ITfDocumentMgr* pDocMgrFocus, ITfDocumentMgr* pDocMgrPrevFocus);
    STDMETHODIMP OnPushContext(ITfContext* pContext);
    STDMETHODIMP OnPopContext(ITfContext* pContext);

    // ITfThreadFocusSink — 线程级焦点通知（应用进程 foreground 变化）。
    // 与 ITfThreadMgrEventSink::OnSetFocus（文档级别）不同。
    // 实现这个接口让我们在 TSF 注册表上看起来像"现代 IME"，让 Chromium / QQNT 等
    // 宿主走完整 IME-first 调度路径而非 fallback。
    STDMETHODIMP OnSetThreadFocus();
    STDMETHODIMP OnKillThreadFocus();

    // ITfUIElement — 候选 UI 元素基础接口。
    // 与 ITfCandidateListUIElement 一起使 IME 在 TSF 中表现为"现代 IME"，让
    // Chromium 类宿主走完整 IME-first 调度。当前用 stub 数据验证 Begin/EndUIElement
    // 注册本身是否影响调度。
    STDMETHODIMP GetDescription(BSTR* pbstrDescription);
    STDMETHODIMP GetGUID(GUID* pguid);
    STDMETHODIMP Show(BOOL bShow);
    STDMETHODIMP IsShown(BOOL* pbShow);

    // ITfCandidateListUIElement — 候选列表元数据（stub）。
    STDMETHODIMP GetUpdatedFlags(DWORD* pdwFlags);
    STDMETHODIMP GetDocumentMgr(ITfDocumentMgr** ppdim);
    STDMETHODIMP GetCount(UINT* puCount);
    STDMETHODIMP GetSelection(UINT* puIndex);
    STDMETHODIMP GetString(UINT uIndex, BSTR* pstr);
    STDMETHODIMP GetPageIndex(UINT* pIndex, UINT uSize, UINT* puPageCnt);
    STDMETHODIMP SetPageIndex(UINT* pIndex, UINT uPageCnt);
    STDMETHODIMP GetCurrentPage(UINT* puPage);

    // ITfCandidateListUIElementBehavior — 接收 TSF 对候选的操作（stub no-op）。
    STDMETHODIMP SetSelection(UINT nIndex);
    STDMETHODIMP Finalize(void);
    STDMETHODIMP Abort(void);

    // 候选可见状态变化时调用，控制 BeginUIElement / EndUIElement / UpdateUIElement.
    // hasCandidates: 新的候选可见状态。线程：与 KeyEventSink 状态变更同一线程。
    void NotifyCandidatesVisibilityChanged(BOOL hasCandidates);

    // ITfFunctionProvider — 把自己以 IID_ITfFunctionProvider 形式注册到 TSF 的
    // ITfSourceSingle（每个 IME 实例只有一个 function provider）。
    // 注意 GetDescription 与 ITfUIElement::GetDescription 同签名 (BSTR*)，
    // C++ 多继承合并为单一 vtable entry，复用同一实现即可（都是给宿主显示的字符串）。
    STDMETHODIMP GetType(GUID* pguid);
    STDMETHODIMP GetFunction(REFGUID rguid, REFIID riid, IUnknown** ppunk);

    // ITfCompositionSink
    STDMETHODIMP OnCompositionTerminated(TfEditCookie ecWrite, ITfComposition* pComposition);

    // ITfDisplayAttributeProvider
    STDMETHODIMP EnumDisplayAttributeInfo(IEnumTfDisplayAttributeInfo** ppEnum);
    STDMETHODIMP GetDisplayAttributeInfo(REFGUID guid, ITfDisplayAttributeInfo** ppInfo);

    // ITfTextLayoutSink
    STDMETHODIMP OnLayoutChange(ITfContext* pContext, TfLayoutCode lCode, ITfContextView* pView);

    // ITfTextEditSink
    STDMETHODIMP OnEndEdit(ITfContext* pContext, TfEditCookie ecReadOnly, ITfEditRecord* pEditRecord);

    // ITfCompartmentEventSink
    STDMETHODIMP OnChange(REFGUID rguid);

    // Get thread manager
    ITfThreadMgr* GetThreadMgr() { return _pThreadMgr; }

    // Get client ID
    TfClientId GetClientId() { return _tfClientId; }

    // Get IPC client
    CIPCClient* GetIPCClient() { return _pIPCClient; }

    // Get hotkey manager
    CHotkeyManager* GetHotkeyManager() { return _pHotkeyManager; }

    // Insert text into current context
    BOOL InsertText(const std::wstring& text);

    // Update composition text (Inline Composition)
    BOOL UpdateComposition(const std::wstring& text, int caretPos);

    // Commit text atomically (end composition + insert text in one EditSession)
    BOOL CommitText(const std::wstring& text);

    // End current composition.
    // pDocMgrHint: 失焦场景下 GetFocus() 已为 nullptr，调用方可传入 pDocMgrPrevFocus
    // 兜底，确保 composition 范围被清空后再 EndComposition；否则 Excel/WPS 等
    // 表格类宿主会把残留 composition 文本提交到目标 doc。
    void EndComposition(ITfDocumentMgr* pDocMgrHint = nullptr);

    // Reset KeyEventSink composing state (called after push pipe commit/clear)
    void ResetComposingState();

    // Insert text and start new composition (for top code commit)
    BOOL InsertTextAndStartComposition(const std::wstring& insertText, const std::wstring& newComposition);

    // Get and consume cached character before caret (set by ITfTextEditSink::OnEndEdit).
    // Returns the cached value and clears it to prevent stale values persisting across
    // key events in apps where OnEndEdit fires late or not at all (e.g., WeChat).
    WCHAR ConsumeCachedPrevChar() { WCHAR c = _cachedPrevChar; _cachedPrevChar = 0; return c; }

    // Get and send caret position to Go Service
    BOOL GetCaretPosition(LONG* px, LONG* py, LONG* pHeight);
    void SendCaretPositionUpdate();

    // Get caret position using TSF APIs (more accurate for browsers)
    BOOL GetCaretPositionFromTSF(LONG* px, LONG* py, LONG* pHeight);
    BOOL GetCompositionStartPosition(LONG* px, LONG* py);

    // Input mode control
    void ToggleInputMode();
    void SetInputMode(BOOL bChineseMode);  // Set mode from service response (no IPC)
    void HandleCtrlSpaceToggle();          // Handle Ctrl+Space internally (bypasses system compartment toggle)
    BOOL IsChineseMode() { return _bChineseMode; }
    BOOL IsFullWidth() { return _bFullWidth; }
    BOOL IsKeyboardDisabled() { return _bKeyboardDisabled; }
    ULONGLONG GetFocusSessionId() const { return _focusSessionId; }
    // 当前实例是否持有输入焦点（OnSetFocus 最后一次收到非 null 的 pDocMgrFocus）。
    // 用于服务重启时避免对无焦点实例触发工具栏显示。
    BOOL HasFocus() const { return _hasFocus; }
    // TRUE when the focused document manager has an editable (non-readonly,
    // non-transitory) context. FALSE when e.g. Chrome passes a doc manager
    // with no active text field (its context is TF_SD_READONLY).
    BOOL HasTextInputContext() const { return _hasTextInputContext; }
    // Lazy re-check via GetFocus() + _DocMgrHasEditableContext(). Updates and
    // returns _hasTextInputContext. Called from KeyEventSink when the cached
    // value is FALSE to handle late-arriving focus changes.
    BOOL RefreshTextInputContext();

    // Check if there's an active composition
    BOOL HasActiveComposition() { return _pComposition != nullptr; }

    // Clear the "composition just started" flag (used by timer fallback path).
    // 同时作废 EditSession 缓存：缓存是 StartComposition EditSession 内部抓的，
    // 那一刻宿主的 reflow 还没完成，缓存坐标是陈旧的。timer 触发时（reflow 已
    // 完成的时刻）必须强制 SendCaretPositionUpdate 走 GetCaretPosition 路径
    // 重新做 EditSession 查询，拿到 reflow 后的真实坐标。
    void ClearCompositionJustStarted()
    {
        _compositionJustStarted = FALSE;
        _hasCachedCaretPos = FALSE;
        _hasCachedCompStartPos = FALSE;
    }

    // Check if last edit session was async (Weasel optimization)
    BOOL IsAsyncEdit() { return _asyncEdit; }
    void ClearAsyncEdit() { _asyncEdit = FALSE; }

    // Update language bar Caps Lock state
    void UpdateCapsLockState(BOOL bCapsLock);

    // Send menu command to Go service
    void SendMenuCommand(const char* command);

    // Send show context menu request to Go service (screen coordinates)
    void SendShowContextMenu(int screenX, int screenY);

    // Update full status from Go service response
    // iconLabel: display text from Go service for taskbar icon (e.g., "中", "英", "A", "拼")
    void UpdateFullStatus(BOOL bChineseMode, BOOL bFullWidth, BOOL bChinesePunct, BOOL bToolbarVisible, BOOL bCapsLock, const wchar_t* iconLabel = nullptr);

private:
    LONG _refCount;
    ITfThreadMgr* _pThreadMgr;
    TfClientId _tfClientId;
    DWORD _dwThreadMgrEventSinkCookie;
    DWORD _dwThreadFocusSinkCookie;
    DWORD _uiElementId;     // ITfUIElementMgr::BeginUIElement 返回的 ID；TF_INVALID_UIELEMENTID 表示未注册
    BOOL  _uiElementShown;  // 当前 IsShown 返回值
    ITfUIElementMgr* _pUIElementMgr;  // 缓存的 UI element 管理器引用，避免每次候选变化都 QI
    ITfSourceSingle* _pSourceSingle;  // 缓存的 ITfSourceSingle 引用（Function Provider 注册用）
    BOOL  _funcProviderRegistered;    // 是否已通过 AdviseSingleSink 注册

    // Win32 RegisterHotKey 支持 — 在候选可见时把 Ctrl+0..9 / Ctrl+Shift+0..9 注册为
    // 系统级热键，由 OS 在 WM_KEYDOWN 派发之前直接消费，规避 QQNT 类 Chromium 宿主的
    // 加速键双处理。无候选时立即 UnregisterHotKey 让宿主使用这些热键。
    HWND  _hHotkeyWnd;                // 隐藏消息窗口，接收 WM_HOTKEY
    ATOM  _hotkeyWndClass;            // RegisterClassEx 返回的窗口类原子
    BOOL  _hotkeysActive;             // 当前是否已 RegisterHotKey 候选热键（Ctrl+0..9 / Ctrl+Shift+0..9）
    // 线程焦点门控：RegisterHotKey 在每个进程内对同一组合键独占。
    // 多进程 IME 实例同时尝试注册会导致 ERROR_HOTKEY_ALREADY_REGISTERED (1409)，
    // 让前台应用拿不到 WM_HOTKEY，反而让残留的后台进程吃掉。
    // 必须把所有 RegisterHotKey 与 thread focus 绑定：只有获得 thread focus 的
    // IME 实例才能注册，失去时立即全部卸载。
    BOOL  _hasThreadFocus;

    BOOL _InitHotkeyWindow();         // 创建窗口类 + 隐藏窗口
    void _UninitHotkeyWindow();       // 反向清理
    void _RegisterCandidateHotkeys(); // 注册 Ctrl+0..9 + Ctrl+Shift+0..9（候选可见时）
    void _UnregisterCandidateHotkeys();
    static LRESULT CALLBACK _HotkeyWndProc(HWND hWnd, UINT msg, WPARAM wParam, LPARAM lParam);
    DWORD _activateFlags;  // ActivateEx flags (TF_TMAE_SECUREMODE, etc.)

    // Components
    CKeyEventSink* _pKeyEventSink;
    CIPCClient* _pIPCClient;
    CLangBarItemButton* _pLangBarItemButton;
    CHotkeyManager* _pHotkeyManager;
    CHostWindow* _pHostWindow;

    // Input mode state
    BOOL _bChineseMode;
    BOOL _bFullWidth;
    BOOL _bKeyboardDisabled;   // GUID_COMPARTMENT_KEYBOARD_DISABLED
    ULONGLONG _focusSessionId;
    BOOL _hasFocus;             // 当前实例持有 TSF 输入焦点时为 TRUE（OnSetFocus 最后收到非 null pDocMgrFocus）
    BOOL _hasTextInputContext;  // TRUE when focused doc mgr has a real text-editing context (GetTextExt succeeds)

    // Composition
    ITfComposition* _pComposition;
    std::wstring _lastCompositionText;  // Cache to skip redundant updates
    int _lastCaretPos = -1;             // Cache caret position to detect cursor movement
    BOOL _asyncEdit;  // Track if last RequestEditSession returned TF_S_ASYNC (Weasel optimization)

    // Cached caret position from edit session (for WebView apps where separate
    // CaretEditSession with TF_INVALID_COOKIE may be rejected)
    RECT _cachedCaretRect;
    RECT _cachedCompStartRect;
    BOOL _hasCachedCaretPos;
    BOOL _hasCachedCompStartPos;
    // Weasel 模式：StartComposition 后第一次 SendCaretPositionUpdate 不立即发 IPC，
    // 改为等 OnLayoutChange（reflow 完成的权威信号）或 50ms timer 兜底。
    BOOL _compositionJustStarted;
    BOOL _needsFocusRecovery;
    LONG _lastFocusCaretX;
    LONG _lastFocusCaretY;
    LONG _lastFocusCaretHeight;
    BOOL _hasLastKnownCaretPos;
    LONG _lastKnownCaretX;
    LONG _lastKnownCaretY;
    LONG _lastKnownCaretHeight;

    // Display Attribute
    TfGuidAtom _gaDisplayAttributeInput;

    // ITfTextLayoutSink registration
    DWORD _dwLayoutSinkCookie;
    ITfContext* _pLayoutSinkContext;  // Context we registered the sink on
    void _AdviseTextLayoutSink(ITfContext* pContext);
    void _UnadviseTextLayoutSink();

    // Returns TRUE if pDocMgr has a non-null, writable, non-transitory top context.
    // Used to set _hasTextInputContext in OnSetFocus and RefreshTextInputContext.
    // Optional pDynFlagsOut receives dwDynamicFlags from TF_STATUS (0 if unavailable).
    BOOL _DocMgrHasEditableContext(ITfDocumentMgr* pDocMgr, DWORD* pDynFlagsOut = nullptr);

    // 读取焦点文档的 TSF InputScope 集合并编码为 bitmask（bit N = 枚举值 N 存在）。
    // 失败或无 InputScope 时返回 0。随 focus_gained 上报给 Go 端做密码框等决策。
    UINT64 _QueryInputScopeMask(ITfDocumentMgr* pDocMgr);

    // 判断焦点 context 是否被宿主置 GUID_COMPARTMENT_KEYBOARD_DISABLED（禁用输入法）。
    // Weasel/小狼毫用此判定密码框：Chromium 密码框置位、无痕普通框不置位，精确区分。
    bool _IsFocusKeyboardDisabled(ITfDocumentMgr* pDocMgr);

    // ITfTextEditSink registration
    DWORD _dwTextEditSinkCookie;
    ITfContext* _pTextEditSinkContext;  // Context we registered the sink on
    void _AdviseTextEditSink(ITfContext* pContext);
    void _UnadviseTextEditSink();

    // Cached character before caret (updated by OnEndEdit, consumed by KeyEventSink)
    WCHAR _cachedPrevChar;

    // Compartment event sink (GUID_COMPARTMENT_KEYBOARD_OPENCLOSE)
    DWORD _dwOpenCloseSinkCookie;
    BOOL _bInCompartmentChange;  // Guard against re-entrant OnChange

    BOOL _InitOpenCloseCompartment();
    void _UninitOpenCloseCompartment();
    BOOL _SetOpenCloseCompartment(BOOL bOpen);

    // Compartment event sink (GUID_COMPARTMENT_KEYBOARD_DISABLED)
    DWORD _dwKeyboardDisabledSinkCookie;

    BOOL _InitKeyboardDisabledCompartment();
    void _UninitKeyboardDisabledCompartment();

    // Compartment event sink (GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION)
    // 用 IME_CMODE_NATIVE 位向外界（KBLSwitch / 任务栏 / 第三方）表达中/英文状态。
    // OPENCLOSE 始终 TRUE 是内部约定（保证英文模式仍触发 OnTestKeyDown），
    // 真实的中英文模式由本 compartment 暴露。
    DWORD _dwConversionSinkCookie;
    BOOL _bInConversionChange;  // Guard against re-entrant OnChange for conversion compartment

    BOOL _InitConversionCompartment();
    void _UninitConversionCompartment();
    BOOL _SetConversionMode(BOOL bChinese);

    BOOL _InitThreadMgrEventSink();
    void _UninitThreadMgrEventSink();

    BOOL _InitKeyEventSink();
    void _UninitKeyEventSink();

    BOOL _InitIPCClient();
    void _UninitIPCClient();

    BOOL _InitLangBarButton();
    void _UninitLangBarButton();

    BOOL _InitDisplayAttribute();
    void _UninitDisplayAttribute();

    // State sync helper (internal): apply status response to local state
    void _SyncStateFromResponse(const ServiceResponse& response);
    void _EnsureHostRenderSetup(const ServiceResponse& response, BOOL forceRefresh);
    // 销毁宿主代理渲染窗口（释放共享内存映射 + 渲染线程 + Band 窗口）。
    // 仅在 Deactivate（IME 卸载）和 _EnsureHostRenderSetup（强制刷新/host render
    // 不可用）时调用。**不要**在失焦时调用：locked/transient DocMgr（SearchHost/任务
    // 管理器）会跳过 focus_gained，销毁后无法重建 → 候选永久不显示。失焦只需靠 Go 的
    // WriteHide 经本进程 event 隐藏窗口。空操作安全。
    void _DestroyHostWindow();

public:
    // Perform full state sync with Go service (sends IMEActivated + processes response).
    // Called after new/re-connection to ensure TSF and service state are consistent.
    void _DoFullStateSync();
    void TryRecoverFocusState();

    // ApplyActivationStatusResponse 应用一份从 push pipe 接收到的 activation status,
    // 等价于原同步路径 (_DoFullStateSync / TryRecoverFocusState) 收到 ReceiveResponse 后
    // 调 _SyncStateFromResponse + _EnsureHostRenderSetup 的组合动作。
    // 由 CLangBarItemButton::_MsgWndProc 在 WM_ACTIVATION_STATUS 上调用, 保证在 TSF 线程。
    void ApplyActivationStatusResponse(const ServiceResponse& response);

    // Get display attribute GUID atom for composition
    TfGuidAtom GetDisplayAttributeInputAtom() { return _gaDisplayAttributeInput; }
};
