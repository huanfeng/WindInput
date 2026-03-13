// Package ui provides native Windows UI for input method
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

// Additional Windows constants for toolbar
const (
	WM_LBUTTONDOWN = 0x0201
	WM_LBUTTONUP   = 0x0202
	WM_MOUSEMOVE   = 0x0200
	WM_RBUTTONUP   = 0x0205
	WM_MOUSELEAVE  = 0x02A3
	WM_TIMER       = 0x0113
	// WM_SETCURSOR defined in window.go

	HTCLIENT = 1

	GWL_WNDPROC = -4

	// Cursor IDs
	IDC_ARROW   = 32512
	IDC_SIZEALL = 32646
	IDC_HAND    = 32649

	// Tooltip constants
	TOOLTIP_TIMER_ID = 1
	TOOLTIP_DELAY_MS = 400  // Delay before showing tooltip
	TOOLTIP_HIDE_MS  = 3000 // Auto-hide after this time
	TME_LEAVE        = 0x00000002
)

var (
	procSetWindowLongPtrW = user32.NewProc("SetWindowLongPtrW")
	procCallWindowProcW   = user32.NewProc("CallWindowProcW")
	procGetCursorPos      = user32.NewProc("GetCursorPos")
	procScreenToClient    = user32.NewProc("ScreenToClient")
	procClientToScreen    = user32.NewProc("ClientToScreen")
	procSetCapture        = user32.NewProc("SetCapture")
	procReleaseCapture    = user32.NewProc("ReleaseCapture")
	procLoadCursorW       = user32.NewProc("LoadCursorW")
	procSetCursor         = user32.NewProc("SetCursor")
	procSetTimer          = user32.NewProc("SetTimer")
	procKillTimer         = user32.NewProc("KillTimer")
	procTrackMouseEvent   = user32.NewProc("TrackMouseEvent")
)

// ToolbarHitResult represents which part of the toolbar was hit
type ToolbarHitResult int

const (
	HitNone           ToolbarHitResult = iota
	HitGrip                            // Drag handle
	HitModeButton                      // Chinese/English mode button
	HitWidthButton                     // Full/Half width button
	HitPunctButton                     // Punctuation button
	HitSettingsButton                  // Settings button
)

// ToolbarState represents the current state of the toolbar
type ToolbarState struct {
	ChineseMode   bool
	CapsLock      bool
	FullWidth     bool
	ChinesePunct  bool
	EffectiveMode int // 0=Chinese, 1=EnglishLower, 2=EnglishUpper
}

// ToolbarCallback represents callbacks for toolbar actions
type ToolbarCallback struct {
	OnToggleMode      func()
	OnToggleWidth     func()
	OnTogglePunct     func()
	OnOpenSettings    func()
	OnPositionChanged func(x, y int)
	OnContextMenu     func(action ToolbarContextMenuAction)
	OnShowMenu        func(screenX, screenY, flipRefY int) // 请求显示统一菜单 (flipRefY: 下方放不下时翻转到此Y上方, 0=禁用)
}

// ToolbarContextMenuAction represents actions from toolbar context menu
type ToolbarContextMenuAction int

const (
	ToolbarMenuSettings ToolbarContextMenuAction = iota
	ToolbarMenuRestartService
	ToolbarMenuAbout
)

// TRACKMOUSEEVENT for TrackMouseEvent API
type TRACKMOUSEEVENT struct {
	CbSize      uint32
	DwFlags     uint32
	HwndTrack   uintptr
	DwHoverTime uint32
}

// ToolbarWindow represents the toolbar window
type ToolbarWindow struct {
	hwnd     windows.HWND
	logger   *slog.Logger
	renderer *ToolbarRenderer

	mu      sync.Mutex
	visible bool
	x, y    int
	width   int
	height  int
	state   ToolbarState

	// Dragging state
	dragging    bool
	dragStartX  int
	dragStartY  int
	dragOffsetX int
	dragOffsetY int

	// Callbacks
	callback *ToolbarCallback

	// Original window procedure for subclassing
	originalWndProc uintptr

	// Tooltip state
	tooltipHwnd    windows.HWND     // Tooltip window handle
	hoverButton    ToolbarHitResult // Currently hovered button
	tooltipVisible bool             // Is tooltip currently visible
	trackingMouse  bool             // Is mouse tracking enabled

	// Context menu (custom popup that doesn't steal focus)
	popupMenu *PopupMenu
}

// Global toolbar instance for window procedure callback
var globalToolbar *ToolbarWindow

// NewToolbarWindow creates a new toolbar window
func NewToolbarWindow(logger *slog.Logger) *ToolbarWindow {
	return &ToolbarWindow{
		logger:   logger,
		renderer: NewToolbarRenderer(),
		state: ToolbarState{
			ChineseMode:  true,
			FullWidth:    false,
			ChinesePunct: true,
		},
	}
}

// toolbarWndProc is the window procedure for the toolbar
func toolbarWndProc(hwnd uintptr, msg uint32, wParam, lParam uintptr) uintptr {
	if globalToolbar == nil {
		ret, _, _ := procDefWindowProcW.Call(hwnd, uintptr(msg), wParam, lParam)
		return ret
	}

	switch msg {
	case WM_LBUTTONDOWN:
		return globalToolbar.handleMouseDown(hwnd, lParam)
	case WM_LBUTTONUP:
		return globalToolbar.handleMouseUp(hwnd, lParam)
	case WM_RBUTTONUP:
		return globalToolbar.handleRightClick(hwnd, lParam)
	case WM_MOUSEMOVE:
		return globalToolbar.handleMouseMoveWithTooltip(hwnd, lParam)
	case WM_MOUSELEAVE:
		return globalToolbar.handleMouseLeave(hwnd)
	case WM_TIMER:
		return globalToolbar.handleTimer(hwnd, wParam)
	case WM_NCHITTEST:
		// Return HTCLIENT so we receive mouse messages
		return HTCLIENT
	case WM_SETCURSOR:
		// Set appropriate cursor based on position
		return globalToolbar.handleSetCursor(hwnd, lParam)

	case WM_DPICHANGED:
		// wParam: LOWORD = new X DPI, HIWORD = new Y DPI
		newDPI := int(wParam & 0xFFFF)
		if newDPI > 0 {
			SetEffectiveDPI(newDPI)
		}
		// Resize toolbar for new DPI and re-render
		globalToolbar.handleDPIChanged()
		return 0
	}

	ret, _, _ := procDefWindowProcW.Call(hwnd, uintptr(msg), wParam, lParam)
	return ret
}

// Create creates the toolbar window
func (w *ToolbarWindow) Create() error {
	w.logger.Info("Creating toolbar window...")

	className, _ := syscall.UTF16PtrFromString("IMEToolbarWindow")

	wc := WNDCLASSEXW{
		CbSize:        uint32(unsafe.Sizeof(WNDCLASSEXW{})),
		LpfnWndProc:   syscall.NewCallback(toolbarWndProc),
		LpszClassName: className,
	}

	ret, _, err := procRegisterClassExW.Call(uintptr(unsafe.Pointer(&wc)))
	if ret == 0 {
		w.logger.Warn("RegisterClassExW failed (may already exist)", "error", err)
	}

	// Create layered window with WS_EX_NOACTIVATE to prevent focus stealing
	// Mouse events still work because we use SetCapture in handleMouseDown
	exStyle := uint32(WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE)
	style := uint32(WS_POPUP)

	// Initial size - match toolbarBaseWidth/Height in toolbar_renderer.go
	w.width = ScaleIntForDPI(116)
	w.height = ScaleIntForDPI(30)

	hwnd, _, err := procCreateWindowExW.Call(
		uintptr(exStyle),
		uintptr(unsafe.Pointer(className)),
		0,
		uintptr(style),
		uintptr(w.x), uintptr(w.y),
		uintptr(w.width), uintptr(w.height),
		0, 0, 0, 0,
	)

	if hwnd == 0 {
		return fmt.Errorf("CreateWindowExW failed: %w", err)
	}

	w.hwnd = windows.HWND(hwnd)
	globalToolbar = w

	w.logger.Info("Toolbar window created", "hwnd", hwnd)

	// Create tooltip window
	w.createTooltipWindow()

	// Create custom popup menu (doesn't steal focus)
	w.popupMenu = NewPopupMenu()
	if err := w.popupMenu.Create(); err != nil {
		w.logger.Warn("Failed to create toolbar popup menu", "error", err)
	}

	// Render initial content
	w.Render()

	return nil
}

// SetCallback sets the callback functions
func (w *ToolbarWindow) SetCallback(callback *ToolbarCallback) {
	w.mu.Lock()
	w.callback = callback
	w.mu.Unlock()
}

// SetGDIFontParams updates GDI font weight and scale for toolbar text rendering
func (w *ToolbarWindow) SetGDIFontParams(weight int, scale float64) {
	if w.renderer != nil {
		w.renderer.SetGDIFontParams(weight, scale)
	}
}

// SetMenuFontParams updates GDI font weight and scale for toolbar's popup menu
func (w *ToolbarWindow) SetMenuFontParams(weight int, scale float64) {
	if w.popupMenu != nil {
		w.popupMenu.SetGDIFontParams(weight, scale)
	}
}

// SetMenuFontSize sets the base font size for toolbar's popup menu
func (w *ToolbarWindow) SetMenuFontSize(size float64) {
	if w.popupMenu != nil {
		w.popupMenu.SetMenuFontSize(size)
	}
}

// SetFontPath updates the primary font for toolbar text and its popup menu.
func (w *ToolbarWindow) SetFontPath(path string) {
	if w.renderer != nil {
		w.renderer.SetFontPath(path)
	}
	if w.popupMenu != nil {
		w.popupMenu.SetFontPath(path)
	}
}

// SetTextRenderMode switches between GDI and FreeType text rendering
func (w *ToolbarWindow) SetTextRenderMode(mode TextRenderMode) {
	if w.renderer != nil {
		w.renderer.SetTextRenderMode(mode)
	}
	if w.popupMenu != nil {
		w.popupMenu.SetTextRenderMode(mode)
	}
}

func (w *ToolbarWindow) SetTheme(resolved *theme.ResolvedTheme) {
	if w.renderer != nil {
		w.renderer.SetTheme(resolved)
	}
	if w.popupMenu != nil {
		w.popupMenu.SetTheme(resolved)
	}
	// Re-render with new theme
	w.Render()
}

// SetState sets the toolbar state and re-renders
func (w *ToolbarWindow) SetState(state ToolbarState) {
	w.mu.Lock()
	w.state = state
	w.mu.Unlock()
	w.Render()
}

// SetPosition sets the toolbar position
func (w *ToolbarWindow) SetPosition(x, y int) {
	w.mu.Lock()
	w.x = x
	w.y = y
	w.mu.Unlock()

	if w.hwnd != 0 {
		procSetWindowPos.Call(
			uintptr(w.hwnd),
			HWND_TOPMOST,
			uintptr(x), uintptr(y),
			0, 0,
			SWP_NOSIZE|SWP_NOACTIVATE,
		)
	}
}

// GetPosition returns the current toolbar position
func (w *ToolbarWindow) GetPosition() (int, int) {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.x, w.y
}

// Render renders the toolbar content
func (w *ToolbarWindow) Render() {
	if w.hwnd == 0 {
		return
	}

	w.mu.Lock()
	state := w.state
	x, y := w.x, w.y
	w.mu.Unlock()

	img := w.renderer.Render(state)
	w.updateLayeredWindow(img, x, y)
}

func (w *ToolbarWindow) updateLayeredWindow(img *image.RGBA, x, y int) error {
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
			BiHeight:      -int32(height),
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

	procSelectObject.Call(hdcMem, hBitmap)

	// Copy image data (RGBA to BGRA channel swap).
	// image.RGBA is already premultiplied alpha, matching UpdateLayeredWindow's expectation.
	pixelCount := width * height
	dstSlice := unsafe.Slice((*byte)(bits), pixelCount*4)

	for i := 0; i < pixelCount; i++ {
		srcIdx := i * 4
		dstIdx := i * 4

		dstSlice[dstIdx+0] = img.Pix[srcIdx+2] // B
		dstSlice[dstIdx+1] = img.Pix[srcIdx+1] // G
		dstSlice[dstIdx+2] = img.Pix[srcIdx+0] // R
		dstSlice[dstIdx+3] = img.Pix[srcIdx+3] // A
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

// Show shows the toolbar window
func (w *ToolbarWindow) Show() {
	if w.hwnd == 0 {
		w.logger.Warn("ToolbarWindow.Show: hwnd is 0")
		return
	}

	w.mu.Lock()
	wasVisible := w.visible
	x, y := w.x, w.y
	w.visible = true
	w.mu.Unlock()

	procShowWindow.Call(uintptr(w.hwnd), SW_SHOW)
	w.logger.Debug("ToolbarWindow.Show", "wasVisible", wasVisible, "x", x, "y", y, "hwnd", w.hwnd)
}

// Hide hides the toolbar window
func (w *ToolbarWindow) Hide() {
	if w.hwnd == 0 {
		w.logger.Warn("ToolbarWindow.Hide: hwnd is 0")
		return
	}

	// Hide context menu first if open
	if w.popupMenu != nil {
		w.popupMenu.Hide()
	}

	w.mu.Lock()
	wasVisible := w.visible
	w.visible = false
	w.mu.Unlock()

	procShowWindow.Call(uintptr(w.hwnd), SW_HIDE)
	w.logger.Debug("ToolbarWindow.Hide", "wasVisible", wasVisible, "hwnd", w.hwnd)
}

// IsVisible returns whether the toolbar is visible
func (w *ToolbarWindow) IsVisible() bool {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.visible
}

// Destroy destroys the toolbar window
func (w *ToolbarWindow) Destroy() {
	// Destroy popup menu first
	if w.popupMenu != nil {
		w.popupMenu.Destroy()
		w.popupMenu = nil
	}
	// Destroy tooltip window
	if w.tooltipHwnd != 0 {
		procDestroyWindow.Call(uintptr(w.tooltipHwnd))
		w.tooltipHwnd = 0
	}
	if w.hwnd != 0 {
		procDestroyWindow.Call(uintptr(w.hwnd))
		w.hwnd = 0
	}
	if globalToolbar == w {
		globalToolbar = nil
	}
	if w.renderer != nil {
		w.renderer.Close()
		w.renderer = nil
	}
}

// handleDPIChanged handles a DPI change: resizes the toolbar and re-renders.
func (w *ToolbarWindow) handleDPIChanged() {
	// Recalculate toolbar size with new DPI
	w.mu.Lock()
	w.width = ScaleIntForDPI(116)
	w.height = ScaleIntForDPI(30)
	w.mu.Unlock()

	// Re-render with the new DPI scale
	w.Render()
}

// HideMenu hides the toolbar context menu if visible
func (w *ToolbarWindow) HideMenu() {
	if w.popupMenu != nil {
		w.popupMenu.Hide()
	}
}

// IsMenuOpen returns true if the toolbar context menu is currently visible
func (w *ToolbarWindow) IsMenuOpen() bool {
	if w.popupMenu != nil {
		return w.popupMenu.IsVisible()
	}
	return false
}

// MenuContainsPoint checks if the given screen coordinates are inside the menu
func (w *ToolbarWindow) MenuContainsPoint(screenX, screenY int) bool {
	if w.popupMenu != nil && w.popupMenu.IsVisible() {
		return w.popupMenu.ContainsPoint(screenX, screenY)
	}
	return false
}
