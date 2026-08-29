#pragma once

#include "Globals.h"
#include "BinaryProtocol.h" // HostWindowKind / HOST_WINDOW_KIND_COUNT for the host window array
// AsyncCaretResult / CaretProbeKind 按值出现在 OnAsyncCaretRectReady 签名里，需要完整定义。
// 反向不成立（CaretEditSession.h 只前置声明 CTextService），故无循环包含。
#include "CaretEditSession.h"
#include <string>
#include <mutex>
#include <vector>
#include <utility>

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
    friend class CReplaceBackwardEditSession;
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
    // noUnderline: 整段不设下划线显示属性（智能符号 HoldComposition 用，
    // 观感与已上屏文本一致；文本仍在组合态内，可被 press2 替换/超时提交）。
    BOOL UpdateComposition(const std::wstring& text, int caretPos, BOOL noUnderline = FALSE);

    // Commit text atomically (end composition + insert text in one EditSession)
    // nonKeyContext=TRUE：调用点**不在按键上下文**中（裸 WM_TIMER 回调、窗口消息回调、
    // COM 回调……）——改用异步编辑会话（TF_ES_ASYNCDONTCARE）且不走 SendInput 兜底，
    // 规避 Word 在这类上下文拒发同步会话（TS_E_SYNCHRONOUS）导致的编码泄漏 + 重复上屏。
    // MSDN 限定 TF_ES_SYNC 只在处理按键时合法，Word 严格照此校验。见 .cpp 注释。
    //
    // ★判据是「有没有按键上下文」，不是「谁调的」。曾写成 fromHoldTimer（只认智能符号
    // 超时那一个调用点），于是同样非按键上下文的 WM_COMMIT_TEXT（鼠标点候选）漏网，
    // 在 Word 里表现为「Sfge杜甫」——组合被孤儿 finalize + 正文 SendInput 重打。
    // 新增调用点时先问：我在 OnKeyDown 的调用栈里吗？不在就传 TRUE。
    //
    // replacingHeld=TRUE：本次提交要**替换**掉 hold 预览态里那个待定的中文符号
    // （智能符号 press2：「。」→「.」）。默认 FALSE = 追加语义，held 符号并入 prefix
    // 与本次文本一起上屏——因为提交用的是组合 range 的 SetText，不并入就会被覆盖掉。
    // 由服务端在 CommitText 响应的 flags bit3 显式声明，见 COMMIT_FLAG_REPLACING_HELD。
    BOOL CommitText(const std::wstring& text, BOOL nonKeyContext = FALSE,
                    BOOL replacingHeld = FALSE);

    // `CommitText(text, TRUE, ...)` 的替代：不直接在当前（非按键）调用栈里发起异步
    // EditSession，而是缓冲文本 + 合成一个触发键，把真正的提交挪到 OnKeyDown 里、
    // 以**按键上下文**同步提交——TF_ES_SYNC 合法，宿主自身的输入时处理链路（含内嵌
    // `\n` 转真实分段）才会被触发。用于鼠标点候选等 push 提交路径（见 .cpp 调用点）。
    //
    // 触发键队列是 KeyEventSink 自己的成员，实现留在那边，这里只做转发 + 失败兜底
    // （同 HandlePairCommitPush 的先例：消息窗只拿得到 TextService）。合成按键注入
    // 失败（KeyEventSink 未装配 / SendInput 出错）时退回旧的 `CommitText(text, TRUE, ...)`
    // 直接异步提交，保证至少不丢字。
    BOOL CommitTextViaSyntheticKey(const std::wstring& text, BOOL replacingHeld = FALSE);

    // 把光标前 count 个已上屏字符替换为 text（智能符号纠错替换）。
    // 优先走 TSF 同步 EditSession（原子、不受输入队列时序/修饰键影响）；
    // 失败时回退到 SendInput（count 次 Backspace + Unicode 注入 text）。
    BOOL ReplacePrecedingChars(int count, const std::wstring& text);

    // 直通 ime.pair 推送落点：转发给 KeyEventSink 上屏 + 左移 + 记一层待跳出深度。
    // 深度是它自己的成员，故实现留在那边，这里只做转发（消息窗只拿得到 TextService）。
    void HandlePairCommitPush(const std::wstring& text, uint32_t moveLeft);

    // End current composition.
    // pDocMgrHint: composition 所属的 DocMgr。**给出即权威**——实现不会再去问 GetFocus()，
    // 因为收口时机可能晚于焦点转移（doc_changed 路径），那时 GetFocus() 指向的是新文档，
    // 拿它跑 EditSession 会用新 context 的 cookie 去清旧 context 的 range。
    // 不给则回落 GetFocus()（其余调用点都在焦点未变时触发）。
    // 清空 composition 范围后再 EndComposition，否则 Excel/WPS 等表格类宿主会把残留
    // composition 文本提交到目标 doc。
    //
    // nonKeyContext: 语义同 CommitText 的同名参数。本方法自身用的就是异步会话（不受
    // 影响），但顶码聚合中它会转调 CommitText 收口前缀——那一步默认走同步会话，在非
    // 按键上下文里会被 Word 拒。判据传不进去就等于把同一个坑留在这条支路上。
    void EndComposition(ITfDocumentMgr* pDocMgrHint = nullptr, BOOL nonKeyContext = FALSE);

    // Reset KeyEventSink composing state (called after push pipe commit/clear)
    // keepPairState=TRUE 时保留自动配对状态，语义见 CKeyEventSink::ResetComposingState。
    void ResetComposingState(BOOL keepPairState = FALSE);

    // 输入态整体清理：结束 composition + 通知服务端清 buffer + 复位 KeyEventSink 会话态。
    // 触发时机**不是**「失去焦点」而是「离开了原来那个文档」——失焦那一刻无从区分抖动
    // 与真正的切换（见 OnSetFocus 判据注释）。两条进入路径共用本函数（OnKillThreadFocus /
    // doc_changed），靠 _focusLostSent 去重。pDocMgrHint 传**离开的那个 doc**（composition
    // 就建在它上面），EndComposition 会直接采信它而不再问 GetFocus()——此刻焦点可能已经
    // 在新文档上了。
    // reason 取 FOCUS_LOST_REASON_*（THREAD / DOC_CHANGED），决定服务端清哪些状态。
    // sendFocusLost=FALSE 时只做本地清理、不通知服务端失焦：新 DocMgr 若会被
    // XamlIsland locked 守卫跳过 focus_gained，发出去的 focus_lost 就没有配对者，
    // 服务端 ime_active 会被永久清掉（实测 explorer 地址栏工具栏消失）。
    void CleanupInputStateForDocChange(ITfDocumentMgr* pDocMgrHint, uint8_t reason,
                                       BOOL sendFocusLost = TRUE);

    // 焦点离开可编辑控件时通知服务端隐藏工具栏（发 FOCUS_LOST_REASON_CTX_LOST）。
    // **只翻可见性标志，不碰输入态**——这是它能在 DocMgr 噪声层安全调用的前提，
    // 实现处有完整说明。靠 _editCtxReported 去重。
    void _ReportEditContextLost();

    // Top-code commit: accumulate the committed text into the pending prefix and
    // keep it INSIDE the composition (Microsoft IME behavior — the real document
    // commit is deferred to the final CommitText). See _pendingCommitPrefix.
    BOOL InsertTextAndStartComposition(const std::wstring& insertText, const std::wstring& newComposition);
    // 新开组合时插入点的位置：常规放末尾，**占位组合（单个空格）放 0**。
    // 与 Rust 侧 COMPOSITION_PLACEHOLDER 成对，见实现处的说明。
    static int _CompositionCaretFor(const std::wstring& composition);

    // Length (in wchars) of the pending top-code commit prefix shown at the head
    // of the composition. Used to segment display attributes and to offset the
    // composition-start coordinate reported to the engine (candidate anchor).
    size_t GetPendingCommitPrefixLength() const { return _pendingCommitPrefix.length(); }

    // 把「已决定要提交」的文本并入待提交前缀，但不结束组合、不真提交（真提交推迟到
    // 最终 CommitText）。用于智能标点顶屏的聚合：候选并入 prefix、中文符号仍作 held 放
    // 同一组合，规避「真提交+立即重开组合」被 diff 式宿主（微信/Tabby/终端）误读吞字。
    // 只并入承诺提交的候选——held 符号勿并入（press2 要替换它，见 CommitAndHold 处注释）。
    void PinCommitTextToPrefix(const std::wstring& text) { _pendingCommitPrefix += text; }

    // Get and consume cached character before caret (set by ITfTextEditSink::OnEndEdit).
    // Returns the cached value and clears it to prevent stale values persisting across
    // key events in apps where OnEndEdit fires late or not at all (e.g., WeChat).
    WCHAR ConsumeCachedPrevChar() { WCHAR c = _cachedPrevChar; _cachedPrevChar = 0; return c; }

    // Get and send caret position to Go Service
    // pSource: 非空时输出命中的是回退链的哪一级（CARET_SRC_*）。**这条链的每一级语义都不同**，
    //          TSF 坐标与 GUI 光标分属两个域，混为一谈正是候选窗错位的历史根因。
    BOOL GetCaretPosition(LONG* px, LONG* py, LONG* pHeight, int* pSource = nullptr);
    void SendCaretPositionUpdate();

    // 非按键上下文（WM_TIMER 等）专用：用异步 edit session 取坐标，结果经
    // OnAsyncCaretRectReady 回调发出。同步锁在这些上下文里会被宿主合法拒绝
    // （TS_E_SYNCHRONOUS），详见 CCaretEditSession::RequestCaretRectAsync。
    // kind 决定回调怎么处理结果：Composition 发正式 caret_update；
    // FirstShowProbe 只发 CMD_CARET_PROBE（wait 档忽略），用于零风险观测时序。
    BOOL RequestCaretPositionUpdateAsync(CaretProbeKind kind = CaretProbeKind::Composition);

    // OnSetFocus 专用：焦点刚到达、**尚无 composition** 时取一次插入点。
    //
    // 与上面那个的差别不只是"有没有组合"：焦点路径的回调不能用 `_pComposition` 判活
    // （它恒为 null），改用 _focusSessionId 判归属；拿到的坐标也不走组合定位，而是补一条
    // CMD_CARET_UPDATE 更新服务端缓存，供状态气泡（ui.status.show_on_focus）落座。
    //
    // 返回 TRUE 仅表示请求已受理。内联档（记事本实测 hrSession=S_OK）回调会在本函数**返回前**
    // 跑完，届时 _lastFocusCaretX/Y 已被刷成 TSF 权威值，可直接随 focus_gained 一起发出去；
    // 排队档（Word 实测 TF_S_ASYNC，1~2ms）则晚于 focus_gained 到达，走补发通道。
    BOOL RequestFocusCaretAsync(ITfDocumentMgr* pDocMgrFocus);

    // 异步 edit session 的结果回调（由 CCaretEditSession 调用）
    void OnAsyncCaretRectReady(const AsyncCaretResult& result);

    // Get caret position using TSF APIs (more accurate for browsers)
    // pUsedCompStart: 非空时输出「caret 是否由组合起点降级顶替」，用于区分两种 TSF 来源
    BOOL GetCaretPositionFromTSF(LONG* px, LONG* py, LONG* pHeight, BOOL* pUsedCompStart = nullptr);
    BOOL GetCompositionStartPosition(LONG* px, LONG* py);

    // Input mode control
    void ToggleInputMode();
    void SetInputMode(BOOL bChineseMode);  // Set mode from service response (no IPC)
    BOOL IsChineseMode() { return _bChineseMode; }
    // 读取 OPENCLOSE compartment 的**真实**当前值（0/1；读取失败返回 -1）。
    // 诊断用：我们始终尝试把它拉回 1，但 _SetOpenCloseCompartment 不保证成功，
    // 镜像态并不可信，只有回读才算数。调用含两次 COM，**热路径上必须先用
    // CFileLogger::IsEnabled 门控**——日志宏的实参在调用点即求值，不会被级别短路。
    LONG GetOpenCloseCompartmentValue();
    BOOL IsFullWidth() { return _bFullWidth; }
    // 软键盘面板是否开着（由服务端经 statusFlags 推送）。见 KeyEventSink 的数字键分支。
    BOOL IsSoftKeyboard() { return _bSoftKeyboard; }
    // 当前面是**键盘面**：按键交还输入法，软键盘总闸只留 Esc 与翻页。
    BOOL IsSoftKeyboardKeys() { return _bSoftKeyboardKeys; }
    BOOL IsKeyboardDisabled() { return _bKeyboardDisabled; }
    // 密码框强制英文抑制当前是否生效（**镜像** core 的 `apply_input_diag`：命中密码
    // InputScope 位 + compartment 未禁用 + 策略开关开）。DLL 必须能自行判定：吃键决策在
    // OnTestKeyDown 完成，早于 IPC，仅靠 core 回 PassThrough 会「吃了再吐」丢键。
    BOOL IsPasswordSuppressActive() const;
    void SetPasswordSuppressEnabled(BOOL bEnabled) { _passwordSuppressEnabled = bEnabled; }
    // 诊断快照采集开关（core 经 CONFIG_KEY_DIAG_SNAPSHOT 推；默认关）。
    void SetDiagSnapshotEnabled(BOOL bEnabled) { _diagSnapshotEnabled = bEnabled; }

    // 语言栏悬停提示（core 经 CONFIG_KEY_LANGBAR_TOOLTIP 推）。文案与选择逻辑全在服务端，
    // 这里只存一份原样交给 CLangBarItemButton::GetTooltipString。
    //
    // ⚠ 必须加锁：config 回调跑在 IPC 读线程，而 GetTooltipString 由系统在 TSF 线程调用。
    // 同类的 SetPasswordSuppressEnabled 之所以裸赋值，是因为 BOOL 的读写天然原子——
    // std::wstring 不是，跨线程裸写会让读方拿到半个字符串。
    void SetLangBarTooltip(const std::wstring& text)
    {
        std::lock_guard<std::mutex> lk(_langBarTooltipMutex);
        _langBarTooltip = text;
    }
    std::wstring GetLangBarTooltip() const
    {
        std::lock_guard<std::mutex> lk(_langBarTooltipMutex);
        return _langBarTooltip;
    }
    // 采集并上报一次诊断快照。开关关闭时**立即返回**，一次 Win32 调用都不做——
    // 采集本身要查三次窗口类名 + band，只有排查时才值得付这个开销。
    // docMgrChanged 由调用方给出（只有 OnSetFocus 知道自己是不是换了文档）。
    void SendDiagSnapshotIfEnabled(ITfDocumentMgr* pDocMgr, BOOL docMgrChanged);

    // 焦点窗口解析（TSF view → GUI thread，**不含**前台窗口兜底）。诊断快照与
    // focus_gained 的窗口类上报共用它——同一判据写两处必漂移。详见实现处注释。
    HWND _ResolveFocusWindow(ITfDocumentMgr* pDocMgr, uint8_t* pSrcOut, uint64_t* pCtxIdOut);
    // 焦点所在顶层窗口的类名，随 focus_gained 上报，供服务端区分壳的过渡型 / 停留型窗口。
    std::wstring _QueryFocusRootWindowClass(ITfDocumentMgr* pDocMgr);
    ULONGLONG GetFocusSessionId() const { return _focusSessionId; }
    // 记录 CapsLock 按键活动时刻（物理按键或服务端 cancel_on_mode_switch 的注入）。
    // Windows 输入系统会在 CapsLock 状态变化后联动写 OPENCLOSE compartment；
    // OnCompartmentChange 据此时间戳抑制该联动噪声，防止被误判为用户模式切换。
    void NoteCapsLockKeyActivity() { _lastCapsKeyTick = GetTickCount64(); }
    // 按键侧兜底的中英切换（Ctrl+Space）。仅在系统未把该键当作 IME 热键时由
    // CKeyEventSink::OnKeyDown 调用，与 OPENCLOSE compartment 路径天然互斥。
    BOOL ToggleModeFromKey();
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

    // HoldComposition: 开启组合显示 text，timeoutMs 毫秒后自动提交中文（智能符号方案）。
    // press2 到来前的任何 CommitText 调用会先通过 CancelHoldTimer 取消定时器。
    BOOL HoldComposition(const std::wstring& text, UINT timeoutMs);

    // 取消 HoldComposition 计时器（若活跃）。安全：_hHoldTimer==0 时为空操作。
    void CancelHoldTimer();

    // 若 HoldComposition 计时器活跃，立即提交中文符号（宿主中断组合时调用，如 PassThrough 键）。
    // nonKeyContext: 语义同 CommitText 的同名参数，透传给收口用的 OnHoldTimerExpired。
    void FlushHoldCompositionIfActive(BOOL nonKeyContext = FALSE);

    // HoldComposition 计时器是否活跃 ⇔ 组合内只有待定的中文符号（外加已承诺提交的 prefix），
    // 不含任何编码——「智能符号预览态」的精确判据。
    // ⚠️ 判据只能是计时器：`_pendingCommitPrefix` 非空在顶码 pre_confirm 聚合时同样成立，
    // 那是真输入会话，拿它当判据会把顶码路径一并误判掉。
    BOOL IsHoldCompositionActive() const { return _hHoldTimer != 0; }

    // 若 HoldComposition 计时器活跃，把 held 符号定格并入 _pendingCommitPrefix（不 commit、
    // 不动文档），供"定格旧符号 + 立即更新/开启组合"场景（连续智能符号、符号后快速输入）
    // 在单一 EditSession 内完成显示更新——规避「commit+立即重启组合」在 Chromium/WPS
    // 下被整锁 diff 误读成替换（与顶码聚合 7f616c2 同思路）。最终 CommitText 一次收口。
    void AbsorbHeldIntoPrefix();

    // direct_commit 顶码：真提交后，余码新组合延迟到触发键 keyup（或兜底定时器）才开。
    // 与 HoldComposition 计时器状态并列、互不干扰。见 top-commit-mode 设计文档 §5。
    void StashDeferredComposition(const std::wstring& composition, UINT fallbackMs);
    void StartDeferredCompositionIfPending();   // keyup / 兜底定时器 / flush 统一入口
    void CancelDeferredComposition();
    BOOL HasDeferredComposition() const { return !_deferredCompText.empty(); }

private:
    // 坐标出口：DPI 归一 + 更新 last known + 发 IPC。同步/异步两条取坐标路径共用。
    void _EmitCaretUpdate(LONG x, LONG y, LONG height, LONG compStartX, LONG compStartY, int source);

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

    // Win32 RegisterHotKey 支持 — 在候选可见时把置顶/删词热键（组合键取自服务端
    // SESSION 热键表，即 keys.pin_candidate / keys.delete_candidate）注册为系统级热键，
    // 由 OS 在 WM_KEYDOWN 派发之前直接消费，规避 QQNT 类 Chromium 宿主的加速键双处理。
    // 无候选时立即 UnregisterHotKey 让宿主使用这些热键。
    HWND  _hHotkeyWnd;                // 隐藏消息窗口，接收 WM_HOTKEY
    ATOM  _hotkeyWndClass;            // RegisterClassEx 返回的窗口类原子
    BOOL  _hotkeysActive;             // 当前是否已 RegisterHotKey 候选热键（组合键取自服务端 SESSION 热键表）
    // 加词热键（Ctrl+= 等）全局拦截：门卫比候选热键更严——中文模式 + 焦点在可编辑文本框 +
    // 非密码框 + 持有 thread focus 才注册，让抢占面积最小化，不干扰非文本框处的宿主快捷键。
    BOOL  _addWordHotkeysActive;      // 当前是否已 RegisterHotKey 加词热键
    bool  _focusIsPassword;           // 当前焦点是否密码框（KEYBOARD_DISABLED）；密码框不注册加词热键
    // 当前焦点的 InputScope 掩码（与 focus_gained / CMD_INPUT_STATE_REPORT 上报的同值）。
    // 上报给 core 之外自己也留一份：IsPasswordSuppressActive 的吃键门控须本地可算。
    UINT64 _focusInputScopeMask;
    BOOL  _passwordSuppressEnabled;   // 抑制策略开关（core 经 CONFIG_KEY_PASSWORD_SUPPRESS 推；默认开）
    // 诊断快照采集开关（core 经 CONFIG_KEY_DIAG_SNAPSHOT 推；**默认关**）。
    // 默认关是硬要求：每次焦点切换都查三次类名 + band 的开销，不该由不排查的用户承担。
    BOOL  _diagSnapshotEnabled;
    // 语言栏悬停提示文本与它的锁。空 = 尚未收到服务端推送（GetTooltipString 回落到本地
    // 默认文案）；握手时服务端必推一次，故空只出现在连接建立前的极短窗口。
    std::wstring _langBarTooltip;
    mutable std::mutex _langBarTooltipMutex;
    // 已注册的加词热键 (RegisterHotKey id, raw hash)。raw hash 高16位=KEYMOD、低16位=VK，
    // 供 UnregisterHotKey 与 WM_HOTKEY 分发反解。最多两项（add_word / open_add_word_dialog）。
    std::vector<std::pair<int, uint32_t>> _addWordHotkeyIds;
    // 已注册的候选热键 (RegisterHotKey id, raw hash)，与 _addWordHotkeyIds 同格式同用途。
    // 组合键不再由本层决定，一律来自服务端推来的 SESSION 热键表（CHotkeyManager::SessionHotkeys）。
    std::vector<std::pair<int, uint32_t>> _candidateHotkeyIds;
    // 线程焦点门控：RegisterHotKey 在每个进程内对同一组合键独占。
    // 多进程 IME 实例同时尝试注册会导致 ERROR_HOTKEY_ALREADY_REGISTERED (1409)，
    // 让前台应用拿不到 WM_HOTKEY，反而让残留的后台进程吃掉。
    // 必须把所有 RegisterHotKey 与 thread focus 绑定：只有获得 thread focus 的
    // IME 实例才能注册，失去时立即全部卸载。
    // 本应用是否持有 TSF 线程焦点。**权威来源只有 ITfThreadFocusSink**
    // （OnSetThreadFocus / OnKillThreadFocus），外加 _InitHotkeyWindow 里的初始种子
    // （TSF 只在 transition 时回调，激活时已在前台的场景收不到通知）。
    // 用途：过滤 compartment 变化噪声——判断「这次 OPENCLOSE/CONVERSION 变化是不是
    // 本应用前台时发生的」。**不要**用 GetForegroundWindow 的 pid 去纠正它，见
    // _isProcessForeground 的说明。
    BOOL  _hasThreadFocus;
    // 本**进程**是否是前台窗口所属进程（GetForegroundWindow 的 pid == 自己）。
    // 仅用于热键注册门卫：多个 IME 实例争抢同一组全局热键会引发
    // ERROR_HOTKEY_ALREADY_REGISTERED(1409)，只有真正拥有前台窗口的那个进程该注册。
    //
    // 与 _hasThreadFocus 必须分开的原因（2026-08-04 DBX/WebView 实测）：多进程宿主里
    // 前台窗口属于渲染进程、TSF 却加载在另一个进程里，两者 pid 不同。此时
    //   本进程是前台进程？ 否 → 不该抢热键          （本字段 = FALSE，正确）
    //   本应用在前台？     是 → compartment 变化是真实用户操作（_hasThreadFocus = TRUE，正确）
    // 同一场景两个判据期望相反。此前共用一个变量，自检定时器按前者把它清零，
    // 导致 OnChange 的 !_hasThreadFocus 早退恒成立、该宿主里中英切换完全失效。
    BOOL  _isProcessForeground;

    BOOL _InitHotkeyWindow();         // 创建窗口类 + 隐藏窗口
    void _UninitHotkeyWindow();       // 反向清理
    void _RegisterCandidateHotkeys(); // 候选可见时注册服务端 SESSION 热键表里的组合键
    void _UnregisterCandidateHotkeys();
    // 加词热键：Reevaluate 可从任意线程调用（内部 PostMessage 到 _hHotkeyWnd 保证在
    // 拥有该窗口的线程执行 RegisterHotKey）；_DoReevaluate/_Register/_Unregister 仅主线程。
    void _ReevaluateAddWordHotkey();   // 线程安全入口：post 消息触发重新评估
    void _DoReevaluateAddWordHotkey(); // 主线程：按门卫条件注册/注销
    void _RegisterAddWordHotkeys();
    void _UnregisterAddWordHotkeys();
    // 中英模式集中 setter：赋值 _bChineseMode 并触发加词热键重评（模式变化是门卫条件之一）。
    // 可从 async reader 线程调用（reeval 内部 post 到窗口线程）。
    void _SetChineseMode(BOOL v);
    // 应用一次中英模式切换（刷统计 / 结束组合 / 通知服务端 / 落模式与两个 compartment）。
    // compartmentAlreadySet=TRUE：OPENCLOSE 已由系统或宿主写成 requestedMode，仅在服务端
    // 仲裁出不同值时回写；FALSE：我们是发起方，必须无条件写。source 只进日志。
    HRESULT _ApplyModeSwitch(BOOL requestedMode, BOOL compartmentAlreadySet, const WCHAR* source);
    static LRESULT CALLBACK _HotkeyWndProc(HWND hWnd, UINT msg, WPARAM wParam, LPARAM lParam);
    DWORD _activateFlags;  // ActivateEx flags (TF_TMAE_SECUREMODE, etc.)

    // Components
    CKeyEventSink* _pKeyEventSink;
    CIPCClient* _pIPCClient;
    CLangBarItemButton* _pLangBarItemButton;
    CHotkeyManager* _pHotkeyManager;
    // One host band window per kind (candidate / tooltip / status). Indexed by
    // HostWindowKind. _pHostWindow[HOST_WINDOW_CANDIDATE] is the candidate window
    // (also the z-order owner of the tooltip/status windows).
    CHostWindow* _pHostWindow[HOST_WINDOW_KIND_COUNT];

    // Input mode state
    BOOL _bChineseMode;
    BOOL _bFullWidth;
    BOOL _bSoftKeyboard;
    BOOL _bSoftKeyboardKeys;
    BOOL _bKeyboardDisabled;   // GUID_COMPARTMENT_KEYBOARD_DISABLED
    ULONGLONG _focusSessionId;
    BOOL _hasFocus;             // 当前实例持有 TSF 输入焦点时为 TRUE（OnSetFocus 最后收到非 null pDocMgrFocus）
    BOOL _hasTextInputContext;  // TRUE when focused doc mgr has a real text-editing context (GetTextExt succeeds)

    // 焦点抖动免疫（见 TextService.cpp OnSetFocus 的判据注释）：缓存上一个真正活跃的
    // DocMgr，用于区分「同一文档抖回来」与「换了文档」。持 AddRef 保活是必须的——
    // 裸指针在旧对象释放后可能被新对象复用同一地址，导致「换了文档」被误判成抖动。
    ITfDocumentMgr* _pLastActiveDocMgr;
    // 上一次**获焦**的 DocMgr，含 locked/transient（XamlIsland）。仅用于判断「同一个 doc
    // 抖回来」，不参与换文档收口。
    //
    // ⚠ 为什么不能复用 _pLastActiveDocMgr 做这个判断：那个缓存**刻意排除** transient
    // DocMgr（理由见 OnSetFocus 的更新处），于是 transient 永远等不到自己，`isSameDocMgr`
    // 恒为假 ⇒ 每次焦点抖动都被判成「换了文档」⇒ 收口时 EndComposition 终止正在进行的
    // 组合，已写入宿主的 preedit 就留在了那里。实测 explorer 地址栏：同一个 DocMgr
    // (0x219BC540) 连续三次获焦全报 sameDoc=0，首字母因此上屏（第二键的 prevChar=0x73='s'
    // 就是它留下的痕迹）。
    //
    // 两者分工：本字段回答「是不是同一个 doc 在抖」，_pLastActiveDocMgr 回答「上一个**真实**
    // 文档是谁」（换文档收口要拿它当 hint，不能是 transient 容器）。同样持 AddRef 保活，
    // 理由同上：裸指针在旧对象释放后可能被新对象复用同一地址。
    ITfDocumentMgr* _pLastFocusedDocMgr;
    // focus_lost 已发出且尚未被 focus_gained 复位。SendFocusLost 不幂等（服务端据此推进
    // 状态机），而清理可能从三条路径进入（换文档 / OnKillThreadFocus / 无可编辑上下文），
    // 故需去重。⚠ CTX_LOST **不**置本标志：它不是真失焦，置了会让随后真正的
    // thread_focus_lost 被吞掉，服务端的 ime_active 就永远清不掉（见 _ReportEditContextLost）。
    BOOL _focusLostSent;

    // 已向服务端上报「当前焦点在可编辑控件里」（focus_gained 送达时置位）。
    // 供 _ReportEditContextLost 在翻转沿去重——DocMgr 级失焦实测可达 60~98 次/秒，
    // 不去重会造成 IPC 洪泛。
    BOOL _editCtxReported;

    // 「不可输入」的判定与呈现已收归 Rust 协调器单点负责（见 InputBlock）。
    // DLL 只上报原始信号，不再持有 _bNoEditContext / 迟滞计时这类第二份状态。
    // ⚠ 保留 IsPasswordSuppressActive()：那是**吃键闸门**，必须在 IPC 之前本地算出。

    // Composition
    ITfComposition* _pComposition;
    // Top-code committed text kept at the head of the composition, not yet
    // committed to the document (Microsoft IME defers the real commit to the
    // final confirmation — verified via Chrome IME event probe: MS Wubi sends
    // compositionupdate '可能y' on top-code, compositionend only at the end).
    std::wstring _pendingCommitPrefix;
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
    // 首帧 reflow 期间已发出的试探采样次数（见 OnLayoutChange 与 CMD_CARET_PROBE）。
    // 每次 StartComposition 归零；限次上报，防 burst 长的宿主刷 IPC。
    int  _firstShowProbeSeq = 0;
    BOOL _needsFocusRecovery;
    LONG _lastFocusCaretX;
    LONG _lastFocusCaretY;
    LONG _lastFocusCaretHeight;
    // 上面这组焦点 caret 的来源（CARET_SRC_*）。**必须与坐标成对读写**——它们分开就等于
    // 又回到「一个 BOOL 把『拿到了坐标』和『拿到了那个坐标』压成同一个 TRUE」的老问题。
    int  _lastFocusCaretSource = CARET_SRC_UNKNOWN;
    // 异步焦点探测的去重：同一个 _focusSessionId 只发起一次。OnSetFocus 在 DocMgr 抖动时
    // 会被反复调用，不去重就会给宿主刷一串 edit session 请求。
    ULONGLONG _focusCaretProbedSession = 0;
    // 本会话的 focus_gained 是否已发出。异步焦点回调据此决定"随包发"还是"补发"：
    // 内联执行时回调早于 SendFocusGained，坐标直接进包；排队执行时晚于它，必须补一条
    // caret_update。**两者都发就会被服务端的 handle_focus_gained 覆写掉**，见回调注释。
    ULONGLONG _focusGainedSentForSession = 0;
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
    // Optional pDynFlagsOut / pStatFlagsOut receive dwDynamicFlags / dwStaticFlags from
    // TF_STATUS (0 if unavailable). 两者要一起取才判得了 locked/transient —— 见
    // IsLockedTransientDocMgr：dynFlags 那一位只是能力位，单独用会误伤 WinUI 3 宿主。
    BOOL _DocMgrHasEditableContext(ITfDocumentMgr* pDocMgr, DWORD* pDynFlagsOut = nullptr,
                                   DWORD* pStatFlagsOut = nullptr);

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
    ULONGLONG _lastCapsKeyTick;  // 最近一次 CapsLock 按键活动（GetTickCount64），见 NoteCapsLockKeyActivity

    // 最近一次 ActivateEx 的时刻。激活后系统会写 compartment 做初始化同步，那不是用户
    // 操作——实测 ActivateEx 后 ~96ms 就有一次 CONVERSION 变化。焦点守卫改用
    // _hasThreadFocus 之后这类噪声不再被顺带挡住（激活时本应用正是前台），故需本时间戳。
    // 手法同 295350e 用 _lastCapsKeyTick 抑制 CapsLock 联动噪声。
    ULONGLONG _lastActivateTick;

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

    // HoldComposition 计时器状态（智能符号 HoldComposition 方案）
    UINT_PTR       _hHoldTimer = 0;           // SetTimer 返回的计时器 ID；0 表示无活跃计时器
    std::wstring   _heldCompositionText;      // press1 进入组合态的中文文本
    // 提交 held 中文符号收口。nonKeyContext=TRUE 表示调用点拿不到同步编辑会话，须走
    // 异步收口：真正的 WM_TIMER 回调（HoldTimerProc）、以及经 EndComposition 从窗口
    // 消息/COM 回调进来的 Flush。按键上下文里的 Flush 路径（PassThrough 透传）保持
    // 同步，以确保与后续透传字符的先后顺序正确。语义同 CommitText 的同名参数。
    void           OnHoldTimerExpired(BOOL nonKeyContext = FALSE);
    static VOID CALLBACK HoldTimerProc(HWND hwnd, UINT uMsg, UINT_PTR idEvent, DWORD dwTime);

    // direct_commit 顶码：真提交后，余码新组合延迟到触发键 keyup（或兜底定时器）才开。
    // 与 HoldComposition 计时器状态并列、互不干扰。见 top-commit-mode 设计文档 §5。
    std::wstring   _deferredCompText;        // 待重开的余码组合；空=无待重开
    UINT_PTR       _hDeferredTimer = 0;      // keyup 兜底定时器 id；0=无
    static VOID CALLBACK DeferredTimerProc(HWND, UINT, UINT_PTR idEvent, DWORD);

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
