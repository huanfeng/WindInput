// Package ui provides native Windows UI for candidate window
package ui

import (
	"fmt"
	"image"
	"log/slog"
	"sync"
	"syscall"
	"unsafe"

	"github.com/huanfeng/wind_input/pkg/theme"
	"golang.org/x/sys/windows"
)

var (
	user32   = windows.NewLazySystemDLL("user32.dll")
	gdi32    = windows.NewLazySystemDLL("gdi32.dll")
	msimg32  = windows.NewLazySystemDLL("msimg32.dll")
	kernel32 = windows.NewLazySystemDLL("kernel32.dll")

	procRegisterClassExW          = user32.NewProc("RegisterClassExW")
	procCreateWindowExW           = user32.NewProc("CreateWindowExW")
	procDefWindowProcW            = user32.NewProc("DefWindowProcW")
	procShowWindow                = user32.NewProc("ShowWindow")
	procUpdateWindow              = user32.NewProc("UpdateWindow")
	procDestroyWindow             = user32.NewProc("DestroyWindow")
	procGetDC                     = user32.NewProc("GetDC")
	procReleaseDC                 = user32.NewProc("ReleaseDC")
	procSetWindowPos              = user32.NewProc("SetWindowPos")
	procUpdateLayeredWindow       = user32.NewProc("UpdateLayeredWindow")
	procGetMessageW               = user32.NewProc("GetMessageW")
	procPeekMessageW              = user32.NewProc("PeekMessageW")
	procTranslateMessage          = user32.NewProc("TranslateMessage")
	procDispatchMessageW          = user32.NewProc("DispatchMessageW")
	procPostQuitMessage           = user32.NewProc("PostQuitMessage")
	procPostMessageW              = user32.NewProc("PostMessageW")
	procGetDpiForSystem           = user32.NewProc("GetDpiForSystem")
	procGetDpiForWindow           = user32.NewProc("GetDpiForWindow")
	procMsgWaitForMultipleObjects = user32.NewProc("MsgWaitForMultipleObjects")
	procMonitorFromPoint          = user32.NewProc("MonitorFromPoint")
	procGetMonitorInfoW           = user32.NewProc("GetMonitorInfoW")
	procGetKeyState               = user32.NewProc("GetKeyState")

	procCreateEventW = kernel32.NewProc("CreateEventW")
	procSetEvent     = kernel32.NewProc("SetEvent")
	procResetEvent   = kernel32.NewProc("ResetEvent")
	procCloseHandle  = kernel32.NewProc("CloseHandle")

	procCreateCompatibleDC = gdi32.NewProc("CreateCompatibleDC")
	procDeleteDC           = gdi32.NewProc("DeleteDC")
	procSelectObject       = gdi32.NewProc("SelectObject")
	procDeleteObject       = gdi32.NewProc("DeleteObject")
	procCreateDIBSection   = gdi32.NewProc("CreateDIBSection")
	procGetDeviceCaps      = gdi32.NewProc("GetDeviceCaps")
)

const (
	WS_EX_LAYERED     = 0x00080000
	WS_EX_TOPMOST     = 0x00000008
	WS_EX_TOOLWINDOW  = 0x00000080
	WS_EX_NOACTIVATE  = 0x08000000
	WS_EX_TRANSPARENT = 0x00000020

	WS_POPUP   = 0x80000000
	WS_VISIBLE = 0x10000000

	SW_HIDE   = 0
	SW_SHOW   = 5
	SW_SHOWNA = 8

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

	// Mouse messages (WM_MOUSEMOVE, WM_LBUTTONDOWN, etc. defined in toolbar_window.go)
	WM_RBUTTONDOWN = 0x0204
	WM_COMMAND     = 0x0111

	// Menu flags
	MF_STRING    = 0x0000
	MF_SEPARATOR = 0x0800
	MF_GRAYED    = 0x0001

	// TrackPopupMenu flags
	TPM_LEFTALIGN   = 0x0000
	TPM_TOPALIGN    = 0x0000
	TPM_BOTTOMALIGN = 0x0020
	TPM_RETURNCMD   = 0x0100
	TPM_NONOTIFY    = 0x0080

	// Candidate context menu IDs
	IDM_CANDIDATE_MOVEUP   = 1001
	IDM_CANDIDATE_MOVEDOWN = 1002
	IDM_CANDIDATE_MOVETOP  = 1003
	IDM_CANDIDATE_DELETE   = 1004
	IDM_CANDIDATE_RESET    = 1005
	IDM_CANDIDATE_SETTINGS = 1006
	IDM_CANDIDATE_ABOUT    = 1007
	IDM_CANDIDATE_COPY     = 1008
	IDM_CANDIDATE_COPYALL  = 1009 // Debug: 复制所有候选
	IDM_CANDIDATE_COPY1P   = 1010 // Debug: 复制前1页候选
	IDM_CANDIDATE_COPY2P   = 1011 // Debug: 复制前2页候选
	IDM_CANDIDATE_COPY3P   = 1012 // Debug: 复制前3页候选

	WM_DPICHANGED = 0x02E0

	WM_UPDATE_CONTENT = WM_USER + 1
	WM_SHOW_WINDOW    = WM_USER + 2
	WM_HIDE_WINDOW    = WM_USER + 3

	BI_RGB = 0

	DIB_RGB_COLORS = 0

	// PeekMessage options
	PM_REMOVE = 0x0001

	// MsgWaitForMultipleObjects flags
	QS_ALLINPUT = 0x04FF

	WAIT_OBJECT_0 = 0x00000000
	WAIT_TIMEOUT  = 0x00000102
	INFINITE      = 0xFFFFFFFF
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

// Global window registry for wndProc to access CandidateWindow instances
var candidateWindows = NewWindowRegistry[CandidateWindow]()

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

	// Mouse interaction support
	hitRects            []CandidateRect // Bounding rectangles for hit testing
	pageUpRect          *CandidateRect  // Bounding rectangle for page up button
	pageDownRect        *CandidateRect  // Bounding rectangle for page down button
	pageStartIndex      int             // 当前页首个候选的全局索引
	totalCandidateCount int             // 候选总数（所有页）
	hasShadowFlags      []bool          // 当前页各候选是否有 Shadow 修改或短语覆盖
	isCommandFlags      []bool          // 当前页各候选是否为命令候选（短语）
	candidateTexts      []string        // 当前页各候选的文本（用于菜单状态判断）
	isPinyinMode        bool            // 是否拼音模式（拼音禁用前移/后移）
	isQuickInputMode    bool            // 是否快捷输入模式（右键菜单只保留复制）
	hoverIndex          int             // Currently hovered candidate index (-1 for none)
	hoverPageBtn        string          // "" = none, "up" = page up hovered, "down" = page down hovered
	trackingMouse       bool            // Whether mouse leave tracking is enabled
	callbacks           *CandidateCallback
	mouseHasMoved       bool // Whether mouse has physically moved since last content update
	hasLastMousePos     bool // Whether we have a stored previous mouse position
	lastMouseX          int  // Last mouse X position (window-relative)
	lastMouseY          int  // Last mouse Y position (window-relative)

	// Drag support: allow user to drag the candidate window by blank area
	dragging   bool // Whether the window is being dragged
	dragStartX int  // Mouse X at drag start (window-relative)
	dragStartY int  // Mouse Y at drag start (window-relative)
	dragPinned bool // Whether position is pinned by user drag (suppress auto-positioning)

	// Custom popup menu (doesn't steal focus)
	popupMenu       *PopupMenu
	menuOpen        bool // Whether context menu is currently open
	menuTargetIndex int  // The candidate index that was right-clicked

	// DPI change callback
	onDPIChanged func()
}

// NewCandidateWindow creates a new candidate window
func NewCandidateWindow(logger *slog.Logger) *CandidateWindow {
	return &CandidateWindow{
		logger:     logger,
		updateCh:   make(chan *image.RGBA, 10),
		closeCh:    make(chan struct{}),
		hoverIndex: -1,
	}
}

// wndProc is the window procedure
func wndProc(hwnd uintptr, msg uint32, wParam, lParam uintptr) uintptr {
	switch msg {
	case WM_DESTROY:
		candidateWindows.Unregister(windows.HWND(hwnd))
		procPostQuitMessage.Call(0)
		return 0

	case WM_NCHITTEST:
		// Return HTCLIENT to receive mouse messages
		return HTCLIENT

	case WM_SETCURSOR:
		w := candidateWindows.Get(windows.HWND(hwnd))
		if w != nil && w.isDragging() {
			cursor, _, _ := procLoadCursorW.Call(0, IDC_SIZEALL)
			if cursor != 0 {
				procSetCursor.Call(cursor)
			}
			return 1
		}
		cursor, _, _ := procLoadCursorW.Call(0, IDC_ARROW)
		if cursor != 0 {
			procSetCursor.Call(cursor)
		}
		return 1

	case WM_MOUSEMOVE:
		w := candidateWindows.Get(windows.HWND(hwnd))
		if w != nil {
			w.handleMouseMove(lParam)
		}
		return 0

	case WM_LBUTTONDOWN:
		w := candidateWindows.Get(windows.HWND(hwnd))
		if w != nil {
			w.handleMouseClick(lParam)
		}
		return 0

	case WM_LBUTTONUP:
		w := candidateWindows.Get(windows.HWND(hwnd))
		if w != nil {
			w.handleDragEnd()
		}
		return 0

	case WM_RBUTTONDOWN:
		w := candidateWindows.Get(windows.HWND(hwnd))
		if w != nil {
			w.handleRightClick(lParam)
		}
		return 0

	case WM_MOUSELEAVE:
		w := candidateWindows.Get(windows.HWND(hwnd))
		if w != nil {
			w.handleMouseLeave()
		}
		return 0

	case WM_DPICHANGED:
		// wParam: LOWORD = new X DPI, HIWORD = new Y DPI
		newDPI := int(wParam & 0xFFFF)
		if newDPI > 0 {
			SetEffectiveDPI(newDPI)
		}
		w := candidateWindows.Get(windows.HWND(hwnd))
		if w != nil {
			w.mu.Lock()
			cb := w.onDPIChanged
			w.mu.Unlock()
			if cb != nil {
				cb()
			}
		}
		return 0
	}
	ret, _, _ := procDefWindowProcW.Call(hwnd, uintptr(msg), wParam, lParam)
	return ret
}

// Create creates the window (must be called from the UI thread)
func (w *CandidateWindow) Create() error {
	w.logger.Info("Creating candidate window...")

	hwnd, err := CreateLayeredWindow(LayeredWindowConfig{
		ClassName: "IMECandidateWindow",
		WndProc:   syscall.NewCallback(wndProc),
	})
	if err != nil {
		return err
	}

	w.hwnd = hwnd
	candidateWindows.Register(w.hwnd, w)
	w.logger.Info("Candidate window created", "hwnd", w.hwnd)

	// Create custom popup menu (doesn't steal focus)
	w.popupMenu = NewPopupMenu()
	if err := w.popupMenu.Create(); err != nil {
		w.logger.Warn("Failed to create popup menu", "error", err)
	}

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
	return UpdateLayeredWindowFromImage(w.hwnd, img, x, y)
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
	// Also hide popup menu if open
	w.HideMenu()

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

// GetPosition returns the current window position
func (w *CandidateWindow) GetPosition() (x, y int) {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.x, w.y
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
	// Destroy popup menu first
	if w.popupMenu != nil {
		w.popupMenu.Destroy()
		w.popupMenu = nil
	}
	if w.hwnd != 0 {
		procDestroyWindow.Call(uintptr(w.hwnd))
		w.hwnd = 0
	}
}

// Handle returns the window handle
func (w *CandidateWindow) Handle() windows.HWND {
	return w.hwnd
}

// SetHitRects sets the bounding rectangles for hit testing
func (w *CandidateWindow) SetHitRects(rects []CandidateRect) {
	w.mu.Lock()
	w.hitRects = rects
	w.mu.Unlock()
}

// SetPageRects sets the bounding rectangles for page up/down buttons
func (w *CandidateWindow) SetPageRects(pageUp, pageDown *CandidateRect) {
	w.mu.Lock()
	w.pageUpRect = pageUp
	w.pageDownRect = pageDown
	w.mu.Unlock()
}

// SetCandidatePageInfo 设置候选分页信息（用于右键菜单判断全局位置）
func (w *CandidateWindow) SetCandidatePageInfo(pageStartIndex, totalCount int) {
	w.mu.Lock()
	w.pageStartIndex = pageStartIndex
	w.totalCandidateCount = totalCount
	w.mu.Unlock()
}

// SetCandidateHasShadow 设置当前页各候选的 Shadow 修改标记
func (w *CandidateWindow) SetCandidateHasShadow(flags []bool) {
	w.mu.Lock()
	w.hasShadowFlags = flags
	w.mu.Unlock()
}

// SetCandidateMenuState 设置右键菜单所需的额外状态
func (w *CandidateWindow) SetCandidateMenuState(texts []string, isPinyin bool, isCommandFlags []bool) {
	w.mu.Lock()
	w.candidateTexts = texts
	w.isPinyinMode = isPinyin
	w.isCommandFlags = isCommandFlags
	w.mu.Unlock()
}

// SetQuickInputMode 设置是否为快捷输入模式（右键菜单只保留复制）
func (w *CandidateWindow) SetQuickInputMode(isQuickInput bool) {
	w.mu.Lock()
	w.isQuickInputMode = isQuickInput
	w.mu.Unlock()
}

// SetCallbacks sets the mouse event callbacks
func (w *CandidateWindow) SetCallbacks(callbacks *CandidateCallback) {
	w.mu.Lock()
	w.callbacks = callbacks
	w.mu.Unlock()
}

// SetOnDPIChanged sets a callback invoked when WM_DPICHANGED is received.
func (w *CandidateWindow) SetOnDPIChanged(fn func()) {
	w.mu.Lock()
	w.onDPIChanged = fn
	w.mu.Unlock()
}

// SetMenuFontParams updates GDI font weight and scale for candidate window's popup menu
func (w *CandidateWindow) SetMenuFontParams(weight int, scale float64) {
	w.mu.Lock()
	if w.popupMenu != nil {
		w.popupMenu.SetGDIFontParams(weight, scale)
	}
	w.mu.Unlock()
}

// SetTextRenderMode sets the text render mode for the candidate window's popup menu.
func (w *CandidateWindow) SetTextRenderMode(mode TextRenderMode) {
	w.mu.Lock()
	if w.popupMenu != nil {
		w.popupMenu.SetTextRenderMode(mode)
	}
	w.mu.Unlock()
}

// SetMenuFontFamily sets the primary font family for the candidate window's popup menu.
func (w *CandidateWindow) SetMenuFontFamily(fontSpec string) {
	w.mu.Lock()
	if w.popupMenu != nil {
		w.popupMenu.SetFontFamily(fontSpec)
	}
	w.mu.Unlock()
}

// SetMenuFontSize sets the base font size for candidate window's popup menu
func (w *CandidateWindow) SetMenuFontSize(size float64) {
	w.mu.Lock()
	if w.popupMenu != nil {
		w.popupMenu.SetMenuFontSize(size)
	}
	w.mu.Unlock()
}

// SetTheme sets the theme for the candidate window's popup menu
func (w *CandidateWindow) SetTheme(resolved *theme.ResolvedTheme) {
	w.mu.Lock()
	if w.popupMenu != nil {
		w.popupMenu.SetTheme(resolved)
	}
	w.mu.Unlock()
}

// GetHoverIndex returns the current hover index
func (w *CandidateWindow) GetHoverIndex() int {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.hoverIndex
}

// ResetHoverIndex resets the hover index and page button hover state
func (w *CandidateWindow) ResetHoverIndex() {
	w.mu.Lock()
	w.hoverIndex = -1
	w.hoverPageBtn = ""
	w.mu.Unlock()
}

// GetHoverPageBtn returns the currently hovered page button ("up", "down", or "")
func (w *CandidateWindow) GetHoverPageBtn() string {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.hoverPageBtn
}
