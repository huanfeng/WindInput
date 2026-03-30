#pragma once

#include "Globals.h"
#include "BinaryProtocol.h"
#include <string>

// HostWindow manages a candidate window created via CreateWindowInBand inside the host process.
// This allows the candidate window to appear above high-Band windows (e.g. Start Menu).
// The window is rendered by Go via shared memory; this class handles display only.
class CHostWindow
{
public:
    CHostWindow();
    ~CHostWindow();

    // Initialize with shared memory and event names from Go service.
    // Creates the Band window and starts the render thread.
    // Returns TRUE on success.
    BOOL Initialize(const wchar_t* shmName, const wchar_t* eventName, DWORD maxBufferSize);

    // Shut down: stop render thread, destroy window, unmap shared memory.
    void Uninitialize();

    // Returns TRUE if the host window is active and rendering.
    BOOL IsActive() const { return _active; }

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

    // Get the Band of the host process's foreground window
    DWORD _GetHostBand();

    // Create the layered window in the host's Band
    BOOL _CreateBandWindow(DWORD band);

    // Window state
    HWND _hwnd;
    ATOM _wndClassAtom;
    BOOL _active;

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

    // Static window proc for the Band window
    static LRESULT CALLBACK _WndProc(HWND hwnd, UINT msg, WPARAM wParam, LPARAM lParam);
};
