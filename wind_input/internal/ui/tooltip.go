// Package ui provides native Windows UI for candidate window
package ui

import (
	"image"
	"image/color"
	"log/slog"
	"sync"
	"syscall"
	"unsafe"

	"github.com/huanfeng/wind_input/internal/theme"
	"github.com/fogleman/gg"
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
}

// NewTooltipWindow creates a new tooltip window
func NewTooltipWindow(logger *slog.Logger) *TooltipWindow {
	return &TooltipWindow{
		logger: logger,
	}
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
var (
	tooltipWindowsMu sync.RWMutex
	tooltipWindows   = make(map[windows.HWND]*TooltipWindow)
)

func registerTooltipWindow(hwnd windows.HWND, w *TooltipWindow) {
	tooltipWindowsMu.Lock()
	tooltipWindows[hwnd] = w
	tooltipWindowsMu.Unlock()
}

func unregisterTooltipWindow(hwnd windows.HWND) {
	tooltipWindowsMu.Lock()
	delete(tooltipWindows, hwnd)
	tooltipWindowsMu.Unlock()
}

// tooltipWndProc is the window procedure for tooltip
func tooltipWndProc(hwnd uintptr, msg uint32, wParam, lParam uintptr) uintptr {
	switch msg {
	case WM_DESTROY:
		unregisterTooltipWindow(windows.HWND(hwnd))
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
	className, _ := syscall.UTF16PtrFromString("IMETooltipWindow")

	wc := WNDCLASSEXW{
		CbSize:        uint32(unsafe.Sizeof(WNDCLASSEXW{})),
		LpfnWndProc:   syscall.NewCallback(tooltipWndProc),
		LpszClassName: className,
	}

	ret, _, err := procRegisterClassExW.Call(uintptr(unsafe.Pointer(&wc)))
	if ret == 0 {
		w.logger.Warn("RegisterClassExW for tooltip failed (may already exist)", "error", err)
	}

	// Create layered window
	exStyle := uint32(WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT)
	style := uint32(WS_POPUP)

	hwnd, _, err := procCreateWindowExW.Call(
		uintptr(exStyle),
		uintptr(unsafe.Pointer(className)),
		0, // No title
		uintptr(style),
		0, 0, 1, 1, // Initial position and size
		0, 0, 0, 0,
	)

	if hwnd == 0 {
		return err
	}

	w.hwnd = windows.HWND(hwnd)
	registerTooltipWindow(w.hwnd, w)
	w.logger.Debug("Tooltip window created", "hwnd", hwnd)

	return nil
}

// Show shows the tooltip at the specified position with the given text
func (w *TooltipWindow) Show(text string, x, y int) {
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
}

// render renders the tooltip text to an image
func (w *TooltipWindow) render(text string) *image.RGBA {
	scale := GetDPIScale()
	bgColor, textColor := w.getTooltipColors()

	// Measure text
	fontSize := 14.0 * scale
	padding := 6.0 * scale

	dc := gg.NewContext(1, 1)
	// Simple font loading
	fontPath := "C:/Windows/Fonts/simhei.ttf"
	if err := dc.LoadFontFace(fontPath, fontSize); err != nil {
		// Fallback
		fontPath = "C:/Windows/Fonts/arial.ttf"
		if err := dc.LoadFontFace(fontPath, fontSize); err != nil {
			w.logger.Warn("Failed to load font for tooltip", "error", err)
			return nil
		}
	}

	tw, _ := dc.MeasureString(text)
	width := tw + padding*2
	height := fontSize + padding*2

	// Create actual context
	dc = gg.NewContext(int(width), int(height))
	if err := dc.LoadFontFace(fontPath, fontSize); err != nil {
		return nil
	}

	// Draw background
	dc.SetColor(bgColor)
	dc.DrawRoundedRectangle(0, 0, width, height, 4*scale)
	dc.Fill()

	// Draw text
	dc.SetColor(textColor)
	dc.DrawString(text, padding, padding+fontSize*0.8)

	return dc.Image().(*image.RGBA)
}

// updateLayeredWindow updates the tooltip's layered window
func (w *TooltipWindow) updateLayeredWindow(img *image.RGBA, x, y int) error {
	bounds := img.Bounds()
	width := bounds.Dx()
	height := bounds.Dy()

	// Get screen DC
	hdcScreen, _, _ := procGetDC.Call(0)
	if hdcScreen == 0 {
		return nil
	}
	defer procReleaseDC.Call(0, hdcScreen)

	// Create compatible DC
	hdcMem, _, _ := procCreateCompatibleDC.Call(hdcScreen)
	if hdcMem == 0 {
		return nil
	}
	defer procDeleteDC.Call(hdcMem)

	// Create DIB section
	bi := BITMAPINFO{
		BmiHeader: BITMAPINFOHEADER{
			BiSize:        uint32(unsafe.Sizeof(BITMAPINFOHEADER{})),
			BiWidth:       int32(width),
			BiHeight:      -int32(height), // Top-down DIB
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
		return nil
	}
	defer procDeleteObject.Call(hBitmap)

	// Select bitmap into DC
	procSelectObject.Call(hdcMem, hBitmap)

	// Copy image data to DIB (convert RGBA to BGRA with premultiplied alpha)
	pixelCount := width * height
	dstSlice := unsafe.Slice((*byte)(bits), pixelCount*4)

	for i := 0; i < pixelCount; i++ {
		srcIdx := i * 4
		dstIdx := i * 4

		r := img.Pix[srcIdx+0]
		g := img.Pix[srcIdx+1]
		b := img.Pix[srcIdx+2]
		a := img.Pix[srcIdx+3]

		// Premultiply alpha
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

	procUpdateLayeredWindow.Call(
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

	return nil
}
