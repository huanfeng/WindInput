package ui

import (
	"image"
	"image/color"
	"log/slog"
	"math"
	"sync"
	"syscall"
	"unsafe"
)

var (
	windDWriteDLL = syscall.NewLazyDLL("wind_dwrite.dll")

	procWindDWriteCreateRenderer  = windDWriteDLL.NewProc("WindDWriteCreateRenderer")
	procWindDWriteDestroyRenderer = windDWriteDLL.NewProc("WindDWriteDestroyRenderer")
	procWindDWriteSetFont         = windDWriteDLL.NewProc("WindDWriteSetFont")
	procWindDWriteSetFontParams   = windDWriteDLL.NewProc("WindDWriteSetFontParams")
	procWindDWriteMeasureString   = windDWriteDLL.NewProc("WindDWriteMeasureString")
	procWindDWriteBeginDraw       = windDWriteDLL.NewProc("WindDWriteBeginDraw")
	procWindDWriteDrawString      = windDWriteDLL.NewProc("WindDWriteDrawString")
	procWindDWriteEndDraw         = windDWriteDLL.NewProc("WindDWriteEndDraw")
	procWindDWriteShutdown        = windDWriteDLL.NewProc("WindDWriteShutdown")
	windDWriteLoadOnce            sync.Once
	windDWriteLoadErr             error
	windDWriteRefsMu              sync.Mutex
	windDWriteActiveHandles       int
)

const (
	dwriteFontWeightNormal = 400
)

func loadWindDWriteDLL() error {
	windDWriteLoadOnce.Do(func() {
		if err := windDWriteDLL.Load(); err != nil {
			windDWriteLoadErr = err
			return
		}
		procs := []*syscall.LazyProc{
			procWindDWriteCreateRenderer,
			procWindDWriteDestroyRenderer,
			procWindDWriteSetFont,
			procWindDWriteSetFontParams,
			procWindDWriteMeasureString,
			procWindDWriteBeginDraw,
			procWindDWriteDrawString,
			procWindDWriteEndDraw,
			procWindDWriteShutdown,
		}
		for _, proc := range procs {
			if err := proc.Find(); err != nil {
				windDWriteLoadErr = err
				return
			}
		}
	})
	return windDWriteLoadErr
}

func boolToUintptr(v bool) uintptr {
	if v {
		return 1
	}
	return 0
}

func registerWindDWriteHandle(component string) {
	windDWriteRefsMu.Lock()
	defer windDWriteRefsMu.Unlock()
	windDWriteActiveHandles++
	slog.Info("DirectWrite renderer handle retained", "component", component, "activeHandles", windDWriteActiveHandles)
}

func releaseWindDWriteHandle(component string) {
	windDWriteRefsMu.Lock()
	if windDWriteActiveHandles > 0 {
		windDWriteActiveHandles--
	}
	active := windDWriteActiveHandles
	windDWriteRefsMu.Unlock()

	slog.Info("DirectWrite renderer handle released", "component", component, "activeHandles", active)

	if active != 0 {
		return
	}

	ret, _, err := procWindDWriteShutdown.Call()
	if ret == 0 {
		slog.Warn("DirectWrite shared resources shutdown failed", "dll", "wind_dwrite.dll", "error", err)
		return
	}
	slog.Info("DirectWrite shared resources released", "dll", "wind_dwrite.dll")
}

// DWriteRenderer provides text drawing and measurement using the native C++ DirectWrite shim.
type DWriteRenderer struct {
	mu sync.Mutex

	component  string
	fontName   string
	fontWeight int
	fontScale  float64

	handle         uintptr
	loaded         bool
	loadFailed     bool
	statusLogged   bool
	lastLoadReason string

	inDraw bool
}

// NewDWriteRenderer creates a new DirectWrite renderer wrapper.
func NewDWriteRenderer(component string) *DWriteRenderer {
	return &DWriteRenderer{
		component:  component,
		fontName:   "Microsoft YaHei",
		fontWeight: dwriteFontWeightNormal,
		fontScale:  1.0,
	}
}

func (r *DWriteRenderer) ensureHandleLocked() bool {
	if r.handle != 0 {
		return true
	}
	if r.loadFailed {
		return false
	}
	if err := loadWindDWriteDLL(); err != nil {
		r.loadFailed = true
		r.lastLoadReason = err.Error()
		if !r.statusLogged {
			slog.Warn("DirectWrite shim unavailable, fallback to GDI", "component", r.component, "dll", "wind_dwrite.dll", "error", err)
			r.statusLogged = true
		}
		return false
	}

	handle, _, _ := procWindDWriteCreateRenderer.Call()
	if handle == 0 {
		r.loadFailed = true
		r.lastLoadReason = "WindDWriteCreateRenderer returned null"
		if !r.statusLogged {
			slog.Warn("DirectWrite renderer creation failed, fallback to GDI", "component", r.component, "dll", "wind_dwrite.dll")
			r.statusLogged = true
		}
		return false
	}

	r.handle = handle
	r.loaded = true
	r.applyConfigLocked()
	registerWindDWriteHandle(r.component)
	if !r.statusLogged {
		slog.Info("DirectWrite renderer initialized", "component", r.component, "dll", "wind_dwrite.dll", "font", r.fontName, "weight", r.fontWeight, "scale", r.fontScale)
		r.statusLogged = true
	}
	return true
}

func (r *DWriteRenderer) applyConfigLocked() {
	if r.handle == 0 {
		return
	}

	fontName, _ := syscall.UTF16PtrFromString(r.fontName)
	procWindDWriteSetFont.Call(r.handle, uintptr(unsafe.Pointer(fontName)))

	scaleBits := math.Float32bits(float32(r.fontScale))
	procWindDWriteSetFontParams.Call(
		r.handle,
		uintptr(r.fontWeight),
		uintptr(scaleBits),
	)
}

// IsAvailable returns true if the shim DLL can be loaded and a renderer handle can be created.
func (r *DWriteRenderer) IsAvailable() bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.ensureHandleLocked()
}

// SetFont sets the font by file path (resolved to a family name for the native shim).
func (r *DWriteRenderer) SetFont(fontPath string) {
	r.mu.Lock()
	defer r.mu.Unlock()

	name := FontPathToName(fontPath)
	if name == r.fontName {
		return
	}
	r.fontName = name
	if r.handle != 0 {
		fontName, _ := syscall.UTF16PtrFromString(name)
		procWindDWriteSetFont.Call(r.handle, uintptr(unsafe.Pointer(fontName)))
	}
}

// SetGDIParams updates font weight and scale (shared config with GDI).
func (r *DWriteRenderer) SetGDIParams(weight int, scale float64) {
	r.mu.Lock()
	defer r.mu.Unlock()

	if weight <= 0 {
		weight = dwriteFontWeightNormal
	}
	if scale <= 0 {
		scale = 1.0
	}
	if weight == r.fontWeight && scale == r.fontScale {
		return
	}

	r.fontWeight = weight
	r.fontScale = scale
	if r.handle != 0 {
		scaleBits := math.Float32bits(float32(scale))
		procWindDWriteSetFontParams.Call(
			r.handle,
			uintptr(weight),
			uintptr(scaleBits),
		)
	}
}

// MeasureString measures text width for the given font size.
func (r *DWriteRenderer) MeasureString(text string, fontSize float64) float64 {
	if text == "" {
		return 0
	}

	r.mu.Lock()
	defer r.mu.Unlock()
	if !r.ensureHandleLocked() {
		return 0
	}

	size := int(math.Round(fontSize))
	textW, _ := syscall.UTF16PtrFromString(text)
	var width int32
	ret, _, _ := procWindDWriteMeasureString.Call(
		r.handle,
		uintptr(unsafe.Pointer(textW)),
		uintptr(size),
		boolToUintptr(containsSymbolChars(text)),
		uintptr(unsafe.Pointer(&width)),
	)
	if ret == 0 {
		return 0
	}
	return float64(width)
}

// BeginDraw starts a batch drawing session on the given image.
func (r *DWriteRenderer) BeginDraw(img *image.RGBA) {
	if img == nil || len(img.Pix) == 0 {
		return
	}

	r.mu.Lock()
	defer r.mu.Unlock()

	if r.inDraw {
		r.endDrawLocked()
	}
	if !r.ensureHandleLocked() {
		return
	}

	bounds := img.Bounds()
	ret, _, _ := procWindDWriteBeginDraw.Call(
		r.handle,
		uintptr(unsafe.Pointer(&img.Pix[0])),
		uintptr(bounds.Dx()),
		uintptr(bounds.Dy()),
		uintptr(img.Stride),
	)
	if ret == 0 {
		return
	}
	r.inDraw = true
}

// DrawString draws text at the given baseline position.
func (r *DWriteRenderer) DrawString(text string, x, y float64, fontSize float64, clr color.Color) {
	if text == "" {
		return
	}

	r.mu.Lock()
	defer r.mu.Unlock()

	if !r.inDraw || r.handle == 0 {
		return
	}

	textW, _ := syscall.UTF16PtrFromString(text)
	cr, cg, cb, ca := clr.RGBA()
	rgba := uint32(byte(cr>>8)) |
		uint32(byte(cg>>8))<<8 |
		uint32(byte(cb>>8))<<16 |
		uint32(byte(ca>>8))<<24

	procWindDWriteDrawString.Call(
		r.handle,
		uintptr(unsafe.Pointer(textW)),
		uintptr(int32(math.Round(x))),
		uintptr(int32(math.Round(y))),
		uintptr(int32(math.Round(fontSize))),
		uintptr(rgba),
		boolToUintptr(containsSymbolChars(text)),
	)
}

// DrawStringWithWeight draws text with a specific font weight (100-900).
// Temporarily switches the font weight for this draw call, then restores it.
func (r *DWriteRenderer) DrawStringWithWeight(text string, x, y float64, fontSize float64, clr color.Color, weight int) {
	if text == "" || weight <= 0 {
		r.DrawString(text, x, y, fontSize, clr)
		return
	}

	r.mu.Lock()
	defer r.mu.Unlock()

	if !r.inDraw || r.handle == 0 {
		return
	}

	// Temporarily switch font weight
	origWeight := r.fontWeight
	if weight != origWeight {
		scaleBits := math.Float32bits(float32(r.fontScale))
		procWindDWriteSetFontParams.Call(r.handle, uintptr(weight), uintptr(scaleBits))
		r.fontWeight = weight
	}

	textW, _ := syscall.UTF16PtrFromString(text)
	cr, cg, cb, ca := clr.RGBA()
	rgba := uint32(byte(cr>>8)) |
		uint32(byte(cg>>8))<<8 |
		uint32(byte(cb>>8))<<16 |
		uint32(byte(ca>>8))<<24

	procWindDWriteDrawString.Call(
		r.handle,
		uintptr(unsafe.Pointer(textW)),
		uintptr(int32(math.Round(x))),
		uintptr(int32(math.Round(y))),
		uintptr(int32(math.Round(fontSize))),
		uintptr(rgba),
		boolToUintptr(containsSymbolChars(text)),
	)

	// Restore original weight
	if weight != origWeight {
		scaleBits := math.Float32bits(float32(r.fontScale))
		procWindDWriteSetFontParams.Call(r.handle, uintptr(origWeight), uintptr(scaleBits))
		r.fontWeight = origWeight
	}
}

func (r *DWriteRenderer) endDrawLocked() {
	if !r.inDraw || r.handle == 0 {
		return
	}
	procWindDWriteEndDraw.Call(r.handle)
	r.inDraw = false
}

// EndDraw finishes the drawing session.
func (r *DWriteRenderer) EndDraw() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.endDrawLocked()
}

// Close releases the native renderer handle.
func (r *DWriteRenderer) Close() {
	r.mu.Lock()
	defer r.mu.Unlock()

	if r.inDraw {
		r.endDrawLocked()
	}
	if r.handle != 0 {
		procWindDWriteDestroyRenderer.Call(r.handle)
		r.handle = 0
		releaseWindDWriteHandle(r.component)
	}
}
