#pragma once

#include "Globals.h"

class CTextService;

// 异步取坐标的用途。它存在是因为回调的作废判据随用途而不同：
//   Composition —— 排队期间用户可能已上屏，判据是「组合还在不在」；
//   Focus       —— 焦点刚到达时没有组合，只能用焦点会话号判归属。
// 两者不能合并成 `_pComposition == nullptr` 一条判据：焦点路径的回调会被全部静默丢弃。
enum class CaretProbeKind
{
    Composition,
    Focus,
    // 组合刚启动即发起的试探：结果发 CMD_CARET_PROBE 而非 caret_update，用来观察排在宿主
    // edit session 之后的异步请求拿到的是 reflow 前还是后的坐标。wait 档忽略 probe，故不改变
    // 现有行为。作废判据同 Composition。
    FirstShowProbe,
    // CARET_RETRY 定时器到期后的那一次取坐标，即重试窗口已过完。与 Composition 的唯一区别是
    // 退化矩形（h<=0）不再丢弃：等满窗口仍退化，说明宿主在答「没有插入点可报」，采信并上报
    // CARET_SRC_TSF_DEFAULT_POS（见 BinaryProtocol.h）。
    // 不能让定时器沿用 Composition：OnLayoutChange 驱动的 probe 也会短暂见到退化矩形，混在一起
    // 就会提前采信、多一次闪跳。作废判据同 Composition。
    RetryDeadline,
};

// 异步取坐标的回调结果。用结构体而非平铺参数：多个 RECT/BOOL 平铺后实参顺序错了编译器不报错。
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
    // caret 无效时降级用了宿主经 IMM32 设的候选窗位置（CARET_SRC_IMM_CANDIDATE_FORM）。
    // 为真时 usedCompStartAsCaret 恒为假，hasCompStart 恒为假（首帧多是上一次组合的旧值，
    // 报成组合起点会被服务端锁住），见 CaretEditSession.cpp 的三级降级。
    BOOL usedImmCandidateForm;
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
                                                 BOOL* pHasCompRect = nullptr,
                                                 BOOL* pUsedImmCandidateForm = nullptr,
                                                 BOOL* pRejectedOutsideView = nullptr);

    /// Chromium 系宿主里，矩形锚点（左上）是否落在本 context 自报的显示区之外。其它宿主、显示区取不到
    /// 或退化时答 FALSE（放行）。组合进行中 Chromium 可能报上一个输入框那次组合的旧坐标，靠它识别。
    static BOOL IsOutsideViewExt(ITfContextView* pView, const RECT& rc);

    /// 宿主是否 Chromium 系（Chrome / Edge / Electron，窗口类 Chrome_WidgetWin_*）。
    static BOOL IsChromiumView(ITfContextView* pView);
    static BOOL IsChromiumContext(ITfContext* pContext);

    // 异步取坐标：用 TF_ES_ASYNCDONTCARE 请求锁，结果经 pOwner->OnAsyncCaretRectReady 回调返回。
    //
    // TF_ES_SYNC 只在按键处理期间可期待成功；WM_TIMER、OnLayoutChange 等上下文里宿主可合法
    // 返回 TS_E_SYNCHRONOUS（Word 恒拒），须走异步排队。见 docs/architecture/
    // caret-position-host-compat-notes.md 的「DLL 侧取坐标的 edit session」。
    //
    // 返回 TRUE 表示请求已受理（可能已内联执行完，也可能排队），FALSE 表示发起失败。
    // hrSession == S_OK 时回调已在本函数返回前跑完；焦点路径依赖这一点让坐标赶在
    // SendFocusGained 之前就位。kind / sessionTag 见 CaretProbeKind 与 AsyncCaretResult。
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
    // 本次是否走了「caret 无效 → 用宿主 IMM32 CANDIDATEFORM 顶替」的三级降级
    BOOL UsedImmCandidateForm() const { return _usedImmCandidateForm; }
    BOOL RejectedOutsideView() const { return _rejectedOutsideView; }
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
    // 本次是否走了三级降级（IMM32 CANDIDATEFORM）。见 DoEditSession 里那一级的注释。
    BOOL _usedImmCandidateForm;
    BOOL _rejectedOutsideView = FALSE; // 取到的坐标落在显示区外被弃用（旧坐标），调用方别再找别的光标顶替
    // 非空 = 异步模式：DoEditSession 完成后直接回调它，因为异步执行时静态入口早已返回、
    // 调用方拿不到结果。持有强引用（AddRef/Release），避免回调到达前 owner 被销毁。
    CTextService* _pAsyncOwner;
    CaretProbeKind _probeKind;
    ULONGLONG _sessionTag;
};
