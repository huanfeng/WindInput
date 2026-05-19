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
func (w *TooltipWindow) SetTextRenderMode(mode TextRenderMode) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.TextBackendManager.SetTextRenderMode(mode)
}

// AddFallbackFont 注册额外的回退字体路径（TTF/OTF）并切换到 FreeType 渲染模式。
// 仅在字体未系统安装时使用；系统已安装字体请使用 SetChaiziFont 配置 DirectWrite fallback。
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

// SetChaiziFont 配置拆字 PUA 字符的渲染字体。
// 若 dwFamilyName 非空（字体已安装到系统），则配置 DirectWrite PUA fallback 并切换到 DW 模式。
// 否则回退到 FreeType 模式加载 fontPath 文件。
func (w *TooltipWindow) SetChaiziFont(fontPath, dwFamilyName string) {
	if dwFamilyName != "" {
		w.mu.Lock()
		defer w.mu.Unlock()
		w.TextBackendManager.SetDWriteFontFallbackForPUA(dwFamilyName)
		w.TextBackendManager.SetTextRenderMode(TextRenderModeDirectWrite)
		return
	}
	if fontPath != "" {
		w.AddFallbackFont(fontPath)
	}
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

// Show shows the tooltip centered horizontally at centerX.
// belowY 为候选项下沿（首选位置：tooltip 顶端贴 belowY）。
// aboveY 为候选项上沿（备用位置：下方不够时 tooltip 底端贴 aboveY）。
// 若 aboveY <= 0，则不启用反向显示，只在下方钳制于工作区。
func (w *TooltipWindow) Show(text string, centerX, belowY, aboveY int) {
	if w.hwnd == 0 || text == "" {
		return
	}

	// 获取候选位置所在显示器工作区，用于宽度/行数裁剪与位置钳制
	workLeft, workTop, workRight, workBottom := GetMonitorWorkAreaFromPoint(centerX, belowY)
	scale := GetDPIScale()
	margin := int(8 * scale)
	maxWidth := workRight - workLeft - margin*2
	if maxWidth < int(80*scale) {
		maxWidth = int(80 * scale) // 极端情况下兜底
	}

	// Render tooltip（render 会按 maxWidth 做单行截断与行数限制）
	img := w.render(text, float64(maxWidth))
	if img == nil {
		return
	}
	tooltipWidth := img.Bounds().Dx()
	tooltipHeight := img.Bounds().Dy()

	// 水平居中并钳制到工作区
	x := centerX - tooltipWidth/2
	if x+tooltipWidth > workRight-margin {
		x = workRight - margin - tooltipWidth
	}
	if x < workLeft+margin {
		x = workLeft + margin
	}

	// 垂直：默认下方，下方放不下且上方有空间则改放上方
	y := belowY
	if y+tooltipHeight > workBottom-margin {
		if aboveY > 0 {
			candidate := aboveY - tooltipHeight - 2
			if candidate >= workTop+margin {
				y = candidate
			} else {
				y = workBottom - margin - tooltipHeight
			}
		} else {
			y = workBottom - margin - tooltipHeight
		}
	}
	if y < workTop+margin {
		y = workTop + margin
	}

	w.mu.Lock()
	w.text = text
	w.visible = true
	w.mu.Unlock()

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

// ForceHide 强制隐藏 tooltip，绕过 mouseOver 保护。用于候选窗关闭、菜单弹出、
// 输入会话结束等"必须立即消失"的场景（避免 tip 残留在屏幕上）。
func (w *TooltipWindow) ForceHide() {
	if w.hwnd == 0 {
		return
	}
	procShowWindow.Call(uintptr(w.hwnd), SW_HIDE)
	w.mu.Lock()
	w.visible = false
	w.mouseOver = false
	w.trackingMouse = false
	w.leaveBlocked = false
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

// render 将 tooltip 文本渲染到图像（支持 \n 换行）。
// maxContentWidth 为可用内容区最大像素宽度（不含 padding）；<=0 表示不限制。
// 超长行会以"…"截断尾部，行数过多则汇总成"… (+N)"。
func (w *TooltipWindow) render(text string, maxContentWidth float64) *image.RGBA {
	scale := GetDPIScale()
	bgColor, textColor := w.getTooltipColors()

	w.mu.Lock()
	td := w.TextDrawer()
	w.mu.Unlock()

	fontSize := 14.0 * scale
	padding := 6.0 * scale
	lineSpacing := 2.0 * scale

	const maxLines = 20

	lines := splitLines(text)
	if len(lines) == 0 {
		return nil
	}

	// 限制最大行数：保留前 maxLines-1 行，最后一行汇总剩余
	if len(lines) > maxLines {
		hidden := len(lines) - (maxLines - 1)
		kept := append([]string{}, lines[:maxLines-1]...)
		kept = append(kept, "… (+"+itoaCompact(hidden)+")")
		lines = kept
	}

	// 单行宽度限制：超过 maxContentWidth 则尾部截断为 "…"
	innerMax := maxContentWidth - padding*2
	if innerMax > 0 {
		for i, line := range lines {
			lw := td.MeasureString(line, fontSize)
			if lw <= innerMax {
				continue
			}
			lines[i] = truncateLineToWidth(td, line, fontSize, innerMax)
		}
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

// itoaCompact 简单 int → 十进制字符串，避免引入 strconv 仅用于一处。
func itoaCompact(n int) string {
	if n == 0 {
		return "0"
	}
	neg := n < 0
	if neg {
		n = -n
	}
	var buf [20]byte
	i := len(buf)
	for n > 0 {
		i--
		buf[i] = byte('0' + n%10)
		n /= 10
	}
	if neg {
		i--
		buf[i] = '-'
	}
	return string(buf[i:])
}

// truncateLineToWidth 将单行裁剪到 ≤ maxWidth 宽度，尾部加 "…"。
// 二分查找最长可放入前缀（按 rune 切，避免破坏多字节字符）。
func truncateLineToWidth(td TextDrawer, line string, fontSize, maxWidth float64) string {
	const ellipsis = "…"
	runes := []rune(line)
	if len(runes) == 0 {
		return line
	}
	ellipsisW := td.MeasureString(ellipsis, fontSize)
	if ellipsisW >= maxWidth {
		return ellipsis // 极端情况下连 "…" 都放不下
	}
	budget := maxWidth - ellipsisW
	lo, hi := 0, len(runes)
	for lo < hi {
		mid := (lo + hi + 1) / 2
		if td.MeasureString(string(runes[:mid]), fontSize) <= budget {
			lo = mid
		} else {
			hi = mid - 1
		}
	}
	if lo <= 0 {
		return ellipsis
	}
	return string(runes[:lo]) + ellipsis
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
