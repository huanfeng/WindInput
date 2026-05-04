// Package ui provides native Windows UI for candidate window
package ui

import (
	"image"
	"image/color"
	"log/slog"
	"slices"
	"strings"
	"sync"
	"syscall"
	"unsafe"

	"github.com/gogpu/gg"
	"github.com/huanfeng/wind_input/pkg/theme"
	"golang.org/x/sys/windows"
)

// TooltipWindow represents a tooltip window for displaying candidate encoding
type TooltipWindow struct {
	hwnd   windows.HWND
	logger *slog.Logger

	mu            sync.Mutex
	visible       bool
	mouseOver     bool
	trackingMouse bool
	leaveBlocked  bool // 右键菜单显示期间抑制 WM_MOUSELEAVE 隐藏
	text          string
	resolvedTheme *theme.ResolvedTheme
	onRightClick  func(text string, x, y int)

	TextBackendManager
}

// NewTooltipWindow creates a new tooltip window
func NewTooltipWindow(logger *slog.Logger) *TooltipWindow {
	w := &TooltipWindow{
		logger:             logger,
		TextBackendManager: NewTextBackendManager("tooltip"),
	}
	w.SetTextRenderMode(TextRenderModeGDI)
	return w
}

// SetGDIFontParams updates GDI font weight and scale for text rendering
func (w *TooltipWindow) SetGDIFontParams(weight int, scale float64) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.TextBackendManager.SetGDIFontParams(weight, scale)
}

// SetFontFamily updates the primary font for tooltip rendering.
func (w *TooltipWindow) SetFontFamily(fontSpec string) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.TextBackendManager.SetFontFamily(fontSpec)
}

// SetTextRenderMode switches between GDI, FreeType, and DirectWrite text rendering.
// If custom fallback fonts are configured (UserFonts non-empty), FreeType mode is
// preserved regardless of the requested mode, because DirectWrite/GDI cannot load
// arbitrary TTF files (e.g., PUA radical fonts) that are not system-registered.
func (w *TooltipWindow) SetTextRenderMode(mode TextRenderMode) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if mode != TextRenderModeFreetype && len(w.TextBackendManager.FontConfig().UserFonts) > 0 {
		mode = TextRenderModeFreetype
	}
	w.TextBackendManager.SetTextRenderMode(mode)
}

// AddFallbackFont 注册额外的回退字体路径（TTF/OTF）并切换到 FreeType 渲染模式。
// 用于在 tooltip 中显示需要专用字体的字符（如五笔字根 PUA 字符）。
func (w *TooltipWindow) AddFallbackFont(fontPath string) {
	if fontPath == "" {
		return
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	fc := w.TextBackendManager.FontConfig()
	if slices.Contains(fc.UserFonts, fontPath) {
		return
	}
	fc.UserFonts = append(fc.UserFonts, fontPath)
	w.TextBackendManager.SetTextRenderMode(TextRenderModeFreetype)
}

// SetOnRightClick registers a callback invoked when the user right-clicks the tooltip.
// The callback receives the tooltip text and the screen cursor position.
func (w *TooltipWindow) SetOnRightClick(cb func(text string, x, y int)) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.onRightClick = cb
}

// SuppressLeave controls whether WM_MOUSELEAVE is allowed to hide the tooltip.
// Set true before showing a popup menu triggered by the tooltip, false when it closes.
func (w *TooltipWindow) SuppressLeave(suppress bool) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.leaveBlocked = suppress
}

// SetTheme sets the theme for the tooltip window
func (w *TooltipWindow) SetTheme(resolved *theme.ResolvedTheme) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.resolvedTheme = resolved
}

// getTooltipColors returns tooltip colors from theme or defaults
func (w *TooltipWindow) getTooltipColors() (bgColor, textColor color.Color) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.resolvedTheme != nil {
		return w.resolvedTheme.Tooltip.BackgroundColor, w.resolvedTheme.Tooltip.TextColor
	}
	return color.RGBA{60, 60, 60, 240}, color.RGBA{255, 255, 255, 255}
}

// Global tooltip window registry
var tooltipWindows = NewWindowRegistry[TooltipWindow]()

// tooltipWndProc is the window procedure for tooltip
func tooltipWndProc(hwnd uintptr, msg uint32, wParam, lParam uintptr) uintptr {
	switch msg {
	case WM_DESTROY:
		tooltipWindows.Unregister(windows.HWND(hwnd))
		return 0
	case WM_MOUSEMOVE:
		if w := tooltipWindows.Get(windows.HWND(hwnd)); w != nil {
			w.mu.Lock()
			needTrack := !w.trackingMouse
			w.mouseOver = true
			w.trackingMouse = true
			w.mu.Unlock()
			if needTrack {
				tme := TRACKMOUSEEVENT{
					CbSize:    uint32(unsafe.Sizeof(TRACKMOUSEEVENT{})),
					DwFlags:   TME_LEAVE,
					HwndTrack: uintptr(hwnd),
				}
				procTrackMouseEvent.Call(uintptr(unsafe.Pointer(&tme)))
			}
		}
		return 0
	case WM_MOUSELEAVE:
		if w := tooltipWindows.Get(windows.HWND(hwnd)); w != nil {
			w.mu.Lock()
			w.mouseOver = false
			w.trackingMouse = false
			blocked := w.leaveBlocked
			w.mu.Unlock()
			if !blocked {
				procShowWindow.Call(hwnd, SW_HIDE)
				w.mu.Lock()
				w.visible = false
				w.mu.Unlock()
			}
		}
		return 0
	case WM_RBUTTONUP:
		if w := tooltipWindows.Get(windows.HWND(hwnd)); w != nil {
			w.mu.Lock()
			text := w.text
			cb := w.onRightClick
			w.mu.Unlock()
			if text != "" && cb != nil {
				// 阻止 SetCapture（弹出菜单）触发的 WM_MOUSELEAVE 隐藏 tooltip
				w.mu.Lock()
				w.leaveBlocked = true
				w.mu.Unlock()
				var pt POINT
				procGetCursorPos.Call(uintptr(unsafe.Pointer(&pt)))
				cb(text, int(pt.X), int(pt.Y))
			}
		}
		return 0
	}
	ret, _, _ := procDefWindowProcW.Call(hwnd, uintptr(msg), wParam, lParam)
	return ret
}

// Create creates the tooltip window (must be called from the UI thread)
func (w *TooltipWindow) Create() error {
	hwnd, err := CreateLayeredWindow(LayeredWindowConfig{
		ClassName: "IMETooltipWindow",
		WndProc:   syscall.NewCallback(tooltipWndProc),
	})
	if err != nil {
		return err
	}

	w.hwnd = hwnd
	tooltipWindows.Register(w.hwnd, w)
	w.logger.Debug("Tooltip window created", "hwnd", w.hwnd)

	return nil
}

// Show shows the tooltip centered horizontally at centerX, below y
func (w *TooltipWindow) Show(text string, centerX, y int) {
	if w.hwnd == 0 || text == "" {
		return
	}

	w.mu.Lock()
	w.text = text
	w.visible = true
	w.mu.Unlock()

	// Render tooltip
	img := w.render(text)
	if img == nil {
		return
	}

	// Center tooltip horizontally relative to the candidate
	tooltipWidth := img.Bounds().Dx()
	x := centerX - tooltipWidth/2

	// Update and show
	w.updateLayeredWindow(img, x, y)
	procShowWindow.Call(uintptr(w.hwnd), SW_SHOW)
}

// Hide hides the tooltip. If the mouse is currently over the tooltip, hiding is
// deferred until the mouse leaves (WM_MOUSELEAVE fires and calls Hide again).
func (w *TooltipWindow) Hide() {
	if w.hwnd == 0 {
		return
	}
	w.mu.Lock()
	over := w.mouseOver
	w.mu.Unlock()
	if over {
		return
	}
	procShowWindow.Call(uintptr(w.hwnd), SW_HIDE)
	w.mu.Lock()
	w.visible = false
	w.mu.Unlock()
}

// IsVisible returns whether the tooltip is visible
func (w *TooltipWindow) IsVisible() bool {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.visible
}

// Destroy destroys the tooltip window
func (w *TooltipWindow) Destroy() {
	if w.hwnd != 0 {
		procDestroyWindow.Call(uintptr(w.hwnd))
		w.hwnd = 0
	}
	w.mu.Lock()
	w.TextBackendManager.Close()
	w.mu.Unlock()
}

// render 将 tooltip 文本渲染到图像（支持 \n 换行）
func (w *TooltipWindow) render(text string) *image.RGBA {
	scale := GetDPIScale()
	bgColor, textColor := w.getTooltipColors()

	w.mu.Lock()
	td := w.TextDrawer()
	w.mu.Unlock()

	fontSize := 14.0 * scale
	padding := 6.0 * scale
	lineSpacing := 2.0 * scale

	lines := splitLines(text)
	if len(lines) == 0 {
		return nil
	}

	// 计算各行宽度，取最大值
	var maxLineWidth float64
	for _, line := range lines {
		lw := td.MeasureString(line, fontSize)
		if lw > maxLineWidth {
			maxLineWidth = lw
		}
	}

	lineH := fontSize + lineSpacing
	width := maxLineWidth + padding*2
	height := lineH*float64(len(lines)) - lineSpacing + padding*2

	// Phase 1: 绘制背景
	dc := gg.NewContext(int(width), int(height))
	dc.SetColor(bgColor)
	dc.DrawRoundedRectangle(0, 0, width, height, 4*scale)
	dc.Fill()

	// Phase 2: 逐行绘制文字
	img := dc.Image().(*image.RGBA)
	td.BeginDraw(img)
	for i, line := range lines {
		y := padding + fontSize*0.8 + float64(i)*lineH
		td.DrawString(line, padding, y, fontSize, textColor)
	}
	td.EndDraw()

	DrawDebugBanner(img)
	return img
}

// splitLines 按 \n 拆分文本为行列表，过滤空行
func splitLines(text string) []string {
	raw := strings.Split(text, "\n")
	var lines []string
	for _, l := range raw {
		l = strings.TrimSpace(l)
		if l != "" {
			lines = append(lines, l)
		}
	}
	return lines
}

// updateLayeredWindow updates the tooltip's layered window
func (w *TooltipWindow) updateLayeredWindow(img *image.RGBA, x, y int) error {
	return UpdateLayeredWindowFromImage(w.hwnd, img, x, y)
}
