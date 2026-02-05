// Package ui provides native Windows UI for candidate window
package ui

import (
	"image"
	"image/color"
	"sync"
	"syscall"
	"unsafe"

	"github.com/fogleman/gg"
	"github.com/huanfeng/wind_input/pkg/theme"
	"golang.org/x/sys/windows"
)

// MenuItem represents a menu item
type MenuItem struct {
	ID        int
	Text      string
	Disabled  bool
	Separator bool
}

// PopupMenuCallback is called when a menu item is selected
type PopupMenuCallback func(id int)

// PopupMenu is a custom-drawn popup menu that doesn't steal focus
type PopupMenu struct {
	hwnd     windows.HWND
	visible  bool
	items    []MenuItem
	callback PopupMenuCallback

	// Rendering
	x, y       int
	width      int
	height     int
	hoverIndex int // -1 for none

	// Theme
	resolvedTheme *theme.ResolvedTheme

	mu sync.Mutex
}

// Menu dimensions (will be scaled for DPI)
const (
	menuItemHeight      = 24
	menuSeparatorHeight = 9
	menuPaddingX        = 24
	menuPaddingY        = 4
	menuMinWidth        = 120
	menuFontSize        = 12.0
	menuCornerRadius    = 6 // Corner radius for rounded rectangle

	// Windows message for popup menu
	WM_CAPTURECHANGED = 0x0215

	// Timer for checking mouse state (for click-outside detection)
	MENU_CHECK_TIMER_ID = 100
	MENU_CHECK_INTERVAL = 50 // ms
)

var (
	procGetAsyncKeyState = user32.NewProc("GetAsyncKeyState")
)

// VK_LBUTTON is the virtual key code for left mouse button
const VK_LBUTTON = 0x01

// Global popup menu registry
var (
	popupMenusMu sync.RWMutex
	popupMenus   = make(map[windows.HWND]*PopupMenu)
)

func registerPopupMenu(hwnd windows.HWND, m *PopupMenu) {
	popupMenusMu.Lock()
	popupMenus[hwnd] = m
	popupMenusMu.Unlock()
}

func unregisterPopupMenu(hwnd windows.HWND) {
	popupMenusMu.Lock()
	delete(popupMenus, hwnd)
	popupMenusMu.Unlock()
}

func getPopupMenu(hwnd windows.HWND) *PopupMenu {
	popupMenusMu.RLock()
	m := popupMenus[hwnd]
	popupMenusMu.RUnlock()
	return m
}

// popupMenuWndProc is the window procedure for popup menu
func popupMenuWndProc(hwnd uintptr, msg uint32, wParam, lParam uintptr) uintptr {
	switch msg {
	case WM_DESTROY:
		unregisterPopupMenu(windows.HWND(hwnd))
		return 0

	case WM_MOUSEMOVE:
		m := getPopupMenu(windows.HWND(hwnd))
		if m != nil {
			m.handleMouseMove(lParam)
		}
		return 0

	case WM_LBUTTONDOWN:
		m := getPopupMenu(windows.HWND(hwnd))
		if m != nil {
			m.handleClick(lParam)
		}
		return 0

	case WM_RBUTTONDOWN:
		// Right-click also closes the menu if outside
		m := getPopupMenu(windows.HWND(hwnd))
		if m != nil {
			m.handleClick(lParam)
		}
		return 0

	case WM_MOUSELEAVE:
		m := getPopupMenu(windows.HWND(hwnd))
		if m != nil {
			m.handleMouseLeave()
		}
		return 0

	case WM_SETCURSOR:
		cursor, _, _ := procLoadCursorW.Call(0, IDC_ARROW)
		if cursor != 0 {
			procSetCursor.Call(cursor)
		}
		return 1

	case WM_CAPTURECHANGED:
		// Capture was taken away from us - hide the menu
		m := getPopupMenu(windows.HWND(hwnd))
		if m != nil && m.IsVisible() {
			m.Hide()
		}
		return 0

	case WM_TIMER:
		if wParam == MENU_CHECK_TIMER_ID {
			m := getPopupMenu(windows.HWND(hwnd))
			if m != nil {
				m.checkMouseState()
			}
		}
		return 0
	}
	ret, _, _ := procDefWindowProcW.Call(hwnd, uintptr(msg), wParam, lParam)
	return ret
}

// NewPopupMenu creates a new popup menu
func NewPopupMenu() *PopupMenu {
	return &PopupMenu{
		hoverIndex: -1,
	}
}

// SetTheme sets the theme for the popup menu
func (m *PopupMenu) SetTheme(resolved *theme.ResolvedTheme) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.resolvedTheme = resolved
}

// getPopupMenuColors returns popup menu colors from theme or defaults
func (m *PopupMenu) getPopupMenuColors() *theme.ResolvedPopupMenuColors {
	if m.resolvedTheme != nil {
		return &m.resolvedTheme.PopupMenu
	}
	// Return default colors
	return &theme.ResolvedPopupMenuColors{
		BackgroundColor: color.RGBA{255, 255, 255, 255},
		BorderColor:     color.RGBA{199, 199, 199, 255},
		TextColor:       color.RGBA{0, 0, 0, 255},
		DisabledColor:   color.RGBA{161, 161, 161, 255},
		HoverBgColor:    color.RGBA{0, 120, 212, 255},
		HoverTextColor:  color.RGBA{255, 255, 255, 255},
		SeparatorColor:  color.RGBA{219, 219, 219, 255},
	}
}

// Create creates the popup menu window
func (m *PopupMenu) Create() error {
	className, _ := syscall.UTF16PtrFromString("IMEPopupMenu")

	wc := WNDCLASSEXW{
		CbSize:        uint32(unsafe.Sizeof(WNDCLASSEXW{})),
		LpfnWndProc:   syscall.NewCallback(popupMenuWndProc),
		LpszClassName: className,
	}

	ret, _, err := procRegisterClassExW.Call(uintptr(unsafe.Pointer(&wc)))
	if ret == 0 {
		// Class might already be registered
		_ = err
	}

	exStyle := uint32(WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE)
	style := uint32(WS_POPUP)

	hwnd, _, err := procCreateWindowExW.Call(
		uintptr(exStyle),
		uintptr(unsafe.Pointer(className)),
		0,
		uintptr(style),
		0, 0, 1, 1,
		0, 0, 0, 0,
	)

	if hwnd == 0 {
		return err
	}

	m.hwnd = windows.HWND(hwnd)
	registerPopupMenu(m.hwnd, m)

	return nil
}

// Show displays the popup menu at the specified position
func (m *PopupMenu) Show(items []MenuItem, x, y int, callback PopupMenuCallback) {
	m.mu.Lock()
	m.items = items
	m.callback = callback
	m.hoverIndex = -1
	m.mu.Unlock()

	// Calculate menu size
	m.calculateSize()

	// Adjust position to stay within screen bounds
	workLeft, workTop, workRight, workBottom := GetMonitorWorkAreaFromPoint(x, y)
	if x+m.width > workRight {
		x = workRight - m.width
	}
	if x < workLeft {
		x = workLeft
	}
	if y+m.height > workBottom {
		y = workBottom - m.height
	}
	if y < workTop {
		y = workTop
	}

	m.mu.Lock()
	m.x = x
	m.y = y
	m.mu.Unlock()

	// Render and show
	m.updateWindow()

	procShowWindow.Call(uintptr(m.hwnd), SW_SHOW)

	m.mu.Lock()
	m.visible = true
	m.mu.Unlock()

	// Capture mouse to detect clicks outside the menu
	procSetCapture.Call(uintptr(m.hwnd))

	// Start timer to check mouse state (backup for cross-process click detection)
	procSetTimer.Call(uintptr(m.hwnd), MENU_CHECK_TIMER_ID, MENU_CHECK_INTERVAL, 0)

	// Start tracking mouse leave
	m.trackMouseLeave()
}

// Hide hides the popup menu
func (m *PopupMenu) Hide() {
	m.mu.Lock()
	wasVisible := m.visible
	m.visible = false
	m.mu.Unlock()

	if wasVisible {
		// Stop the mouse check timer
		procKillTimer.Call(uintptr(m.hwnd), MENU_CHECK_TIMER_ID)
		// Release mouse capture
		procReleaseCapture.Call()
		procShowWindow.Call(uintptr(m.hwnd), SW_HIDE)
	}
}

// IsVisible returns whether the menu is visible
func (m *PopupMenu) IsVisible() bool {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.visible
}

// Destroy destroys the popup menu window
func (m *PopupMenu) Destroy() {
	if m.hwnd != 0 {
		procDestroyWindow.Call(uintptr(m.hwnd))
		m.hwnd = 0
	}
}

// calculateSize calculates the menu dimensions
func (m *PopupMenu) calculateSize() {
	scale := GetDPIScale()

	m.mu.Lock()
	defer m.mu.Unlock()

	m.width = int(float64(menuMinWidth) * scale)
	m.height = int(float64(menuPaddingY*2) * scale)

	// Create a temporary context to measure text
	dc := gg.NewContext(1, 1)
	fontSize := menuFontSize * scale
	m.loadFont(dc, fontSize)

	for _, item := range m.items {
		if item.Separator {
			m.height += int(float64(menuSeparatorHeight) * scale)
		} else {
			m.height += int(float64(menuItemHeight) * scale)
			// Calculate text width
			tw, _ := dc.MeasureString(item.Text)
			itemWidth := int(tw + float64(menuPaddingX*2)*scale)
			if itemWidth > m.width {
				m.width = itemWidth
			}
		}
	}
}

// loadFont loads font for the context
func (m *PopupMenu) loadFont(dc *gg.Context, fontSize float64) {
	fonts := []string{
		"C:/Windows/Fonts/msyh.ttc",
		"C:/Windows/Fonts/simhei.ttf",
		"C:/Windows/Fonts/simsun.ttc",
		"C:/Windows/Fonts/segoeui.ttf",
	}
	for _, path := range fonts {
		if err := dc.LoadFontFace(path, fontSize); err == nil {
			return
		}
	}
}

// render renders the menu to an image
func (m *PopupMenu) render() *image.RGBA {
	m.mu.Lock()
	items := m.items
	hoverIdx := m.hoverIndex
	width := m.width
	height := m.height
	colors := m.getPopupMenuColors()
	m.mu.Unlock()

	scale := GetDPIScale()
	fontSize := menuFontSize * scale
	itemH := int(float64(menuItemHeight) * scale)
	sepH := int(float64(menuSeparatorHeight) * scale)
	padX := float64(menuPaddingX) * scale
	padY := int(float64(menuPaddingY) * scale)

	dc := gg.NewContext(width, height)
	m.loadFont(dc, fontSize)

	// Calculate corner radius with DPI scaling
	radius := float64(menuCornerRadius) * scale

	// Fill background with rounded rectangle
	dc.SetRGBA(1, 1, 1, 0) // Transparent background first
	dc.Clear()

	// Draw filled rounded rectangle for background
	dc.SetColor(colors.BackgroundColor)
	dc.DrawRoundedRectangle(0.5, 0.5, float64(width)-1, float64(height)-1, radius)
	dc.Fill()

	// Set clip to rounded rectangle so hover backgrounds don't overflow
	dc.DrawRoundedRectangle(1, 1, float64(width)-2, float64(height)-2, radius-1)
	dc.Clip()

	// Draw items
	y := padY
	for i, item := range items {
		if item.Separator {
			// Draw separator line
			sepY := float64(y + sepH/2)
			dc.SetColor(colors.SeparatorColor)
			dc.DrawLine(4*scale, sepY, float64(width)-4*scale, sepY)
			dc.Stroke()
			y += sepH
		} else {
			// Draw item background if hovered
			if i == hoverIdx && !item.Disabled {
				dc.SetColor(colors.HoverBgColor)
				dc.DrawRectangle(1, float64(y), float64(width-2), float64(itemH))
				dc.Fill()
			}

			// Draw text
			if item.Disabled {
				dc.SetColor(colors.DisabledColor)
			} else if i == hoverIdx {
				dc.SetColor(colors.HoverTextColor)
			} else {
				dc.SetColor(colors.TextColor)
			}

			textY := float64(y) + float64(itemH)/2 + fontSize/3
			dc.DrawString(item.Text, padX, textY)

			y += itemH
		}
	}

	// Reset clip and draw border
	dc.ResetClip()
	dc.SetColor(colors.BorderColor)
	dc.DrawRoundedRectangle(0.5, 0.5, float64(width)-1, float64(height)-1, radius)
	dc.Stroke()

	return dc.Image().(*image.RGBA)
}

// updateWindow updates the layered window with the rendered image
func (m *PopupMenu) updateWindow() {
	img := m.render()

	m.mu.Lock()
	x, y := m.x, m.y
	width, height := m.width, m.height
	m.mu.Unlock()

	hdcScreen, _, _ := procGetDC.Call(0)
	if hdcScreen == 0 {
		return
	}
	defer procReleaseDC.Call(0, hdcScreen)

	hdcMem, _, _ := procCreateCompatibleDC.Call(hdcScreen)
	if hdcMem == 0 {
		return
	}
	defer procDeleteDC.Call(hdcMem)

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
	hBitmap, _, _ := procCreateDIBSection.Call(
		hdcMem,
		uintptr(unsafe.Pointer(&bi)),
		DIB_RGB_COLORS,
		uintptr(unsafe.Pointer(&bits)),
		0, 0,
	)
	if hBitmap == 0 {
		return
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

	ptSrc := POINT{X: 0, Y: 0}
	ptDst := POINT{X: int32(x), Y: int32(y)}
	size := SIZE{Cx: int32(width), Cy: int32(height)}
	blend := BLENDFUNCTION{
		BlendOp:             AC_SRC_OVER,
		BlendFlags:          0,
		SourceConstantAlpha: 255,
		AlphaFormat:         AC_SRC_ALPHA,
	}

	procUpdateLayeredWindow.Call(
		uintptr(m.hwnd),
		hdcScreen,
		uintptr(unsafe.Pointer(&ptDst)),
		uintptr(unsafe.Pointer(&size)),
		hdcMem,
		uintptr(unsafe.Pointer(&ptSrc)),
		0,
		uintptr(unsafe.Pointer(&blend)),
		ULW_ALPHA,
	)
}

// trackMouseLeave enables mouse leave tracking
func (m *PopupMenu) trackMouseLeave() {
	tme := TRACKMOUSEEVENT{
		CbSize:    uint32(unsafe.Sizeof(TRACKMOUSEEVENT{})),
		DwFlags:   TME_LEAVE,
		HwndTrack: uintptr(m.hwnd),
	}
	procTrackMouseEvent.Call(uintptr(unsafe.Pointer(&tme)))
}

// handleMouseMove handles mouse move events
func (m *PopupMenu) handleMouseMove(lParam uintptr) {
	mouseX := int(int16(lParam & 0xFFFF))
	mouseY := int(int16((lParam >> 16) & 0xFFFF))

	m.mu.Lock()
	menuWidth := m.width
	menuHeight := m.height
	oldHover := m.hoverIndex

	// Only show hover if mouse is actually inside the menu bounds
	// (SetCapture can send mouse events even when cursor is outside)
	if mouseX >= 0 && mouseX < menuWidth && mouseY >= 0 && mouseY < menuHeight {
		m.hoverIndex = m.hitTest(mouseY)
	} else {
		m.hoverIndex = -1
	}

	if m.hoverIndex != oldHover {
		m.mu.Unlock()
		// Re-render with new hover state
		m.updateWindow()
		m.trackMouseLeave()
	} else {
		m.mu.Unlock()
	}
}

// handleMouseLeave handles mouse leave events
func (m *PopupMenu) handleMouseLeave() {
	m.mu.Lock()
	if m.hoverIndex != -1 {
		m.hoverIndex = -1
		m.mu.Unlock()
		m.updateWindow()
	} else {
		m.mu.Unlock()
	}
}

// handleClick handles mouse click events
func (m *PopupMenu) handleClick(lParam uintptr) {
	// Extract mouse position (can be outside window when using SetCapture)
	mouseX := int(int16(lParam & 0xFFFF))
	mouseY := int(int16((lParam >> 16) & 0xFFFF))

	m.mu.Lock()
	menuWidth := m.width
	menuHeight := m.height
	m.mu.Unlock()

	// Check if click is outside the menu bounds
	if mouseX < 0 || mouseX >= menuWidth || mouseY < 0 || mouseY >= menuHeight {
		// Click outside menu - just hide it (don't trigger any callback)
		m.Hide()
		return
	}

	m.mu.Lock()
	index := m.hitTest(mouseY)

	if index >= 0 && index < len(m.items) {
		item := m.items[index]
		if !item.Disabled && !item.Separator {
			callback := m.callback
			id := item.ID
			m.mu.Unlock()

			// Hide menu first
			m.Hide()

			// Then call callback
			if callback != nil {
				callback(id)
			}
			return
		}
	}
	m.mu.Unlock()
}

// hitTest returns the item index at the given Y position
func (m *PopupMenu) hitTest(mouseY int) int {
	scale := GetDPIScale()
	itemH := int(float64(menuItemHeight) * scale)
	sepH := int(float64(menuSeparatorHeight) * scale)
	padY := int(float64(menuPaddingY) * scale)

	y := padY
	for i, item := range m.items {
		var h int
		if item.Separator {
			h = sepH
		} else {
			h = itemH
		}

		if mouseY >= y && mouseY < y+h {
			if item.Separator {
				return -1
			}
			return i
		}
		y += h
	}
	return -1
}

// ContainsPoint checks if the given screen coordinates are within the menu
func (m *PopupMenu) ContainsPoint(screenX, screenY int) bool {
	m.mu.Lock()
	defer m.mu.Unlock()

	if !m.visible {
		return false
	}

	return screenX >= m.x && screenX < m.x+m.width &&
		screenY >= m.y && screenY < m.y+m.height
}

// GetBounds returns the menu bounds (x, y, width, height)
func (m *PopupMenu) GetBounds() (int, int, int, int) {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.x, m.y, m.width, m.height
}

// checkMouseState checks if mouse button is pressed outside the menu
// This is a backup mechanism for cross-process click detection where SetCapture doesn't work
func (m *PopupMenu) checkMouseState() {
	if !m.IsVisible() {
		return
	}

	// Check if left mouse button is pressed
	state, _, _ := procGetAsyncKeyState.Call(VK_LBUTTON)
	// GetAsyncKeyState returns: high-order bit set if key is down
	if state&0x8000 == 0 {
		return // Mouse button not pressed
	}

	// Get current cursor position (screen coordinates)
	var pt struct{ X, Y int32 }
	procGetCursorPos.Call(uintptr(unsafe.Pointer(&pt)))

	// Check if cursor is outside the menu
	m.mu.Lock()
	menuX, menuY := m.x, m.y
	menuW, menuH := m.width, m.height
	m.mu.Unlock()

	if int(pt.X) < menuX || int(pt.X) >= menuX+menuW ||
		int(pt.Y) < menuY || int(pt.Y) >= menuY+menuH {
		// Mouse pressed outside menu - close it
		m.Hide()
	}
}
