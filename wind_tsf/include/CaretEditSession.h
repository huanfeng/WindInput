#pragma once

#include "Globals.h"

class CTextService;

// 异步取坐标的用途。**回调的作废判据随用途而不同**，这是它必须存在的唯一理由：
//   Composition —— 组合期间取坐标，排队期间用户可能已上屏 ⇒ 判据是「组合还在不在」；
//   Focus       —— 焦点刚到达时取坐标，此时**根本没有组合** ⇒ 只能用焦点会话号判归属。
// 曾把两者压成同一条 `_pComposition == nullptr` 判据，结果焦点路径的回调 100% 被丢弃，
// 而日志上一切正常（请求受理成功、edit session 也跑完了），是个彻底静默的失效。
enum class CaretProbeKind
{
    Composition,
    Focus,
    // 组合刚启动即发起的**试探**：结果不走正式 caret_update，而是发 CMD_CARET_PROBE。
    // 目的是回答一个至今没有实测数据的问题——异步请求排在宿主当前 edit session 之后执行，
    // 那么它拿到的究竟是 reflow **前**还是**后**的坐标？
    // 走 probe 通道意味着 wait 档一律忽略、fast 档才读，因此本探测**不改变任何现有行为**。
    // 作废判据同 Composition（靠 _pComposition 判活）。
    FirstShowProbe,
};

// 异步取坐标的回调结果。
//
// **刻意用结构体而非平铺参数**：这里已有两个 RECT 和三个 BOOL，平铺后调用点就是一串没有
// 名字的 TRUE/FALSE，且每加一个字段都要改动所有调用点的实参顺序——顺序错了还不报错。
struct AsyncCaretResult
{
    RECT caretRect;
    RECT compStartRect;
    BOOL hasCompStart;
    // 整个组合 range 的包围矩形（GetTextExt(compRange)，未折叠）。
    // 与 compStartRect 的区别：后者把 range 折叠到起点，只答「组合从哪开始」；
    // 本字段答「组合占了多大一块」，换行后两者分处不同行。见 BinaryProtocol.h 的 CaretPayloadV3。
    RECT compRect;
    BOOL hasCompRect;
    // caret 无效时降级用了组合起点顶替。仍属 TSF 语义域（CARET_SRC_TSF_COMPOSITION）。
    BOOL usedCompStartAsCaret;
    CaretProbeKind kind;
    // 发起时刻的归属标记。Focus 用 _focusSessionId（回调到达时焦点可能已经切走，
    // 那份坐标属于上一个应用）；Composition 不用它，靠 _pComposition 判活。
    ULONGLONG sessionTag;
};

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

    // Execute the session and get both caret position and composition start position
    // 组合进行中 selection 的 GetTextExt 退化时，会降级用组合起点当 caret（见 DoEditSession）
    // compStartOffset: 组合起点偏移（wchar 数），见 SetCompositionStartOffset
    // pUsedCompStartAsCaret: 非空时输出「本次是否走了降级」，供调用方标注坐标来源（CARET_SRC_*）
    static BOOL GetCaretAndCompositionStartRect(ITfContext* pContext, TfClientId tfClientId,
                                                 ITfComposition* pComposition,
                                                 RECT* pCaretRect, RECT* pCompStartRect, BOOL* pHasCompStart,
                                                 LONG compStartOffset = 0,
                                                 BOOL* pUsedCompStartAsCaret = nullptr,
                                                 RECT* pCompRect = nullptr,
                                                 BOOL* pHasCompRect = nullptr);

    // 异步取坐标：用 TF_ES_ASYNCDONTCARE 请求锁，结果经 pOwner->OnAsyncCaretRectReady 回调返回。
    //
    // 上面两个同步入口用的 TF_ES_SYNC 只在**按键处理期间**可以期待成功——这是 MSDN 对该标志的
    // 明文限制（"should only be used in documented situations (such as keystroke handling)"）。
    // 在 WM_TIMER、OnLayoutChange 这类非按键上下文里，宿主可以合法地拒绝同步锁并返回
    // TS_E_SYNCHRONOUS（Word 实测 15/15 全拒），此时必须走异步：宿主会把请求排队，等文档可用
    // 时再回调 DoEditSession，而不是当场失败。
    //
    // 返回 TRUE 表示请求已被受理（可能已同步执行完，也可能排队等待回调），FALSE 表示发起失败。
    //
    // ⚠ `hrSession == S_OK` 意味着 manager 选择**内联执行**（记事本实测），此时 DoEditSession
    // 连同 OnAsyncCaretRectReady 回调已经在本函数返回**之前**跑完了。焦点路径依赖这一点：
    // 内联档下坐标能赶在同一次 OnSetFocus 的 SendFocusGained 之前就位。
    //
    // kind / sessionTag 见 CaretProbeKind 与 AsyncCaretResult。
    static BOOL RequestCaretRectAsync(ITfContext* pContext, TfClientId tfClientId,
                                       ITfComposition* pComposition, LONG compStartOffset,
                                       CTextService* pOwner,
                                       CaretProbeKind kind = CaretProbeKind::Composition,
                                       ULONGLONG sessionTag = 0);

    // Get the result after DoEditSession is called
    BOOL GetResult(RECT* prc);

    // Set composition to also query its start position
    void SetComposition(ITfComposition* pComposition) { _pComposition = pComposition; }
    // 组合起点偏移（wchar 数）：组合头部有顶码待提交前缀时，上报的组合起点
    // 应指向余码段起点（候选窗锚点跟随余码，而非已顶出的文字）。
    void SetCompositionStartOffset(LONG offset) { _compStartOffset = offset; }
    BOOL GetCompositionStartResult(RECT* prc);
    // 整个组合 range 的包围矩形；返回 FALSE 表示本次没取到（调用方应上报四值全 0）
    BOOL GetCompositionRectResult(RECT* prc);
    // 本次是否走了「用组合起点顶替 caret」的降级
    BOOL UsedCompStartAsCaret() const { return _usedCompStartAsCaret; }
    // 设为异步模式并持有 owner 强引用；见 RequestCaretRectAsync
    void SetAsyncOwner(CTextService* pOwner);
    // 异步回调的用途与归属标记，见 CaretProbeKind / AsyncCaretResult
    void SetProbe(CaretProbeKind kind, ULONGLONG sessionTag) { _probeKind = kind; _sessionTag = sessionTag; }

private:
    LONG _refCount;
    ITfContext* _pContext;
    ITfComposition* _pComposition;
    LONG _compStartOffset;
    RECT _caretRect;
    RECT _compositionStartRect;
    BOOL _hasCompositionStart;
    // 整个组合 range 的包围矩形，见 AsyncCaretResult::compRect
    RECT _compositionRect;
    BOOL _hasCompositionRect;
    BOOL _succeeded;
    // 本次是否走了「caret 无效 → 用组合起点顶替」的降级路径。用于给上报坐标标注来源：
    // 降级值仍属 TSF 语义域（CARET_SRC_TSF_COMPOSITION），与 GUI 回退有本质区别。
    BOOL _usedCompStartAsCaret;
    // 非空 = 异步模式：DoEditSession 完成后直接回调它，因为异步执行时静态入口早已返回、
    // 调用方拿不到结果。持有强引用（AddRef/Release），避免回调到达前 owner 被销毁。
    CTextService* _pAsyncOwner;
    CaretProbeKind _probeKind;
    ULONGLONG _sessionTag;
};
