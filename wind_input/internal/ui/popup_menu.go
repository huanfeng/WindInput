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

	// DPI locked at Show() time. Menus do not read the global effective DPI
	// during their lifetime — other windows (candidate / toolbar) may rewrite
	// it asynchronously, which would suddenly resize an already-visible menu.
	// 0 = not locked, fall back to GetEffectiveDPI().
	lockedDPI int

	// Foreground window snapshot captured at Show() time. The menu auto-hides
	// when the foreground HWND changes (e.g., the host app opens a Save dialog
	// in response to Ctrl+S, or the user Alt+Tabs to another window).
	// Only used by the root menu; submenus inherit closure from the root.
	ownerForeground uintptr

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
	procGetAsyncKeyState    = user32.NewProc("GetAsyncKeyState")
	procSetWindowsHookExW   = user32.NewProc("SetWindowsHookExW")
	procUnhookWindowsHookEx = user32.NewProc("UnhookWindowsHookEx")
	procCallNextHookEx      = user32.NewProc("CallNextHookEx")
	procRtlMoveMemory       = kernel32.NewProc("RtlMoveMemory")
)

// VK_LBUTTON is the virtual key code for left mouse button
const VK_LBUTTON = 0x01

// Keyboard constants for menu navigation
const (
	WH_KEYBOARD_LL = 13

	WM_KEYDOWN = 0x0100

	VK_BACK   = 0x08
	VK_TAB    = 0x09
	VK_RETURN = 0x0D
	VK_SHIFT  = 0x10
	VK_CTRL   = 0x11
	VK_ALT    = 0x12
	VK_ESCAPE = 0x1B
	VK_SPACE  = 0x20
	VK_PRIOR  = 0x21 // PageUp
	VK_NEXT   = 0x22 // PageDown
	VK_END    = 0x23
	VK_HOME   = 0x24
	VK_LEFT   = 0x25
	VK_UP     = 0x26
	VK_RIGHT  = 0x27
	VK_DOWN   = 0x28
	VK_LWIN   = 0x5B
	VK_RWIN   = 0x5C
)

// Global popup menu registry
var popupMenus = NewWindowRegistry[PopupMenu]()

// Global keyboard hook state for popup menu navigation
var (
	menuKeyboardHook   uintptr
	menuKeyboardHookCb uintptr
	activeRootMenu     *PopupMenu

	// Keyboard navigation mode: suppresses spurious mouse events until mouse actually moves.
	// When the user navigates the menu via keyboard, Windows may generate phantom
	// WM_MOUSEMOVE/WM_MOUSELEAVE events (e.g., when a submenu window is shown/hidden).
	// We record the cursor position on the first key press and ignore mouse events
	// until the cursor physically moves to a different position.
	menuKbNavActive bool
	menuKbNavMouseX int32
	menuKbNavMouseY int32
)

func init() {
	menuKeyboardHookCb = syscall.NewCallback(menuKeyboardHookProc)
}

// menuKeyboardHookProc is the low-level keyboard hook callback.
// It intercepts keyboard input when a popup menu is visible, enabling
// arrow key navigation, Enter/Escape, and shortcut key activation.
func menuKeyboardHookProc(nCode, wParam, lParam uintptr) uintptr {
	if int32(nCode) >= 0 && activeRootMenu != nil && wParam == WM_KEYDOWN {
		// Read vkCode from KBDLLHOOKSTRUCT via RtlMoveMemory to satisfy go vet.
		// lParam points to a Windows-allocated struct; its first DWORD is vkCode.
		var vkCode uint32
		procRtlMoveMemory.Call(uintptr(unsafe.Pointer(&vkCode)), lParam, 4)
		if activeRootMenu.handleKeyDown(vkCode) {
			return 1 // Key consumed, don't pass to next hook
		}
	}
	ret, _, _ := procCallNextHookEx.Call(menuKeyboardHook, uintptr(nCode), wParam, lParam)
	return ret
}

// installMenuKeyboardHook installs a low-level keyboard hook for menu navigation.
// Must be called from the UI thread (which has a message loop).
func installMenuKeyboardHook(root *PopupMenu) {
	if menuKeyboardHook != 0 {
		return
	}
	activeRootMenu = root
	menuKeyboardHook, _, _ = procSetWindowsHookExW.Call(
		WH_KEYBOARD_LL,
		menuKeyboardHookCb,
		0, // hMod: 0 for low-level hooks
		0, // dwThreadId: 0 for all threads
	)
}

// uninstallMenuKeyboardHook removes the low-level keyboard hook.
func uninstallMenuKeyboardHook() {
	if menuKeyboardHook != 0 {
		procUnhookWindowsHookEx.Call(menuKeyboardHook)
		menuKeyboardHook = 0
	}
	activeRootMenu = nil
	menuKbNavActive = false
}

// popupMenuWndProc is the window procedure for popup menu
func popupMenuWndProc(hwnd uintptr, msg uint32, wParam, lParam uintptr) uintptr {
	switch msg {
	case WM_DESTROY:
		popupMenus.Unregister(windows.HWND(hwnd))
		return 0

	case WM_MOUSEMOVE:
		m := popupMenus.Get(windows.HWND(hwnd))
		if m != nil {
			m.handleMouseMove(lParam)
		}
		return 0

	case WM_LBUTTONDOWN:
		m := popupMenus.Get(windows.HWND(hwnd))
		if m != nil {
			m.handleClick(lParam)
		}
		return 0

	case WM_RBUTTONDOWN:
		// Right-click also closes the menu if outside
		m := popupMenus.Get(windows.HWND(hwnd))
		if m != nil {
			m.handleClick(lParam)
		}
		return 0

	case WM_MOUSELEAVE:
		m := popupMenus.Get(windows.HWND(hwnd))
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
		m := popupMenus.Get(windows.HWND(hwnd))
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
		m := popupMenus.Get(windows.HWND(hwnd))
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
	lockedDPI := parent.lockedDPI
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
		lockedDPI:            lockedDPI,
	}
}

// dpiForPoint returns the effective DPI of the monitor containing the given
// screen point. Falls back to the global system DPI when the per-monitor API
// is unavailable.
func dpiForPoint(x, y int) int {
	if procGetDpiForMonitor.Find() == nil {
		pt := uintptr(uint32(x)) | (uintptr(uint32(y)) << 32)
		hMonitor, _, _ := procMonitorFromPoint.Call(pt, MONITOR_DEFAULTTONEAREST)
		if hMonitor != 0 {
			var dpiX, dpiY uint32
			ret, _, _ := procGetDpiForMonitor.Call(
				hMonitor,
				MDT_EFFECTIVE_DPI,
				uintptr(unsafe.Pointer(&dpiX)),
				uintptr(unsafe.Pointer(&dpiY)),
			)
			if ret == 0 && dpiX > 0 {
				return int(dpiX)
			}
		}
	}
	return GetSystemDPI()
}

// dpiScale returns the DPI scale factor this menu should use. The value is
// captured at Show() time and frozen for the menu's lifetime to immunize
// against global DPI changes triggered by sibling windows (candidate, toolbar)
// while the menu is visible.
func (m *PopupMenu) dpiScale() float64 {
	dpi := m.lockedDPI
	if dpi <= 0 {
		dpi = GetEffectiveDPI()
	}
	return float64(dpi) / float64(DefaultDPI)
}

func (m *PopupMenu) resolvePrimaryFontFamilyLocked() string {
	return m.fontConfig.ResolvePrimaryFontFamily()
}

func (m *PopupMenu) ensureTextRendererLocked() *TextRenderer {
	if m.textRenderer != nil {
		return m.textRenderer
	}
	tr := NewTextRenderer()
	tr.SetGDIParams(m.fontConfig.GetEffectiveGDIWeight(), m.fontConfig.GetEffectiveGDIScale())
	if family := m.resolvePrimaryFontFamilyLocked(); family != "" {
		tr.SetFont(family)
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
	if family := m.resolvePrimaryFontFamilyLocked(); family != "" {
		dwr.SetFont(family)
	}
	m.dwriteRenderer = dwr
	return dwr
}

func (m *PopupMenu) ensureFontCacheLocked() *fontCache {
	if m.fontCache == nil {
		m.fontCache = newFontCache()
	}
	// 菜单走 gg/text 时必须跳过 TTC，否则用户把主字体设成 msyh.ttc 会直接失效。
	if resolved := m.fontConfig.ResolveTextPrimaryFont(); resolved != "" {
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

// SetFontFamily updates the primary font for popup menu rendering.
func (m *PopupMenu) SetFontFamily(fontSpec string) {
	m.mu.Lock()
	sub := m.submenu
	m.fontConfig.SetPrimaryFont(fontSpec)
	resolvedFamily := m.resolvePrimaryFontFamilyLocked()
	textResolved := m.fontConfig.ResolveTextPrimaryFont()
	if m.fontCache != nil && textResolved != "" {
		m.fontCache.mu.Lock()
		// 原生后端和 gg/text 后端分别更新，避免把 TTC 路径喂给 gg/text。
		_ = m.fontCache.loadFont(textResolved)
		m.fontCache.mu.Unlock()
	}
	if m.textRenderer != nil {
		m.textRenderer.SetFont(resolvedFamily)
	}
	if m.dwriteRenderer != nil {
		m.dwriteRenderer.SetFont(resolvedFamily)
	}
	m.mu.Unlock()

	if sub != nil {
		sub.SetFontFamily(fontSpec)
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
	hwnd, err := CreateLayeredWindow(LayeredWindowConfig{
		ClassName: "IMEPopupMenu",
		WndProc:   syscall.NewCallback(popupMenuWndProc),
	})
	if err != nil {
		return err
	}

	m.hwnd = hwnd
	popupMenus.Register(m.hwnd, m)

	return nil
}

// Show displays the popup menu at the specified position
func (m *PopupMenu) Show(items []MenuItem, x, y int, callback PopupMenuCallback) {
	// Lock DPI for the entire visible lifetime of this menu (root only;
	// submenus inherit via newPopupMenuShared). Determined from the monitor
	// containing the show point, so it survives global DPI rewrites.
	m.mu.Lock()
	isChildEarly := m.parentMenu != nil
	m.mu.Unlock()
	if !isChildEarly {
		dpi := dpiForPoint(x, y)
		m.mu.Lock()
		m.lockedDPI = dpi
		m.mu.Unlock()
	}

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

	// Only root menu captures mouse, starts timer, and installs keyboard hook
	if !isChild {
		// Capture mouse to detect clicks outside the menu
		procSetCapture.Call(uintptr(m.hwnd))

		// Start timer to check mouse state (backup for cross-process click detection)
		procSetTimer.Call(uintptr(m.hwnd), MENU_CHECK_TIMER_ID, MENU_CHECK_INTERVAL, 0)

		// Install keyboard hook for arrow keys, Enter, Escape, and shortcut keys
		installMenuKeyboardHook(m)

		// Snapshot the foreground window so the timer can auto-hide the menu
		// when focus moves to another window (e.g., host app opens a Save
		// dialog after Ctrl+S, or the user Alt+Tabs away).
		fg, _, _ := procGetForegroundWindow.Call()
		m.mu.Lock()
		m.ownerForeground = fg
		m.mu.Unlock()
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
			// Only root menu releases capture, stops check timer, and removes keyboard hook
			procKillTimer.Call(uintptr(m.hwnd), MENU_CHECK_TIMER_ID)
			procReleaseCapture.Call()
			uninstallMenuKeyboardHook()
		}
		procShowWindow.Call(uintptr(m.hwnd), SW_HIDE)
	}

	// Release the locked DPI / foreground snapshot so the next Show() picks up
	// fresh values (e.g., menu reopened on a different monitor).
	m.mu.Lock()
	m.lockedDPI = 0
	m.ownerForeground = 0
	m.mu.Unlock()
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
	scale := m.dpiScale()

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
