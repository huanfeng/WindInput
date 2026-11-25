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
    // `result.compRect` 无条件拷贝（见 DoEditSession 末尾），`_hasCompositionRect` 为 FALSE 时
    // 拷的就是这块内存，不能留未初始化值。
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

            // 组合进行中（含刚开、尚未重排时的试探），Chromium 报的可能是上一个输入框里那次组合的
            // 旧坐标。落在本 context 显示区之外就当无效，交给下方降级；仍取不到时调用方沿用上次坐标。
            if ((_pComposition != nullptr || _probeKind == CaretProbeKind::FirstShowProbe)
                && IsOutsideViewExt(pContextView, _caretRect))
            {
                WIND_LOG_DEBUG(L"CaretEditSession: 坐标落在本 context 显示区之外，不采信\n");
                _succeeded = FALSE;
                _rejectedOutsideView = TRUE;
            }
        }
        else
        {
            WIND_LOG_ERROR_FMT(L"CaretEditSession: GetTextExt failed hr=0x%08X\n", hr);
        }

        sel[0].range->Release();

        // 组合起点不受 _succeeded 守卫：selection 取失败或退化时（如 shell 临时输入小窗），
        // 组合起点往往仍有效，是唯一可信的坐标。
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
                        // 打全 4 值：与 caret rect 同口径便于比对，且能看出是否退化
                        // （top==bottom 是宿主尚未 reflow 的信号）。
                        WIND_LOG_DEBUG_FMT(L"CaretEditSession: Composition start rect (%ld, %ld, %ld, %ld) clipped=%d\n",
                                  _compositionStartRect.left, _compositionStartRect.top,
                                  _compositionStartRect.right, _compositionStartRect.bottom, clippedComp);
                    }
                    else
                    {
                        // 失败即上报 compStart=(0,0)，Rust 侧据此判定「本轮 reflow 坐标未到」继续等。
                        WIND_LOG_DEBUG_FMT(L"CaretEditSession: Composition start GetTextExt failed hr=0x%08X\n", hr);
                    }
                    pStartRange->Release();
                }

                // 整个组合 range 的包围矩形（不折叠）：答「组合占了多大一块」，换行后只有它能
                // 看出跨行。这里只如实上报，取舍在服务端与下方降级里做；形态见
                // docs/architecture/caret-position-host-compat-notes.md 的「组合矩形的三种形态」。
                // TS_E_NOLAYOUT 是「此刻布局还没算完」（宿主随后发 OnLayoutChange），日志要与真失败分开。
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

        // ── 降级的共同前提：本 context 的显示区（GetScreenExt），用作越界校验 ──
        //
        // 宿主给的垃圾坐标（F 类游戏宿主）靠「落在自己声明的显示区外」判出来。GetScreenExt 自身
        // 可能退化（shell context），退化时不校验，否则误杀正常宿主。只在要降级时才取（有跨进程成本），
        // Chromium 系例外见 IsOutsideViewExt。与 TextService.cpp 的 `IsScreenPointOutsideAllMonitors`
        // 并存：本条更严，但退化时只剩那条兜底。
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

        // 宿主没给高度时补的默认行高，二级降级与 IMM32 那一级共用。必须按宿主视角的 DPI 换算
        // （坐标就在宿主的感知级别下），别套 LangBarItemButton 抬高线程感知级别那套：宿主 unaware
        // 时回 96，坐标本就是虚拟化的 96dpi，正好不该缩放。符号动态取（Win10 1607+），取不到按 96。
        const auto hostDefaultCaretHeight = [&]() -> LONG {
            static auto pGetDpiForWindow = reinterpret_cast<UINT(WINAPI*)(HWND)>(
                GetProcAddress(GetModuleHandleW(L"user32.dll"), "GetDpiForWindow"));
            UINT dpi = (pGetDpiForWindow != nullptr && hwndHost != nullptr) ? pGetDpiForWindow(hwndHost) : 0;
            if (dpi == 0)
                dpi = 96;
            return MulDiv(WIND_DEFAULT_CARET_HEIGHT, (int)dpi, 96);
        };

        // 「宿主对本 context 没有插入点可报」的指纹：三次布局查询给出同一个退化矩形（与 CARET_RETRY
        // 判决共用 IsHostDefaultPosition）。必须在任何一级降级改动这几个矩形之前取。
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

        // 一级降级：caret 无效而组合起点有效时，用组合起点当 caret。候选窗本就跟随正在编辑的文本，
        // 否则上层会下坠到 GUIThreadInfo，取到属于别的窗口的系统光标。
        const LONG compStartHeight = _compositionStartRect.bottom - _compositionStartRect.top;
        if (caretUnusable() && _hasCompositionStart && compStartHeight > 0)
        {
            if (!insideScreenExt(_compositionStartRect.left, _compositionStartRect.top))
            {
                // 组合起点落在宿主自己声明的显示区之外 ⇒ 垃圾坐标，采信了候选窗会钉死在屏幕角上。
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

                // 降级帧的组合矩形同样作废：selection 退化说明宿主还没算完布局，同一 edit session
                // 里的其它查询也是旧值，且 caret 与矩形一致地陈旧，服务端的「caret 落在矩形内」判据
                // 挡不住，只有这里知道本帧降级过。见 caret-position-host-compat-notes.md 的 E 类。
                _hasCompositionRect = FALSE;
            }
        }

        // 二级降级：caret 与组合起点都拿不到、但组合整体矩形有效时，用它的左上角。针对只对非零长度
        // range 给矩形且不填高度的宿主（G 类），详见 caret-position-host-compat-notes.md。
        // - 必须配越界校验：只靠「宽度非零」会采信 F 类宿主角落里 w=1 的垃圾矩形。
        // - 失败关闭（无 GetScreenExt 就不走，与一级降级相反）：跳过只是退回无本级的行为，放行则可能
        //   以 TSF_COMPOSITION 权威来源送出错坐标。
        // - 宿主默认位置指纹命中时不走：最大化后显示区会包住那个角点，越界校验单独挡不住。
        // - 不支持顶码偏移（整体矩形切不出余码段），已知取舍。
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
                // 显示区下沿更近时取它，那才是这一行真正的底；给整个多行文本框矩形的宿主，min 自然
                // 落回默认高度。守卫 `> _caretRect.top` 保证高度为正（退化矩形下游当「没拿到坐标」）。
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
            // 同样作废组合矩形：协调器的 `frame_layout_degenerate` 把 `bottom == top` 判成布局没算完、
            // 整帧关闭逃生阀，与本级前提相反；矩形的信息已吸收进 caret。
            _hasCompositionRect = FALSE;
            WIND_LOG_DEBUG_FMT(L"CaretEditSession: caret 与组合起点均不可用，二级降级用组合整体矩形 "
                               L"(%ld, %ld, %ld, %ld)，补高度后 caret=(%ld, %ld, h=%ld)\n",
                               _compositionRect.left, _compositionRect.top,
                               _compositionRect.right, _compositionRect.bottom,
                               _caretRect.left, _caretRect.top,
                               _caretRect.bottom - _caretRect.top);
        }

        // 三级降级：宿主经 IMM32 设的候选窗位置（CANDIDATEFORM）。走 CUAS 的宿主（Java AWT/Swing、
        // SDL 游戏）往往只经 ImmSetCandidateWindow 声明位置，不反映到 GetTextExt。
        // 只在两种帧上探测：`hostDefaultPos`（唯一会采信的情形）与 `!_succeeded`（只记日志）；
        // 其余退化帧多是还没排完版，每帧查 IMC 只会淹日志。
        // - 只认 CANDIDATEFORM，不认 COMPOSITIONFORM（不少宿主给后者的是输入框左上角）。
        // - 采信后作废组合起点与组合矩形：宿主异步回写，首帧多是上次组合的旧值，锁成锚点就整场修不回。
        // - ptCurrentPos 是客户区坐标，按 GetWnd 的窗口换算；AWT 子窗口偏移的已知缺口与日志里
        //   viaRoot 的用途见 caret-position-host-compat-notes.md 的「第三条通道」。
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

        // 一律判失败，交给标着真实来源的回退链（GUI caret → last known）。别拿 GetScreenExt
        // 左上角冒充插入点报成功：它会躲过下游所有判据、伪装成 TSF 权威坐标。焦点刚到达时
        // selection 未建立是常态，这条是主路径。见 caret-position-host-compat-notes.md「已否定的方向」。
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

    // 补发 IMN_OPENCANDIDATE 的宿主异步回写位置，组合首帧多是旧值，不报组合起点以免被锁成锚点。
    // 三级降级采信时已自行作废，这里覆盖 CUAS 改答 CANDIDATEFORM 点、走 selection 路径的情形。
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

BOOL CCaretEditSession::IsChromiumView(ITfContextView* pView)
{
    HWND hwnd = nullptr;
    WCHAR cls[64] = {};
    return pView != nullptr && SUCCEEDED(pView->GetWnd(&hwnd)) && hwnd != nullptr &&
           GetClassNameW(hwnd, cls, ARRAYSIZE(cls)) != 0 &&
           (wcsncmp(cls, L"Chrome_WidgetWin_", 17) == 0 || wcscmp(cls, L"Chrome_RenderWidgetHostHWND") == 0);
}

BOOL CCaretEditSession::IsChromiumContext(ITfContext* pContext)
{
    ITfContextView* pView = nullptr;
    if (pContext == nullptr || FAILED(pContext->GetActiveView(&pView)) || pView == nullptr)
        return FALSE;
    const BOOL yes = IsChromiumView(pView);
    pView->Release();
    return yes;
}

BOOL CCaretEditSession::IsOutsideViewExt(ITfContextView* pView, const RECT& rc)
{
    // 只认 Chromium 系：旧坐标是它组合字符框缓存的问题，别的宿主的显示区未必覆盖插入点
    // （隐藏 IME 窗、单元格编辑框外扩等），不拿它去否决。
    if (!IsChromiumView(pView))
        return FALSE;
    RECT ext = {};
    if (FAILED(pView->GetScreenExt(&ext)) || ext.right <= ext.left || ext.bottom <= ext.top)
    {
        return FALSE;
    }
    return rc.left < ext.left || rc.left > ext.right || rc.top < ext.top || rc.top > ext.bottom;
}

// Static method to get both caret rect and composition start rect
BOOL CCaretEditSession::GetCaretAndCompositionStartRect(ITfContext* pContext, TfClientId tfClientId,
                                                         ITfComposition* pComposition,
                                                         RECT* pCaretRect, RECT* pCompStartRect, BOOL* pHasCompStart,
                                                         LONG compStartOffset,
                                                         BOOL* pUsedCompStartAsCaret,
                                                         RECT* pCompRect, BOOL* pHasCompRect,
                                                         BOOL* pUsedImmCandidateForm,
                                                         BOOL* pRejectedOutsideView)
{
    if (pRejectedOutsideView)
    {
        *pRejectedOutsideView = FALSE;
    }
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

    if (pRejectedOutsideView)
    {
        *pRejectedOutsideView = pEditSession->RejectedOutsideView();
    }

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
