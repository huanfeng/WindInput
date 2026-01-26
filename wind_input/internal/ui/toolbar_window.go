// Package ui provides native Windows UI for input method
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

// Additional Windows constants for toolbar
const (
	WM_LBUTTONDOWN = 0x0201
	WM_LBUTTONUP   = 0x0202
	WM_MOUSEMOVE   = 0x0200
	WM_RBUTTONUP   = 0x0205
	// WM_SETCURSOR defined in window.go

	HTCLIENT = 1

	GWL_WNDPROC = -4

	// Cursor IDs
	IDC_ARROW    = 32512
	IDC_SIZEALL  = 32646
	IDC_HAND     = 32649
)

var (
	procSetWindowLongPtrW = user32.NewProc("SetWindowLongPtrW")
	procCallWindowProcW   = user32.NewProc("CallWindowProcW")
	procGetCursorPos      = user32.NewProc("GetCursorPos")
	procScreenToClient    = user32.NewProc("ScreenToClient")
	procSetCapture        = user32.NewProc("SetCapture")
	procReleaseCapture    = user32.NewProc("ReleaseCapture")
	procLoadCursorW       = user32.NewProc("LoadCursorW")
	procSetCursor         = user32.NewProc("SetCursor")
)

// ToolbarHitResult represents which part of the toolbar was hit
type ToolbarHitResult int

const (
	HitNone ToolbarHitResult = iota
	HitGrip                  // Drag handle
	HitModeButton            // Chinese/English mode button
	HitWidthButton           // Full/Half width button
	HitPunctButton           // Punctuation button
	HitSettingsButton        // Settings button
)

// ToolbarState represents the current state of the toolbar
type ToolbarState struct {
	ChineseMode bool
	CapsLock    bool
	FullWidth   bool
	ChinesePunct bool
}

// ToolbarCallback represents callbacks for toolbar actions
type ToolbarCallback struct {
	OnToggleMode     func()
	OnToggleWidth    func()
	OnTogglePunct    func()
	OnOpenSettings   func()
	OnPositionChanged func(x, y int)
}

// ToolbarWindow represents the toolbar window
type ToolbarWindow struct {
	hwnd     windows.HWND
	logger   *slog.Logger
	renderer *ToolbarRenderer

	mu       sync.Mutex
	visible  bool
	x, y     int
	width    int
	height   int
	state    ToolbarState

	// Dragging state
	dragging   bool
	dragStartX int
	dragStartY int
	dragOffsetX int
	dragOffsetY int

	// Callbacks
	callback *ToolbarCallback

	// Original window procedure for subclassing
	originalWndProc uintptr
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
	case WM_MOUSEMOVE:
		return globalToolbar.handleMouseMove(hwnd, lParam)
	case WM_NCHITTEST:
		// Return HTCLIENT so we receive mouse messages
		return HTCLIENT
	case WM_SETCURSOR:
		// Set appropriate cursor based on position
		return globalToolbar.handleSetCursor(hwnd, lParam)
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

	// Initial size
	w.width = ScaleIntForDPI(140)
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

	// Render initial content
	w.Render()

	return nil
}

// handleMouseDown handles WM_LBUTTONDOWN
func (w *ToolbarWindow) handleMouseDown(hwnd uintptr, lParam uintptr) uintptr {
	x := int(int16(lParam & 0xFFFF))
	y := int(int16((lParam >> 16) & 0xFFFF))

	hit := w.renderer.HitTest(x, y, w.width, w.height)

	w.logger.Debug("Toolbar mouse down", "x", x, "y", y, "hit", hit)

	w.mu.Lock()
	w.dragStartX = x
	w.dragStartY = y

	if hit == HitGrip {
		// Start dragging immediately for grip
		w.dragging = true
		w.dragOffsetX = x
		w.dragOffsetY = y
		w.mu.Unlock()

		// Capture mouse
		procSetCapture.Call(hwnd)
	} else {
		// For buttons, we track the start position but don't start dragging yet
		w.dragging = false
		w.mu.Unlock()

		// Capture mouse to ensure we get the mouse up event
		procSetCapture.Call(hwnd)
	}

	return 0
}

// handleMouseUp handles WM_LBUTTONUP
func (w *ToolbarWindow) handleMouseUp(hwnd uintptr, lParam uintptr) uintptr {
	x := int(int16(lParam & 0xFFFF))
	y := int(int16((lParam >> 16) & 0xFFFF))

	w.mu.Lock()
	wasDragging := w.dragging
	startX := w.dragStartX
	startY := w.dragStartY
	w.dragging = false
	w.mu.Unlock()

	// Release capture
	procReleaseCapture.Call()

	if wasDragging {
		// Save position
		if w.callback != nil && w.callback.OnPositionChanged != nil {
			w.callback.OnPositionChanged(w.x, w.y)
		}
		return 0
	}

	// Handle button click - only if released in the same area as pressed
	hitUp := w.renderer.HitTest(x, y, w.width, w.height)
	hitDown := w.renderer.HitTest(startX, startY, w.width, w.height)

	w.logger.Debug("Toolbar mouse up", "x", x, "y", y, "hitUp", hitUp, "hitDown", hitDown)

	// Only trigger click if pressed and released on the same button
	if hitUp == hitDown && w.callback != nil {
		switch hitUp {
		case HitModeButton:
			w.logger.Info("Mode button clicked")
			if w.callback.OnToggleMode != nil {
				w.callback.OnToggleMode()
			}
		case HitWidthButton:
			w.logger.Info("Width button clicked")
			if w.callback.OnToggleWidth != nil {
				w.callback.OnToggleWidth()
			}
		case HitPunctButton:
			w.logger.Info("Punct button clicked")
			if w.callback.OnTogglePunct != nil {
				w.callback.OnTogglePunct()
			}
		case HitSettingsButton:
			w.logger.Info("Settings button clicked")
			if w.callback.OnOpenSettings != nil {
				w.callback.OnOpenSettings()
			}
		}
	}

	return 0
}

// handleMouseMove handles WM_MOUSEMOVE
func (w *ToolbarWindow) handleMouseMove(hwnd uintptr, lParam uintptr) uintptr {
	w.mu.Lock()
	if !w.dragging {
		w.mu.Unlock()
		return 0
	}

	// Get current cursor position in screen coordinates
	var pt POINT
	procGetCursorPos.Call(uintptr(unsafe.Pointer(&pt)))

	// Calculate new window position
	newX := int(pt.X) - w.dragOffsetX
	newY := int(pt.Y) - w.dragOffsetY

	w.x = newX
	w.y = newY
	w.mu.Unlock()

	// Move the window
	procSetWindowPos.Call(
		uintptr(w.hwnd),
		HWND_TOPMOST,
		uintptr(newX), uintptr(newY),
		0, 0,
		SWP_NOSIZE|SWP_NOACTIVATE,
	)

	return 0
}

// handleSetCursor handles WM_SETCURSOR - sets appropriate cursor based on mouse position
func (w *ToolbarWindow) handleSetCursor(hwnd uintptr, lParam uintptr) uintptr {
	// Get current cursor position
	var pt POINT
	procGetCursorPos.Call(uintptr(unsafe.Pointer(&pt)))

	// Convert to client coordinates
	procScreenToClient.Call(hwnd, uintptr(unsafe.Pointer(&pt)))

	x := int(pt.X)
	y := int(pt.Y)

	// Determine which cursor to show based on hit test
	hit := w.renderer.HitTest(x, y, w.width, w.height)

	var cursorID uintptr
	switch hit {
	case HitGrip:
		// Grip area - show move cursor
		cursorID = IDC_SIZEALL
	case HitModeButton, HitWidthButton, HitPunctButton, HitSettingsButton:
		// Button area - show hand cursor
		cursorID = IDC_HAND
	default:
		// Default - show arrow cursor
		cursorID = IDC_ARROW
	}

	// Load and set cursor
	cursor, _, _ := procLoadCursorW.Call(0, cursorID)
	if cursor != 0 {
		procSetCursor.Call(cursor)
	}

	return 1 // Return TRUE to indicate we handled the message
}

// SetCallback sets the callback functions
func (w *ToolbarWindow) SetCallback(callback *ToolbarCallback) {
	w.mu.Lock()
	w.callback = callback
	w.mu.Unlock()
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

	// Copy image data (RGBA to BGRA with premultiplied alpha)
	pixelCount := width * height
	dstSlice := unsafe.Slice((*byte)(bits), pixelCount*4)

	for i := 0; i < pixelCount; i++ {
		srcIdx := i * 4
		dstIdx := i * 4

		r := img.Pix[srcIdx+0]
		g := img.Pix[srcIdx+1]
		b := img.Pix[srcIdx+2]
		a := img.Pix[srcIdx+3]

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
	if w.hwnd != 0 {
		procDestroyWindow.Call(uintptr(w.hwnd))
		w.hwnd = 0
	}
	if globalToolbar == w {
		globalToolbar = nil
	}
}
