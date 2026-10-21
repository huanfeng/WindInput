#include "CaretEditSession.h"
#include "TextService.h"
#include "Globals.h"
#include "CaretDefaultPosPolicy.h"
#include "ImmCandidateFormPolicy.h"
#include "ImmOpenCandidateNudge.h"
// 仅供下方「IMM32 候选位置」降级使用：走 CUAS 的老宿主（Java AWT、多数游戏）经 IMM32 声明
// 候选位置，而那条路不反映到 TSF 的 GetTextExt 上（见 DoEditSession 里那一级的注释）。
#include <imm.h>
#pragma comment(lib, "imm32.lib")

static_assert(wind::caret::kCfsDefault == CFS_DEFAULT, "ImmCandidateFormPolicy.h 与 imm.h 不一致");
static_assert(wind::caret::kCfsPoint == CFS_POINT, "ImmCandidateFormPolicy.h 与 imm.h 不一致");
static_assert(wind::caret::kCfsCandidatePos == CFS_CANDIDATEPOS, "ImmCandidateFormPolicy.h 与 imm.h 不一致");
static_assert(wind::caret::kCfsExclude == CFS_EXCLUDE, "ImmCandidateFormPolicy.h 与 imm.h 不一致");

CCaretEditSession::CCaretEditSession(ITfContext* pContext)
    : _refCount(1)
    , _pContext(pContext)
    , _pComposition(nullptr)
    , _compStartOffset(0)
    , _hasCompositionStart(FALSE)
    , _hasCompositionRect(FALSE)
    , _succeeded(FALSE)
    , _usedCompStartAsCaret(FALSE)
    , _usedImmCandidateForm(FALSE)
    , _pAsyncOwner(nullptr)
    , _probeKind(CaretProbeKind::Composition)
    , _sessionTag(0)
{
    if (_pContext)
    {
        _pContext->AddRef();
    }
    ZeroMemory(&_caretRect, sizeof(_caretRect));
    ZeroMemory(&_compositionStartRect, sizeof(_compositionStartRect));
    // `result.compRect` 是**无条件**拷贝的（见 DoEditSession 末尾），`_hasCompositionRect`
    // 为 FALSE 时拷的就是这块内存；二级降级让它成了降级链的承重件，更不能留未初始化值。
    ZeroMemory(&_compositionRect, sizeof(_compositionRect));
}

CCaretEditSession::~CCaretEditSession()
{
    SafeRelease(_pContext);
    SafeRelease(_pAsyncOwner);
}

STDAPI CCaretEditSession::QueryInterface(REFIID riid, void** ppvObj)
{
    if (ppvObj == nullptr)
        return E_INVALIDARG;

    *ppvObj = nullptr;

    if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfEditSession))
    {
        *ppvObj = (ITfEditSession*)this;
    }

    if (*ppvObj)
    {
        AddRef();
        return S_OK;
    }

    return E_NOINTERFACE;
}

STDAPI_(ULONG) CCaretEditSession::AddRef()
{
    return InterlockedIncrement(&_refCount);
}

STDAPI_(ULONG) CCaretEditSession::Release()
{
    LONG cr = InterlockedDecrement(&_refCount);

    if (cr == 0)
    {
        delete this;
    }

    return cr;
}

STDAPI CCaretEditSession::DoEditSession(TfEditCookie ec)
{
    _succeeded = FALSE;

    if (!_pContext)
    {
        WIND_LOG_ERROR(L"CaretEditSession: Context is null\n");
        return E_FAIL;
    }

    // Get the active view
    ITfContextView* pContextView = nullptr;
    HRESULT hr = _pContext->GetActiveView(&pContextView);
    if (FAILED(hr) || pContextView == nullptr)
    {
        WIND_LOG_ERROR(L"CaretEditSession: Failed to get active view\n");
        return hr;
    }

    // Get the current selection
    TF_SELECTION sel[1];
    ULONG fetched = 0;
    hr = _pContext->GetSelection(ec, TF_DEFAULT_SELECTION, 1, sel, &fetched);

    if (SUCCEEDED(hr) && fetched > 0 && sel[0].range != nullptr)
    {
        // Get the text extent of the selection (caret position)
        BOOL clipped = FALSE;
        hr = pContextView->GetTextExt(ec, sel[0].range, &_caretRect, &clipped);

        if (SUCCEEDED(hr))
        {
            _succeeded = TRUE;

            WIND_LOG_DEBUG_FMT(L"CaretEditSession: Got caret rect (%ld, %ld, %ld, %ld) clipped=%d\n",
                      _caretRect.left, _caretRect.top, _caretRect.right, _caretRect.bottom, clipped);
        }
        else
        {
            WIND_LOG_ERROR_FMT(L"CaretEditSession: GetTextExt failed hr=0x%08X\n", hr);
        }

        sel[0].range->Release();

        // If a composition is set, also get the start position of the composition range
        //
        // ⚠ 这里**不再受 _succeeded 守卫**（2026-08-01）：caret(selection) 取失败或退化时，
        // 组合起点往往仍然有效——实测 shell 的临时输入小窗，selection 恒返回退化矩形
        // (2559,1367,2560,1367) h=0，而 composition range 给出有效的 (473,189,473,217)。
        // 那时它是手上唯一可信的坐标，原先却因为这个守卫压根不去取。
        if (_pComposition != nullptr)
        {
            ITfRange* pCompRange = nullptr;
            hr = _pComposition->GetRange(&pCompRange);
            if (SUCCEEDED(hr) && pCompRange != nullptr)
            {
                // Clone the range and collapse to the start
                ITfRange* pStartRange = nullptr;
                hr = pCompRange->Clone(&pStartRange);
                if (SUCCEEDED(hr) && pStartRange != nullptr)
                {
                    pStartRange->Collapse(ec, TF_ANCHOR_START);
                    // 有顶码待提交前缀时，组合起点偏移到余码段起点
                    if (_compStartOffset > 0)
                    {
                        LONG moved = 0;
                        pStartRange->ShiftEnd(ec, _compStartOffset, &moved, nullptr);
                        pStartRange->ShiftStart(ec, _compStartOffset, &moved, nullptr);
                    }
                    BOOL clippedComp = FALSE;
                    hr = pContextView->GetTextExt(ec, pStartRange, &_compositionStartRect, &clippedComp);
                    if (SUCCEEDED(hr))
                    {
                        _hasCompositionStart = TRUE;
                        // 打全 4 值 + clipped：与上面 caret rect 同口径，便于直接比对两者差值
                        // （caret 落在组合内容之后、compStart 锚在组合头部，两者之差 = 已插入的
                        // 组合宽度）。只打 left/bottom 时看不出该 rect 是否退化（top==bottom），
                        // 而退化正是宿主尚未 reflow 的信号。
                        WIND_LOG_DEBUG_FMT(L"CaretEditSession: Composition start rect (%ld, %ld, %ld, %ld) clipped=%d\n",
                                  _compositionStartRect.left, _compositionStartRect.top,
                                  _compositionStartRect.right, _compositionStartRect.bottom, clippedComp);
                    }
                    else
                    {
                        // 失败即 compStart=(0,0) 上报，Rust 侧据此判定「本轮 reflow 坐标未到」而继续
                        // 等待。此前无日志，表现为「组合起点一直锁不上」却查不到原因。
                        WIND_LOG_DEBUG_FMT(L"CaretEditSession: Composition start GetTextExt failed hr=0x%08X\n", hr);
                    }
                    pStartRange->Release();
                }

                // ★ 整个组合 range 的包围矩形——与上面那次的区别只有「折不折叠」，
                // 但答的是完全不同的问题：折叠后答「组合从哪开始」，不折叠答「组合占了
                // 多大一块」。组合换行后这两者分处不同行，只有后者能看出「跨行了」。
                //
                // ⚠ 这里**不做任何判断、不参与决策**（2026-09-05 探针阶段）：各宿主对跨行
                // range 的 GetTextExt 行为尚未逐一验证，规范说返回包围矩形，实现可能只返回
                // 首行、也可能失败。先如实上报 + 打日志，看清真实返回值再定锚点公式。
                //
                // ⚠ TS_E_NOLAYOUT 不是「这个宿主不支持」，而是「此刻布局还没算完」——
                // 宿主随后会经 OnLayoutChange 通知，届时重查即可。日志必须把它与真失败分开，
                // 否则会把一个暂时状态误读成宿主能力缺失。
                BOOL clippedComp = FALSE;
                HRESULT hrCompRect = pContextView->GetTextExt(ec, pCompRange, &_compositionRect,
                                                              &clippedComp);
                if (SUCCEEDED(hrCompRect))
                {
                    _hasCompositionRect = TRUE;
                    WIND_LOG_DEBUG_FMT(L"CaretEditSession: Composition rect (%ld, %ld, %ld, %ld) "
                                       L"w=%ld h=%ld clipped=%d\n",
                                       _compositionRect.left, _compositionRect.top,
                                       _compositionRect.right, _compositionRect.bottom,
                                       _compositionRect.right - _compositionRect.left,
                                       _compositionRect.bottom - _compositionRect.top, clippedComp);
                }
                else if (hrCompRect == TS_E_NOLAYOUT)
                {
                    WIND_LOG_DEBUG(L"CaretEditSession: Composition rect 暂无布局 (TS_E_NOLAYOUT)，"
                                   L"等宿主 OnLayoutChange 后重查\n");
                }
                else
                {
                    WIND_LOG_DEBUG_FMT(L"CaretEditSession: Composition rect GetTextExt failed hr=0x%08X\n",
                                       hrCompRect);
                }

                pCompRange->Release();
            }
        }

        // ── 两级锚点降级的共同前提：本 context 的显示区，用作越界校验 ──────────
        //
        // GetScreenExt 是 TSF 语义内的参照系，不依赖窗口层级与前台状态。2026-09-12 实测
        // 它在游戏宿主上可靠：流放之路回 (273,216,2833,1656)，正是游戏窗口，而同一宿主给的
        // 组合坐标恒为 (13,44)（落在窗外）——据此就能把「宿主给的是垃圾坐标」判出来。
        //
        // ⚠ 它自身可能退化：shell context 上实测返回 (0,1368,0,1368)。退化时**不校验**（放行），
        //   否则会把正常宿主一并误杀。
        // ⚠ 只在确实要降级时才取：GetScreenExt 有跨宿主成本，且每帧打日志会淹掉其它行。
        //
        // ★ 与 `IsScreenPointOutsideAllMonitors`（TextService.cpp）**并存而非取代**。本注释
        // 前身写的是「若实测可靠，将来可取代『所有显示器』做越界校验」——2026-09-12 实测证明
        // 它确实可靠，但结论是两条都要留：那条问「这点落在任何一块屏上吗」，本条问「落在宿主
        // 自己声明的显示区内吗」，后者严得多也精确得多；而 GetScreenExt 会退化（下方分支），
        // 退化时只剩前者兜底，所以它独当不了一面。
        const auto caretUnusable = [&]() -> bool {
            return !_succeeded || (_caretRect.bottom - _caretRect.top) <= 0;
        };
        const LONG caretHeight = _caretRect.bottom - _caretRect.top;

        RECT rcScreenExt = {};
        bool hasScreenExt = false;
        // 宿主窗口句柄：二级降级按宿主视角换算 DPI、下方 IMM32 探测取 IMC，两处共用一次查询。
        HWND hwndHost = nullptr;
        if (caretUnusable())
        {
            (void)pContextView->GetWnd(&hwndHost);
            if (SUCCEEDED(pContextView->GetScreenExt(&rcScreenExt))
                && rcScreenExt.right > rcScreenExt.left && rcScreenExt.bottom > rcScreenExt.top)
            {
                hasScreenExt = true;
                WIND_LOG_DEBUG_FMT(L"CaretEditSession: context GetScreenExt = (%ld, %ld, %ld, %ld)\n",
                                   rcScreenExt.left, rcScreenExt.top, rcScreenExt.right, rcScreenExt.bottom);
            }
            else
            {
                WIND_LOG_DEBUG(L"CaretEditSession: context GetScreenExt 不可用或退化，本帧不做越界校验\n");
            }
        }
        // 只校验**锚点**（左上），不校验补出来的 bottom：组合矩形贴着显示区下沿时
        // 补完高度会探出去一点，而下游 place_window 有屏幕钳制兜着，为此拒掉一个好锚点不划算。
        const auto insideScreenExt = [&](LONG x, LONG y) -> bool {
            if (!hasScreenExt)
                return true;
            return x >= rcScreenExt.left && x <= rcScreenExt.right
                && y >= rcScreenExt.top && y <= rcScreenExt.bottom;
        };

        // 宿主没给高度时补的默认行高。二级降级与 IMM32 那一级共用。
        //
        // ★ 必须按**宿主视角**的 DPI 换算：GetTextExt / ClientToScreen 给的坐标就在宿主的感知
        // 级别下，直接补 20 个设备像素，在 4K/高缩放的宿主上只有真实行高的一半，候选窗顶边会
        // 落在正文行中段、压住正在输入的那一行——正是候选窗定位要消灭的症状。
        // ⚠ 不要套用 LangBarItemButton 里 SetThreadDpiAwarenessContext 抬高感知级别那套：
        // 那里要的是「主屏的物理真值」，这里要的是「与宿主坐标同参照系」，抬高反而错——
        // 宿主 unaware 时它回 96，而那时坐标本就是虚拟化的 96dpi 坐标，正好不该缩放。
        // 符号动态取（Win10 1607+），与本仓其余 DPI 调用同一惯例；取不到按 96 处理。
        const auto hostDefaultCaretHeight = [&]() -> LONG {
            static auto pGetDpiForWindow = reinterpret_cast<UINT(WINAPI*)(HWND)>(
                GetProcAddress(GetModuleHandleW(L"user32.dll"), "GetDpiForWindow"));
            UINT dpi = (pGetDpiForWindow != nullptr && hwndHost != nullptr) ? pGetDpiForWindow(hwndHost) : 0;
            if (dpi == 0)
                dpi = 96;
            return MulDiv(WIND_DEFAULT_CARET_HEIGHT, (int)dpi, 96);
        };

        // 「宿主对本 context 没有插入点可报」的指纹：三次布局查询给出同一个退化矩形（判据与
        // TextService 的 CARET_RETRY 判决共用 wind::caret::IsHostDefaultPosition）。必须在任何
        // 一级降级改动这几个矩形**之前**取，下方二级降级与 IMM32 那一级都要用它。
        const bool hostDefaultPos =
            _succeeded
            && wind::caret::IsHostDefaultPosition(_caretRect, _hasCompositionStart != FALSE,
                                                  _compositionStartRect, _hasCompositionRect != FALSE,
                                                  _compositionRect);
        // 这类宿主多半只在收到 IMN_OPENCANDIDATE 后才经 IMM32 报位置，而 CUAS 不替我们发——
        // 记下本线程，此后每次组合起止补发（见 ImmOpenCandidateNudge.h）。
        // 同线程别的输入框排版正常时解除，否则下方会一直不报组合起点。同样要在降级改动矩形之前判；
        // 与置位一样是线程级判定，不分取坐标的种类。
        if (hostDefaultPos)
            wind::caret::NoteHostDefaultPositionForImm();
        else if (wind::caret::ShowsLaidOutComposition(_succeeded != FALSE, _caretRect, _hasCompositionRect != FALSE,
                                                       _compositionRect))
            wind::caret::NoteLaidOutCompositionForImm();

        // ★ 一级降级：caret 无效而组合起点有效时，用组合起点当 caret。
        //
        // 候选窗本来就该跟随「正在编辑的那段文本」，而不是插入点——两者只差一个组合宽度。
        // 原先的行为是让上层判定 caret 无效后下坠到 GUIThreadInfo，去取一个**属于别的窗口**
        // 的系统光标（shell 场景实测取到任务栏残留的 (0,1388)，与真实位置差 1171px），
        // 那才是真正的错位。手上既然有同一个 edit session、同一个 cookie 下语义精确的矩形，
        // 就没有理由舍近求远。
        const LONG compStartHeight = _compositionStartRect.bottom - _compositionStartRect.top;
        if (caretUnusable() && _hasCompositionStart && compStartHeight > 0)
        {
            if (!insideScreenExt(_compositionStartRect.left, _compositionStartRect.top))
            {
                // 宿主给的组合起点落在它自己声明的显示区之外 ⇒ 是垃圾坐标，不采信。
                // 流放之路即此类：组合起点恒为 (13,44)，而显示区是 (273,216,2833,1656)。
                // 采信了它候选窗就钉死在屏幕左上角，比没有坐标更糟。
                WIND_LOG_DEBUG_FMT(L"CaretEditSession: 组合起点 (%ld, %ld) 落在 GetScreenExt "
                                   L"(%ld, %ld, %ld, %ld) 之外，判为垃圾坐标，不采信\n",
                                   _compositionStartRect.left, _compositionStartRect.top,
                                   rcScreenExt.left, rcScreenExt.top, rcScreenExt.right, rcScreenExt.bottom);
            }
            else
            {
                WIND_LOG_DEBUG_FMT(L"CaretEditSession: caret 无效(succeeded=%d h=%ld)，降级用组合起点 (%ld, %ld, %ld, %ld)\n",
                                   _succeeded, caretHeight,
                                   _compositionStartRect.left, _compositionStartRect.top,
                                   _compositionStartRect.right, _compositionStartRect.bottom);
                _caretRect = _compositionStartRect;
                _succeeded = TRUE;
                _usedCompStartAsCaret = TRUE;

                // ★★★ 降级帧的组合矩形同样作废。
                //
                // 走到这里意味着 selection 的 GetTextExt 失败或退化——那是「宿主此刻还没算完
                // 布局」（TS_E_NOLAYOUT 语义），而**同一次 edit session 里的其它布局查询同样
                // 不可信**，不只是 selection。QQ 实测（2026-09-05 12:28:36）：降级帧的
                // compRect=(2158,1498,2174,1532) w=16 h=34，是组合刚开始时那一个字符的旧矩形；
                // 紧随其后的正常帧是 (2158,1498,2410,1572) w=252 h=74。两者交替上报，锚点就在
                // 1532/1572 之间每秒抖十几次。
                //
                // ⚠ 服务端那条「caret 落在矩形内」的可信判据挡不住它：降级帧的 caret 与 rect
                // 是**一致地陈旧**——自洽，但都是旧值。只有在这里、在知道「本次降级过」的地方
                // 才判得了。
                //
                // 作废后服务端本帧取不到矩形，回退既有锚点逻辑；下一帧正常帧带着真矩形到达，
                // 锚点随即回到正确值。实测这一去一回不产生位移：降级帧的 caret 恰是组合起点，
                // 与已锁锚点的距离远不到逃生阀阈值。
                _hasCompositionRect = FALSE;
            }
        }

        // ★★ 二级降级：caret 与组合起点**都拿不到**，但「组合整体矩形」有效时，用它的左上角。
        //
        // 针对的是这样一类宿主：对**零长度** range 一律回 TS_E_NOLAYOUT，只有**非零长度**的
        // range 才给矩形，而且只填水平信息、把 bottom 留成 top（高度 0）。虚幻引擎的洛克王国
        // 实测即此形（2026-09-13，`wind_tsf.NRC-Win64-Shipping.*.log`）：
        //
        //     GetTextExt failed hr=0x80040505                       ← selection（零长度）
        //     Composition start GetTextExt failed hr=0x80040505     ← 组合起点（零长度）
        //     Composition rect (1353,1647,1399,1647) w=46 h=0       ← 组合整体，位置正确
        //
        // 宽度随编码串增长（16→19→25→28→46）、left/top 随输入移动，说明宿主**已经算完布局**，
        // 只是不填高度——这与「退化矩形 = 布局没算完」那个前提正相反，不该一并丢弃。此前三条路
        // 全断后退到兜底坐标 (640,332)，而真实位置 (1353,1647) 就摆在同一帧日志里没被用。
        //
        // ⚠ 必须与上面的越界校验配套，单独上会误伤：流放之路的组合矩形是 (3839,2063,3840,2063)，
        //   w=1 也 > 0，只靠「宽度非零」会把候选窗从左上角挪到右下角，一样错。是越界校验把它挡住的。
        //
        // ⚠ **不支持顶码偏移**（`_compStartOffset`）：一级降级用的组合起点已按它平移过
        //   （锚点跟随余码而非已顶出的文字），而整体矩形是一整块，无法按 wchar 偏移切分出
        //   余码那一段。于是顶码场景下走到本级的宿主，锚点会落在整段组合的左边而非余码起点。
        //   这是已知取舍而非遗漏：本级的前提本就是「零长度 range 一律拿不到矩形」，那种宿主
        //   连组合起点都给不出，没有更精确的来源可选；锚在整段左边仍远好于退到兜底坐标。
        // ⚠⚠ **失败关闭**：`hasScreenExt` 为假时本级直接不走（注意与一级降级相反——那级放行是
        //   既有行为，收紧会造成回归）。二级降级是新增能力，两边的代价不对称：GetScreenExt 拿不到
        //   时跳过它，只是退回没有本级时的行为（无损）；而放行则可能产出一个**自信的错坐标**——
        //   流放之路那份 (3839,2063,3840,2063) 只有越界校验一个人挡着，一旦放行，它会以
        //   CARET_SRC_TSF_COMPOSITION（TSF 权威域）的名义通过下游每一道闸（h>0、在显示器内、
        //   caret_is_valid），把「我没拿到坐标」伪装成「我拿到了一个权威坐标」，正是本文件末尾
        //   那段撤销记录要根除的谎报，且概率触发比必然触发更难查。
        // ⚠⚠ **宿主默认位置指纹命中时不走**（`hostDefaultPos`，2026-09-30）：医疗 HIS/LIS 的
        //   Java 客户端（CUAS 宿主）三次查询恒为工作区右下角 (1919,1039,1920,1039)，w=1 同样 > 0；
        //   窗口一最大化，GetScreenExt=(0,23,1920,1040) 就把这个角点包了进去，越界校验失守，
        //   本级把它补上高度以 TSF_COMPOSITION 送出，候选窗整场钉在屏幕右缘。流放之路那次靠的是
        //   角点恰好在窗外，这次证明越界校验单独挡不住——「三者全同」才是它没在答位置的证据
        //   （真在排版的宿主，组合矩形宽度随编码串增长，不会与零长度选区的矩形逐像素相同）。
        //   命中后交给下方 IMM32 那一级；它也拿不到时，由 CARET_RETRY 判决按宿主默认位置采信，
        //   与 Illustrator 同一条路，来源如实标成 TSF_DEFAULT_POS。
        if (caretUnusable() && hostDefaultPos && _hasCompositionRect)
        {
            WIND_LOG_DEBUG_FMT(L"CaretEditSession: 组合整体矩形 (%ld, %ld, %ld, %ld) 与选区、组合起点全同，"
                               L"是宿主默认位置而非布局，二级降级不采信\n",
                               _compositionRect.left, _compositionRect.top,
                               _compositionRect.right, _compositionRect.bottom);
        }
        if (caretUnusable()
            && wind::caret::AllowCompositionRectFallback(
                hostDefaultPos, hasScreenExt, _hasCompositionRect != FALSE,
                _compositionRect.right - _compositionRect.left,
                insideScreenExt(_compositionRect.left, _compositionRect.top)))
        {
            _caretRect = _compositionRect;
            if (_caretRect.bottom <= _caretRect.top)
            {
                // 宿主没给高度。候选窗只拿它决定「落在文本下方多远」。
                LONG bottom = _caretRect.top + hostDefaultCaretHeight();
                // ★ 宿主自己声明的显示区下沿若比这更近，就用它——那才是这一行文本真正的底，
                // 补出来的默认高度只是没有更好信息时的替代品。
                //
                // 洛克王国实测（2026-09-13，200% 缩放）：组合矩形 top=2004、默认高度换算后
                // 40px ⇒ 2044，而 GetScreenExt=(1066,1948,2174,2008) 的 bottom=2008 才是输入框
                // 下沿，候选窗因此比输入框低了 36px（肉眼可见地偏下）。取 min 后正好贴合。
                //
                // 两类宿主都不会更差：给**输入框**矩形的（如本例）取到真实行底；给**整个多行
                // 文本框**矩形的，组合在中间行时框底远在下方，min 自然落回默认高度，行为不变。
                // 守卫 `> _caretRect.top` 保证结果高度为正——退化矩形会被下游当「没拿到坐标」。
                if (hasScreenExt && rcScreenExt.bottom > _caretRect.top
                    && rcScreenExt.bottom < bottom)
                {
                    bottom = rcScreenExt.bottom;
                }
                _caretRect.bottom = bottom;
            }
            _succeeded = TRUE;
            // 语义上锚点同样来自组合区（而非 selection），故沿用同一来源标记，
            // 让服务端继续按 CARET_SRC_TSF_COMPOSITION 处理。
            _usedCompStartAsCaret = TRUE;
            // ★★ 与一级降级同样作废组合矩形——理由在这里更硬：协调器侧
            // （message_handler.rs 的 `frame_layout_degenerate`）把 `bottom == top` 判成
            // 「宿主连布局都没算出来」，会**整帧关闭逃生阀**；而本级的前提恰好相反（宿主算完了
            // 布局、只是不填高度）。两侧前提相反却看同一份矩形，留着它就是让协调器按错误前提
            // 永久关掉那个宿主的锚点纠错。矩形的信息已被吸收进 caret，重复上报不增加信息。
            _hasCompositionRect = FALSE;
            WIND_LOG_DEBUG_FMT(L"CaretEditSession: caret 与组合起点均不可用，二级降级用组合整体矩形 "
                               L"(%ld, %ld, %ld, %ld)，补高度后 caret=(%ld, %ld, h=%ld)\n",
                               _compositionRect.left, _compositionRect.top,
                               _compositionRect.right, _compositionRect.bottom,
                               _caretRect.left, _caretRect.top,
                               _caretRect.bottom - _caretRect.top);
        }

        // ── 三级降级：宿主经 IMM32 设的候选窗位置（CANDIDATEFORM）─────────────────
        //
        // 走到这里说明上面两级都没能给出可用的 caret。探测只在两种帧上做（其余退化帧多是宿主
        // 还没排完版，每帧都查 IMC、打一行日志只会淹掉别的行）：
        //   - `hostDefaultPos`：宿主经 TSF 明说没有插入点——唯一**会采信**的情形；
        //   - `!_succeeded`：GetTextExt 整个失败（游戏类宿主），只记日志、攒实测数据。
        // ⚠ 旧探测的条件只有后者，而 Illustrator 与这批 Java 宿主都是「成功但退化」——它们的
        //   IMM 数据此前一条都没有，是**结构上**就不会执行，不是宿主没设 CANDIDATEFORM。
        //
        // 走 IMM32→TSF 兼容层（CUAS）的宿主往往
        // **根本不经 TSF 传坐标**：
        //   - Java AWT/Swing：收到 IMN_OPENCANDIDATE / IMN_CHANGECANDIDATE 后按
        //     `getTextLocation(leading(0))` 算出组合起点**下沿**，经 `ImmSetCandidateWindow`
        //     写进 CANDIDATEFORM（客户区坐标）。GetTextExt 对它三次查询恒为工作区右下角
        //     (1919,1039,1920,1039)——医疗 HIS/LIS 客户端实测（2026-09-30 用户日志）。微软拼音
        //     在同一宿主上能跟随光标，靠的只能是这一份。
        //   - SDL 的 `SDL_SetTextInputRect` 同样实现为 `ImmSetCandidateWindow`，多数自绘 UI 的
        //     游戏同理。流放之路实测连 IMM32 也没设，那类宿主本级拿不到东西，行为不变。
        //
        // IMN_* 通知由 CUAS 在我们 BeginUIElement / UpdateUIElement 时发给宿主（日志里的
        // host_reads=1 就是 IMM32 桥在读候选串）；AWT 还要投递到 EDT 再回来才写 CANDIDATEFORM。
        // 所以组合的第一帧**通常**读到的是上一次组合留下的值，或者还没有值（退回原有路径：
        // CARET_RETRY 判决 / GUI 回退）。
        //
        // ★ 只认 CANDIDATEFORM，不认 COMPOSITIONFORM：前者答「候选窗放哪」，后者答「组合窗放哪」，
        //   不少宿主给后者的是输入框左上角。两者照旧一并打日志，供日后比对。
        // ★★ 采信后**作废组合起点**（上报 (0,0)），不改写成同一个点：服务端对同一次组合只锁
        //   首个有效组合起点，而首帧恰恰多半是上一次组合的旧值——连打时新旧起点只差已上屏那几个
        //   字的宽度，落不到大偏移逃生阀的阈值（3 倍行高）里，锁上就整场修不回来。不报起点，
        //   服务端锚点跟着逐帧的 caret 走，宿主写入新值的下一帧就跟上（AWT 的这个点在一次组合内
        //   本就不动，锁不锁无所谓）。留着那个右下角像素则更糟：锚点直接锁在角落上。
        //   组合整体矩形同样作废，理由同二级降级。
        // ⚠ CANDIDATEFORM 的 ptCurrentPos 是**客户区**坐标，ClientToScreen 必须用宿主设它时的
        //   那个窗口。hwndHost 取自 ITfContextView::GetWnd，CUAS context 下即 IMC 所属窗口；
        //   若不是，换算结果会整体偏出显示区，被 AcceptCandidateFormPoint 的越界校验挡住。
        //   ⚠ 已知缺口：AWT 按**顶层窗口**客户区原点换算（GetTopLevelParentForWindow），IMC 所属的
        //   却可能是它的子窗口。Swing 的焦点代理窗口在框架 (0,0) 处，两者等价；纯 AWT 重量级
        //   控件（java.awt.TextArea）会整体偏一个子窗口偏移，且多半仍在显示区内、挡不住。日志里
        //   一并打出按 GA_ROOT 换算的值，等真机数据定夺。
        if (caretUnusable() && (hostDefaultPos || !_succeeded))
        {
            if (hwndHost != nullptr)
            {
                HIMC himc = ImmGetContext(hwndHost);
                if (himc != nullptr)
                {
                    CANDIDATEFORM cand = {};
                    COMPOSITIONFORM comp = {};
                    const BOOL okCand = ImmGetCandidateWindow(himc, 0, &cand);
                    const BOOL okComp = ImmGetCompositionWindow(himc, &comp);
                    ImmReleaseContext(hwndHost, himc);
                    POINT ptCand = cand.ptCurrentPos;
                    POINT ptComp = comp.ptCurrentPos;
                    ClientToScreen(hwndHost, &ptCand);
                    ClientToScreen(hwndHost, &ptComp);
                    const HWND hwndRoot = GetAncestor(hwndHost, GA_ROOT);
                    POINT ptCandRoot = cand.ptCurrentPos;
                    if (hwndRoot != nullptr)
                        ClientToScreen(hwndRoot, &ptCandRoot);
                    WIND_LOG_DEBUG_FMT(
                        L"CaretEditSession: IMM32 probe hwnd=0x%p root=0x%p cand=%d style=0x%08X "
                        L"client=(%ld,%ld) screen=(%ld,%ld) viaRoot=(%ld,%ld) area=(%ld,%ld,%ld,%ld) | "
                        L"comp=%d style=0x%08X client=(%ld,%ld) screen=(%ld,%ld) hostDefaultPos=%d\n",
                        (void*)hwndHost, (void*)hwndRoot,
                        okCand ? 1 : 0, cand.dwStyle,
                        cand.ptCurrentPos.x, cand.ptCurrentPos.y, ptCand.x, ptCand.y,
                        ptCandRoot.x, ptCandRoot.y,
                        cand.rcArea.left, cand.rcArea.top, cand.rcArea.right, cand.rcArea.bottom,
                        okComp ? 1 : 0, comp.dwStyle,
                        comp.ptCurrentPos.x, comp.ptCurrentPos.y, ptComp.x, ptComp.y, hostDefaultPos ? 1 : 0);

                    if (wind::caret::AcceptCandidateFormPoint(hostDefaultPos, okCand != FALSE, cand.dwStyle,
                                                              ptCand.x, ptCand.y, hasScreenExt, rcScreenExt))
                    {
                        // 落点是文本行**下沿**：caret 的 bottom 取它，top 往上补一个默认行高。
                        _caretRect.left = ptCand.x;
                        _caretRect.right = ptCand.x + 1;
                        _caretRect.bottom = ptCand.y;
                        _caretRect.top = ptCand.y - hostDefaultCaretHeight();
                        _hasCompositionStart = FALSE;
                        _hasCompositionRect = FALSE;
                        _succeeded = TRUE;
                        _usedCompStartAsCaret = FALSE;
                        _usedImmCandidateForm = TRUE;
                        WIND_LOG_DEBUG_FMT(L"CaretEditSession: 三级降级采信 IMM32 CANDIDATEFORM (%ld,%ld)，"
                                           L"caret=(%ld, %ld, h=%ld)\n",
                                           ptCand.x, ptCand.y, _caretRect.left, _caretRect.top,
                                           _caretRect.bottom - _caretRect.top);
                    }
                }
                else
                {
                    WIND_LOG_DEBUG_FMT(L"CaretEditSession: IMM32 probe hwnd=0x%p 无 IMC（宿主未开 IMM32 上下文）\n",
                                       (void*)hwndHost);
                }
            }
            else
            {
                WIND_LOG_DEBUG(L"CaretEditSession: IMM32 probe 取不到宿主 HWND\n");
            }
        }
    }
    else
    {
        // No selection, try to get the end of the document or use insertion point
        WIND_LOG_DEBUG(L"CaretEditSession: No selection available\n");

        // ⚠ 这里曾用「GetScreenExt 左上角 + 20px」冒充插入点并置 _succeeded=TRUE
        // （2026-08-01 撤销）。那是一条**以成功的形式失败**的路径：
        //
        // - 返回的根本不是插入点，而是整个显示区的左上角，除非碰巧否则必错；
        // - `height` 恒为 20 > 0，躲过 GetCaretPositionFromTSF 的退化矩形判据；
        // - 左上角通常落在某个显示器内，也躲过越界判据；
        // - 于是它一路以 CARET_SRC_TSF_SELECTION 的名义流到定位逻辑，把「我没拿到坐标」
        //   伪装成「我拿到了一个 TSF 权威坐标」——正是本轮要根除的那类谎报。
        //
        // 而且 GetScreenExt 在 shell context 上实测返回退化矩形 (0,1368,0,1368)，
        // 占位值会直接落到屏幕边缘。
        //
        // 现在一律判失败。上层回退链会接手（GUI caret → last known），那些值虽然也不精确，
        // 但**标着自己真实的来源**，消费端能据此决定要不要用。焦点路径尤其依赖这一点：
        // 焦点刚到达时 selection 尚未建立是常态，这条分支在那里是主路径而非冷门分支。
        RECT rcScreenExt = {};
        if (SUCCEEDED(pContextView->GetScreenExt(&rcScreenExt)))
        {
            WIND_LOG_DEBUG_FMT(L"CaretEditSession: 无 selection；GetScreenExt=(%ld,%ld,%ld,%ld)，不作插入点采信\n",
                               rcScreenExt.left, rcScreenExt.top, rcScreenExt.right, rcScreenExt.bottom);
        }
        else
        {
            WIND_LOG_DEBUG(L"CaretEditSession: 无 selection 且 GetScreenExt 失败\n");
        }
    }

    pContextView->Release();

    // 补发 IMN_OPENCANDIDATE 的宿主：位置由宿主异步回写（AWT 经 EDT），组合首帧读到的多半是
    // 上一次组合的旧值。不报组合起点，服务端锚点逐帧跟随 caret，回写一到就跟上；报了则旧值
    // 被锁成整场组合的锚点（换输入框时新旧差不到大偏移逃生阀的 3 倍行高）。三级降级采信时
    // 已自行作废，这里覆盖 CUAS 改答 CANDIDATEFORM 点、走 selection 路径的情形。
    if (_succeeded && wind::caret::IsImmOpenCandidateHost())
    {
        _hasCompositionStart = FALSE;
        _hasCompositionRect = FALSE;
    }

    // 异步模式：结果只能从这里出去——静态入口在排队执行时早已返回。
    // 失败时**不回调**：服务端会继续等自己的兜底超时，用按键时缓存的坐标显示，
    // 那份坐标来自按键路径的同步 edit session，比任何回退值都可信。
    if (_pAsyncOwner != nullptr)
    {
        if (_succeeded)
        {
            AsyncCaretResult result = {};
            result.caretRect            = _caretRect;
            result.compStartRect        = _compositionStartRect;
            result.hasCompStart         = _hasCompositionStart;
            result.compRect             = _compositionRect;
            result.hasCompRect          = _hasCompositionRect;
            result.usedCompStartAsCaret = _usedCompStartAsCaret;
            result.usedImmCandidateForm = _usedImmCandidateForm;
            result.kind                 = _probeKind;
            result.sessionTag           = _sessionTag;
            _pAsyncOwner->OnAsyncCaretRectReady(result);
        }
        else
        {
            WIND_LOG_DEBUG(L"CaretEditSession(async): no rect obtained, not notifying owner\n");
        }
    }

    return _succeeded ? S_OK : E_FAIL;
}

BOOL CCaretEditSession::GetResult(RECT* prc)
{
    if (_succeeded && prc)
    {
        *prc = _caretRect;
        return TRUE;
    }
    return FALSE;
}

void CCaretEditSession::SetAsyncOwner(CTextService* pOwner)
{
    SafeRelease(_pAsyncOwner);
    _pAsyncOwner = pOwner;
    if (_pAsyncOwner)
    {
        _pAsyncOwner->AddRef();
    }
}

BOOL CCaretEditSession::GetCompositionStartResult(RECT* prc)
{
    if (_hasCompositionStart && prc)
    {
        *prc = _compositionStartRect;
        return TRUE;
    }
    return FALSE;
}

BOOL CCaretEditSession::GetCompositionRectResult(RECT* prc)
{
    if (_hasCompositionRect && prc)
    {
        *prc = _compositionRect;
        return TRUE;
    }
    return FALSE;
}

// Static method to get both caret rect and composition start rect
BOOL CCaretEditSession::GetCaretAndCompositionStartRect(ITfContext* pContext, TfClientId tfClientId,
                                                         ITfComposition* pComposition,
                                                         RECT* pCaretRect, RECT* pCompStartRect, BOOL* pHasCompStart,
                                                         LONG compStartOffset,
                                                         BOOL* pUsedCompStartAsCaret,
                                                         RECT* pCompRect, BOOL* pHasCompRect,
                                                         BOOL* pUsedImmCandidateForm)
{
    if (pUsedCompStartAsCaret)
    {
        *pUsedCompStartAsCaret = FALSE;
    }
    if (pUsedImmCandidateForm)
    {
        *pUsedImmCandidateForm = FALSE;
    }
    if (pHasCompRect)
    {
        *pHasCompRect = FALSE;
    }
    if (pContext == nullptr || pCaretRect == nullptr)
    {
        return FALSE;
    }

    CCaretEditSession* pEditSession = new CCaretEditSession(pContext);
    if (pEditSession == nullptr)
    {
        return FALSE;
    }

    pEditSession->SetComposition(pComposition);
    pEditSession->SetCompositionStartOffset(compStartOffset);

    HRESULT hrSession = S_OK;
    HRESULT hr = pContext->RequestEditSession(
        tfClientId,
        pEditSession,
        TF_ES_SYNC | TF_ES_READ,
        &hrSession
    );

    BOOL result = FALSE;
    if (SUCCEEDED(hr) && SUCCEEDED(hrSession))
    {
        result = pEditSession->GetResult(pCaretRect);
        if (pCompStartRect && pHasCompStart)
        {
            *pHasCompStart = pEditSession->GetCompositionStartResult(pCompStartRect);
        }
        if (pUsedCompStartAsCaret)
        {
            *pUsedCompStartAsCaret = pEditSession->UsedCompStartAsCaret();
        }
        if (pUsedImmCandidateForm)
        {
            *pUsedImmCandidateForm = pEditSession->UsedImmCandidateForm();
        }
        if (pCompRect && pHasCompRect)
        {
            *pHasCompRect = pEditSession->GetCompositionRectResult(pCompRect);
        }
    }
    else
    {
        WIND_LOG_ERROR_FMT(L"RequestEditSession failed hr=0x%08X, hrSession=0x%08X\n", hr, hrSession);
    }

    pEditSession->Release();
    return result;
}

// Static method to request the caret rect asynchronously (see header for why)
BOOL CCaretEditSession::RequestCaretRectAsync(ITfContext* pContext, TfClientId tfClientId,
                                               ITfComposition* pComposition, LONG compStartOffset,
                                               CTextService* pOwner,
                                               CaretProbeKind kind, ULONGLONG sessionTag)
{
    if (pContext == nullptr || pOwner == nullptr)
    {
        return FALSE;
    }

    CCaretEditSession* pEditSession = new CCaretEditSession(pContext);
    if (pEditSession == nullptr)
    {
        return FALSE;
    }

    pEditSession->SetComposition(pComposition);
    pEditSession->SetCompositionStartOffset(compStartOffset);
    pEditSession->SetAsyncOwner(pOwner);
    pEditSession->SetProbe(kind, sessionTag);

    HRESULT hrSession = S_OK;
    HRESULT hr = pContext->RequestEditSession(
        tfClientId,
        pEditSession,
        TF_ES_ASYNCDONTCARE | TF_ES_READ,
        &hrSession
    );

    // 释放我们这一份引用。异步排队时 TSF 自己持有一份，对象活到 DoEditSession 回调完成为止。
    pEditSession->Release();

    if (FAILED(hr))
    {
        WIND_LOG_ERROR_FMT(L"RequestCaretRectAsync: RequestEditSession failed hr=0x%08X\n", hr);
        return FALSE;
    }

    // hrSession == TF_S_ASYNC 表示已排队、回调稍后到达；S_OK 表示 manager 选择了同步执行，
    // 此时回调已经在上面的调用里跑完了。两者都算受理成功。
    WIND_LOG_DEBUG_FMT(L"RequestCaretRectAsync: accepted hrSession=0x%08X (%s)\n",
                       hrSession,
                       hrSession == TF_S_ASYNC ? L"queued" : L"executed inline");
    return TRUE;
}
