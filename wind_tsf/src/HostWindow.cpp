#include "HostWindow.h"
#include "FileLogger.h"

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
{
}

CHostWindow::~CHostWindow()
{
    Uninitialize();
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
    return DefWindowProcW(hwnd, msg, wParam, lParam);
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

BOOL CHostWindow::Initialize(const wchar_t* shmName, const wchar_t* eventName, DWORD maxBufferSize)
{
    WIND_LOG_INFO_FMT(L"HostWindow: Initializing, shm=%s, event=%s, maxSize=%u\n",
        shmName, eventName, maxBufferSize);

    // Resolve undocumented APIs
    if (!_ResolveAPIs())
        return FALSE;

    // Check host Band
    DWORD hostBand = GetHostBand();
    if (hostBand <= 1)
    {
        WIND_LOG_INFO_FMT(L"HostWindow: Host band=%u (<=1), not needed\n", hostBand);
        return FALSE;
    }

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
    if (!_CreateBandWindow(hostBand))
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
    if (_hwnd && IsWindowVisible(_hwnd))
    {
        ShowWindow(_hwnd, SW_HIDE);
    }
}
