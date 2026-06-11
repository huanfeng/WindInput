#include "HostWindow.h"
#include "FileLogger.h"
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

static const wchar_t* HOST_WND_CLASS = L"WindInputHostCandidateWnd";
// Accessed only on the UI thread (STA). No synchronization needed.
static ATOM s_hostWndClassAtom = 0;

CHostWindow::CHostWindow()
    : _hwnd(NULL)
    , _wndClassAtom(0)
    , _active(FALSE)
    , _currentBand(0)
    , _hSharedMem(NULL)
    , _pSharedMem(nullptr)
    , _maxBufferSize(0)
    , _hEvent(NULL)
    , _hThread(NULL)
    , _hStopEvent(NULL)
    , _lastSequence(0)
    , _pfnCreateWindowInBand(nullptr)
    , _pfnGetWindowBand(nullptr)
    , _pIPCClient(nullptr)
    , _frameX(0)
    , _frameY(0)
    , _rectLockInit(FALSE)
    , _lastHoverIndex(INT_MIN)
    , _trackingMouse(FALSE)
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

DWORD CHostWindow::GetHostBand()
{
    DWORD currentPID = GetCurrentProcessId();
    DWORD band = 0;

    // Try the foreground window first
    HWND hwndFg = GetForegroundWindow();
    if (hwndFg)
    {
        DWORD fgPID = 0;
        GetWindowThreadProcessId(hwndFg, &fgPID);
        if (fgPID == currentPID)
        {
            if (_pfnGetWindowBand(hwndFg, &band) && band > 1)
                return band;
        }
    }

    // Enumerate top-level windows owned by this process
    struct EnumData
    {
        DWORD targetPID;
        GetWindowBand_t pfnGetWindowBand;
        DWORD bestBand;
    } enumData = { currentPID, _pfnGetWindowBand, 0 };

    EnumWindows([](HWND hwnd, LPARAM lParam) -> BOOL
    {
        auto* data = reinterpret_cast<EnumData*>(lParam);
        DWORD pid = 0;
        GetWindowThreadProcessId(hwnd, &pid);
        if (pid == data->targetPID)
        {
            DWORD b = 0;
            if (data->pfnGetWindowBand(hwnd, &b) && b > data->bestBand)
                data->bestBand = b;
        }
        return TRUE;
    }, (LPARAM)&enumData);

    return enumData.bestBand;
}

LRESULT CALLBACK CHostWindow::_WndProc(HWND hwnd, UINT msg, WPARAM wParam, LPARAM lParam)
{
    CHostWindow* self = reinterpret_cast<CHostWindow*>(GetWindowLongPtrW(hwnd, GWLP_USERDATA));
    // Never take focus on click. WS_EX_NOACTIVATE blocks foreground activation but a
    // click can still pull keyboard focus off the host's edit control (host then fires
    // KILLFOCUS → composition terminates → candidates vanish). MA_NOACTIVATE tells the
    // host "don't activate me, and don't eat the click" so WM_LBUTTONDOWN still arrives.
    // Handle this even before resolving `self` so it applies during window creation too.
    if (msg == WM_MOUSEACTIVATE)
        return MA_NOACTIVATE;

    if (self != nullptr)
    {
        switch (msg)
        {
        case WM_LBUTTONDOWN:
            // Client coords (window is sized to the bitmap → coords match panel-local rects).
            self->_OnMouseClick((int)(short)LOWORD(lParam), (int)(short)HIWORD(lParam));
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
void CHostWindow::_UpdateHitRects(const SharedRenderHeader* header)
{
    if (!_rectLockInit)
        return;

    EnterCriticalSection(&_rectLock);
    _frameX = header->x;
    _frameY = header->y;
    _hitRects.clear();

    uint32_t count = header->rectCount;
    if (count > MAX_HOST_RENDER_RECTS)
        count = MAX_HOST_RENDER_RECTS;

    // Validate the table lies fully within the mapped buffer before reading it.
    uint64_t tableEnd = (uint64_t)header->rectsOffset + (uint64_t)count * sizeof(HostRenderHitRect);
    if (count > 0 && header->rectsOffset >= sizeof(SharedRenderHeader) && tableEnd <= _maxBufferSize)
    {
        const HostRenderHitRect* table =
            reinterpret_cast<const HostRenderHitRect*>((const char*)_pSharedMem + header->rectsOffset);
        _hitRects.assign(table, table + count);
    }
    // Sync the hover-dedup baseline to what Go actually highlighted in THIS frame. When a
    // content change (typing) clears the highlight without any mouse event, Go renders with
    // renderedHoverIndex = -1; adopting it here means a later real move back onto the same
    // candidate index differs from the baseline and re-sends hover (re-highlighting). Without
    // this, _lastHoverIndex kept the pre-typing index and deduped the move away. Held under
    // _rectLock because the UI thread (_OnMouseMove/_OnMouseLeave) also touches it.
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

void CHostWindow::_OnMouseClick(int clientX, int clientY)
{
    int hit = _HitTest(clientX, clientY);
    WIND_LOG_INFO_FMT(L"HostWindow: mouse click at (%d,%d) hit=%d\n", clientX, clientY, hit);
    if (hit == INT_MIN || _pIPCClient == nullptr)
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

    // Filter synthetic moves: when the candidate window repaints or follows the caret under
    // a physically-stationary cursor, Windows still posts WM_MOUSEMOVE (with new client
    // coords because the window moved). Compare the SCREEN cursor position — it only changes
    // on real movement — so typing with the cursor parked over a candidate doesn't flicker
    // the highlight/tooltip. Robust regardless of window/content changes (unlike comparing
    // client coords or frame geometry, which break when re-showing identical candidates).
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

    // Dedup against what's actually on screen. _lastHoverIndex is updated by BOTH this UI
    // thread and the render thread (_UpdateHitRects syncs it to the frame's
    // renderedHoverIndex). Guard the compare-and-set with _rectLock so a concurrent render
    // sync can't slip between our read and write and make us dedup against a stale value
    // (which would skip re-highlighting). _rectLock is recursive, so _HitTest above
    // re-entering it is safe.
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
        _SendHover(-1); // tell Go the cursor left the candidate area (hide tooltip / clear highlight)
}

void CHostWindow::_OnMouseWheel(int delta)
{
    if (delta == 0 || _pIPCClient == nullptr)
        return;

    // Forward the raw wheel delta to Go as a distinct event — do NOT page here. The
    // standard (local) candidate window has no wheel paging, so Go owns the decision
    // (default: no-op) and any unified scroll behavior is added there behind config.
    int32_t raw = (int32_t)delta;
    _pIPCClient->SendAsync(CMD_CANDIDATE_SCROLL, &raw, sizeof(raw));
}

void CHostWindow::_SendHover(int index)
{
    if (_pIPCClient == nullptr)
        return;

    // Compute the tooltip screen anchor from the hovered candidate's panel-local rect
    // + this frame's screen origin, so Go can position the tooltip without knowing the
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

    // For WS_POPUP windows, the hWndParent parameter sets the "owner" window.
    // Owned windows always appear above their owner in z-order.
    // Use the foreground window from the same process as owner to ensure the
    // candidate window appears above the host's search panel (especially at band=13).
    HWND owner = NULL;
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

    // Show the window (non-activating)
    ShowWindow(_hwnd, SW_SHOWNA);

    return TRUE;
}

BOOL CHostWindow::Initialize(const wchar_t* shmName, const wchar_t* eventName, DWORD maxBufferSize, CIPCClient* ipcClient)
{
    WIND_LOG_INFO_FMT(L"HostWindow: Initializing, shm=%s, event=%s, maxSize=%u\n",
        shmName, eventName, maxBufferSize);

    _pIPCClient = ipcClient; // weak ref for routing mouse events back to Go

    // Resolve undocumented APIs
    if (!_ResolveAPIs())
        return FALSE;

    // Query host Band for window PLACEMENT only — the decision to host-render is
    // owned by the Go service (whitelist). We get here because Go set hostRender=1
    // and sent setup, so we MUST honor it and create the window even for normal
    // apps (band<=1). For high-band hosts (taskbar search=13, Start Menu=6) we place
    // at that band to escape occlusion; otherwise we fall back to band 1 (desktop) —
    // combined with WS_EX_TOPMOST + owner window the candidate still renders above
    // the host's content. (Previously this aborted at band<=1, which left whitelisted
    // normal apps with no host window while Go still suppressed its own window →
    // candidates invisible.)
    DWORD hostBand = GetHostBand();
    DWORD targetBand = (hostBand > 1) ? hostBand : 1;
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

            const SharedRenderHeader* header = (const SharedRenderHeader*)_pSharedMem;

            // Validate magic
            if (header->magic != SHARED_RENDER_MAGIC)
                continue;

            // Check if this is a new frame
            if (header->sequence == _lastSequence)
                continue;
            _lastSequence = header->sequence;

            // Check visibility flag
            if (!(header->flags & SHARED_FLAG_VISIBLE))
            {
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

            // Get pointer to pixel data (right after header)
            const void* pixelData = (const char*)_pSharedMem + sizeof(SharedRenderHeader);
            _RenderFrame(header, pixelData);
        }
        else
        {
            // Error or timeout
            break;
        }
    }

    WIND_LOG_INFO(L"HostWindow: Render thread stopped\n");
}

void CHostWindow::_RenderFrame(const SharedRenderHeader* header, const void* pixelData)
{
    // Capture this frame's hit geometry + screen origin for mouse routing before we
    // paint, so a click landing right after the repaint hit-tests against fresh rects.
    _UpdateHitRects(header);

    int width = (int)header->width;
    int height = (int)header->height;

    // Get screen DC
    HDC hdcScreen = GetDC(NULL);
    if (!hdcScreen) return;

    HDC hdcMem = CreateCompatibleDC(hdcScreen);
    if (!hdcMem)
    {
        ReleaseDC(NULL, hdcScreen);
        return;
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
        return;
    }

    HGDIOBJ oldBmp = SelectObject(hdcMem, hBitmap);

    // Copy BGRA pixels from shared memory (already in correct format for Windows)
    memcpy(bits, pixelData, header->dataSize);

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
        _hwnd,
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

    if (ok)
    {
        // Bring to topmost z-order within the band on every frame.
        // Without this, the host process's own windows (e.g., taskbar search popup)
        // can cover the candidate window when both share the same or adjacent bands.
        SetWindowPos(_hwnd, HWND_TOPMOST, 0, 0, 0, 0,
                     SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
    }

    // Cleanup
    SelectObject(hdcMem, oldBmp);
    DeleteObject(hBitmap);
    DeleteDC(hdcMem);
    ReleaseDC(NULL, hdcScreen);
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
}
