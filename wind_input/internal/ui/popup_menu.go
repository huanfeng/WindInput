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
	Checked   bool       // 勾选状态（显示 ✓）
	Children  []MenuItem // 子菜单项（非空时显示 ▸，hover展开）
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

	// Submenu support
	submenu      *PopupMenu // 当前展开的子菜单实例
	submenuIndex int        // 展开子菜单对应的父菜单项索引(-1=无)
	parentMenu   *PopupMenu // 父菜单引用
	hasChecked   bool       // items中是否有Checked项
	hasChildren  bool       // items中是否有Children项

	// Flip support: when menu can't fit below Y, flip above flipRefY
	flipRefY int // 翻转参考Y（0=禁用）

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
	menuCheckMarkWidth  = 18
	menuArrowWidth      = 14

	// Windows message for popup menu
	WM_CAPTURECHANGED = 0x0215

	// Timer for checking mouse state (for click-outside detection)
	MENU_CHECK_TIMER_ID = 100
	MENU_CHECK_INTERVAL = 50 // ms

	// Timer for submenu expand delay
	SUBMENU_TIMER_ID = 101
	SUBMENU_DELAY_MS = 250 // ms
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
			// Don't hide if capture was taken by our submenu
			m.mu.Lock()
			sub := m.submenu
			m.mu.Unlock()
			if sub != nil && sub.hwnd != 0 && windows.HWND(wParam) == sub.hwnd {
				return 0
			}
			m.Hide()
		}
		return 0

	case WM_TIMER:
		m := getPopupMenu(windows.HWND(hwnd))
		if m != nil {
			switch wParam {
			case MENU_CHECK_TIMER_ID:
				m.checkMouseState()
			case SUBMENU_TIMER_ID:
				procKillTimer.Call(hwnd, SUBMENU_TIMER_ID)
				m.mu.Lock()
				idx := m.hoverIndex
				m.mu.Unlock()
				if idx >= 0 {
					m.showSubmenu(idx)
				}
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
		hoverIndex:   -1,
		submenuIndex: -1,
	}
}

// SetTheme sets the theme for the popup menu
func (m *PopupMenu) SetTheme(resolved *theme.ResolvedTheme) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.resolvedTheme = resolved
}

// SetFlipRefY sets the Y coordinate to flip above when there's not enough space below.
// Set to 0 to disable flip behavior.
func (m *PopupMenu) SetFlipRefY(y int) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.flipRefY = y
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
	m.submenuIndex = -1
	// Scan items for checked/children flags
	m.hasChecked = false
	m.hasChildren = false
	for _, item := range items {
		if item.Checked {
			m.hasChecked = true
		}
		if len(item.Children) > 0 {
			m.hasChildren = true
		}
	}
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
	// Vertical: prefer below, flip above flipRefY if not enough space
	m.mu.Lock()
	flipY := m.flipRefY
	m.flipRefY = 0 // 使用后重置
	m.mu.Unlock()
	if y+m.height > workBottom {
		if flipY > 0 {
			aboveY := flipY - m.height
			if aboveY >= workTop {
				y = aboveY
			} else {
				y = workBottom - m.height
			}
		} else {
			y = workBottom - m.height
		}
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
	isChild := m.parentMenu != nil
	m.mu.Unlock()

	// Only root menu captures mouse and starts timer
	if !isChild {
		// Capture mouse to detect clicks outside the menu
		procSetCapture.Call(uintptr(m.hwnd))

		// Start timer to check mouse state (backup for cross-process click detection)
		procSetTimer.Call(uintptr(m.hwnd), MENU_CHECK_TIMER_ID, MENU_CHECK_INTERVAL, 0)
	}

	// Start tracking mouse leave
	m.trackMouseLeave()
}

// Hide hides the popup menu
func (m *PopupMenu) Hide() {
	// Hide submenu first
	m.hideSubmenu()

	m.mu.Lock()
	wasVisible := m.visible
	m.visible = false
	isChild := m.parentMenu != nil
	m.mu.Unlock()

	if wasVisible {
		// Stop timers
		procKillTimer.Call(uintptr(m.hwnd), SUBMENU_TIMER_ID)
		if !isChild {
			// Only root menu releases capture and stops check timer
			procKillTimer.Call(uintptr(m.hwnd), MENU_CHECK_TIMER_ID)
			procReleaseCapture.Call()
		}
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
	m.hideSubmenu()
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

	extraLeft := 0.0
	if m.hasChecked {
		extraLeft = float64(menuCheckMarkWidth) * scale
	}
	extraRight := 0.0
	if m.hasChildren {
		extraRight = float64(menuArrowWidth) * scale
	}

	m.width = int(float64(menuMinWidth)*scale + extraLeft + extraRight)
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
			itemWidth := int(tw + float64(menuPaddingX)*scale + extraLeft + extraRight + float64(menuPaddingX)*scale)
			if itemWidth > m.width {
				m.width = itemWidth
			}
		}
	}
}

// loadFont loads font for the context (Chinese text)
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

// loadSymbolFont loads a symbol-capable font for rendering ✓ ▸ etc.
func (m *PopupMenu) loadSymbolFont(dc *gg.Context, fontSize float64) {
	fonts := []string{
		"C:/Windows/Fonts/seguisym.ttf", // Segoe UI Symbol (Win7+, best coverage)
		"C:/Windows/Fonts/segmdl2.ttf",  // Segoe MDL2 Assets (Win10+)
		"C:/Windows/Fonts/segoeui.ttf",  // Segoe UI
		"C:/Windows/Fonts/arial.ttf",    // Arial
		"C:/Windows/Fonts/msyh.ttc",     // Microsoft YaHei (fallback)
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
	submenuIdx := m.submenuIndex
	width := m.width
	height := m.height
	hasChecked := m.hasChecked
	hasChildren := m.hasChildren
	colors := m.getPopupMenuColors()
	m.mu.Unlock()

	scale := GetDPIScale()
	fontSize := menuFontSize * scale
	itemH := int(float64(menuItemHeight) * scale)
	sepH := int(float64(menuSeparatorHeight) * scale)
	padX := float64(menuPaddingX) * scale
	padY := int(float64(menuPaddingY) * scale)
	checkW := 0.0
	if hasChecked {
		checkW = float64(menuCheckMarkWidth) * scale
	}
	arrowW := 0.0
	if hasChildren {
		arrowW = float64(menuArrowWidth) * scale
	}

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
			// Determine if this item should be highlighted
			isHovered := (i == hoverIdx && !item.Disabled) || (i == submenuIdx)

			// Draw item background if hovered or submenu is open for this item
			if isHovered {
				dc.SetColor(colors.HoverBgColor)
				dc.DrawRectangle(1, float64(y), float64(width-2), float64(itemH))
				dc.Fill()
			}

			// Draw check mark using symbol font
			if item.Checked {
				if item.Disabled {
					dc.SetColor(colors.DisabledColor)
				} else if isHovered {
					dc.SetColor(colors.HoverTextColor)
				} else {
					dc.SetColor(colors.TextColor)
				}
				m.loadSymbolFont(dc, fontSize)
				cx := padX/2 + checkW/2
				cy := float64(y) + float64(itemH)/2 + fontSize/3
				sw, _ := dc.MeasureString("✓")
				dc.DrawString("✓", cx-sw/2, cy)
				m.loadFont(dc, fontSize) // switch back to text font
			}

			// Draw text
			if item.Disabled {
				dc.SetColor(colors.DisabledColor)
			} else if isHovered {
				dc.SetColor(colors.HoverTextColor)
			} else {
				dc.SetColor(colors.TextColor)
			}

			textX := padX + checkW
			textY := float64(y) + float64(itemH)/2 + fontSize/3
			dc.DrawString(item.Text, textX, textY)

			// Draw submenu arrow using symbol font
			if len(item.Children) > 0 {
				if item.Disabled {
					dc.SetColor(colors.DisabledColor)
				} else if isHovered {
					dc.SetColor(colors.HoverTextColor)
				} else {
					dc.SetColor(colors.TextColor)
				}
				m.loadSymbolFont(dc, fontSize)
				ax := float64(width) - padX/2 - arrowW/2
				ay := float64(y) + float64(itemH)/2 + fontSize/3
				sw, _ := dc.MeasureString("▸")
				dc.DrawString("▸", ax-sw/2, ay)
				m.loadFont(dc, fontSize) // switch back to text font
			}

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
	menuX := m.x
	menuY := m.y
	sub := m.submenu
	oldHover := m.hoverIndex

	// Check if mouse is in submenu area (for event forwarding)
	insideParent := mouseX >= 0 && mouseX < menuWidth && mouseY >= 0 && mouseY < menuHeight
	m.mu.Unlock()

	// If submenu is open and mouse is in submenu area, forward to submenu
	// This takes priority even when the submenu overlaps with the parent menu
	if sub != nil {
		screenX := menuX + mouseX
		screenY := menuY + mouseY
		if m.forwardMouseMoveToSubmenu(screenX, screenY) {
			// Mouse is in submenu - keep parent hover on submenu item, don't change
			return
		}
	}

	m.mu.Lock()
	// Only show hover if mouse is actually inside the menu bounds
	if insideParent {
		m.hoverIndex = m.hitTest(mouseY)
	} else {
		m.hoverIndex = -1
	}

	newHover := m.hoverIndex

	if newHover != oldHover {
		// Check if the new hover item has children
		hasChildren := false
		if newHover >= 0 && newHover < len(m.items) {
			hasChildren = len(m.items[newHover].Children) > 0
		}
		submenuIdx := m.submenuIndex
		m.mu.Unlock()

		// Kill any pending submenu timer
		procKillTimer.Call(uintptr(m.hwnd), SUBMENU_TIMER_ID)

		if hasChildren {
			// Start submenu delay timer
			procSetTimer.Call(uintptr(m.hwnd), SUBMENU_TIMER_ID, SUBMENU_DELAY_MS, 0)
		} else if submenuIdx >= 0 && newHover != submenuIdx {
			// Before closing submenu, check if mouse is in the submenu tree
			if newHover == -1 {
				// Mouse is outside parent menu - convert to screen coords and check submenu
				screenX := menuX + mouseX
				screenY := menuY + mouseY
				if !m.isPointInMenuTree(screenX, screenY) {
					m.hideSubmenu()
				}
				// else: mouse is in submenu area, keep submenu open
			} else {
				// Moved to a different menu item - close submenu
				m.hideSubmenu()
			}
		}

		// Re-render with new hover state
		m.updateWindow()
		m.trackMouseLeave()
	} else {
		m.mu.Unlock()
	}
}

// forwardMouseMoveToSubmenu forwards a mouse move event to the submenu if the screen
// coordinates are inside it. Returns true if forwarded.
func (m *PopupMenu) forwardMouseMoveToSubmenu(screenX, screenY int) bool {
	m.mu.Lock()
	sub := m.submenu
	m.mu.Unlock()

	if sub == nil {
		return false
	}

	sub.mu.Lock()
	sx, sy, sw, sh := sub.x, sub.y, sub.width, sub.height
	subVisible := sub.visible
	sub.mu.Unlock()

	if !subVisible || screenX < sx || screenX >= sx+sw || screenY < sy || screenY >= sy+sh {
		return false
	}

	// Convert to client coordinates relative to submenu
	clientX := screenX - sx
	clientY := screenY - sy
	newLParam := uintptr(uint16(clientX)) | (uintptr(uint16(clientY)) << 16)
	sub.handleMouseMove(newLParam)
	return true
}

// forwardClickToSubmenu forwards a click event to the submenu if the screen
// coordinates are inside it. Returns true if forwarded.
func (m *PopupMenu) forwardClickToSubmenu(screenX, screenY int) bool {
	m.mu.Lock()
	sub := m.submenu
	m.mu.Unlock()

	if sub == nil {
		return false
	}

	sub.mu.Lock()
	sx, sy, sw, sh := sub.x, sub.y, sub.width, sub.height
	subVisible := sub.visible
	sub.mu.Unlock()

	if !subVisible || screenX < sx || screenX >= sx+sw || screenY < sy || screenY >= sy+sh {
		return false
	}

	// Convert to client coordinates relative to submenu
	clientX := screenX - sx
	clientY := screenY - sy
	newLParam := uintptr(uint16(clientX)) | (uintptr(uint16(clientY)) << 16)
	sub.handleClick(newLParam)
	return true
}

// handleMouseLeave handles mouse leave events
func (m *PopupMenu) handleMouseLeave() {
	// Use GetCursorPos to check actual cursor position
	// This handles the case where events are forwarded via SetCapture from parent menu:
	// WM_MOUSELEAVE fires because Windows doesn't think the cursor is over this window,
	// but the cursor is actually in our bounds (events are forwarded from parent).
	var pt struct{ X, Y int32 }
	procGetCursorPos.Call(uintptr(unsafe.Pointer(&pt)))
	screenX := int(pt.X)
	screenY := int(pt.Y)

	m.mu.Lock()
	x, y, w, h := m.x, m.y, m.width, m.height
	submenuIdx := m.submenuIndex
	m.mu.Unlock()

	// If cursor is still inside this menu, don't clear hover
	if screenX >= x && screenX < x+w && screenY >= y && screenY < y+h {
		return
	}

	// If submenu is open, check if mouse is still in the menu tree
	if submenuIdx >= 0 {
		if m.isPointInMenuTree(screenX, screenY) {
			return // Mouse is in submenu area, don't clear hover
		}
	}

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
	menuX := m.x
	menuY := m.y
	m.mu.Unlock()

	// If submenu is open, check if click is in submenu area first
	// This takes priority even when the submenu overlaps with the parent menu
	screenX := menuX + mouseX
	screenY := menuY + mouseY
	if m.forwardClickToSubmenu(screenX, screenY) {
		return
	}

	// Check if click is outside the menu bounds
	if mouseX < 0 || mouseX >= menuWidth || mouseY < 0 || mouseY >= menuHeight {
		// Click outside menu tree - hide everything
		m.Hide()
		return
	}

	m.mu.Lock()
	index := m.hitTest(mouseY)

	if index >= 0 && index < len(m.items) {
		item := m.items[index]
		if !item.Disabled && !item.Separator {
			// If item has children, show submenu instead of triggering callback
			if len(item.Children) > 0 {
				m.mu.Unlock()
				m.showSubmenu(index)
				return
			}

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

// showSubmenu creates and shows a submenu for the item at the given index
func (m *PopupMenu) showSubmenu(index int) {
	m.mu.Lock()
	if index < 0 || index >= len(m.items) || len(m.items[index].Children) == 0 {
		m.mu.Unlock()
		return
	}
	// Already showing this submenu
	if m.submenuIndex == index && m.submenu != nil {
		m.mu.Unlock()
		return
	}
	children := m.items[index].Children
	resolvedTheme := m.resolvedTheme
	callback := m.callback
	menuX := m.x
	menuWidth := m.width
	m.mu.Unlock()

	// Hide existing submenu if any
	m.hideSubmenu()

	// Calculate submenu position (right side of parent item)
	scale := GetDPIScale()
	itemH := int(float64(menuItemHeight) * scale)
	sepH := int(float64(menuSeparatorHeight) * scale)
	padY := int(float64(menuPaddingY) * scale)

	itemY := padY
	m.mu.Lock()
	for i, item := range m.items {
		if i == index {
			break
		}
		if item.Separator {
			itemY += sepH
		} else {
			itemY += itemH
		}
	}
	menuY := m.y
	m.mu.Unlock()

	subX := menuX + menuWidth - 2 // Slight overlap
	subY := menuY + itemY

	// Create submenu
	sub := NewPopupMenu()
	sub.parentMenu = m
	if resolvedTheme != nil {
		sub.resolvedTheme = resolvedTheme
	}
	if err := sub.Create(); err != nil {
		return
	}

	m.mu.Lock()
	m.submenu = sub
	m.submenuIndex = index
	m.mu.Unlock()

	// Show submenu - callback proxies to parent's callback
	sub.Show(children, subX, subY, func(id int) {
		// Propagate to root callback and hide root menu
		if callback != nil {
			callback(id)
		}
	})

	// Re-render parent to show highlight on submenu item
	m.updateWindow()
}

// hideSubmenu hides and cleans up the current submenu
func (m *PopupMenu) hideSubmenu() {
	m.mu.Lock()
	sub := m.submenu
	m.submenu = nil
	m.submenuIndex = -1
	m.mu.Unlock()

	if sub != nil {
		sub.Hide()
		sub.Destroy()
	}
}

// isPointInSubmenu checks if coordinates (relative to parent menu window) are inside the submenu
func (m *PopupMenu) isPointInSubmenu(clientX, clientY int) bool {
	m.mu.Lock()
	sub := m.submenu
	menuX := m.x
	menuY := m.y
	m.mu.Unlock()

	if sub == nil {
		return false
	}

	// Convert to screen coordinates
	screenX := menuX + clientX
	screenY := menuY + clientY

	return sub.isPointInMenuTree(screenX, screenY)
}

// isPointInMenuTree checks if screen coordinates are in this menu or any of its submenus
func (m *PopupMenu) isPointInMenuTree(screenX, screenY int) bool {
	m.mu.Lock()
	x, y, w, h := m.x, m.y, m.width, m.height
	visible := m.visible
	sub := m.submenu
	m.mu.Unlock()

	if !visible {
		return false
	}

	if screenX >= x && screenX < x+w && screenY >= y && screenY < y+h {
		return true
	}

	if sub != nil {
		return sub.isPointInMenuTree(screenX, screenY)
	}

	return false
}

// ContainsPoint checks if the given screen coordinates are within the menu tree
func (m *PopupMenu) ContainsPoint(screenX, screenY int) bool {
	return m.isPointInMenuTree(screenX, screenY)
}

// GetBounds returns the menu bounds (x, y, width, height)
func (m *PopupMenu) GetBounds() (int, int, int, int) {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.x, m.y, m.width, m.height
}

// checkMouseState checks if mouse button is pressed outside the menu tree
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

	// Check if cursor is inside the menu tree (including submenus)
	if !m.isPointInMenuTree(int(pt.X), int(pt.Y)) {
		// Mouse pressed outside menu tree - close it
		m.Hide()
	}
}
