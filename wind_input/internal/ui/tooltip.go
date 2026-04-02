// Package ui provides native Windows UI for candidate window
package ui

import (
	"image"
	"image/color"
	"log/slog"
	"sync"
	"syscall"

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
	text          string
	resolvedTheme *theme.ResolvedTheme

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

// SetFontPath updates the primary font for tooltip rendering.
func (w *TooltipWindow) SetFontPath(path string) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.TextBackendManager.SetFontPath(path)
}

// SetTextRenderMode switches between GDI, FreeType, and DirectWrite text rendering
func (w *TooltipWindow) SetTextRenderMode(mode TextRenderMode) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.TextBackendManager.SetTextRenderMode(mode)
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
	case WM_NCHITTEST:
		// Return HTTRANSPARENT so mouse events pass through
		return ^uintptr(0) // -1 as uintptr
	}
	ret, _, _ := procDefWindowProcW.Call(hwnd, uintptr(msg), wParam, lParam)
	return ret
}

// Create creates the tooltip window (must be called from the UI thread)
func (w *TooltipWindow) Create() error {
	hwnd, err := CreateLayeredWindow(LayeredWindowConfig{
		ClassName:  "IMETooltipWindow",
		WndProc:    syscall.NewCallback(tooltipWndProc),
		ExtraStyle: WS_EX_TRANSPARENT,
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

// Hide hides the tooltip
func (w *TooltipWindow) Hide() {
	if w.hwnd == 0 {
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

// render renders the tooltip text to an image
func (w *TooltipWindow) render(text string) *image.RGBA {
	scale := GetDPIScale()
	bgColor, textColor := w.getTooltipColors()

	w.mu.Lock()
	td := w.TextDrawer()
	w.mu.Unlock()

	fontSize := 14.0 * scale
	padding := 6.0 * scale

	// Measure text width using TextDrawer
	tw := td.MeasureString(text, fontSize)
	width := tw + padding*2
	height := fontSize + padding*2

	// Phase 1: Draw shapes with gg
	dc := gg.NewContext(int(width), int(height))
	dc.SetColor(bgColor)
	dc.DrawRoundedRectangle(0, 0, width, height, 4*scale)
	dc.Fill()

	// Phase 2: Draw text with TextDrawer
	img := dc.Image().(*image.RGBA)
	td.BeginDraw(img)
	td.DrawString(text, padding, padding+fontSize*0.8, fontSize, textColor)
	td.EndDraw()

	DrawDebugBanner(img)
	return img
}

// updateLayeredWindow updates the tooltip's layered window
func (w *TooltipWindow) updateLayeredWindow(img *image.RGBA, x, y int) error {
	return UpdateLayeredWindowFromImage(w.hwnd, img, x, y)
}
