// Package ui provides native Windows UI for candidate window
package ui

import (
	"fmt"
	"image"
	"log/slog"
	"sync"
	"syscall"
	"unsafe"

	"golang.org/x/sys/windows"
)

var (
	user32   = windows.NewLazySystemDLL("user32.dll")
	gdi32    = windows.NewLazySystemDLL("gdi32.dll")
	msimg32  = windows.NewLazySystemDLL("msimg32.dll")
	kernel32 = windows.NewLazySystemDLL("kernel32.dll")

	procRegisterClassExW         = user32.NewProc("RegisterClassExW")
	procCreateWindowExW          = user32.NewProc("CreateWindowExW")
	procDefWindowProcW           = user32.NewProc("DefWindowProcW")
	procShowWindow               = user32.NewProc("ShowWindow")
	procUpdateWindow             = user32.NewProc("UpdateWindow")
	procDestroyWindow            = user32.NewProc("DestroyWindow")
	procGetDC                    = user32.NewProc("GetDC")
	procReleaseDC                = user32.NewProc("ReleaseDC")
	procSetWindowPos             = user32.NewProc("SetWindowPos")
	procUpdateLayeredWindow      = user32.NewProc("UpdateLayeredWindow")
	procGetMessageW              = user32.NewProc("GetMessageW")
	procPeekMessageW             = user32.NewProc("PeekMessageW")
	procTranslateMessage         = user32.NewProc("TranslateMessage")
	procDispatchMessageW         = user32.NewProc("DispatchMessageW")
	procPostQuitMessage          = user32.NewProc("PostQuitMessage")
	procPostMessageW             = user32.NewProc("PostMessageW")
	procGetDpiForSystem          = user32.NewProc("GetDpiForSystem")
	procMsgWaitForMultipleObjects = user32.NewProc("MsgWaitForMultipleObjects")
	procMonitorFromPoint         = user32.NewProc("MonitorFromPoint")
	procGetMonitorInfoW          = user32.NewProc("GetMonitorInfoW")
	procGetKeyState              = user32.NewProc("GetKeyState")

	procCreateEventW  = kernel32.NewProc("CreateEventW")
	procSetEvent      = kernel32.NewProc("SetEvent")
	procResetEvent    = kernel32.NewProc("ResetEvent")
	procCloseHandle   = kernel32.NewProc("CloseHandle")

	procCreateCompatibleDC   = gdi32.NewProc("CreateCompatibleDC")
	procDeleteDC             = gdi32.NewProc("DeleteDC")
	procSelectObject         = gdi32.NewProc("SelectObject")
	procDeleteObject         = gdi32.NewProc("DeleteObject")
	procCreateDIBSection     = gdi32.NewProc("CreateDIBSection")
	procGetDeviceCaps        = gdi32.NewProc("GetDeviceCaps")
)

// DPI constants
const (
	DefaultDPI = 96
	LOGPIXELSX = 88
	LOGPIXELSY = 90
)

// GetSystemDPI returns the system DPI
func GetSystemDPI() int {
	// Try Windows 10 1607+ API first
	if procGetDpiForSystem.Find() == nil {
		ret, _, _ := procGetDpiForSystem.Call()
		if ret != 0 {
			return int(ret)
		}
	}

	// Fallback: Use GetDeviceCaps with screen DC
	hdcScreen, _, _ := procGetDC.Call(0)
	if hdcScreen != 0 {
		defer procReleaseDC.Call(0, hdcScreen)
		dpi, _, _ := procGetDeviceCaps.Call(hdcScreen, LOGPIXELSX)
		if dpi != 0 {
			return int(dpi)
		}
	}

	return DefaultDPI
}

// GetDPIScale returns the DPI scale factor (1.0 = 100%, 1.5 = 150%, etc.)
func GetDPIScale() float64 {
	dpi := GetSystemDPI()
	return float64(dpi) / float64(DefaultDPI)
}

// ScaleForDPI scales a value according to the current DPI
func ScaleForDPI(value float64) float64 {
	return value * GetDPIScale()
}

// ScaleIntForDPI scales an integer value according to the current DPI
func ScaleIntForDPI(value int) int {
	return int(float64(value) * GetDPIScale())
}

const (
	WS_EX_LAYERED     = 0x00080000
	WS_EX_TOPMOST     = 0x00000008
	WS_EX_TOOLWINDOW  = 0x00000080
	WS_EX_NOACTIVATE  = 0x08000000
	WS_EX_TRANSPARENT = 0x00000020

	WS_POPUP   = 0x80000000
	WS_VISIBLE = 0x10000000

	SW_HIDE = 0
	SW_SHOW = 5

	SWP_NOMOVE     = 0x0002
	SWP_NOSIZE     = 0x0001
	SWP_NOZORDER   = 0x0004
	SWP_NOACTIVATE = 0x0010

	HWND_TOPMOST = ^uintptr(0) // -1

	ULW_ALPHA = 0x00000002

	AC_SRC_OVER  = 0x00
	AC_SRC_ALPHA = 0x01

	WM_USER      = 0x0400
	WM_DESTROY   = 0x0002
	WM_NCHITTEST = 0x0084
	WM_SETCURSOR = 0x0020

	WM_UPDATE_CONTENT = WM_USER + 1
	WM_SHOW_WINDOW    = WM_USER + 2
	WM_HIDE_WINDOW    = WM_USER + 3

	BI_RGB = 0

	DIB_RGB_COLORS = 0

	// PeekMessage options
	PM_REMOVE = 0x0001

	// MsgWaitForMultipleObjects flags
	QS_ALLINPUT = 0x04FF

	WAIT_OBJECT_0  = 0x00000000
	WAIT_TIMEOUT   = 0x00000102
	INFINITE       = 0xFFFFFFFF
)

type WNDCLASSEXW struct {
	CbSize        uint32
	Style         uint32
	LpfnWndProc   uintptr
	CbClsExtra    int32
	CbWndExtra    int32
	HInstance     windows.Handle
	HIcon         windows.Handle
	HCursor       windows.Handle
	HbrBackground windows.Handle
	LpszMenuName  *uint16
	LpszClassName *uint16
	HIconSm       windows.Handle
}

type MSG struct {
	HWnd    windows.HWND
	Message uint32
	WParam  uintptr
	LParam  uintptr
	Time    uint32
	Pt      POINT
}

type POINT struct {
	X, Y int32
}

type SIZE struct {
	Cx, Cy int32
}

type BLENDFUNCTION struct {
	BlendOp             byte
	BlendFlags          byte
	SourceConstantAlpha byte
	AlphaFormat         byte
}

type BITMAPINFOHEADER struct {
	BiSize          uint32
	BiWidth         int32
	BiHeight        int32
	BiPlanes        uint16
	BiBitCount      uint16
	BiCompression   uint32
	BiSizeImage     uint32
	BiXPelsPerMeter int32
	BiYPelsPerMeter int32
	BiClrUsed       uint32
	BiClrImportant  uint32
}

type BITMAPINFO struct {
	BmiHeader BITMAPINFOHEADER
	BmiColors [1]uint32
}

// CandidateWindow represents a native Windows candidate window
type CandidateWindow struct {
	hwnd   windows.HWND
	logger *slog.Logger

	mu      sync.Mutex
	visible bool
	x, y    int
	width   int
	height  int

	// For thread-safe updates
	updateCh chan *image.RGBA
	closeCh  chan struct{}
}

// NewCandidateWindow creates a new candidate window
func NewCandidateWindow(logger *slog.Logger) *CandidateWindow {
	return &CandidateWindow{
		logger:   logger,
		updateCh: make(chan *image.RGBA, 10),
		closeCh:  make(chan struct{}),
	}
}

// wndProc is the window procedure
func wndProc(hwnd uintptr, msg uint32, wParam, lParam uintptr) uintptr {
	switch msg {
	case WM_DESTROY:
		procPostQuitMessage.Call(0)
		return 0
	case WM_NCHITTEST:
		// Return HTTRANSPARENT (-1) so mouse events pass through
		// This prevents the busy cursor when hovering over the window
		return ^uintptr(0) // -1 as uintptr
	case WM_SETCURSOR:
		// Don't change cursor - let the underlying window handle it
		return 1
	}
	ret, _, _ := procDefWindowProcW.Call(hwnd, uintptr(msg), wParam, lParam)
	return ret
}

// Create creates the window (must be called from the UI thread)
func (w *CandidateWindow) Create() error {
	w.logger.Info("Creating candidate window...")

	className, _ := syscall.UTF16PtrFromString("IMECandidateWindow")

	wc := WNDCLASSEXW{
		CbSize:        uint32(unsafe.Sizeof(WNDCLASSEXW{})),
		LpfnWndProc:   syscall.NewCallback(wndProc),
		LpszClassName: className,
	}

	ret, _, err := procRegisterClassExW.Call(uintptr(unsafe.Pointer(&wc)))
	if ret == 0 {
		// Class might already be registered, continue anyway
		w.logger.Warn("RegisterClassExW failed (may already exist)", "error", err)
	}

	// Create layered window
	exStyle := uint32(WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE)
	style := uint32(WS_POPUP)

	hwnd, _, err := procCreateWindowExW.Call(
		uintptr(exStyle),
		uintptr(unsafe.Pointer(className)),
		0, // No title
		uintptr(style),
		0, 0, 1, 1, // Initial position and size
		0, 0, 0, 0,
	)

	if hwnd == 0 {
		return fmt.Errorf("CreateWindowExW failed: %w", err)
	}

	w.hwnd = windows.HWND(hwnd)
	w.logger.Info("Candidate window created", "hwnd", hwnd)

	return nil
}

// Run runs the message loop (blocking, call from dedicated goroutine)
func (w *CandidateWindow) Run() {
	w.logger.Info("Starting window message loop...")

	var msg MSG
	for {
		ret, _, _ := procGetMessageW.Call(
			uintptr(unsafe.Pointer(&msg)),
			0, 0, 0,
		)

		if ret == 0 || ret == ^uintptr(0) { // 0 = WM_QUIT, -1 = error
			break
		}

		procTranslateMessage.Call(uintptr(unsafe.Pointer(&msg)))
		procDispatchMessageW.Call(uintptr(unsafe.Pointer(&msg)))
	}

	w.logger.Info("Window message loop ended")
}

// UpdateContent updates the window content with the given image
func (w *CandidateWindow) UpdateContent(img *image.RGBA, x, y int) error {
	if w.hwnd == 0 {
		return fmt.Errorf("window not created")
	}

	w.mu.Lock()
	w.x = x
	w.y = y
	w.width = img.Bounds().Dx()
	w.height = img.Bounds().Dy()
	w.mu.Unlock()

	return w.updateLayeredWindow(img, x, y)
}

func (w *CandidateWindow) updateLayeredWindow(img *image.RGBA, x, y int) error {
	bounds := img.Bounds()
	width := bounds.Dx()
	height := bounds.Dy()

	// Get screen DC
	hdcScreen, _, _ := procGetDC.Call(0)
	if hdcScreen == 0 {
		return fmt.Errorf("GetDC failed")
	}
	defer procReleaseDC.Call(0, hdcScreen)

	// Create compatible DC
	hdcMem, _, _ := procCreateCompatibleDC.Call(hdcScreen)
	if hdcMem == 0 {
		return fmt.Errorf("CreateCompatibleDC failed")
	}
	defer procDeleteDC.Call(hdcMem)

	// Create DIB section
	bi := BITMAPINFO{
		BmiHeader: BITMAPINFOHEADER{
			BiSize:        uint32(unsafe.Sizeof(BITMAPINFOHEADER{})),
			BiWidth:       int32(width),
			BiHeight:      -int32(height), // Top-down DIB
			BiPlanes:      1,
			BiBitCount:    32,
			BiCompression: BI_RGB,
		},
	}

	var bits unsafe.Pointer
	hBitmap, _, err := procCreateDIBSection.Call(
		hdcMem,
		uintptr(unsafe.Pointer(&bi)),
		DIB_RGB_COLORS,
		uintptr(unsafe.Pointer(&bits)),
		0, 0,
	)
	if hBitmap == 0 {
		return fmt.Errorf("CreateDIBSection failed: %w", err)
	}
	defer procDeleteObject.Call(hBitmap)

	// Select bitmap into DC
	procSelectObject.Call(hdcMem, hBitmap)

	// Copy image data to DIB (convert RGBA to BGRA with premultiplied alpha)
	pixelCount := width * height
	dstSlice := unsafe.Slice((*byte)(bits), pixelCount*4)

	for i := 0; i < pixelCount; i++ {
		srcIdx := i * 4
		dstIdx := i * 4

		r := img.Pix[srcIdx+0]
		g := img.Pix[srcIdx+1]
		b := img.Pix[srcIdx+2]
		a := img.Pix[srcIdx+3]

		// Premultiply alpha
		if a == 255 {
			dstSlice[dstIdx+0] = b
			dstSlice[dstIdx+1] = g
			dstSlice[dstIdx+2] = r
			dstSlice[dstIdx+3] = a
		} else if a == 0 {
			dstSlice[dstIdx+0] = 0
			dstSlice[dstIdx+1] = 0
			dstSlice[dstIdx+2] = 0
			dstSlice[dstIdx+3] = 0
		} else {
			dstSlice[dstIdx+0] = byte(uint16(b) * uint16(a) / 255)
			dstSlice[dstIdx+1] = byte(uint16(g) * uint16(a) / 255)
			dstSlice[dstIdx+2] = byte(uint16(r) * uint16(a) / 255)
			dstSlice[dstIdx+3] = a
		}
	}

	// Update layered window
	ptSrc := POINT{X: 0, Y: 0}
	ptDst := POINT{X: int32(x), Y: int32(y)}
	size := SIZE{Cx: int32(width), Cy: int32(height)}
	blend := BLENDFUNCTION{
		BlendOp:             AC_SRC_OVER,
		BlendFlags:          0,
		SourceConstantAlpha: 255,
		AlphaFormat:         AC_SRC_ALPHA,
	}

	ret, _, err := procUpdateLayeredWindow.Call(
		uintptr(w.hwnd),
		hdcScreen,
		uintptr(unsafe.Pointer(&ptDst)),
		uintptr(unsafe.Pointer(&size)),
		hdcMem,
		uintptr(unsafe.Pointer(&ptSrc)),
		0,
		uintptr(unsafe.Pointer(&blend)),
		ULW_ALPHA,
	)

	if ret == 0 {
		return fmt.Errorf("UpdateLayeredWindow failed: %w", err)
	}

	return nil
}

// Show shows the window
func (w *CandidateWindow) Show() {
	if w.hwnd == 0 {
		return
	}
	procShowWindow.Call(uintptr(w.hwnd), SW_SHOW)
	w.mu.Lock()
	w.visible = true
	w.mu.Unlock()
}

// Hide hides the window
func (w *CandidateWindow) Hide() {
	if w.hwnd == 0 {
		return
	}
	procShowWindow.Call(uintptr(w.hwnd), SW_HIDE)
	w.mu.Lock()
	w.visible = false
	w.mu.Unlock()
}

// IsVisible returns whether the window is visible
func (w *CandidateWindow) IsVisible() bool {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.visible
}

// SetPosition sets the window position
func (w *CandidateWindow) SetPosition(x, y int) {
	if w.hwnd == 0 {
		return
	}
	procSetWindowPos.Call(
		uintptr(w.hwnd),
		HWND_TOPMOST,
		uintptr(x), uintptr(y),
		0, 0,
		SWP_NOSIZE|SWP_NOACTIVATE,
	)
}

// Destroy destroys the window
func (w *CandidateWindow) Destroy() {
	if w.hwnd != 0 {
		procDestroyWindow.Call(uintptr(w.hwnd))
		w.hwnd = 0
	}
}

// Handle returns the window handle
func (w *CandidateWindow) Handle() windows.HWND {
	return w.hwnd
}

// CreateEvent creates a Windows event object
func CreateEvent() (windows.Handle, error) {
	ret, _, err := procCreateEventW.Call(0, 1, 0, 0) // Manual reset, initial state = not signaled
	if ret == 0 {
		return 0, err
	}
	return windows.Handle(ret), nil
}

// SetEvent sets the event to signaled state
func SetEvent(event windows.Handle) {
	procSetEvent.Call(uintptr(event))
}

// ResetEvent resets the event to non-signaled state
func ResetEvent(event windows.Handle) {
	procResetEvent.Call(uintptr(event))
}

// CloseEvent closes the event handle
func CloseEvent(event windows.Handle) {
	procCloseHandle.Call(uintptr(event))
}

// MsgWaitForMultipleObjects waits for messages or events
// Returns: 0 = event signaled, 1 = message available, WAIT_TIMEOUT = timeout
func MsgWaitForMultipleObjects(event windows.Handle, timeoutMs uint32) uint32 {
	handles := [1]uintptr{uintptr(event)}
	ret, _, _ := procMsgWaitForMultipleObjects.Call(
		1,                               // nCount
		uintptr(unsafe.Pointer(&handles[0])), // pHandles
		0,                               // bWaitAll = FALSE
		uintptr(timeoutMs),              // dwMilliseconds
		QS_ALLINPUT,                     // dwWakeMask
	)
	return uint32(ret)
}

// PeekMessage checks for a message without blocking
func PeekMessage(msg *MSG) bool {
	ret, _, _ := procPeekMessageW.Call(
		uintptr(unsafe.Pointer(msg)),
		0, 0, 0,
		PM_REMOVE,
	)
	return ret != 0
}

// ProcessMessage translates and dispatches a message
func ProcessMessage(msg *MSG) {
	procTranslateMessage.Call(uintptr(unsafe.Pointer(msg)))
	procDispatchMessageW.Call(uintptr(unsafe.Pointer(msg)))
}

// MONITORINFO structure for GetMonitorInfo
type MONITORINFO struct {
	CbSize    uint32
	RcMonitor RECT
	RcWork    RECT
	DwFlags   uint32
}

// RECT structure
type RECT struct {
	Left   int32
	Top    int32
	Right  int32
	Bottom int32
}

// Monitor flags
const (
	MONITOR_DEFAULTTONEAREST = 0x00000002
	VK_CAPITAL               = 0x14 // CapsLock key
)

// GetMonitorWorkAreaFromPoint returns the work area (excluding taskbar) of the monitor
// containing the specified point. Returns (left, top, right, bottom).
func GetMonitorWorkAreaFromPoint(x, y int) (left, top, right, bottom int) {
	// MonitorFromPoint expects POINT struct packed into a single 64-bit value on x64 Windows ABI
	// POINT struct: { LONG x, LONG y } = 8 bytes total
	// In x64 calling convention, 8-byte structs are passed in a single register
	// Low 32 bits = x, High 32 bits = y
	pt := uintptr(uint32(x)) | (uintptr(uint32(y)) << 32)

	hMonitor, _, _ := procMonitorFromPoint.Call(
		pt,
		MONITOR_DEFAULTTONEAREST,
	)

	if hMonitor == 0 {
		// Fallback to primary monitor work area
		return 0, 0, 1920, 1080
	}

	// Get monitor info
	var mi MONITORINFO
	mi.CbSize = uint32(unsafe.Sizeof(mi))
	ret, _, _ := procGetMonitorInfoW.Call(hMonitor, uintptr(unsafe.Pointer(&mi)))

	if ret == 0 {
		// Fallback
		return 0, 0, 1920, 1080
	}

	return int(mi.RcWork.Left), int(mi.RcWork.Top), int(mi.RcWork.Right), int(mi.RcWork.Bottom)
}

// GetCurrentMonitorWorkArea returns the work area (excluding taskbar) of the monitor
// containing the mouse cursor. Returns (left, top, right, bottom).
func GetCurrentMonitorWorkArea() (left, top, right, bottom int) {
	// Get cursor position
	var pt POINT
	procGetCursorPos.Call(uintptr(unsafe.Pointer(&pt)))

	return GetMonitorWorkAreaFromPoint(int(pt.X), int(pt.Y))
}

// GetDefaultToolbarPosition returns the default position for the toolbar
// (bottom-right corner of the current monitor's work area)
func GetDefaultToolbarPosition(toolbarWidth, toolbarHeight int) (x, y int) {
	left, top, right, bottom := GetCurrentMonitorWorkArea()

	// Position at bottom-right corner with some margin (DPI scaled)
	margin := ScaleIntForDPI(10)
	x = right - toolbarWidth - margin
	y = bottom - toolbarHeight - margin

	// Ensure position is within work area
	if x < left {
		x = left + margin
	}
	if y < top {
		y = top + margin
	}

	return x, y
}

// GetToolbarPositionForCaret returns the toolbar position for the monitor containing the caret
// (bottom-right corner of that monitor's work area)
func GetToolbarPositionForCaret(caretX, caretY, toolbarWidth, toolbarHeight int) (x, y int) {
	left, top, right, bottom := GetMonitorWorkAreaFromPoint(caretX, caretY)

	// Position at bottom-right corner with some margin (DPI scaled)
	margin := ScaleIntForDPI(10)
	x = right - toolbarWidth - margin
	y = bottom - toolbarHeight - margin

	// Ensure position is within work area
	if x < left {
		x = left + margin
	}
	if y < top {
		y = top + margin
	}

	return x, y
}

// GetCapsLockState returns the current state of CapsLock key
// Returns true if CapsLock is ON, false otherwise
func GetCapsLockState() bool {
	state, _, _ := procGetKeyState.Call(uintptr(VK_CAPITAL))
	// The low-order bit indicates toggle state (0 = off, 1 = on)
	return (state & 0x0001) != 0
}

// CandidateLayout represents the layout direction of the candidate window
type CandidateLayout int

const (
	LayoutVertical   CandidateLayout = iota // Candidates displayed vertically (current default)
	LayoutHorizontal                        // Candidates displayed horizontally (future)
)

// AdjustCandidatePosition adjusts the candidate window position to ensure it stays within screen bounds.
// Parameters:
//   - caretX, caretY: the caret position (where input is happening)
//   - caretHeight: height of the caret/cursor
//   - windowWidth, windowHeight: size of the candidate window
//   - layout: the layout direction of the candidate window
//
// Returns adjusted (x, y) position for the candidate window.
func AdjustCandidatePosition(caretX, caretY, caretHeight, windowWidth, windowHeight int, layout CandidateLayout) (x, y int) {
	// Get the work area of the monitor containing the caret
	workLeft, workTop, workRight, workBottom := GetMonitorWorkAreaFromPoint(caretX, caretY)

	// Small gap between caret and candidate window
	const gap = 5

	switch layout {
	case LayoutHorizontal:
		// Horizontal layout: prefer to show to the right of caret
		// If not enough space on the right, show on the left
		x = caretX + gap
		y = caretY

		// Check right boundary
		if x+windowWidth > workRight {
			// Not enough space on the right, show on the left
			x = caretX - windowWidth - gap
		}

		// Ensure x is within left boundary
		if x < workLeft {
			x = workLeft
		}

		// Check bottom boundary for vertical overflow
		if y+windowHeight > workBottom {
			y = workBottom - windowHeight
		}

		// Ensure y is within top boundary
		if y < workTop {
			y = workTop
		}

	case LayoutVertical:
		fallthrough
	default:
		// Vertical layout (default): prefer to show below caret
		// If not enough space below, show above
		x = caretX
		y = caretY + caretHeight + gap

		// Check bottom boundary
		if y+windowHeight > workBottom {
			// Not enough space below, show above the caret
			y = caretY - windowHeight - gap
		}

		// Ensure y is within top boundary (in case window is very tall)
		if y < workTop {
			y = workTop
		}

		// Check right boundary for horizontal overflow
		if x+windowWidth > workRight {
			x = workRight - windowWidth
		}

		// Ensure x is within left boundary
		if x < workLeft {
			x = workLeft
		}
	}

	return x, y
}
