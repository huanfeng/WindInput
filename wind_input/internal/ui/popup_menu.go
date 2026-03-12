// Package ui provides native Windows UI for candidate window
package ui

import (
	"image/color"
	"sync"
	"syscall"
	"unsafe"

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

	// Text rendering
	fontCache            *fontCache
	textRenderer         *TextRenderer
	dwriteRenderer       *DWriteRenderer
	textDrawer           TextDrawer
	renderMode           TextRenderMode
	fontConfig           *FontConfig
	menuFontSizeOverride float64 // 0 = use default menuFontSize constant

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

// NewPopupMenu creates a new popup menu with its own rendering resources.
// Menus default to SemiBold (600) weight for better readability at small font sizes.
func NewPopupMenu() *PopupMenu {
	fontCfg := NewFontConfig()
	fontCfg.SetGDIFontWeight(FontWeightSemiBold)

	return &PopupMenu{
		hoverIndex:   -1,
		submenuIndex: -1,
		renderMode:   TextRenderModeGDI,
		fontConfig:   fontCfg,
	}
}

// newPopupMenuShared creates a submenu that shares rendering resources with its parent.
// This avoids duplicating fontCache, TextRenderer, and FontConfig per submenu,
// significantly reducing memory when submenus are created/destroyed frequently.
func newPopupMenuShared(parent *PopupMenu) *PopupMenu {
	parent.mu.Lock()
	menuFontSizeOverride := parent.menuFontSizeOverride
	resolvedTheme := parent.resolvedTheme
	parent.mu.Unlock()

	return &PopupMenu{
		hoverIndex:           -1,
		submenuIndex:         -1,
		fontCache:            parent.fontCache,
		textRenderer:         parent.textRenderer,
		dwriteRenderer:       parent.dwriteRenderer,
		textDrawer:           parent.textDrawer,
		renderMode:           parent.renderMode,
		fontConfig:           parent.fontConfig,
		menuFontSizeOverride: menuFontSizeOverride,
		resolvedTheme:        resolvedTheme,
	}
}

func (m *PopupMenu) resolvePrimaryFontPathLocked() string {
	resolved := m.fontConfig.ResolvePrimaryFont()
	if resolved != "" {
		m.fontConfig.SetPrimaryFont(resolved)
	}
	return resolved
}

func (m *PopupMenu) ensureTextRendererLocked() *TextRenderer {
	if m.textRenderer != nil {
		return m.textRenderer
	}
	tr := NewTextRenderer()
	tr.SetGDIParams(m.fontConfig.GetEffectiveGDIWeight(), m.fontConfig.GetEffectiveGDIScale())
	if resolved := m.resolvePrimaryFontPathLocked(); resolved != "" {
		tr.SetFont(resolved)
	}
	m.textRenderer = tr
	return tr
}

func (m *PopupMenu) ensureDWriteRendererLocked() *DWriteRenderer {
	if m.dwriteRenderer != nil {
		return m.dwriteRenderer
	}
	dwr := NewDWriteRenderer("popup_menu")
	dwr.SetGDIParams(m.fontConfig.GetEffectiveGDIWeight(), m.fontConfig.GetEffectiveGDIScale())
	if resolved := m.resolvePrimaryFontPathLocked(); resolved != "" {
		dwr.SetFont(resolved)
	}
	m.dwriteRenderer = dwr
	return dwr
}

func (m *PopupMenu) ensureFontCacheLocked() *fontCache {
	if m.fontCache == nil {
		m.fontCache = newFontCache()
	}
	if resolved := m.resolvePrimaryFontPathLocked(); resolved != "" {
		m.fontCache.mu.Lock()
		_ = m.fontCache.loadFont(resolved)
		m.fontCache.mu.Unlock()
	}
	return m.fontCache
}

func (m *PopupMenu) releaseGDIBackendLocked() {
	if m.parentMenu != nil {
		return
	}
	if m.textRenderer != nil {
		m.textRenderer.Close()
		m.textRenderer = nil
	}
}

func (m *PopupMenu) releaseDWriteBackendLocked() {
	if m.parentMenu != nil {
		return
	}
	if m.dwriteRenderer != nil {
		m.dwriteRenderer.Close()
		m.dwriteRenderer = nil
	}
}

func (m *PopupMenu) releaseFreeTypeBackendLocked() {
	if m.parentMenu != nil {
		return
	}
	if m.fontCache != nil {
		m.fontCache.Close()
		m.fontCache = nil
	}
}

func (m *PopupMenu) ensureActiveTextDrawerLocked() {
	switch m.renderMode {
	case TextRenderModeFreetype:
		fc := m.ensureFontCacheLocked()
		m.releaseGDIBackendLocked()
		m.releaseDWriteBackendLocked()
		m.textDrawer = newFreeTypeDrawer(fc, m.fontConfig)
	case TextRenderModeDirectWrite:
		dwr := m.ensureDWriteRendererLocked()
		if dwr != nil && dwr.IsAvailable() {
			m.releaseGDIBackendLocked()
			m.releaseFreeTypeBackendLocked()
			m.textDrawer = newDirectWriteDrawer(dwr)
			return
		}
		m.releaseDWriteBackendLocked()
		tr := m.ensureTextRendererLocked()
		m.releaseFreeTypeBackendLocked()
		m.textDrawer = newGDIDrawer(tr)
	default:
		tr := m.ensureTextRendererLocked()
		m.releaseDWriteBackendLocked()
		m.releaseFreeTypeBackendLocked()
		m.textDrawer = newGDIDrawer(tr)
	}
}

// SetGDIFontParams updates GDI font weight and scale for text rendering
func (m *PopupMenu) SetGDIFontParams(weight int, scale float64) {
	m.mu.Lock()
	sub := m.submenu
	m.fontConfig.SetGDIFontWeight(weight)
	m.fontConfig.SetGDIFontScale(scale)
	if m.textRenderer != nil {
		m.textRenderer.SetGDIParams(weight, scale)
	}
	if m.dwriteRenderer != nil {
		m.dwriteRenderer.SetGDIParams(weight, scale)
	}
	m.mu.Unlock()

	if sub != nil {
		sub.SetGDIFontParams(weight, scale)
	}
}

// SetFontPath updates the primary font for popup menu rendering.
func (m *PopupMenu) SetFontPath(path string) {
	m.mu.Lock()
	sub := m.submenu
	m.fontConfig.SetPrimaryFont(path)
	resolved := m.resolvePrimaryFontPathLocked()
	if resolved != "" {
		if m.fontCache != nil {
			m.fontCache.mu.Lock()
			_ = m.fontCache.loadFont(resolved)
			m.fontCache.mu.Unlock()
		}
		if m.textRenderer != nil {
			m.textRenderer.SetFont(resolved)
		}
		if m.dwriteRenderer != nil {
			m.dwriteRenderer.SetFont(resolved)
		}
	}
	m.mu.Unlock()

	if sub != nil {
		sub.SetFontPath(path)
	}
}

// SetMenuFontSize sets the base font size for menu text (before DPI scaling).
// Pass 0 to use the default (menuFontSize constant = 12.0).
func (m *PopupMenu) SetMenuFontSize(size float64) {
	m.mu.Lock()
	m.menuFontSizeOverride = size
	sub := m.submenu
	m.mu.Unlock()

	if sub != nil {
		sub.SetMenuFontSize(size)
	}
}

// getMenuFontSize returns the effective menu font size (base, before DPI scaling).
func (m *PopupMenu) getMenuFontSize() float64 {
	if m.menuFontSizeOverride > 0 {
		return m.menuFontSizeOverride
	}
	return menuFontSize
}

// getMenuItemHeight returns the effective menu item height (base, before DPI scaling).
// Auto-adapts to font size: baseline is fontSize=12 → itemHeight=24 (2x ratio).
// Minimum is menuItemHeight (24) to avoid cramped layout at small font sizes.
func (m *PopupMenu) getMenuItemHeight() int {
	fs := m.getMenuFontSize()
	h := int(fs * 2)
	if h < menuItemHeight {
		h = menuItemHeight
	}
	return h
}

// SetTextRenderMode switches between GDI, FreeType, and DirectWrite text rendering
func (m *PopupMenu) SetTextRenderMode(mode TextRenderMode) {
	m.mu.Lock()
	m.renderMode = mode
	m.ensureActiveTextDrawerLocked()
	sub := m.submenu
	m.mu.Unlock()

	if sub != nil {
		sub.SetTextRenderMode(mode)
	}
}

// SetTheme sets the theme for the popup menu
func (m *PopupMenu) SetTheme(resolved *theme.ResolvedTheme) {
	m.mu.Lock()
	m.resolvedTheme = resolved
	sub := m.submenu
	m.mu.Unlock()

	if sub != nil {
		sub.SetTheme(resolved)
	}
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
	m.ensureActiveTextDrawerLocked()
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
	if m.parentMenu == nil {
		m.mu.Lock()
		m.releaseFreeTypeBackendLocked()
		m.releaseGDIBackendLocked()
		m.releaseDWriteBackendLocked()
		m.mu.Unlock()
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

	// Use TextDrawer for text measurement (consistent with render)
	fontSize := m.getMenuFontSize() * scale
	m.ensureActiveTextDrawerLocked()
	td := m.textDrawer

	itemH := m.getMenuItemHeight()
	for _, item := range m.items {
		if item.Separator {
			m.height += int(float64(menuSeparatorHeight) * scale)
		} else {
			m.height += int(float64(itemH) * scale)
			// Calculate text width using TextDrawer
			tw := td.MeasureString(item.Text, fontSize)
			itemWidth := int(tw + float64(menuPaddingX)*scale + extraLeft + extraRight + float64(menuPaddingX)*scale)
			if itemWidth > m.width {
				m.width = itemWidth
			}
		}
	}
}
