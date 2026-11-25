#include "HostWindow.h"
#include "FileLogger.h"
#include "HostBandPolicy.h"
#include "HostClickPolicy.h"
#include "HostPassLayerPolicy.h"
#include "IPCClient.h"
#include <climits>
#ifndef WHEEL_DELTA
#define WHEEL_DELTA 120
#endif

// WS_EX constants for layered window
#ifndef WS_EX_LAYERED
#define WS_EX_LAYERED     0x00080000
#endif
#ifndef WS_EX_TOPMOST
#define WS_EX_TOPMOST     0x00000008
#endif
#ifndef WS_EX_TOOLWINDOW
#define WS_EX_TOOLWINDOW  0x00000080
#endif
#ifndef WS_EX_NOACTIVATE
#define WS_EX_NOACTIVATE  0x08000000
#endif
#ifndef WS_EX_TRANSPARENT
#define WS_EX_TRANSPARENT 0x00000020
#endif

static const wchar_t* HOST_WND_CLASS = L"WindInputHostCandidateWnd";
// Accessed only on the UI thread (STA). No synchronization needed.
static ATOM s_hostWndClassAtom = 0;

CHostWindow::CHostWindow()
    : _hwnd(NULL)
    , _hwndPass(NULL)
    , _wndClassAtom(0)
    , _active(FALSE)
    , _currentBand(0)
    , _probedHostBand(0)
    , _requestedBand(0)
    , _hasOwner(FALSE)
    , _hSharedMem(NULL)
    , _pSharedMem(nullptr)
    , _maxBufferSize(0)
    , _hEvent(NULL)
    , _hThread(NULL)
    , _hStopEvent(NULL)
    , _lastSequence(0)
    , _pfnCreateWindowInBand(nullptr)
    , _pfnGetWindowBand(nullptr)
    , _windowKind(HOST_WINDOW_CANDIDATE)
    , _instanceId(0)
    , _ownerOverride(NULL)
    , _pIPCClient(nullptr)
    , _frameX(0)
    , _frameY(0)
    , _rectLockInit(FALSE)
    , _lastHoverIndex(INT_MIN)
    , _trackingMouse(FALSE)
    , _pressedHit(INT_MIN)
    , _lastScreenX(0)
    , _lastScreenY(0)
    , _hasScreenPos(FALSE)
{
    InitializeCriticalSection(&_rectLock);
    _rectLockInit = TRUE;
}

CHostWindow::~CHostWindow()
{
    Uninitialize();
    if (_rectLockInit)
    {
        DeleteCriticalSection(&_rectLock);
        _rectLockInit = FALSE;
    }
}

BOOL CHostWindow::_ResolveAPIs()
{
    HMODULE hUser32 = GetModuleHandleW(L"user32.dll");
    if (!hUser32)
        return FALSE;

    _pfnCreateWindowInBand = (CreateWindowInBand_t)GetProcAddress(hUser32, "CreateWindowInBand");
    _pfnGetWindowBand = (GetWindowBand_t)GetProcAddress(hUser32, "GetWindowBand");

    if (!_pfnCreateWindowInBand || !_pfnGetWindowBand)
    {
        WIND_LOG_WARN(L"HostWindow: CreateWindowInBand or GetWindowBand not found\n");
        return FALSE;
    }

    return TRUE;
}

namespace {
// Local alias matching CHostWindow::GetWindowBand_t (a private member typedef,
// not visible at namespace scope). Identical function-pointer typedefs are the
// same type, so assigning _pfnGetWindowBand into this field is well-formed.
typedef BOOL (WINAPI* PfnGetWindowBand)(HWND hwnd, DWORD* pdwBand);

struct BandEnumData
{
    DWORD targetPID;
    PfnGetWindowBand pfnGetWindowBand;
    DWORD bestBand;
};

// 显式 CALLBACK 的自由函数：x86 下 lambda 转不成 stdcall 的 WNDENUMPROC，别改回 lambda。
BOOL CALLBACK BandEnumProc(HWND hwnd, LPARAM lParam)
{
    auto* data = reinterpret_cast<BandEnumData*>(lParam);
    DWORD pid = 0;
    GetWindowThreadProcessId(hwnd, &pid);
    RECT rc = {};
    GetWindowRect(hwnd, &rc);
    if (pid == data->targetPID &&
        wind::hostband::CountsForHostBand(IsWindowVisible(hwnd) != FALSE, rc.right - rc.left, rc.bottom - rc.top))
    {
        DWORD b = 0;
        if (data->pfnGetWindowBand(hwnd, &b) && b > data->bestBand)
            data->bestBand = b;
    }
    return TRUE;
}
} // namespace

DWORD CHostWindow::GetForegroundHostBand()
{
    if (!_pfnGetWindowBand)
        return 0;
    HWND hwndFg = GetForegroundWindow();
    if (!hwndFg)
        return 0;
    DWORD fgPID = 0;
    GetWindowThreadProcessId(hwndFg, &fgPID);
    if (fgPID != GetCurrentProcessId())
        return 0;
    DWORD band = 0;
    return _pfnGetWindowBand(hwndFg, &band) ? band : 0;
}

DWORD CHostWindow::GetHostBand()
{
    DWORD fgBand = GetForegroundHostBand();
    if (fgBand > 1)
        return fgBand;

    // 本线程焦点/活动窗口的根窗口：前台不是本进程时（Win11 表情面板，前台仍是记事本）
    // 它是比枚举更直接的依据。取不到就是 0，交给枚举兜底。
    DWORD focusRootBand = 0;
    if (_pfnGetWindowBand)
    {
        GUITHREADINFO gti = {};
        gti.cbSize = sizeof(gti);
        if (GetGUIThreadInfo(GetCurrentThreadId(), &gti))
        {
            HWND h = gti.hwndFocus ? gti.hwndFocus : gti.hwndActive;
            HWND root = h ? GetAncestor(h, GA_ROOT) : nullptr;
            DWORD b = 0;
            if (root && _pfnGetWindowBand(root, &b))
                focusRootBand = b;
        }
    }
    if (focusRootBand > 1)
        return wind::hostband::PickHostBand(fgBand, focusRootBand, 0);

    // Enumerate visible, non-empty top-level windows owned by this process.
    // 前台不是本进程时这里只是猜（SearchHost 开机预启动时窗口还不在开始菜单那一层），
    // 建窗后由 CTextService::RecheckHostBand 在一段输入的第一个键按前台 band 复核。
    BandEnumData enumData = { GetCurrentProcessId(), _pfnGetWindowBand, 0 };
    EnumWindows(BandEnumProc, (LPARAM)&enumData);

    return wind::hostband::PickHostBand(fgBand, focusRootBand, enumData.bestBand);
}

LRESULT CALLBACK CHostWindow::_WndProc(HWND hwnd, UINT msg, WPARAM wParam, LPARAM lParam)
{
    CHostWindow* self = reinterpret_cast<CHostWindow*>(GetWindowLongPtrW(hwnd, GWLP_USERDATA));
    // 点击不夺焦：WS_EX_NOACTIVATE 挡不住点击把焦点从宿主编辑框拉走（随即 KILLFOCUS 终止组合）；
    // MA_NOACTIVATE 不激活也不吃掉点击。放在解析 `self` 之前，建窗期间同样生效。
    if (msg == WM_MOUSEACTIVATE)
        return MA_NOACTIVATE;

    // Mouse interaction is candidate-only. Tooltip/status are pure-display band windows
    // (their occlusion fix is z-order, not interaction), so they fall through to
    // DefWindowProc for all mouse messages.
    if (self != nullptr && self->_windowKind == HOST_WINDOW_CANDIDATE)
    {
        switch (msg)
        {
        case WM_LBUTTONDOWN:
            // Client coords (window is sized to the bitmap → coords match panel-local rects).
            self->_OnMouseDown((int)(short)LOWORD(lParam), (int)(short)HIWORD(lParam));
            return 0;
        case WM_LBUTTONUP:
            self->_OnMouseUp((int)(short)LOWORD(lParam), (int)(short)HIWORD(lParam));
            return 0;
        case WM_CAPTURECHANGED:
            // Capture taken away mid-press (another window grabbed it): the press is void.
            self->_pressedHit = INT_MIN;
            return 0;
        case WM_MOUSEMOVE:
            self->_OnMouseMove((int)(short)LOWORD(lParam), (int)(short)HIWORD(lParam));
            return 0;
        case WM_MOUSELEAVE:
            self->_OnMouseLeave();
            return 0;
        case WM_MOUSEWHEEL:
            // Wheel delta in HIWORD of wParam (signed). Default Win10+ "scroll inactive
            // windows under the pointer" routes this to our NOACTIVATE window.
            self->_OnMouseWheel((int)(short)HIWORD(wParam));
            return 0;
        }
    }
    return DefWindowProcW(hwnd, msg, wParam, lParam);
}

// _UpdateHitRects snapshots the embedded rect table of a freshly rendered frame.
// Called on the render thread; guarded by _rectLock against UI-thread hit-testing.
void CHostWindow::_UpdateHitRects(const FrameSnapshot& frame)
{
    if (!_rectLockInit)
        return;

    const SharedRenderHeader* header = &frame.header;
    EnterCriticalSection(&_rectLock);
    _frameX = header->x;
    _frameY = header->y;
    // 表已在拷帧时按帧头拷贝校验、截到上限（HostFrameSnapshot.h）。
    _hitRects = frame.rects;
    // 悬停去重基线同步为本帧实际高亮的项：打字清掉高亮时，鼠标移回同一项才会重新发 hover。
    // 持 _rectLock，因为 UI 线程（_OnMouseMove/_OnMouseLeave）也写它。
    _lastHoverIndex = header->renderedHoverIndex;
    LeaveCriticalSection(&_rectLock);
}

// _HitTest returns the candidate index (>=0), a page button (-1 up / -2 down), or
// INT_MIN when the point hits nothing interactive. Candidates take priority; page
// buttons are only matched when no candidate contains the point.
int CHostWindow::_HitTest(int clientX, int clientY)
{
    if (!_rectLockInit)
        return INT_MIN;

    int candidateHit = INT_MIN;
    int pageHit = INT_MIN;

    EnterCriticalSection(&_rectLock);
    for (const HostRenderHitRect& r : _hitRects)
    {
        if (clientX >= r.x && clientX < r.x + r.w &&
            clientY >= r.y && clientY < r.y + r.h)
        {
            if (r.index >= 0)
            {
                candidateHit = r.index;
                break; // candidate wins outright
            }
            else if (pageHit == INT_MIN)
            {
                pageHit = r.index; // remember page button, keep scanning for a candidate
            }
        }
    }
    LeaveCriticalSection(&_rectLock);

    return (candidateHit != INT_MIN) ? candidateHit : pageHit;
}

void CHostWindow::_OnMouseDown(int clientX, int clientY)
{
    _pressedHit = _HitTest(clientX, clientY);
    if (_pressedHit != INT_MIN)
        SetCapture(_hwnd); // still get the release after the cursor leaves → judge "abandon"
}

void CHostWindow::_OnMouseUp(int clientX, int clientY)
{
    const int pressed = _pressedHit;
    _pressedHit = INT_MIN;
    if (GetCapture() == _hwnd)
        ReleaseCapture(); // posts WM_CAPTURECHANGED; _pressedHit is already cleared
    // Hidden while held (commit / Esc / focus loss came from the keyboard or the service): the hit
    // rects are stale, the release must not select whatever sat under the cursor.
    if (!IsWindowVisible(_hwnd))
        return;
    int hit = _HitTest(clientX, clientY);
    WIND_LOG_INFO_FMT(L"HostWindow: mouse click at (%d,%d) pressed=%d hit=%d\n", clientX, clientY,
                      pressed, hit);
    if (!wind::hostclick::SelectsOnRelease(pressed, hit) || _pIPCClient == nullptr)
        return;

    int32_t idx = (int32_t)hit; // >=0 candidate, -1 page up, -2 page down
    _pIPCClient->SendAsync(CMD_CANDIDATE_SELECT, &idx, sizeof(idx));
}

void CHostWindow::_OnMouseMove(int clientX, int clientY)
{
    // Arm WM_MOUSELEAVE tracking so hover state resets when the cursor exits the window.
    if (!_trackingMouse)
    {
        TRACKMOUSEEVENT tme = { sizeof(TRACKMOUSEEVENT), TME_LEAVE, _hwnd, 0 };
        if (TrackMouseEvent(&tme))
            _trackingMouse = TRUE;
    }

    // 过滤合成的 WM_MOUSEMOVE：窗口重绘或跟随光标移动时鼠标没动也会收到。比屏幕坐标
    // （只随真实移动变化），客户区坐标或帧几何在窗口移动、重显同样候选时都不可靠。
    POINT pt = { clientX, clientY };
    ClientToScreen(_hwnd, &pt);
    if (_hasScreenPos && pt.x == _lastScreenX && pt.y == _lastScreenY)
        return; // cursor did not move in screen space → synthetic move, ignore
    _lastScreenX = pt.x;
    _lastScreenY = pt.y;
    _hasScreenPos = TRUE;

    int hit = _HitTest(clientX, clientY);
    // Encode the hover index for CMD_CANDIDATE_HOVER. NOTE this differs from the
    // rect/select convention (where -1/-2 are the page buttons) because hover needs a
    // distinct "nothing" value: >=0 candidate, -1 nothing, -2 page-up, -3 page-down.
    int hoverIndex;
    if (hit >= 0)
        hoverIndex = hit;        // candidate
    else if (hit == -1)
        hoverIndex = -2;         // page-up button (rect index -1)
    else if (hit == -2)
        hoverIndex = -3;         // page-down button (rect index -2)
    else
        hoverIndex = -1;         // INT_MIN: nothing

    // _lastHoverIndex 由 UI 线程与渲染线程（_UpdateHitRects）共同写，比较并设置要持 _rectLock，
    // 否则会按陈旧值去重而漏掉重新高亮。_rectLock 可重入，上面 _HitTest 再进无妨。
    BOOL changed = TRUE;
    if (_rectLockInit) EnterCriticalSection(&_rectLock);
    if (hoverIndex == _lastHoverIndex)
        changed = FALSE;
    else
        _lastHoverIndex = hoverIndex;
    if (_rectLockInit) LeaveCriticalSection(&_rectLock);

    if (!changed)
        return; // no change → skip the round trip
    _SendHover(hoverIndex);
}

void CHostWindow::_OnMouseLeave()
{
    _trackingMouse = FALSE;
    // Guarded like _OnMouseMove: the render thread also writes _lastHoverIndex.
    BOOL doSend = FALSE;
    if (_rectLockInit) EnterCriticalSection(&_rectLock);
    if (_lastHoverIndex != -1)
    {
        _lastHoverIndex = -1;
        doSend = TRUE;
    }
    if (_rectLockInit) LeaveCriticalSection(&_rectLock);
    if (doSend)
        _SendHover(-1); // tell the service the cursor left the candidate area (hide tooltip / clear highlight)
}

void CHostWindow::_OnMouseWheel(int delta)
{
    if (delta == 0 || _pIPCClient == nullptr)
        return;

    // Forward the raw wheel delta to the service as a distinct event — do NOT page here. The
    // standard (local) candidate window has no wheel paging, so the service owns the decision
    // (default: no-op) and any unified scroll behavior is added there behind config.
    int32_t raw = (int32_t)delta;
    _pIPCClient->SendAsync(CMD_CANDIDATE_SCROLL, &raw, sizeof(raw));
}

void CHostWindow::_SendHover(int index)
{
    if (_pIPCClient == nullptr)
        return;

    // Compute the tooltip screen anchor from the hovered candidate's panel-local rect
    // + this frame's screen origin, so the service can position the tooltip without knowing the
    // host window's geometry. For index<0 (left area) anchors are 0 (unused).
    int32_t payload[4] = { (int32_t)index, 0, 0, 0 };
    if (index >= 0 && _rectLockInit)
    {
        EnterCriticalSection(&_rectLock);
        for (const HostRenderHitRect& r : _hitRects)
        {
            if (r.index == index)
            {
                payload[1] = _frameX + r.x + r.w / 2; // anchorX = rect horizontal center
                payload[2] = _frameY + r.y + r.h + 2; // belowY = just under the candidate
                payload[3] = _frameY + r.y - 2;       // aboveY = just above (fallback)
                break;
            }
        }
        LeaveCriticalSection(&_rectLock);
    }

    _pIPCClient->SendAsync(CMD_CANDIDATE_HOVER, payload, sizeof(payload));
}

BOOL CHostWindow::_CreateBandWindow(DWORD band)
{
    // Register the class once per process. HostWindow instances are recreated
    // across TSF Activate/Deactivate cycles, but the window class registration
    // remains process-wide.
    if (s_hostWndClassAtom == 0)
    {
        WNDCLASSEXW wc = { sizeof(WNDCLASSEXW) };
        wc.lpfnWndProc = _WndProc;
        wc.hInstance = g_hInstance;
        wc.lpszClassName = HOST_WND_CLASS;
        s_hostWndClassAtom = RegisterClassExW(&wc);
        if (s_hostWndClassAtom == 0)
        {
            DWORD err = GetLastError();
            WIND_LOG_ERROR_FMT(L"HostWindow: RegisterClassExW failed, err=%u\n", err);
            return FALSE;
        }
    }

    _wndClassAtom = s_hostWndClassAtom;
    if (_wndClassAtom == 0)
    {
        WIND_LOG_ERROR(L"HostWindow: window class atom missing after registration\n");
        return FALSE;
    }

    DWORD exStyle = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
    DWORD style = WS_POPUP;

    // WS_POPUP 的 hWndParent 即 owner，被拥有的窗口总在 owner 之上。提示/状态窗传入候选窗作 owner，
    // 免被候选窗遮挡；候选窗用本进程的前台窗口作 owner，才能压在宿主搜索面板之上（band=13）。
    HWND owner = NULL;
    if (_ownerOverride != NULL)
    {
        owner = _ownerOverride;
        WIND_LOG_INFO_FMT(L"HostWindow: Using override owner hwnd=0x%p (kind=%u) for band=%u\n",
            owner, (unsigned)_windowKind, band);
    }
    else
    {
        HWND hwndFg = GetForegroundWindow();
        if (hwndFg)
        {
            DWORD fgPID = 0;
            GetWindowThreadProcessId(hwndFg, &fgPID);
            if (fgPID == GetCurrentProcessId())
            {
                owner = hwndFg;
                WIND_LOG_INFO_FMT(L"HostWindow: Using owner hwnd=0x%p for band=%u\n", owner, band);
            }
        }
    }

    _hasOwner = (owner != NULL);

    _hwnd = _pfnCreateWindowInBand(
        exStyle,
        _wndClassAtom,
        L"",
        style,
        0, 0, 1, 1,
        owner,  // owner window for correct z-order
        NULL,   // no menu
        g_hInstance,
        NULL,   // no param
        band
    );

    // 受限的 UWP/AppContainer 宿主（如 WinStore.App.exe）对 band=1 回 ERROR_INVALID_PARAMETER，
    // 改用 band=0 重试；它们本就不在高 band，TOPMOST + 前台 owner 足以压住内容。
    if (!_hwnd && band != 0)
    {
        DWORD err = GetLastError();
        WIND_LOG_WARN_FMT(L"HostWindow: CreateWindowInBand failed at band=%u err=%u, retrying at band=0\n", band, err);
        band = 0;
        _hwnd = _pfnCreateWindowInBand(
            exStyle, _wndClassAtom, L"", style, 0, 0, 1, 1,
            owner, NULL, g_hInstance, NULL, band);
    }

    if (!_hwnd)
    {
        WIND_LOG_ERROR_FMT(L"HostWindow: CreateWindowInBand failed, band=%u, err=%u\n", band, GetLastError());
        return FALSE;
    }

    // Stash `this` so the (static) window proc can route mouse messages to this
    // instance. Set before ShowWindow so the first mouse messages already resolve.
    SetWindowLongPtrW(_hwnd, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(this));

    // Verify the actual band
    DWORD actualBand = 0;
    _pfnGetWindowBand(_hwnd, &actualBand);
    _currentBand = actualBand;
    WIND_LOG_INFO_FMT(L"HostWindow: Created Band window, hwnd=0x%p, band=%u, actual=%u\n",
        _hwnd, band, actualBand);

    // 皮肤穿透装饰窗口：同 owner、同（回退后的）band，鼠标穿透。不设 GWLP_USERDATA，窗口过程按
    // 无实例处理（只回 DefWindowProc）。建不出来不算失败：穿透层合回主层（按 block）。
    if (_windowKind == HOST_WINDOW_CANDIDATE)
    {
        _hwndPass = _pfnCreateWindowInBand(
            exStyle | WS_EX_TRANSPARENT, _wndClassAtom, L"", style, 0, 0, 1, 1,
            owner, NULL, g_hInstance, NULL, band);
        if (!_hwndPass)
        {
            WIND_LOG_WARN_FMT(L"HostWindow: pass-through decor window failed at band=%u err=%u; "
                              L"pass decor drawn into the candidate\n", band, GetLastError());
        }
    }

    // Show the window (non-activating)
    ShowWindow(_hwnd, SW_SHOWNA);

    return TRUE;
}

BOOL CHostWindow::Initialize(const wchar_t* shmName, const wchar_t* eventName, DWORD maxBufferSize,
                             uint32_t instanceId, CIPCClient* ipcClient, HostWindowKind kind, HWND ownerOverride)
{
    WIND_LOG_INFO_FMT(L"HostWindow: Initializing, kind=%u, instance=%u, shm=%s, event=%s, maxSize=%u\n",
        (unsigned)kind, instanceId, shmName, eventName, maxBufferSize);

    _windowKind = kind;
    _instanceId = instanceId;
    _ownerOverride = ownerOverride;
    _pIPCClient = ipcClient; // weak ref for routing mouse events back to the service (candidate only)

    // Resolve undocumented APIs
    if (!_ResolveAPIs())
        return FALSE;

    // 查 band 只为决定放在哪一层；要不要宿主内渲染由服务端（白名单）决定，走到这里就必须建窗，
    // 普通应用（band<=1）也不例外，否则服务端已压掉自己的窗口、候选就不可见了。高 band 宿主
    // （任务栏搜索 13、开始菜单 6）放在同层，其余退到 band 1，靠 TOPMOST + owner 压住内容。
    DWORD hostBand = GetHostBand();
    DWORD targetBand = (hostBand > 1) ? hostBand : 1;
    _probedHostBand = hostBand;
    _requestedBand = targetBand;
    WIND_LOG_INFO_FMT(L"HostWindow: host band=%u, placing at band=%u\n", hostBand, targetBand);

    // Open shared memory
    _hSharedMem = OpenFileMappingW(FILE_MAP_READ, FALSE, shmName);
    if (!_hSharedMem)
    {
        WIND_LOG_ERROR_FMT(L"HostWindow: OpenFileMapping failed, err=%u\n", GetLastError());
        return FALSE;
    }

    _pSharedMem = MapViewOfFile(_hSharedMem, FILE_MAP_READ, 0, 0, maxBufferSize);
    if (!_pSharedMem)
    {
        WIND_LOG_ERROR_FMT(L"HostWindow: MapViewOfFile failed, err=%u\n", GetLastError());
        CloseHandle(_hSharedMem);
        _hSharedMem = NULL;
        return FALSE;
    }
    _maxBufferSize = maxBufferSize;

    // Open named event
    _hEvent = OpenEventW(SYNCHRONIZE, FALSE, eventName);
    if (!_hEvent)
    {
        WIND_LOG_ERROR_FMT(L"HostWindow: OpenEvent failed, err=%u\n", GetLastError());
        UnmapViewOfFile(_pSharedMem);
        _pSharedMem = nullptr;
        CloseHandle(_hSharedMem);
        _hSharedMem = NULL;
        return FALSE;
    }

    // Use the same band as the host process. Other IMEs (e.g., Weasel) confirm
    // that band 13 (ZBID_IMMERSIVE_SEARCH) is correct for the taskbar search context.
    if (!_CreateBandWindow(targetBand))
    {
        CloseHandle(_hEvent);
        _hEvent = NULL;
        UnmapViewOfFile(_pSharedMem);
        _pSharedMem = nullptr;
        CloseHandle(_hSharedMem);
        _hSharedMem = NULL;
        return FALSE;
    }

    // Create stop event for render thread
    _hStopEvent = CreateEventW(NULL, TRUE, FALSE, NULL); // manual-reset, initially non-signaled

    // Start render thread
    _hThread = CreateThread(NULL, 0, _RenderThread, this, 0, NULL);
    if (!_hThread)
    {
        WIND_LOG_ERROR_FMT(L"HostWindow: CreateThread failed, err=%u\n", GetLastError());
        Uninitialize();
        return FALSE;
    }

    _active = TRUE;
    WIND_LOG_INFO_FMT(L"HostWindow: Initialized successfully, band=%u\n", hostBand);
    return TRUE;
}

void CHostWindow::Uninitialize()
{
    const BOOL hadResources = (_active || _hwnd || _hSharedMem || _pSharedMem || _hEvent || _hThread || _hStopEvent || (_lastSequence != 0));

    _active = FALSE;

    // Signal render thread to stop
    if (_hStopEvent)
    {
        SetEvent(_hStopEvent);
    }

    // Wait for render thread to finish
    if (_hThread)
    {
        WaitForSingleObject(_hThread, 2000); // 2s timeout
        CloseHandle(_hThread);
        _hThread = NULL;
    }

    if (_hStopEvent)
    {
        CloseHandle(_hStopEvent);
        _hStopEvent = NULL;
    }

    // Destroy window
    if (_hwnd)
    {
        DestroyWindow(_hwnd);
        _hwnd = NULL;
    }
    if (_hwndPass)
    {
        DestroyWindow(_hwndPass);
        _hwndPass = NULL;
    }

    // Unmap shared memory
    if (_pSharedMem)
    {
        UnmapViewOfFile(_pSharedMem);
        _pSharedMem = nullptr;
    }
    if (_hSharedMem)
    {
        CloseHandle(_hSharedMem);
        _hSharedMem = NULL;
    }

    // Close event
    if (_hEvent)
    {
        CloseHandle(_hEvent);
        _hEvent = NULL;
    }

    _wndClassAtom = 0;
    _lastSequence = 0;
    if (hadResources)
    {
        WIND_LOG_INFO(L"HostWindow: Uninitialized\n");
    }
}

BOOL CHostWindow::UpdateBand(DWORD newBand)
{
    if (!_pfnCreateWindowInBand || !_pfnGetWindowBand)
        return FALSE;

    if (newBand <= 1 || newBand == _currentBand)
        return TRUE; // No change needed

    WIND_LOG_INFO_FMT(L"HostWindow: Band changed %u -> %u, recreating window\n",
        _currentBand, newBand);

    if (_hwnd)
    {
        DestroyWindow(_hwnd);
        _hwnd = NULL;
    }
    if (_hwndPass)
    {
        DestroyWindow(_hwndPass);
        _hwndPass = NULL;
    }

    _requestedBand = newBand;
    if (!_CreateBandWindow(newBand))
    {
        WIND_LOG_ERROR_FMT(L"HostWindow: Failed to recreate window at band=%u\n", newBand);
        return FALSE;
    }

    return TRUE;
}

DWORD WINAPI CHostWindow::_RenderThread(LPVOID param)
{
    CHostWindow* self = (CHostWindow*)param;
    self->_RenderLoop();
    return 0;
}

void CHostWindow::_RenderLoop()
{
    WIND_LOG_INFO(L"HostWindow: Render thread started\n");

    HANDLE waitHandles[2] = { _hStopEvent, _hEvent };

    while (true)
    {
        DWORD result = WaitForMultipleObjects(2, waitHandles, FALSE, INFINITE);

        if (result == WAIT_OBJECT_0)
        {
            // Stop event signaled
            break;
        }
        else if (result == WAIT_OBJECT_0 + 1)
        {
            // Frame event signaled - read shared memory and render
            if (!_pSharedMem || !_hwnd)
                continue;

            // 帧头只读这一次：服务端随时会改写 SHM，下面的校验与拷贝全按这份拷贝。
            const uint8_t* shm = static_cast<const uint8_t*>(_pSharedMem);
            const SharedRenderHeader hdr = wind::hostframe::CopyHeader<SharedRenderHeader>(shm);
            const SharedRenderHeader* header = &hdr;

            // Validate magic
            if (header->magic != SHARED_RENDER_MAGIC)
                continue;

            // Check if this is a new frame
            // _lastSequence 只在这一帧真正处理完（隐藏或画出）时才记：帧头可能读到写了一半的
            // （新 sequence 配旧尺寸），校验或拷帧失败时若已记下，服务端写完再发的事件会因
            // sequence 相同被当成旧帧跳过，候选窗停在旧内容上。
            if (header->sequence == _lastSequence)
                continue;

            // 本进程多个 TextService 实例共享一块 SHM 且都被唤醒：只有帧可见且指向本实例才画，
            // 否则隐藏，保证同时只有一个 band 窗口显示。
            BOOL visible = (header->flags & SHARED_FLAG_VISIBLE) != 0;
            BOOL forMe = (header->targetInstanceId == _instanceId);
            if (!visible || !forMe)
            {
                _lastSequence = header->sequence;
                _HideWindow();
                continue;
            }

            // Validate data fits in buffer
            DWORD requiredSize = sizeof(SharedRenderHeader) + header->dataSize;
            if (requiredSize > _maxBufferSize)
            {
                WIND_LOG_WARN_FMT(L"HostWindow: Frame too large: %u bytes\n", requiredSize);
                continue;
            }

            if (header->width == 0 || header->height == 0)
                continue;

            // 像素 / 命中矩形 / 穿透层拷出来；拷的途中被改写过（sequence 变了）就丢掉这一帧——
            // 服务端写完那一帧会再发一次事件，下一轮按它画。
            if (!wind::hostframe::CopyPayload(shm, _maxBufferSize, hdr, MAX_HOST_RENDER_RECTS, _frame))
            {
                WIND_LOG_DEBUG_FMT(L"HostWindow: frame %u rewritten while copying, dropped\n",
                                   header->sequence);
                continue;
            }
            _lastSequence = header->sequence;
            _RenderFrame(_frame);
        }
        else
        {
            // Error or timeout
            break;
        }
    }

    WIND_LOG_INFO(L"HostWindow: Render thread stopped\n");
}

void CHostWindow::_RenderFrame(const FrameSnapshot& frame)
{
    // Capture this frame's hit geometry + screen origin for mouse routing before we
    // paint, so a click landing right after the repaint hit-tests against fresh rects.
    _UpdateHitRects(frame);

    // 皮肤穿透装饰层（只发给声明了能力的 DLL）：先贴穿透窗口，贴不上（或没有穿透窗口）就合回主层，
    // 不让装饰凭空消失一帧。
    const SharedRenderHeader* header = &frame.header;
    const bool hasPass = !frame.pass.empty();
    const bool passBelow = wind::hostpass::PassLayerBelow(header->flags);
    const bool passOk = hasPass && _hwndPass &&
                        _BlitLayered(_hwndPass, header, frame.pass.data(), nullptr, false);
    const auto plan = wind::hostpass::PlanPassPresent(hasPass, _hwndPass != NULL, passOk);
    const void* merge = plan == wind::hostpass::PassPresent::Merge ? frame.pass.data() : nullptr;

    if (!_BlitLayered(_hwnd, header, frame.pixels.data(), merge, passBelow))
    {
        _HidePassWindow(); // 主层没贴上：别留下一块孤零零的装饰
        return;
    }
    // Bring to topmost z-order within the band on every frame.
    // Without this, the host process's own windows (e.g., taskbar search popup)
    // can cover the candidate window when both share the same or adjacent bands.
    _PlaceWindows(plan == wind::hostpass::PassPresent::Window, passBelow);
}

void CHostWindow::_PlaceWindows(bool withPass, bool passBelow)
{
    const UINT flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW;
    if (!withPass)
    {
        _HidePassWindow();
        SetWindowPos(_hwnd, HWND_TOPMOST, 0, 0, 0, 0, flags);
        return;
    }
    // 与服务端本地窗口路径同一规则：装饰全在窗体下 → 穿透窗紧贴候选窗下面，否则在上面。在上的那个
    // TOPMOST、另一个插在它后面，一次提交：分两次 SetWindowPos 的话，两窗会先后各跳一下。
    HWND top = passBelow ? _hwnd : _hwndPass;
    HWND under = passBelow ? _hwndPass : _hwnd;
    HDWP batch = BeginDeferWindowPos(2);
    if (batch)
        batch = DeferWindowPos(batch, top, HWND_TOPMOST, 0, 0, 0, 0, flags);
    if (batch)
        batch = DeferWindowPos(batch, under, top, 0, 0, 0, 0, flags);
    if (!batch || !EndDeferWindowPos(batch))
    {
        SetWindowPos(top, HWND_TOPMOST, 0, 0, 0, 0, flags);
        SetWindowPos(under, top, 0, 0, 0, 0, flags);
    }
}

BOOL CHostWindow::_BlitLayered(HWND hwnd, const SharedRenderHeader* header, const void* pixels,
                               const void* pass, bool passBelow)
{
    int width = (int)header->width;
    int height = (int)header->height;

    // Get screen DC
    HDC hdcScreen = GetDC(NULL);
    if (!hdcScreen) return FALSE;

    HDC hdcMem = CreateCompatibleDC(hdcScreen);
    if (!hdcMem)
    {
        ReleaseDC(NULL, hdcScreen);
        return FALSE;
    }

    // Create DIB section
    BITMAPINFO bi = {};
    bi.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
    bi.bmiHeader.biWidth = width;
    bi.bmiHeader.biHeight = -height; // top-down
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    bi.bmiHeader.biCompression = BI_RGB;

    void* bits = nullptr;
    HBITMAP hBitmap = CreateDIBSection(hdcMem, &bi, DIB_RGB_COLORS, &bits, NULL, 0);
    if (!hBitmap || !bits)
    {
        DeleteDC(hdcMem);
        ReleaseDC(NULL, hdcScreen);
        return FALSE;
    }

    HGDIOBJ oldBmp = SelectObject(hdcMem, hBitmap);

    // Copy BGRA pixels from shared memory (already in correct format for Windows)
    memcpy(bits, pixels, header->dataSize);
    if (pass)
    {
        wind::hostpass::CompositePass(static_cast<uint8_t*>(bits),
                                      static_cast<const uint8_t*>(pass), header->dataSize,
                                      passBelow);
    }

    // UpdateLayeredWindow with position + content
    POINT ptSrc = { 0, 0 };
    POINT ptDst = { header->x, header->y };
    SIZE size = { (LONG)width, (LONG)height };
    BLENDFUNCTION blend = {};
    blend.BlendOp = AC_SRC_OVER;
    blend.BlendFlags = 0;
    blend.SourceConstantAlpha = 255;
    blend.AlphaFormat = AC_SRC_ALPHA;

    BOOL ok = UpdateLayeredWindow(
        hwnd,
        hdcScreen,
        &ptDst,
        &size,
        hdcMem,
        &ptSrc,
        0,
        &blend,
        ULW_ALPHA
    );

    if (!ok)
    {
        WIND_LOG_WARN_FMT(L"HostWindow: UpdateLayeredWindow failed, err=%u\n", GetLastError());
    }

    // Cleanup
    SelectObject(hdcMem, oldBmp);
    DeleteObject(hBitmap);
    DeleteDC(hdcMem);
    ReleaseDC(NULL, hdcScreen);
    return ok;
}

void CHostWindow::_HidePassWindow()
{
    if (_hwndPass && IsWindowVisible(_hwndPass))
    {
        ShowWindow(_hwndPass, SW_HIDE);
    }
}

void CHostWindow::_HideWindow()
{
    // Drop stale hit geometry so a stray mouse message can't act on a hidden frame,
    // and reset hover so the next show starts clean.
    if (_rectLockInit)
    {
        EnterCriticalSection(&_rectLock);
        _hitRects.clear();
        _lastHoverIndex = INT_MIN; // under lock: UI thread reads it in _OnMouseMove
        LeaveCriticalSection(&_rectLock);
    }
    else
    {
        _lastHoverIndex = INT_MIN;
    }

    if (_hwnd && IsWindowVisible(_hwnd))
    {
        ShowWindow(_hwnd, SW_HIDE);
    }
    _HidePassWindow();
    wind::hostframe::ReleaseBuffers(_frame); // 只在渲染线程调，与拷帧同线程
}
