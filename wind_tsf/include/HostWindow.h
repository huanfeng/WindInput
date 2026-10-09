#pragma once

#include "Globals.h"
#include "BinaryProtocol.h"
#include <string>
#include <vector>

class CIPCClient; // forward decl: host window routes mouse events back to Go via SendAsync

// HostWindow manages a candidate window created via CreateWindowInBand inside the host process.
// This allows the candidate window to appear above high-Band windows (e.g. Start Menu).
// The window is rendered by Go via shared memory; this class handles display + mouse routing.
class CHostWindow
{
public:
    CHostWindow();
    ~CHostWindow();

    // Initialize with shared memory and event names from Go service.
    // Creates the Band window and starts the render thread. ipcClient (may be null) is
    // used to route mouse click/hover back to Go (CMD_CANDIDATE_SELECT / CMD_CANDIDATE_HOVER).
    // kind selects the window role: only HOST_WINDOW_CANDIDATE enables mouse interaction;
    // tooltip/status are pure display. ownerOverride (may be NULL) forces the band window's
    // owner — the candidate's hwnd is passed for tooltip/status so they sit above the
    // candidate in z-order (owned windows always render above their owner). instanceId is
    // this connection's bridge clientID; the render thread renders a frame only when
    // SharedRenderHeader.targetInstanceId matches it, otherwise it hides (so sibling
    // instances in the same process — e.g. two Notepad windows — don't all show the frame).
    // Returns TRUE on success.
    BOOL Initialize(const wchar_t* shmName, const wchar_t* eventName, DWORD maxBufferSize,
                    uint32_t instanceId, CIPCClient* ipcClient, HostWindowKind kind, HWND ownerOverride);

    // The band window handle (NULL until created). Used as the z-order owner for
    // sibling host windows (tooltip/status owned by the candidate).
    HWND GetHwnd() const { return _hwnd; }

    // Shut down: stop render thread, destroy window, unmap shared memory.
    void Uninitialize();

    // Returns TRUE if the host window is active and rendering.
    BOOL IsActive() const { return _active; }

    // 本 band 窗口**实际所在**的 band（CreateWindowInBand 之后由 GetWindowBand 复核所得）。
    // 0 = 尚未建窗。与 GetHostBand() 不是一回事：那个是"宿主处于哪个 band"的探测结果，
    // 用来决定往哪建；这个是"我们最后建成的窗口落在哪"。诊断快照上报的是后者——
    // 排查时要看的是既成事实，不是当初的意图。
    DWORD GetCurrentBand() const { return _currentBand; }

    // Recreate the display window at a new Band (called from TSF thread on focus change).
    // Shared memory and render thread are NOT affected.
    BOOL UpdateBand(DWORD newBand);

    // Get the Band of the host process's foreground window.
    DWORD GetHostBand();

    // 只看前台窗口：前台属于本进程时返回它的 band，否则 0。不像 GetHostBand 那样退到
    // EnumWindows——这里跑在键路径上，且「前台不是本进程」时根本没有可比的依据。
    DWORD GetForegroundHostBand();

    // 建窗当时的判定依据与结果，供复核（RecheckHostBand）和落位上报（diag.host_render_placed）。
    // probed = GetHostBand() 探到的宿主 band；requested = 据此决定往哪建；owner = 候选建窗
    // 时有没有拿到本进程的前台窗口作 owner。复核拿 requested 比而不拿 actual：建窗回退到
    // band=0 时 actual 恒与前台 band 不等，拿它比会每个键都重建一次。
    DWORD GetProbedHostBand() const { return _probedHostBand; }
    DWORD GetRequestedBand() const { return _requestedBand; }
    BOOL HasOwner() const { return _hasOwner; }

private:
    // Render thread entry point
    static DWORD WINAPI _RenderThread(LPVOID param);
    void _RenderLoop();

    // Render one frame from shared memory
    void _RenderFrame(const SharedRenderHeader* header, const void* pixelData);

    // Hide the window
    void _HideWindow();

    // Try to resolve CreateWindowInBand and GetWindowBand from user32.dll
    BOOL _ResolveAPIs();

    // Create the layered window in the host's Band
    BOOL _CreateBandWindow(DWORD band);

    // ── Mouse interaction (UI thread) ───────────────────────────────────────
    // Hit-test client coords against the latest rect table. Returns the candidate
    // index (>=0), a page button (-1 up / -2 down), or INT_MIN for no hit.
    int  _HitTest(int clientX, int clientY);
    void _OnMouseClick(int clientX, int clientY);
    void _OnMouseMove(int clientX, int clientY);
    void _OnMouseLeave();
    void _OnMouseWheel(int delta);
    // Send a hover notification (index + screen anchor) to Go; index<0 = left area.
    void _SendHover(int index);
    // Snapshot the embedded rect table for a freshly rendered frame (render thread).
    void _UpdateHitRects(const SharedRenderHeader* header);

    // Window state
    HWND _hwnd;
    ATOM _wndClassAtom;
    BOOL _active;
    DWORD _currentBand; // Band of the current window (0 if no window)
    DWORD _probedHostBand; // GetHostBand() at Initialize
    DWORD _requestedBand;  // band passed to _CreateBandWindow (before band=0 fallback)
    BOOL  _hasOwner;       // band window was created with a non-NULL owner

    // Shared memory
    HANDLE _hSharedMem;
    void*  _pSharedMem;
    DWORD  _maxBufferSize;

    // Event for signaling new frames
    HANDLE _hEvent;

    // Render thread
    HANDLE _hThread;
    HANDLE _hStopEvent; // Signaled to stop the render thread

    // Last rendered sequence (to skip stale frames)
    UINT32 _lastSequence;

    // Function pointers for undocumented APIs
    typedef HWND (WINAPI* CreateWindowInBand_t)(
        DWORD dwExStyle,
        ATOM atom,
        LPCWSTR lpWindowName,
        DWORD dwStyle,
        int X, int Y, int nWidth, int nHeight,
        HWND hWndParent,
        HMENU hMenu,
        HINSTANCE hInstance,
        LPVOID lpParam,
        DWORD dwBand
    );

    typedef BOOL (WINAPI* GetWindowBand_t)(HWND hwnd, DWORD* pdwBand);

    CreateWindowInBand_t _pfnCreateWindowInBand;
    GetWindowBand_t      _pfnGetWindowBand;

    // Window role. Only HOST_WINDOW_CANDIDATE routes mouse events to Go; tooltip/status
    // are pure-display band windows (their occlusion fix is z-order, not interaction).
    HostWindowKind _windowKind;
    // This window's host-render instance ID (bridge clientID). The render thread renders a
    // frame only when SharedRenderHeader.targetInstanceId == _instanceId; otherwise it hides.
    // Lets multiple TextService instances in one process share the global SHM without all
    // mirroring each other's frames (fixes "two candidate layers, only one hides").
    uint32_t _instanceId;
    // Forced band-window owner (NULL = derive from foreground). Non-candidate windows are
    // owned by the candidate hwnd so they stay above it in z-order.
    HWND _ownerOverride;

    // ── Mouse routing state ─────────────────────────────────────────────────
    CIPCClient* _pIPCClient; // weak ref (owned by TextService); routes mouse events to Go

    // Latest frame's hit geometry + screen placement. Written by the render thread in
    // _UpdateHitRects, read by the UI thread in _WndProc; guarded by _rectLock.
    CRITICAL_SECTION _rectLock;
    std::vector<HostRenderHitRect> _hitRects;
    int32_t _frameX;     // last frame screen X (= header.x), for hover anchor
    int32_t _frameY;     // last frame screen Y (= header.y)
    BOOL    _rectLockInit;

    // Hover-dedup baseline = the element currently highlighted on screen (hover encoding:
    // >=0 candidate, -1 none, -2 page-up, -3 page-down; INT_MIN = none yet). Written by the
    // UI thread (_OnMouseMove/_OnMouseLeave) AND the render thread (_UpdateHitRects syncs it
    // to each frame's renderedHoverIndex so a content change that clears the highlight
    // updates the baseline). All accesses are guarded by _rectLock.
    int  _lastHoverIndex;
    BOOL _trackingMouse;  // WM_MOUSELEAVE tracking armed (UI thread only)

    // Synthetic-move filter (UI thread only). When the candidate window repaints or
    // follows the caret under a physically-stationary cursor, Windows still posts
    // WM_MOUSEMOVE (with new client coords because the window moved). We compare the
    // SCREEN cursor position, which only changes on real movement — so typing with the
    // cursor parked over a candidate no longer flickers the highlight/tooltip. (The local
    // window achieves the same via ResetMouseTracking + "ignore first move after content
    // change", but geometry-based content detection is unreliable when re-showing identical
    // candidates; the screen-position test is robust regardless of window/content changes.)
    int  _lastScreenX;   // last processed move cursor screen X
    int  _lastScreenY;   // last processed move cursor screen Y
    BOOL _hasScreenPos;  // _lastScreenX/Y hold a valid previous position

    // Static window proc for the Band window
    static LRESULT CALLBACK _WndProc(HWND hwnd, UINT msg, WPARAM wParam, LPARAM lParam);
};
