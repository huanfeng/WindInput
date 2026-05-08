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
                     public ITfCompositionSink,
                     public ITfDisplayAttributeProvider,
                     public ITfTextLayoutSink,
                     public ITfTextEditSink,
                     public ITfCompartmentEventSink
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
    BOOL _DocMgrHasEditableContext(ITfDocumentMgr* pDocMgr);

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

public:
    // Perform full state sync with Go service (sends IMEActivated + processes response).
    // Called after new/re-connection to ensure TSF and service state are consistent.
    void _DoFullStateSync();
    void TryRecoverFocusState();

    // Get display attribute GUID atom for composition
    TfGuidAtom GetDisplayAttributeInputAtom() { return _gaDisplayAttributeInput; }
};
