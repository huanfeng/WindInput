#pragma once

#include "Globals.h"
#include <ctffunc.h> // ITfIntegratableCandidateListUIElement（Dota 2 等自绘候选宿主的必需接口）
#include "BinaryProtocol.h" // HostWindowKind / HOST_WINDOW_KIND_COUNT for the host window array
#include "UiElementPolicy.h" // 候选 UI 元素的纯判据（可单测，见 tests/uielement_policy_test.cpp）
// AsyncCaretResult / CaretProbeKind 按值出现在 OnAsyncCaretRectReady 签名里，需要完整定义。
// 反向不成立（CaretEditSession.h 只前置声明 CTextService），故无循环包含。
#include "CaretEditSession.h"
// 「宿主没有插入点可报」的纯判据（可单测，见 tests/caret_default_pos_policy_test.cpp）。
#include "CaretDefaultPosPolicy.h"
#include "HostBandPolicy.h" // host 候选窗 band 复核判据 + 落位上报编码（可单测）
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
                     // ITfIntegratableCandidateListUIElement — 让能自绘候选的宿主（Dota 2 等经 VALVEIME001
                     // 自绘的游戏引擎）把我们的候选画出来；缺它时宿主读了候选数据也不画。
                     public ITfIntegratableCandidateListUIElement,
                     // ITfFunctionProvider — 经 ITfSourceSingle::AdviseSingleSink 注册为本实例的
                     // Function Provider，让 Chromium / QQNT 等宿主识别为完整 IME、走 IME-first 调度。
                     public ITfFunctionProvider
{
    friend class CUpdateCompositionEditSession;
    friend class CEndCompositionEditSession;
    friend class CCommitTextEditSession;
    friend class CReplaceBackwardEditSession;
    friend class CInsertTextEditSession;
    friend class CDropLeftoverEditSession;
    friend class CPurgeLeftoverEditSession;
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

    // ITfThreadFocusSink — 线程级焦点通知（应用进程 foreground 变化），与文档级的
    // ITfThreadMgrEventSink::OnSetFocus 不同。也让 Chromium / QQNT 等宿主走 IME-first 调度。
    STDMETHODIMP OnSetThreadFocus();
    STDMETHODIMP OnKillThreadFocus();

    // ITfUIElement — 候选 UI 元素基础接口。支撑 UI-less 模式：宿主（全屏游戏、SDL / Unreal 等
    // 引擎）在 BeginUIElement 里回 pbShow=FALSE 即接管候选绘制，候选数据经
    // ITfCandidateListUIElement 交给宿主，服务端不再弹候选窗。状态机见 TextService.cpp。
    STDMETHODIMP GetDescription(BSTR* pbstrDescription);
    STDMETHODIMP GetGUID(GUID* pguid);
    STDMETHODIMP Show(BOOL bShow);
    STDMETHODIMP IsShown(BOOL* pbShow);

    // ITfCandidateListUIElement — 候选列表数据。宿主接管时答 _uiSnapshot（从服务端拉取
    // 的快照）；宿主不接管时沿用占位数据（count=1 / "…"），那是 Chromium 热键调度所需。
    STDMETHODIMP GetUpdatedFlags(DWORD* pdwFlags);
    STDMETHODIMP GetDocumentMgr(ITfDocumentMgr** ppdim);
    STDMETHODIMP GetCount(UINT* puCount);
    STDMETHODIMP GetSelection(UINT* puIndex);
    STDMETHODIMP GetString(UINT uIndex, BSTR* pstr);
    STDMETHODIMP GetPageIndex(UINT* pIndex, UINT uSize, UINT* puPageCnt);
    STDMETHODIMP SetPageIndex(UINT* pIndex, UINT uPageCnt);
    STDMETHODIMP GetCurrentPage(UINT* puPage);

    // ITfCandidateListUIElementBehavior — 宿主对候选的操作，经 CMD_UIELEMENT_ACTION 回流服务端。
    STDMETHODIMP SetSelection(UINT nIndex);
    STDMETHODIMP Finalize(void);
    STDMETHODIMP Abort(void);

    // ITfIntegratableCandidateListUIElement — 见继承处说明（Dota 2 等自绘候选宿主的必需接口）。
    STDMETHODIMP SetIntegrationStyle(GUID guidIntegrationStyle);
    STDMETHODIMP GetSelectionStyle(TfIntegratableCandidateListSelectionStyle* ptfSelectionStyle);
    STDMETHODIMP OnKeyDown(WPARAM wParam, LPARAM lParam, BOOL* pfEaten);
    STDMETHODIMP ShowCandidateNumbers(BOOL* pfShow);
    STDMETHODIMP FinalizeExactCompositionString(void);

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
    // nonKeyContext=TRUE：调用点不在按键上下文中（WM_TIMER、窗口消息、COM 回调等），改用异步
    // 编辑会话（TF_ES_ASYNCDONTCARE）且不走 SendInput 兜底。TF_ES_SYNC 只在处理按键时合法，Word
    // 在这类上下文拒发同步会话，会导致编码泄漏与重复上屏。判据是「在不在 OnKeyDown 调用栈里」，
    // 不是「谁调的」，新增调用点时按此判断。
    //
    // replacingHeld=TRUE：替换掉 hold 预览态里待定的中文符号（智能符号 press2）。默认 FALSE 为
    // 追加语义，held 符号并入 prefix 一起上屏（组合 range 的 SetText 会覆盖它）。由服务端经
    // COMMIT_FLAG_REPLACING_HELD 声明。
    // pAsyncDeferred（可选出参）：置 TRUE 表示提交交给了异步 EditSession、尚未落定。调用方若要在
    // 提交后立刻注入按键（配对左移）必须看这一位，返回值区分不出。SendInput 兜底不置位：注入的
    // 文本与随后的按键天然有序。
    BOOL CommitText(const std::wstring& text, BOOL nonKeyContext = FALSE,
                    BOOL replacingHeld = FALSE, BOOL* pAsyncDeferred = nullptr);

    // 以 Unicode 按键（VK_PACKET）注入文本；失败返回 FALSE，调用方回退 CommitText。
    // 配对插入（pair_via_comp）与 AutoCAD 类宿主的空闲态 ASCII 提交用它。
    BOOL InjectUnicodeText(const std::wstring& text);

    // `CommitText(text, TRUE, ...)` 的替代：缓冲文本并合成触发键，把提交挪到 OnKeyDown 里以按键
    // 上下文同步提交，宿主的输入时处理（含内嵌 `\n` 转分段）才会触发。用于鼠标点候选等 push 路径。
    // 实现在 KeyEventSink（这里只转发）；合成按键注入失败时退回 `CommitText(text, TRUE, ...)`。
    BOOL CommitTextViaSyntheticKey(const std::wstring& text, BOOL replacingHeld = FALSE);

    // 把光标前 count 个已上屏字符替换为 text（智能符号纠错替换）。
    // 优先走 TSF 同步 EditSession（原子、不受输入队列时序/修饰键影响）；
    // 失败时回退到 SendInput（count 次 Backspace + Unicode 注入 text）。
    BOOL ReplacePrecedingChars(int count, const std::wstring& text);

    // 直通 ime.pair 推送落点：转发给 KeyEventSink 上屏 + 左移 + 记一层待跳出深度。
    // 深度是它自己的成员，故实现留在那边，这里只做转发（消息窗只拿得到 TextService）。
    void HandlePairCommitPush(const std::wstring& text, uint32_t moveLeft);

    // End current composition.
    // pDocMgrHint: composition 所属的 DocMgr，给出即权威，不再问 GetFocus()：doc_changed 路径上
    // 焦点已在新文档，拿它的 context 去清旧 range 会出错。不给则回落 GetFocus()。
    // 清空 composition 范围后再 EndComposition，否则 Excel/WPS 会把残留文本提交到目标 doc。
    //
    // nonKeyContext: 语义同 CommitText。本方法自身用异步会话，但顶码聚合中会转调 CommitText
    // 收口前缀，须把判据传下去。
    void EndComposition(ITfDocumentMgr* pDocMgrHint = nullptr, BOOL nonKeyContext = FALSE);

    // Reset KeyEventSink composing state (called after push pipe commit/clear)
    // keepPairState=TRUE 时保留自动配对状态，语义见 CKeyEventSink::ResetComposingState。
    void ResetComposingState(BOOL keepPairState = FALSE);

    // 输入态整体清理：结束 composition + 通知服务端清 buffer + 复位 KeyEventSink 会话态。
    // 触发时机是「离开了原来那个文档」而非「失去焦点」（失焦那一刻分不清抖动与切换）。
    // OnKillThreadFocus 与 doc_changed 共用本函数，靠 _focusLostSent 去重。pDocMgrHint 传离开的
    // 那个 doc。reason 取 FOCUS_LOST_REASON_*（THREAD / DOC_CHANGED）。
    // sendFocusLost=FALSE 时只做本地清理：新 DocMgr 若会被 XamlIsland locked 守卫跳过
    // focus_gained，focus_lost 就没有配对者，服务端 ime_active 会被永久清掉。
    void CleanupInputStateForDocChange(ITfDocumentMgr* pDocMgrHint, uint8_t reason,
                                       BOOL sendFocusLost = TRUE);

    // 焦点离开可编辑控件时通知服务端隐藏工具栏（发 FOCUS_LOST_REASON_CTX_LOST）。
    // 只翻可见性标志、不碰输入态，这是它能在 DocMgr 噪声层安全调用的前提。靠 _editCtxReported 去重。
    void _ReportEditContextLost();

    // Top-code commit: accumulate the committed text into the pending prefix and
    // keep it INSIDE the composition (Microsoft IME behavior — the real document
    // commit is deferred to the final CommitText). See _pendingCommitPrefix.
    BOOL InsertTextAndStartComposition(const std::wstring& insertText, const std::wstring& newComposition);
    // 新开组合时插入点的位置：常规放末尾，**占位组合（单个空格 / ZWSP / 盲文空白，见 CompositionPlaceholder.h）放 0**。
    // 与 Rust 侧 COMPOSITION_PLACEHOLDER 成对，见实现处的说明。
    static int _CompositionCaretFor(const std::wstring& composition);

    // Length (in wchars) of the pending top-code commit prefix shown at the head
    // of the composition. Used to segment display attributes and to offset the
    // composition-start coordinate reported to the engine (candidate anchor).
    size_t GetPendingCommitPrefixLength() const { return _pendingCommitPrefix.length(); }

    // 把已决定提交的文本并入待提交前缀，不结束组合、不真提交（推迟到最终 CommitText）。用于智能
    // 标点顶屏聚合，规避「真提交 + 立即重开组合」被 diff 式宿主（微信/Tabby/终端）误读吞字。
    // held 符号勿并入（press2 要替换它）。
    void PinCommitTextToPrefix(const std::wstring& text) { _pendingCommitPrefix += text; }

    // Get and consume cached character before caret (set by ITfTextEditSink::OnEndEdit).
    // Returns the cached value and clears it to prevent stale values persisting across
    // key events in apps where OnEndEdit fires late or not at all (e.g., WeChat).
    WCHAR ConsumeCachedPrevChar() { WCHAR c = _cachedPrevChar; _cachedPrevChar = 0; return c; }

    // 有键透传进宿主后，缓存是按键之前的快照，作废。能读文档的宿主随后会经 OnEndEdit 重填；
    // Tabby（xterm.js）这类不会，prevChar 落空，press2 守卫按「读不回」处理。
    // 数字后智能标点不读这个缓存，见 SmartPunctPrevChar.h。
    void InvalidateCachedPrevChar() { _cachedPrevChar = 0; }

    // Get and send caret position to the service
    // pSource: 非空时输出命中的是回退链的哪一级（CARET_SRC_*）。TSF 坐标与 GUI 光标分属两个域，
    //          消费端不能混用。
    // pRejectedOutsideView: 候选窗更新用。TSF 坐标被判越界时输出原因并停止回退。
    BOOL GetCaretPosition(LONG* px, LONG* py, LONG* pHeight, int* pSource = nullptr,
                          RECT* pCompRect = nullptr, BOOL* pHasCompRect = nullptr,
                          BOOL* pRejectedOutsideView = nullptr);
    void SendCaretPositionUpdate();

    // 非按键上下文（WM_TIMER 等）专用：用异步 edit session 取坐标，结果经 OnAsyncCaretRectReady
    // 发出（见 CCaretEditSession::RequestCaretRectAsync）。kind 的含义见 CaretProbeKind。
    BOOL RequestCaretPositionUpdateAsync(CaretProbeKind kind = CaretProbeKind::Composition);

    // OnSetFocus 专用：焦点刚到达、**尚无 composition** 时取一次插入点。
    //
    // 回调用 _focusSessionId 判归属（`_pComposition` 恒为 null）；坐标补一条 CMD_CARET_UPDATE
    // 更新服务端缓存，供状态气泡（ui.status.show_on_focus）落座。
    //
    // 返回 TRUE 仅表示请求已受理。内联执行（hrSession=S_OK）时回调在返回前跑完，
    // _lastFocusCaretX/Y 可随 focus_gained 一起发；排队（TF_S_ASYNC）则晚到，走补发通道。
    BOOL RequestFocusCaretAsync(ITfDocumentMgr* pDocMgrFocus);

    // 异步 edit session 的结果回调（由 CCaretEditSession 调用）
    void OnAsyncCaretRectReady(const AsyncCaretResult& result);

    // Get caret position using TSF APIs (more accurate for browsers)
    // pUsedCompStart: 非空时输出「caret 是否由组合起点降级顶替」，用于区分两种 TSF 来源
    // pCompRect/pHasCompRect：整个组合 range 的包围矩形，见 BinaryProtocol.h 的 CaretPayloadV3
    // pIsDefaultPos: 非空时输出「本次返回的是宿主的『没有插入点』默认位置」
    //                （CARET_SRC_TSF_DEFAULT_POS，高度为合成值）。为真时 pUsedCompStart 无意义。
    // pUsedImmCandidateForm: 非空时输出「caret 是否由宿主 IMM32 CANDIDATEFORM 降级顶替」
    //                （CARET_SRC_IMM_CANDIDATE_FORM）。为真时 pUsedCompStart 恒为假。
    BOOL GetCaretPositionFromTSF(LONG* px, LONG* py, LONG* pHeight, BOOL* pUsedCompStart = nullptr,
                                 RECT* pCompRect = nullptr, BOOL* pHasCompRect = nullptr,
                                 BOOL* pIsDefaultPos = nullptr, BOOL* pUsedImmCandidateForm = nullptr,
                                 BOOL* pRejectedOutsideView = nullptr);
    BOOL GetCompositionStartPosition(LONG* px, LONG* py);

    // Input mode control
    void ToggleInputMode();
    void SetInputMode(BOOL bChineseMode);  // Set mode from service response (no IPC)
    BOOL IsChineseMode() { return _bChineseMode; }
    // 读取 OPENCLOSE compartment 的真实当前值（0/1；失败返回 -1）。诊断用：_SetOpenCloseCompartment
    // 不保证成功，只有回读才算数。含两次 COM，热路径上须先用 CFileLogger::IsEnabled 门控。
    LONG GetOpenCloseCompartmentValue();
    BOOL IsFullWidth() { return _bFullWidth; }
    // 当前是否中文标点态，标点透传判据按它二选一（见 CKeyEventSink::_IsPassthroughPunctKey）。
    // 与 _bFullWidth 在完全相同的五处写入（构造、_SyncStateFromResponse、ModePush 回调、状态推送、
    // UpdateFullStatus），漏一处则切标点态后第一个标点按旧态判。
    BOOL IsChinesePunct() { return _bChinesePunct; }
    // 软键盘面板是否开着（由服务端经 statusFlags 推送）。见 KeyEventSink 的数字键分支。
    BOOL IsSoftKeyboard() { return _bSoftKeyboard; }
    // 当前面是键盘面：按键交还输入法，软键盘总闸只留 Esc 与翻页。
    BOOL IsSoftKeyboardKeys() { return _bSoftKeyboardKeys; }
    BOOL IsKeyboardDisabled() { return _bKeyboardDisabled; }
    // 密码框强制英文抑制当前是否生效（镜像 core 的 `apply_input_diag`：命中密码 InputScope 位 +
    // compartment 未禁用 + 策略开关开）。DLL 须自行判定：吃键决策早于 IPC，靠 core 回 PassThrough
    // 会「吃了再吐」。
    BOOL IsPasswordSuppressActive() const;
    // 焦点是不是密码框（IS_PASSWORD / IS_NUMERIC_PASSWORD），与强制英文开关无关：
    // 扩展旁路上报据此一个键都不报（隐私），而上面那个是吃键闸门，管的是输入行为。
    BOOL IsPasswordScope() const;
    void SetPasswordSuppressEnabled(BOOL bEnabled) { _passwordSuppressEnabled = bEnabled; }
    // 诊断快照采集开关（core 经 CONFIG_KEY_DIAG_SNAPSHOT 推；默认关）。
    void SetDiagSnapshotEnabled(BOOL bEnabled) { _diagSnapshotEnabled = bEnabled; }
    // 窗口标题采集开关（core 经 CONFIG_KEY_COMPAT_TITLE_MATCH 推「本进程可能命中标题规则」；默认关）。
    // config 回调跑在 IPC 读线程、读在 TSF 线程，用 Interlocked 读写一个 LONG。
    // 关→开时（且非断连清零）请 TSF 线程补发一次 focus_gained，见 ResyncFocusForTitleMatch。
    void SetTitleMatchEnabled(BOOL bEnabled);
    BOOL IsTitleMatchEnabled() const { return _titleMatchEnabled != 0; }
    // 标题采集开关刚打开：若仍有焦点且本焦点会话的 focus_gained 没带标题（新进程首焦与握手推送
    // 竞态），补发一次带类名与标题的，让标题规则（含 initial_mode）生效。TSF 线程调用。
    void ResyncFocusForTitleMatch();
    // 空文本兜底占位字符的档位（core 经 CONFIG_KEY_COMPOSITION_PLACEHOLDER 按本进程 compat
    // 规则推；0 = 空格（默认）、1 = ZWSP、2 = 盲文空白）。config 回调跑在 IPC 读线程，读在 TSF
    // 线程，故用 Interlocked 读写一个 LONG。
    void SetCompositionPlaceholderKind(uint8_t kind) { InterlockedExchange(&_compositionPlaceholderKind, (LONG)kind); }
    // 含换行的上屏方式（core 经 CONFIG_KEY_MULTILINE_COMMIT 按本进程 compat 规则推；0 = commit（默认）、
    // 1 = paste，见 MultilineCommit.h）。跨线程读写同上。
    void SetMultilineCommitMode(uint8_t mode) { InterlockedExchange(&_multilineCommitMode, (LONG)mode); }

    // 语言栏悬停提示（core 经 CONFIG_KEY_LANGBAR_TOOLTIP 推）。文案与选择逻辑全在服务端，
    // 这里只存一份原样交给 CLangBarItemButton::GetTooltipString。
    //
    // 必须加锁：写在 IPC 读线程、读在 TSF 线程，std::wstring 不像 BOOL 那样读写原子。
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
    // 采集并上报一次诊断快照。开关关闭时立即返回，一次 Win32 调用都不做。
    // docMgrChanged 由调用方给出（只有 OnSetFocus 知道自己是不是换了文档）。
    void SendDiagSnapshotIfEnabled(ITfDocumentMgr* pDocMgr, BOOL docMgrChanged);

    // 焦点窗口解析（TSF view → GUI thread，不含前台窗口兜底）。诊断快照与 focus_gained 的
    // 窗口类上报共用，别各写一份。
    HWND _ResolveFocusWindow(ITfDocumentMgr* pDocMgr, uint8_t* pSrcOut, uint64_t* pCtxIdOut);
    // 焦点所在顶层窗口的类名与标题，随 focus_gained 上报。类名供服务端区分壳的过渡型 / 停留型
    // 窗口、匹配 compat 的 class 条件；标题只在 _titleMatchEnabled 时取（否则恒空），匹配 title 条件。
    void _QueryFocusRootWindowIdentity(ITfDocumentMgr* pDocMgr, std::wstring& windowClass,
                                       std::wstring& windowTitle);
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
    BOOL HasThreadFocus() const { return _hasThreadFocus; }
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
    // 有组合，或组合刚被宿主打断、缓冲仍保留着（恢复中）。会清服务端缓冲的同步判据要看它。
    BOOL HasCompositionOrRecover() { return _pComposition != nullptr || !_recoverLeftover.empty(); }
    BOOL HasRecoverRecord() const { return !_recoverLeftover.empty(); }

    // Clear the "composition just started" flag (used by timer fallback path).
    // 同时作废 StartComposition 时抓的坐标缓存（那时宿主还没 reflow），迫使 timer 触发时重新查询。
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

    // Send menu command to the service
    void SendMenuCommand(const char* command);

    // Send show context menu request to the service (screen coordinates)
    void SendShowContextMenu(int screenX, int screenY);

    // Update full status from the service response
    // iconLabel: display text from the service for taskbar icon (e.g., "中", "英", "A", "拼")
    void UpdateFullStatus(BOOL bChineseMode, BOOL bFullWidth, BOOL bChinesePunct, BOOL bToolbarVisible, BOOL bCapsLock, const wchar_t* iconLabel = nullptr);

    // HoldComposition: 开启组合显示 text，timeoutMs 毫秒后自动提交中文（智能符号方案）。
    // press2 到来前的任何 CommitText 调用会先通过 CancelHoldTimer 取消定时器。
    BOOL HoldComposition(const std::wstring& text, UINT timeoutMs);

    // 取消 HoldComposition 计时器（若活跃）。安全：_hHoldTimer==0 时为空操作。
    void CancelHoldTimer();

    // 若 HoldComposition 计时器活跃，立即提交中文符号（宿主中断组合时调用，如 PassThrough 键）。
    // nonKeyContext: 语义同 CommitText 的同名参数，透传给收口用的 OnHoldTimerExpired。
    void FlushHoldCompositionIfActive(BOOL nonKeyContext = FALSE);

    // HoldComposition 计时器是否活跃，即组合内只有待定中文符号（加已承诺的 prefix）、不含编码，
    // 是「智能符号预览态」的精确判据。只能看计时器：`_pendingCommitPrefix` 非空在顶码聚合时也成立。
    BOOL IsHoldCompositionActive() const { return _hHoldTimer != 0; }

    // 若 HoldComposition 计时器活跃，把 held 符号定格并入 _pendingCommitPrefix（不 commit、不动
    // 文档），让连续智能符号等场景在单一 EditSession 内更新显示，规避「commit + 立即重启组合」在
    // Chromium/WPS 下被误读成替换。最终 CommitText 一次收口。
    void AbsorbHeldIntoPrefix();

    // direct_commit 顶码：真提交后，余码新组合延迟到触发键 keyup（或兜底定时器）才开。
    // 与 HoldComposition 计时器状态并列、互不干扰。见 top-commit-mode 设计文档 §5。
    // triggerVk：触发这次提交的键（空格 / 顶码键），keyup 只认它（见 DeferredCompositionPolicy.h）；0=不限。
    void StashDeferredComposition(const std::wstring& composition, UINT fallbackMs, UINT triggerVk = 0);
    UINT DeferredTriggerVk() const { return _deferredTriggerVk; }
    void StartDeferredCompositionIfPending();   // keyup / 兜底定时器 / flush 统一入口
    void CancelDeferredComposition();
    BOOL HasDeferredComposition() const { return !_deferredCompText.empty(); }

private:
    // 多行文本的粘贴上屏：写剪贴板（CRLF）+ 注入 Ctrl+V。只由 CCommitTextEditSession 在组合结束后调用。
    // 返回 FALSE = 没能注入（剪贴板写不进 / SendInput 一个都没发出），调用方须自己插入原文兜底。
    BOOL _PasteTextViaClipboard(const std::wstring& text);

    // 坐标出口：DPI 归一 + 更新 last known + 发 IPC。同步/异步两条取坐标路径共用。
    // pCompRect：整个组合 range 的包围矩形，nullptr / 四值全 0 表示本帧没取到。
    // 探针阶段只上报不决策，见 BinaryProtocol.h 的 CaretPayloadV3。
    void _EmitCaretUpdate(LONG x, LONG y, LONG height, LONG compStartX, LONG compStartY, int source,
                          const RECT* pCompRect = nullptr);

    LONG _refCount;
    ITfThreadMgr* _pThreadMgr;
    TfClientId _tfClientId;
    DWORD _dwThreadMgrEventSinkCookie;
    DWORD _dwThreadFocusSinkCookie;
    DWORD _uiElementId;     // ITfUIElementMgr::BeginUIElement 返回的 ID；TF_INVALID_UIELEMENTID 表示未注册
    BOOL  _uiElementShown;  // 当前 IsShown 返回值（元素存续期间的可见态；EndUIElement 后 FALSE）
    ITfUIElementMgr* _pUIElementMgr;  // 缓存的 UI element 管理器引用，避免每次候选变化都 QI
    // ── UI-less（宿主自绘候选）状态 ──
    // 宿主是否接管候选绘制：BeginUIElement 回 pbShow=FALSE / 之后 Show(FALSE) 置 TRUE，
    // Show(TRUE) 置 FALSE。与 _uiElementShown 分开存：那个构造时与 EndUIElement 后都是 FALSE，
    // 拿它当「宿主接管」会把普通宿主误报成接管。
    BOOL  _uiHostDraws;
    BOOL  _uiLessThread;    // ActivateEx 带 TF_TMAE_UIELEMENTENABLEDONLY：宿主声明不要 TIP 的 UI
    // 上一次经 CMD_UIELEMENT_STATE 报给服务端的 flags；-1 = 尚未报过（激活后必报一次）。
    // 只在变化时发，服务端按 pid 记账决定弹不弹候选窗。
    int32_t _uiElementStateSent;
    // 宿主接管时从服务端拉取的候选快照（CMD_UIELEMENT_QUERY）。宿主眼里列表就这么长。
    struct UiElementSnapshot
    {
        std::vector<std::wstring> items;
        UINT selected = 0;
        UINT pageSize = 1;
        UINT currentPage = 0;
    };
    UiElementSnapshot _uiSnapshot;
    DWORD _uiUpdatedFlags;  // 上次 UpdateUIElement 相对前一快照的变化位（TF_CLUIE_*），GetUpdatedFlags 回答
    // 快照相对服务端候选状态是否已过期。候选变了但宿主没声明接管时只置位、不拉取；
    // 真有宿主来读 getter 时才补拉一次（见 _EnsureUiSnapshotFresh）。只 UpdateUIElement 不刷快照，
    // 宿主读到的就是上一帧（见 docs/design/game-compat-tsf-uielement.md §1.4）。
    BOOL  _uiSnapshotDirty;
    // 宿主虽未声明接管（pbShow=TRUE），但确实来读过候选串。sticky：一旦读过就认定
    // 它在画，本次激活期间不再翻回。判定点见 _NoteHostReadCandidates。
    BOOL  _uiHostReadsCandidates;
    // 「宿主在画候选」：声明接管（pbShow=FALSE / Show(FALSE) / UI-less 线程）或实际来读过候选串。
    // 后者是推断，core 默认不据它收窗（compat 的 host_drawn_candidates opt-in）；DLL 只如实报告。
    BOOL  _UiElementHostDraws() const
    {
        return wind::uielement::HostDraws(_uiHostDraws != FALSE, _uiHostReadsCandidates != FALSE)
                   ? TRUE
                   : FALSE;
    }
    // 取光标坐标是否该整条短路。宿主接管绘制时不需要坐标，而向 D3D 独占全屏的游戏反复发
    // GetTextExt 会把渲染线程拖死。只看声明（_uiHostDraws / _uiLessThread），不看读取闩：core
    // 默认不据闩收窗，那些宿主仍弹我们的候选窗、仍需坐标。
    // 已知缺口（刻意留着）：推断生效、core 已收窗时这里仍照常探 caret；补法是加 core→DLL 的
    // 「本进程已被收窗」推送。见 docs/design/game-compat-tsf-uielement.md §1.4 的「两条刻意留着的缺口」。
    BOOL  _CaretQuerySuppressed() const { return _uiLessThread || _uiHostDraws; }
    // getter 答快照还是占位。
    //
    // 已认定宿主在画（声明或读过）则恒答快照，与 SetSelection / Finalize / Abort 的放行判据
    // `_UiElementHostDraws()` 取齐，否则 getter 之间自相矛盾。
    // 尚未认定时要求快照非空且不脏：脏快照会让 GetCount 答上一键的条数、GetString 答本键内容。
    // 答占位（count=1）保守但自洽，下一键即恢复。
    BOOL  _UiElementUseSnapshot() const
    {
        return wind::uielement::UseSnapshot(_uiHostDraws != FALSE,
                                            _uiHostReadsCandidates != FALSE,
                                            _uiSnapshot.items.empty(),
                                            _uiSnapshotDirty != FALSE)
                   ? TRUE
                   : FALSE;
    }
    void  _ReportUiElementState();        // flags 变化时发 CMD_UIELEMENT_STATE
    BOOL  _RefreshUiElementSnapshot();    // 同步拉取快照并计算 _uiUpdatedFlags；失败清空快照
    void  _UpdateUiElementForHost();      // 宿主接管时：拉快照 + UpdateUIElement
    // getter 入口的补拉：快照过期就现拉一次。
    //
    // `contentRead` = 本次是否在取候选内容本身（只有 GetString 传 TRUE）。GetCount 是 msctf 自己
    // 也会问的，无条件补拉等于给每个宿主每次按键都加一次 UI 线程上的同步 IPC。
    //
    // 取元信息的 getter 里那句补拉当前不可达（脏位只在未认定在画时置起，合闩前必先补拉清脏），
    // 留作防御；入口每次调 `InvariantBroken`，破了记 WARN。别改成 constexpr 常量 + 单测断言，
    // 那与本文件无耦合、破了也不红。也别写成「闩合上后所有 getter 都补拉」：同源来自急刷，
    // 合闩那一次读取序列里本就不成立（见 docs/design/game-compat-tsf-uielement.md §1.4）。
    void  _EnsureUiSnapshotFresh(BOOL contentRead = FALSE);
    // 宿主读了候选串 → 置 sticky 闩并立刻报服务端（它据此收掉我们自己的候选窗）。
    void  _NoteHostReadCandidates();
    void  _SendUiElementAction(uint32_t action, uint32_t arg);
    ITfSourceSingle* _pSourceSingle;  // 缓存的 ITfSourceSingle 引用（Function Provider 注册用）
    BOOL  _funcProviderRegistered;    // 是否已通过 AdviseSingleSink 注册

    // Win32 RegisterHotKey 支持：候选可见时把置顶/删词热键（组合键取自服务端 SESSION 热键表）
    // 注册为系统级热键，由 OS 在 WM_KEYDOWN 派发前消费，规避 QQNT 类 Chromium 宿主的加速键双处理。
    // 无候选时立即注销，让宿主使用这些热键。
    HWND  _hHotkeyWnd;                // 隐藏消息窗口，接收 WM_HOTKEY
    ATOM  _hotkeyWndClass;            // RegisterClassEx 返回的窗口类原子
    BOOL  _hotkeysActive;             // 当前是否已 RegisterHotKey 候选热键（组合键取自服务端 SESSION 热键表）
    BOOL  _candidatesVisible;         // 最近一次 NotifyCandidatesVisibilityChanged 的值；模式切回中文时据此补注册
    // 加词热键（Ctrl+= 等）全局拦截：门卫比候选热键更严——中文模式 + 焦点在可编辑文本框 +
    // 非密码框 + 持有 thread focus 才注册，让抢占面积最小化，不干扰非文本框处的宿主快捷键。
    BOOL  _addWordHotkeysActive;      // 当前是否已 RegisterHotKey 加词热键
    bool  _focusIsPassword;           // 当前焦点是否密码框（KEYBOARD_DISABLED）；密码框不注册加词热键
    // 当前焦点的 InputScope 掩码（与 focus_gained / CMD_INPUT_STATE_REPORT 上报的同值）。
    // 上报给 core 之外自己也留一份：IsPasswordSuppressActive 的吃键门控须本地可算。
    UINT64 _focusInputScopeMask;
    BOOL  _passwordSuppressEnabled;   // 抑制策略开关（core 经 CONFIG_KEY_PASSWORD_SUPPRESS 推；默认开）
    // 空文本兜底占位档位（core 经 CONFIG_KEY_COMPOSITION_PLACEHOLDER 推；默认 0 = 空格）。
    // 见 SetCompositionPlaceholderKind / CompositionPlaceholder.h。
    volatile LONG _compositionPlaceholderKind;
    // 多行上屏方式（core 经 CONFIG_KEY_MULTILINE_COMMIT 推；默认 0 = commit）。见 SetMultilineCommitMode。
    volatile LONG _multilineCommitMode;
    // 诊断快照采集开关（core 经 CONFIG_KEY_DIAG_SNAPSHOT 推；默认关，开销不该由不排查的用户承担）。
    BOOL  _diagSnapshotEnabled;
    // 窗口标题采集开关（core 经 CONFIG_KEY_COMPAT_TITLE_MATCH 推；默认关）。标题是用户数据，
    // 规则表里没有标题规则就一次都不取（WindowTitlePolicy.h）。
    volatile LONG _titleMatchEnabled;
    // 本焦点会话发出的 focus_gained 是在标题采集关着时发的（没带标题）。开关随后打开时据此补发。
    BOOL _focusSentTitleBlind;
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
    // 本应用是否持有 TSF 线程焦点。权威来源只有 ITfThreadFocusSink（OnSetThreadFocus /
    // OnKillThreadFocus），外加 _InitHotkeyWindow 里的初始种子（激活时已在前台收不到通知）。
    // 用于过滤 compartment 变化噪声；不要用 GetForegroundWindow 的 pid 纠正它，见
    // _isProcessForeground。热键注册门卫要求它与 _isProcessForeground 同时成立。
    BOOL  _hasThreadFocus;
    // 本进程是否是前台窗口所属进程（GetForegroundWindow 的 pid == 自己）。仅用于热键注册门卫：
    // 多个 IME 实例争抢同一组全局热键会得 ERROR_HOTKEY_ALREADY_REGISTERED，前台应用反而收不到。
    // 与 _hasThreadFocus 必须分开：多进程宿主里前台窗口属于渲染进程，两个判据期望相反。
    // 见 docs/architecture/tsf-docmgr-focus-semantics.md §3.5。
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
    // 仲裁出不同值时回写；FALSE：我们是发起方，必须无条件写。source 只进日志（本地日志
    // 与服务端日志同源——它随 CMD_SYSTEM_MODE_SWITCH 一起送出，见 BinaryProtocol.h）。
    HRESULT _ApplyModeSwitch(BOOL requestedMode, BOOL compartmentAlreadySet, ModeSwitchSource source);
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
    BOOL _bChinesePunct;
    BOOL _bSoftKeyboard;
    BOOL _bSoftKeyboardKeys;
    BOOL _bKeyboardDisabled;   // GUID_COMPARTMENT_KEYBOARD_DISABLED
    ULONGLONG _focusSessionId;
    BOOL _hasFocus;             // 当前实例持有 TSF 输入焦点时为 TRUE（OnSetFocus 最后收到非 null pDocMgrFocus）
    BOOL _hasTextInputContext;  // TRUE when focused doc mgr has a real text-editing context (GetTextExt succeeds)

    // 焦点抖动免疫（见 OnSetFocus 的判据注释）：上一个真实活跃的 DocMgr，换文档收口拿它当 hint。
    // 必须 AddRef 保活：裸指针在旧对象释放后地址可能被复用，「换了文档」会被误判成抖动。
    ITfDocumentMgr* _pLastActiveDocMgr;
    // 上一次获焦的 DocMgr，含 locked/transient（XamlIsland），只回答「是不是同一个 doc 在抖」。
    // 不能复用 _pLastActiveDocMgr：它刻意排除 transient，transient 抖回来会被判成换文档、终止
    // 组合，preedit 留在宿主里（docs/architecture/tsf-docmgr-focus-semantics.md §3.1）。
    // 同样 AddRef 保活。
    ITfDocumentMgr* _pLastFocusedDocMgr;
    // focus_lost 已发出且尚未被 focus_gained 复位。SendFocusLost 不幂等，清理又有三条入口，故需去重。
    // CTX_LOST 不置本标志：它不是真失焦，置了会吞掉随后真正的 thread_focus_lost，服务端
    // ime_active 永远清不掉。
    BOOL _focusLostSent;

    // 已向服务端上报「当前焦点在可编辑控件里」（focus_gained 送达时置位）。
    // 供 _ReportEditContextLost 在翻转沿去重：DocMgr 级失焦可达每秒近百次。
    BOOL _editCtxReported;

    // 焦点空窗诊断。宿主重建焦点期间 TSF 不把键交给输入法（微信/Qt 把 DocMgr 置 NULL，Chromium 给
    // 无可编辑上下文的 DocMgr），键直落宿主而两侧日志都不留痕，故主动记下空窗起止与成因，
    // 供排查「首字符变英文」。0 = 不在空窗中；连续抖动不重新计时（见 _BeginFocusGap）。
    ULONGLONG _focusGapStartTick = 0;
    const WCHAR* _focusGapReason = nullptr;

    // 「不可输入」的判定与呈现已收归 Rust 协调器单点负责（见 InputBlock）。
    // DLL 只上报原始信号，不再持有 _bNoEditContext / 迟滞计时这类第二份状态。
    // 例外是 IsPasswordSuppressActive()：吃键闸门，必须在 IPC 之前本地算出。

    // Composition
    ITfComposition* _pComposition;
    // Top-code committed text kept at the head of the composition, not yet
    // committed to the document (Microsoft IME defers the real commit to the
    // final confirmation — verified via Chrome IME event probe: MS Wubi sends
    // compositionupdate '可能y' on top-code, compositionend only at the end).
    std::wstring _pendingCommitPrefix;
    std::wstring _lastCompositionText;  // Cache to skip redundant updates
    int _lastCaretPos = -1;             // Cache caret position to detect cursor movement
    // 宿主过早终止组合后留在文档里的组合文字（见 CompositionRecoverPolicy.h）；空 = 不在恢复中。
    // 下一次写文档的编辑会话开头经 _DropRecoverLeftover 核对并删掉。
    std::wstring _recoverLeftover;
    ITfContext* _recoverContext = nullptr;   // 残留所在的 context（持有引用）；核对与清除只认它
    ITfRange* _recoverRange = nullptr;       // 残留区间（终止时组合区间的副本，持有引用），解除时据此清
    LONG _recoverAcpStart = 0;               // 残留在文档里的起点（ACP 偏移）；核对靠位置，不只靠文字
    ULONGLONG _recoverArmTick = 0;           // 进入恢复的时刻，焦点空档判「宿主在转移输入」用
    ULONGLONG _lastCompositionWriteTick = 0; // 上次写组合文字的时刻，判「过早终止」用
    // 在编辑会话里删掉光标前的残留（光标前恰好是残留才删），并清掉记录。无残留时什么也不做。
    void _DropRecoverLeftover(TfEditCookie ec, ITfContext* pContext);
    void _ArmRecover(ITfContext* pContext, ITfRange* pRange, const std::wstring& leftover, LONG acpStart);
    void _ClearRecoverRecord();
    void _DisarmRecover(const wchar_t* why); // 解除恢复，并按记下的位置异步清掉残留
    void _PurgeLeftover(TfEditCookie ec, ITfRange* pRange, const std::wstring& text);
    // 光标处无选区、恰在残留末尾、且残留文字未变。成立时 *ppLeftover 返回覆盖残留的 range（调用方 Release）。
    BOOL _LeftoverBeforeCaret(TfEditCookie ec, ITfContext* pContext, ITfRange** ppLeftover);
    void _RequestDropLeftover(); // 无组合时单独开一个编辑会话删残留（服务端清空缓冲）
    BOOL _asyncEdit;  // Track if last RequestEditSession returned TF_S_ASYNC (Weasel optimization)

    // Cached caret position from edit session (for WebView apps where separate
    // CaretEditSession with TF_INVALID_COOKIE may be rejected)
    RECT _cachedCaretRect;
    RECT _cachedCompStartRect;
    BOOL _hasCachedCaretPos;
    BOOL _hasCachedCompStartPos;
    // 整个组合 range 的包围矩形（与 _cachedCompStartRect 同源同 cookie）。
    // 见 BinaryProtocol.h 的 CaretPayloadV3。
    RECT _cachedCompRect;
    BOOL _hasCachedCompRect;
    // Weasel 模式：StartComposition 后第一次 SendCaretPositionUpdate 不立即发 IPC，
    // 改为等 OnLayoutChange（reflow 完成的权威信号）或 50ms timer 兜底。
    BOOL _compositionJustStarted;
    // 首帧 reflow 期间已发出的试探采样次数（见 OnLayoutChange 与 CMD_CARET_PROBE）。
    // 每次 StartComposition 归零；限次上报，防 burst 长的宿主刷 IPC。
    int  _firstShowProbeSeq = 0;
    // 本次组合已判定「宿主对本 context 没有插入点可报」（CARET_SRC_TSF_DEFAULT_POS），由
    // CaretProbeKind::RetryDeadline 那次取坐标作出。闩住是为了同步路径：此后每个按键拿到同一个
    // 退化矩形，不闩就跌到无关控件的 GUI 光标，锚点被服务端重锁过去。
    //
    // 只有 StartComposition 置 FALSE。组合结束出口有四个，不逐个复位，由读闩处的
    // wind::caret::LatchApplies(_caretDefaultPosLatched, _pComposition != nullptr) 限定作用域。
    // 任何一帧有效矩形（h>0）都撤销判决，同步与异步同口径，唯 FirstShowProbe 除外。
    BOOL _caretDefaultPosLatched = FALSE;
    BOOL _needsFocusRecovery;
    LONG _lastFocusCaretX;
    LONG _lastFocusCaretY;
    LONG _lastFocusCaretHeight;
    // 上面这组焦点 caret 的来源（CARET_SRC_*），必须与坐标成对读写。
    int  _lastFocusCaretSource = CARET_SRC_UNKNOWN;
    // 异步焦点探测的去重：同一个 _focusSessionId 只发起一次。OnSetFocus 在 DocMgr 抖动时
    // 会被反复调用，不去重就会给宿主刷一串 edit session 请求。
    ULONGLONG _focusCaretProbedSession = 0;
    // 本会话的 focus_gained 是否已发出。异步焦点回调据此决定随包发还是补发 caret_update；
    // 两者都发会被服务端的 handle_focus_gained 覆写掉，见回调注释。
    ULONGLONG _focusGainedSentForSession = 0;
    BOOL _hasLastKnownCaretPos;
    // 本线程的宿主是否给过 TSF 域坐标（SELECTION / COMPOSITION / CACHED）。用于挡掉同步会话被拒后
    // 回退出的跨窗口 GUI 光标，见 SendCaretPositionUpdate。
    BOOL _hostGaveTsfCaret;
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
    // 失败或无 InputScope 时返回 0。随 focus_gained 上报给服务端做密码框等决策。
    UINT64 _QueryInputScopeMask(ITfDocumentMgr* pDocMgr);

    // 激活（含切回本输入法）时补读一次当前焦点的 InputScope / context 级 KEYBOARD_DISABLED，
    // 更新自留掩码并上报 input_state_report。原因：切换输入法不会重发 OnSetFocus，
    // 新实例若不补读，服务端会一直沿用上一实例留下的密码态。
    void _ReportFocusInputStateOnActivate();

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

    // 最近一次 ActivateEx 的时刻。激活后约百毫秒内系统会写 compartment 做初始化同步，不是用户
    // 操作，据此抑制（激活时本应用正是前台，_hasThreadFocus 挡不住）。手法同 _lastCapsKeyTick。
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
    // 提交 held 中文符号收口。nonKeyContext 语义同 CommitText：WM_TIMER 回调与从窗口消息/COM
    // 回调进来的 Flush 走异步；按键上下文里的 Flush 保持同步，以保证与后续透传字符的先后顺序。
    void           OnHoldTimerExpired(BOOL nonKeyContext = FALSE);
    static VOID CALLBACK HoldTimerProc(HWND hwnd, UINT uMsg, UINT_PTR idEvent, DWORD dwTime);

    // direct_commit 顶码的延迟组合状态，见 StashDeferredComposition。
    std::wstring   _deferredCompText;        // 待重开的余码组合；空=无待重开
    UINT           _deferredTriggerVk = 0;   // 延迟组合的触发键；0=任意 keyup 均可开
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
    // 向服务端要 setup 并建整组 host 窗口，建成后上报落位。prev 非空 = 复核重建，
    // 带上被换掉那个窗口的依据一并上报。
    void _CreateHostWindows(const wind::hostband::Placement* prev);
    // 已有 host 候选窗且需要按前台重建，判据见 HostBandPolicy.h。
    BOOL _HostBandNeedsRebuild();
    void _ReportHostRenderPlaced(const wind::hostband::Placement* prev);
    // 销毁宿主代理渲染窗口（释放共享内存映射 + 渲染线程 + Band 窗口）。
    // 仅在 Deactivate、_EnsureHostRenderSetup（强制刷新 / host render 不可用）和 RecheckHostBand
    // 时调用。不要在失焦时调用：locked/transient DocMgr（SearchHost / 任务管理器）会跳过
    // focus_gained，销毁后无法重建、候选永久不显示；失焦靠服务端 WriteHide 隐藏窗口即可。
    void _DestroyHostWindow();

public:
    // Perform full state sync with the service (sends IMEActivated + processes response).
    // Called after new/re-connection to ensure TSF and service state are consistent.
    // 返回 IME_ACTIVATED 是否真发出去了；SERVICE_READY 路径据此决定是否重试。
    // fromServiceReady：管道忙不计熔断、失败日志降为 INFO（重试路径自己打判决日志）。
    BOOL _DoFullStateSync(BOOL fromServiceReady = FALSE);
    // 按键 / 焦点路径已建立服务端状态：停掉 SERVICE_READY 的失败重试。
    void CancelServiceReadyRetry();
    void TryRecoverFocusState();
    // ScheduleFocusRecoveryRetry 的定时器到点时调（TSF 线程）：只补发 focus_gained。
    // 返回 TRUE = 补上了或不必再补；FALSE = 发送失败，由调用方决定是否再排一次。
    BOOL RetryFocusRecovery();
    // 按前台 band 复核 host 候选窗的落位，不符则整组重建。一段输入的第一个键时调，
    // 见 TextService.cpp 的实现注释。why 只进日志。
    void RecheckHostBand(const wchar_t* why);
    // 用当前焦点重发一次 focus_gained（caret / InputScope / 类名 / 标题现取）。延迟恢复与
    // 标题开关补发共用。why 只进日志。
    BOOL _ResendFocusGained(const wchar_t* why);

    // 焦点空窗记账，见 _focusGapStartTick。_Begin 可重复调用（只认第一次），
    // _End 在 focus_gained 真正送达后结算。
    void _BeginFocusGap(const WCHAR* reason);
    void _EndFocusGap();

    // 应用一份从 push pipe 收到的 activation status（_SyncStateFromResponse + _EnsureHostRenderSetup）。
    // 由 CLangBarItemButton::_MsgWndProc 在 WM_ACTIVATION_STATUS 上调用，在 TSF 线程。
    void ApplyActivationStatusResponse(const ServiceResponse& response);

    // Get display attribute GUID atom for composition
    TfGuidAtom GetDisplayAttributeInputAtom() { return _gaDisplayAttributeInput; }
};
