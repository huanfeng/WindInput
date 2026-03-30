package ui

// Pure Go DirectWrite renderer — calls system dwrite.dll via COM vtable,
// no custom wind_dwrite.dll dependency.
// Reference: github.com/huanfeng/go-wui/platform/windows/dwrite.go

import (
	"image"
	"image/color"
	"log/slog"
	"math"
	"sync"
	"sync/atomic"
	"syscall"
	"unsafe"
)

// ═══════════════════════════════════════════════════════════════
// COM infrastructure
// ═══════════════════════════════════════════════════════════════

type dwGUID struct {
	Data1 uint32
	Data2 uint16
	Data3 uint16
	Data4 [8]byte
}

var dwIIDFactory = dwGUID{
	0xb859ee5a, 0xd838, 0x4b5b,
	[8]byte{0xa2, 0xe8, 0x1a, 0xdc, 0x7d, 0x93, 0xdb, 0x48},
}

// dwComCall invokes a COM method by vtable index.
func dwComCall(obj uintptr, vtableIndex int, args ...uintptr) (uintptr, error) {
	vtablePtr := *(*uintptr)(unsafe.Pointer(obj))
	methodPtr := *(*uintptr)(unsafe.Pointer(vtablePtr + uintptr(vtableIndex)*unsafe.Sizeof(uintptr(0))))
	allArgs := make([]uintptr, 0, 1+len(args))
	allArgs = append(allArgs, obj)
	allArgs = append(allArgs, args...)
	ret, _, _ := syscall.SyscallN(methodPtr, allArgs...)
	if int32(ret) < 0 {
		return ret, syscall.Errno(ret)
	}
	return ret, nil
}

// dwComRelease calls IUnknown::Release (vtable index 2).
func dwComRelease(obj uintptr) {
	if obj != 0 {
		dwComCall(obj, 2)
	}
}

// ═══════════════════════════════════════════════════════════════
// DirectWrite constants and types
// ═══════════════════════════════════════════════════════════════

const (
	dwFactoryTypeShared = 0
	dwFontStyleNormal   = 0
	dwFontStretchNormal = 5
)

// IDWriteFactory vtable indices.
const (
	dwVtCreateRenderingParams = 10
	dwVtCreateTextFormat      = 15
	dwVtGetGdiInterop         = 17
	dwVtCreateTextLayout      = 18
)

// IDWriteTextLayout vtable indices (IDWriteTextFormat occupies 0–27).
const (
	dwLayoutVtDraw           = 58
	dwLayoutVtGetLineMetrics = 59
	dwLayoutVtGetMetrics     = 60
)

// IDWriteGdiInterop vtable.
const dwGdiVtCreateBitmapRenderTarget = 7

// IDWriteBitmapRenderTarget vtable.
const (
	dwBmpVtDrawGlyphRun    = 3
	dwBmpVtGetMemoryDC     = 4
	dwBmpVtSetPixelsPerDip = 6
)

const dwObjBitmap = 7

type dwTextMetrics struct {
	Left                             float32
	Top                              float32
	Width                            float32
	WidthIncludingTrailingWhitespace float32
	Height                           float32
	LayoutWidth                      float32
	LayoutHeight                     float32
	MaxBidiReorderingDepth           uint32
	LineCount                        uint32
}

type dwLineMetrics struct {
	Length                   uint32
	TrailingWhitespaceLength uint32
	NewlineLength            uint32
	Height                   float32
	Baseline                 float32
	IsTrimmed                int32
}

// ═══════════════════════════════════════════════════════════════
// Go-implemented IDWriteTextRenderer COM object
// ═══════════════════════════════════════════════════════════════

// IID constants for QueryInterface.
var (
	dwIIDUnknown = dwGUID{
		0x00000000, 0x0000, 0x0000,
		[8]byte{0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46},
	}
	dwIIDTextRenderer = dwGUID{
		0xef8a8135, 0x5cc6, 0x45fe,
		[8]byte{0x88, 0x25, 0xc5, 0xa0, 0x72, 0x4e, 0xb8, 0x19},
	}
	dwIIDPixelSnapping = dwGUID{
		0xeaf3a2da, 0xecf4, 0x4d24,
		[8]byte{0xb6, 0x44, 0xb3, 0x4f, 0x68, 0x42, 0x02, 0x4b},
	}
)

func dwGUIDEqual(a, b *dwGUID) bool {
	return a.Data1 == b.Data1 && a.Data2 == b.Data2 && a.Data3 == b.Data3 && a.Data4 == b.Data4
}

// dwriteMatrix is used by GetCurrentTransform (identity matrix).
type dwriteMatrix struct {
	M11, M12, M21, M22, Dx, Dy float32
}

// goTextRendererVtable is the COM vtable for IDWriteTextRenderer.
// Layout: IUnknown(3) + IDWritePixelSnapping(3) + IDWriteTextRenderer(4) = 10.
type goTextRendererVtable struct {
	QueryInterface          uintptr
	AddRef                  uintptr
	Release                 uintptr
	IsPixelSnappingDisabled uintptr
	GetCurrentTransform     uintptr
	GetPixelsPerDip         uintptr
	DrawGlyphRun            uintptr
	DrawUnderline           uintptr
	DrawStrikethrough       uintptr
	DrawInlineObject        uintptr
}

// goTextRenderer is a Go-implemented IDWriteTextRenderer COM object.
// The vtable pointer MUST be the first field (COM convention).
type goTextRenderer struct {
	vtable       *goTextRendererVtable // must be first field
	refCount     int32
	bitmapTarget uintptr // IDWriteBitmapRenderTarget*
	renderParams uintptr // IDWriteRenderingParams*
	textColor    uint32  // COLORREF (0x00BBGGRR)
}

// Global vtable — initialized once, shared by all goTextRenderer instances.
var (
	globalDWTextRendererVtable     *goTextRendererVtable
	globalDWTextRendererVtableOnce sync.Once
)

func initDWTextRendererVtable() *goTextRendererVtable {
	globalDWTextRendererVtableOnce.Do(func() {
		globalDWTextRendererVtable = &goTextRendererVtable{
			QueryInterface:          syscall.NewCallback(dwTR_QueryInterface),
			AddRef:                  syscall.NewCallback(dwTR_AddRef),
			Release:                 syscall.NewCallback(dwTR_Release),
			IsPixelSnappingDisabled: syscall.NewCallback(dwTR_IsPixelSnappingDisabled),
			GetCurrentTransform:     syscall.NewCallback(dwTR_GetCurrentTransform),
			GetPixelsPerDip:         syscall.NewCallback(dwTR_GetPixelsPerDip),
			DrawGlyphRun:            dwCGODrawGlyphRunCallback(), // CGO bridge — correct float ABI
			DrawUnderline:           syscall.NewCallback(dwTR_DrawUnderline),
			DrawStrikethrough:       syscall.NewCallback(dwTR_DrawStrikethrough),
			DrawInlineObject:        syscall.NewCallback(dwTR_DrawInlineObject),
		}
	})
	return globalDWTextRendererVtable
}

// --- COM callback implementations ---

const dwSOK = 0

func dwTR_QueryInterface(this uintptr, riid uintptr, ppvObject uintptr) uintptr {
	if ppvObject == 0 {
		return 0x80004003 // E_POINTER
	}
	iid := (*dwGUID)(unsafe.Pointer(riid))
	if dwGUIDEqual(iid, &dwIIDUnknown) ||
		dwGUIDEqual(iid, &dwIIDTextRenderer) ||
		dwGUIDEqual(iid, &dwIIDPixelSnapping) {
		*(*uintptr)(unsafe.Pointer(ppvObject)) = this
		dwTR_AddRef(this)
		return dwSOK
	}
	*(*uintptr)(unsafe.Pointer(ppvObject)) = 0
	return 0x80004002 // E_NOINTERFACE
}

func dwTR_AddRef(this uintptr) uintptr {
	tr := (*goTextRenderer)(unsafe.Pointer(this))
	return uintptr(atomic.AddInt32(&tr.refCount, 1))
}

func dwTR_Release(this uintptr) uintptr {
	tr := (*goTextRenderer)(unsafe.Pointer(this))
	return uintptr(atomic.AddInt32(&tr.refCount, -1))
}

func dwTR_IsPixelSnappingDisabled(this uintptr, clientDrawingContext uintptr, isDisabled uintptr) uintptr {
	if isDisabled != 0 {
		*(*int32)(unsafe.Pointer(isDisabled)) = 0 // FALSE — pixel snapping enabled
	}
	return dwSOK
}

func dwTR_GetCurrentTransform(this uintptr, clientDrawingContext uintptr, transform uintptr) uintptr {
	if transform != 0 {
		m := (*dwriteMatrix)(unsafe.Pointer(transform))
		*m = dwriteMatrix{M11: 1.0, M22: 1.0} // identity
	}
	return dwSOK
}

func dwTR_GetPixelsPerDip(this uintptr, clientDrawingContext uintptr, pixelsPerDip uintptr) uintptr {
	if pixelsPerDip != 0 {
		*(*float32)(unsafe.Pointer(pixelsPerDip)) = 1.0
	}
	return dwSOK
}

// dwTR_DrawGlyphRun is NOT used — DrawGlyphRun is handled by the CGO bridge
// (dwrite_cgo_windows.go) which correctly receives float parameters from XMM
// registers via the C trampoline. This stub exists only as documentation.
// The vtable entry points to dwCGODrawGlyphRunCallback() instead.

func dwTR_DrawUnderline(this, clientDrawingContext, baselineOriginX, baselineOriginY, underline, clientDrawingEffect uintptr) uintptr {
	return dwSOK
}

func dwTR_DrawStrikethrough(this, clientDrawingContext, baselineOriginX, baselineOriginY, strikethrough, clientDrawingEffect uintptr) uintptr {
	return dwSOK
}

func dwTR_DrawInlineObject(this, clientDrawingContext, originX, originY, inlineObject uintptr, isSideways, isRightToLeft uintptr, clientDrawingEffect uintptr) uintptr {
	return dwSOK
}

// ═══════════════════════════════════════════════════════════════
// Shared initialisation (singleton factory)
// ═══════════════════════════════════════════════════════════════

var (
	dwriteDLL               *syscall.LazyDLL
	procDWriteCreateFactory *syscall.LazyProc

	// Extra GDI procs for bitmap readback (not declared elsewhere in this package).
	procDWGetDIBits        *syscall.LazyProc
	procDWSetDIBits        *syscall.LazyProc
	procDWGetCurrentObject *syscall.LazyProc

	dwriteSharedFactory uintptr
	dwriteInitOnce      sync.Once
	dwriteInitErr       error
)

func initDWriteShared() error {
	dwriteInitOnce.Do(func() {
		dwriteDLL = syscall.NewLazyDLL("dwrite.dll")
		procDWriteCreateFactory = dwriteDLL.NewProc("DWriteCreateFactory")
		if err := procDWriteCreateFactory.Find(); err != nil {
			dwriteInitErr = err
			return
		}

		gdi := syscall.NewLazyDLL("gdi32.dll")
		procDWGetDIBits = gdi.NewProc("GetDIBits")
		procDWSetDIBits = gdi.NewProc("SetDIBits")
		procDWGetCurrentObject = gdi.NewProc("GetCurrentObject")

		var factory uintptr
		hr, _, _ := procDWriteCreateFactory.Call(
			dwFactoryTypeShared,
			uintptr(unsafe.Pointer(&dwIIDFactory)),
			uintptr(unsafe.Pointer(&factory)),
		)
		if int32(hr) < 0 || factory == 0 {
			dwriteInitErr = syscall.Errno(hr)
			return
		}
		dwriteSharedFactory = factory
	})
	return dwriteInitErr
}

// ═══════════════════════════════════════════════════════════════
// GDI-interop backend (bitmap render target for pixel readback)
// ═══════════════════════════════════════════════════════════════

type dwBackend struct {
	gdiInterop    uintptr // IDWriteGdiInterop*
	bitmapTarget  uintptr // IDWriteBitmapRenderTarget*
	renderParams  uintptr // IDWriteRenderingParams*
	renderer      *goTextRenderer
	width, height int
}

func newDWBackend(factory uintptr) (*dwBackend, error) {
	b := &dwBackend{}

	// IDWriteFactory::GetGdiInterop (vtable 17)
	var gdiInterop uintptr
	_, err := dwComCall(factory, dwVtGetGdiInterop, uintptr(unsafe.Pointer(&gdiInterop)))
	if err != nil {
		return nil, err
	}
	b.gdiInterop = gdiInterop

	// IDWriteFactory::CreateRenderingParams (vtable 10)
	var renderParams uintptr
	_, err = dwComCall(factory, dwVtCreateRenderingParams, uintptr(unsafe.Pointer(&renderParams)))
	if err != nil {
		dwComRelease(gdiInterop)
		return nil, err
	}
	b.renderParams = renderParams

	// Create the Go text renderer COM object.
	vtable := initDWTextRendererVtable()
	b.renderer = &goTextRenderer{
		vtable:       vtable,
		refCount:     1,
		renderParams: renderParams,
	}

	return b, nil
}

func (b *dwBackend) ensureSize(w, h int) error {
	// Always recreate at exact size to avoid SetDIBits/GetDIBits width mismatch.
	if w == b.width && h == b.height && b.bitmapTarget != 0 {
		return nil
	}
	if b.bitmapTarget != 0 {
		dwComRelease(b.bitmapTarget)
		b.bitmapTarget = 0
	}
	var target uintptr
	_, err := dwComCall(b.gdiInterop, dwGdiVtCreateBitmapRenderTarget,
		0, // hdc = NULL → use screen DC
		uintptr(uint32(w)),
		uintptr(uint32(h)),
		uintptr(unsafe.Pointer(&target)),
	)
	if err != nil {
		return err
	}
	b.bitmapTarget = target
	b.width = w
	b.height = h
	// Force 1:1 pixel mapping — the default uses system DPI which would
	// scale the rendered text (e.g., 1.5x at 150% DPI).
	dwComCall(target, dwBmpVtSetPixelsPerDip, uintptr(math.Float32bits(1.0)))
	// Update renderer's bitmapTarget pointer.
	if b.renderer != nil {
		b.renderer.bitmapTarget = target
	}
	return nil
}

func (b *dwBackend) getMemoryDC() uintptr {
	if b.bitmapTarget == 0 {
		return 0
	}
	dc, _ := dwComCall(b.bitmapTarget, dwBmpVtGetMemoryDC)
	return dc
}

// copyFromImage copies an RGBA region from the Go image into the GDI bitmap
// as BGRA, so that GDI text rendering blends with the real background.
func (b *dwBackend) copyFromImage(src *image.RGBA, srcX, srcY, w, h int) {
	memDC := b.getMemoryDC()
	if memDC == 0 {
		return
	}
	hBitmap, _, _ := procDWGetCurrentObject.Call(memDC, dwObjBitmap)
	if hBitmap == 0 {
		return
	}

	bounds := src.Bounds()
	stride := w * 4
	buf := make([]byte, stride*h)

	for py := 0; py < h; py++ {
		imgY := srcY + py
		for px := 0; px < w; px++ {
			imgX := srcX + px
			di := py*stride + px*4
			if imgX >= bounds.Min.X && imgX < bounds.Max.X &&
				imgY >= bounds.Min.Y && imgY < bounds.Max.Y {
				si := (imgY-bounds.Min.Y)*src.Stride + (imgX-bounds.Min.X)*4
				// RGBA → BGRA
				buf[di+0] = src.Pix[si+2] // B
				buf[di+1] = src.Pix[si+1] // G
				buf[di+2] = src.Pix[si+0] // R
				buf[di+3] = src.Pix[si+3] // A (preserved but unused by GDI)
			}
			// Out-of-bounds pixels stay zero (black).
		}
	}

	bmi := BITMAPINFO{
		BmiHeader: BITMAPINFOHEADER{
			BiSize:        uint32(unsafe.Sizeof(BITMAPINFOHEADER{})),
			BiWidth:       int32(w),
			BiHeight:      -int32(h), // top-down
			BiPlanes:      1,
			BiBitCount:    32,
			BiCompression: BI_RGB,
		},
	}
	procDWSetDIBits.Call(
		memDC, hBitmap,
		0, uintptr(h),
		uintptr(unsafe.Pointer(&buf[0])),
		uintptr(unsafe.Pointer(&bmi)),
		DIB_RGB_COLORS,
	)
}

// copyToImageRGB reads BGRA pixels from the GDI bitmap and writes only
// the R,G,B channels back to the target image, preserving the original alpha.
// This matches the C++ shim's approach: ClearType renders directly on the
// real background, so no alpha extraction is needed.
func (b *dwBackend) copyToImageRGB(dst *image.RGBA, dstX, dstY, w, h int) {
	memDC := b.getMemoryDC()
	if memDC == 0 {
		return
	}
	hBitmap, _, _ := procDWGetCurrentObject.Call(memDC, dwObjBitmap)
	if hBitmap == 0 {
		return
	}
	bmi := BITMAPINFO{
		BmiHeader: BITMAPINFOHEADER{
			BiSize:        uint32(unsafe.Sizeof(BITMAPINFOHEADER{})),
			BiWidth:       int32(w),
			BiHeight:      -int32(h), // top-down
			BiPlanes:      1,
			BiBitCount:    32,
			BiCompression: BI_RGB,
		},
	}
	stride := w * 4
	pixels := make([]byte, stride*h)
	ret, _, _ := procDWGetDIBits.Call(
		memDC, hBitmap,
		0, uintptr(h),
		uintptr(unsafe.Pointer(&pixels[0])),
		uintptr(unsafe.Pointer(&bmi)),
		DIB_RGB_COLORS,
	)
	if ret == 0 {
		return
	}

	bounds := dst.Bounds()
	for py := 0; py < h; py++ {
		imgY := dstY + py
		if imgY < bounds.Min.Y || imgY >= bounds.Max.Y {
			continue
		}
		for px := 0; px < w; px++ {
			imgX := dstX + px
			if imgX < bounds.Min.X || imgX >= bounds.Max.X {
				continue
			}
			si := py*stride + px*4
			di := (imgY-bounds.Min.Y)*dst.Stride + (imgX-bounds.Min.X)*4
			// BGRA → RGB (keep original alpha)
			dst.Pix[di+0] = pixels[si+2] // R
			dst.Pix[di+1] = pixels[si+1] // G
			dst.Pix[di+2] = pixels[si+0] // B
			// dst.Pix[di+3] unchanged — preserve original alpha
		}
	}
}

func (b *dwBackend) close() {
	b.renderer = nil
	if b.bitmapTarget != 0 {
		dwComRelease(b.bitmapTarget)
		b.bitmapTarget = 0
	}
	if b.renderParams != 0 {
		dwComRelease(b.renderParams)
		b.renderParams = 0
	}
	if b.gdiInterop != 0 {
		dwComRelease(b.gdiInterop)
		b.gdiInterop = 0
	}
}

// ═══════════════════════════════════════════════════════════════
// Text format / layout helpers
// ═══════════════════════════════════════════════════════════════

func dwCreateTextFormat(factory uintptr, family string, weight int, fontSize float64) (uintptr, error) {
	familyW, _ := syscall.UTF16PtrFromString(family)
	localeW, _ := syscall.UTF16PtrFromString("en-us")
	var textFormat uintptr
	_, err := dwComCall(factory, dwVtCreateTextFormat,
		uintptr(unsafe.Pointer(familyW)),
		0, // fontCollection = NULL (system default)
		uintptr(weight),
		dwFontStyleNormal,
		dwFontStretchNormal,
		uintptr(math.Float32bits(float32(fontSize))),
		uintptr(unsafe.Pointer(localeW)),
		uintptr(unsafe.Pointer(&textFormat)),
	)
	if err != nil {
		return 0, err
	}
	return textFormat, nil
}

func dwCreateTextLayout(factory uintptr, textFormat uintptr, text string) (uintptr, error) {
	textUTF16, _ := syscall.UTF16FromString(text)
	var layout uintptr
	_, err := dwComCall(factory, dwVtCreateTextLayout,
		uintptr(unsafe.Pointer(&textUTF16[0])),
		uintptr(uint32(len(textUTF16)-1)), // exclude NUL
		textFormat,
		uintptr(math.Float32bits(float32(100000))), // maxWidth (huge → single line)
		uintptr(math.Float32bits(float32(100000))), // maxHeight
		uintptr(unsafe.Pointer(&layout)),
	)
	if err != nil {
		return 0, err
	}
	return layout, nil
}

func dwGetTextMetrics(layout uintptr) (dwTextMetrics, error) {
	var m dwTextMetrics
	_, err := dwComCall(layout, dwLayoutVtGetMetrics, uintptr(unsafe.Pointer(&m)))
	return m, err
}

func dwGetLineMetrics(layout uintptr) ([]dwLineMetrics, error) {
	var lineCount uint32
	dwComCall(layout, dwLayoutVtGetLineMetrics, 0, 0, uintptr(unsafe.Pointer(&lineCount)))
	if lineCount == 0 {
		return nil, nil
	}
	lines := make([]dwLineMetrics, lineCount)
	var actualCount uint32
	_, err := dwComCall(layout, dwLayoutVtGetLineMetrics,
		uintptr(unsafe.Pointer(&lines[0])),
		uintptr(lineCount),
		uintptr(unsafe.Pointer(&actualCount)),
	)
	if err != nil {
		return nil, err
	}
	return lines[:actualCount], nil
}

// ═══════════════════════════════════════════════════════════════
// DWriteRenderer — public API (unchanged, no DLL dependency)
// ═══════════════════════════════════════════════════════════════

const (
	dwriteFontWeightNormal = 400
	dwriteSymbolFont       = "Segoe UI Symbol"
)

var (
	dwriteRefsMu        sync.Mutex
	dwriteActiveHandles int
)

func registerDWriteHandle(component string) {
	dwriteRefsMu.Lock()
	defer dwriteRefsMu.Unlock()
	dwriteActiveHandles++
	slog.Info("DirectWrite renderer handle retained",
		"component", component, "activeHandles", dwriteActiveHandles)
}

func releaseDWriteHandle(component string) {
	dwriteRefsMu.Lock()
	if dwriteActiveHandles > 0 {
		dwriteActiveHandles--
	}
	active := dwriteActiveHandles
	dwriteRefsMu.Unlock()
	slog.Info("DirectWrite renderer handle released",
		"component", component, "activeHandles", active)
}

// DWriteRenderer provides text drawing and measurement using DirectWrite
// via pure Go COM calls to system dwrite.dll (no custom DLL needed).
type DWriteRenderer struct {
	mu sync.Mutex

	component  string
	fontName   string
	fontWeight int
	fontScale  float64

	backend *dwBackend

	// Cached text format (most-recently-used parameters).
	cachedFormat       uintptr
	cachedFormatFamily string
	cachedFormatWeight int
	cachedFormatSize   float64

	loaded         bool
	loadFailed     bool
	statusLogged   bool
	lastLoadReason string

	inDraw bool
	target *image.RGBA
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

func (r *DWriteRenderer) ensureInitLocked() bool {
	if r.loaded {
		return true
	}
	if r.loadFailed {
		return false
	}

	if err := initDWriteShared(); err != nil {
		r.loadFailed = true
		r.lastLoadReason = err.Error()
		if !r.statusLogged {
			slog.Warn("DirectWrite unavailable, fallback to GDI",
				"component", r.component, "error", err)
			r.statusLogged = true
		}
		return false
	}

	backend, err := newDWBackend(dwriteSharedFactory)
	if err != nil {
		r.loadFailed = true
		r.lastLoadReason = err.Error()
		if !r.statusLogged {
			slog.Warn("DirectWrite GDI backend creation failed",
				"component", r.component, "error", err)
			r.statusLogged = true
		}
		return false
	}
	r.backend = backend
	r.loaded = true
	registerDWriteHandle(r.component)
	if !r.statusLogged {
		slog.Info("DirectWrite renderer initialized (pure Go)",
			"component", r.component, "font", r.fontName,
			"weight", r.fontWeight, "scale", r.fontScale)
		r.statusLogged = true
	}
	return true
}

// getFormatLocked returns a cached or freshly-created text format.
func (r *DWriteRenderer) getFormatLocked(family string, weight int, fontSize float64) uintptr {
	if r.cachedFormat != 0 &&
		r.cachedFormatFamily == family &&
		r.cachedFormatWeight == weight &&
		r.cachedFormatSize == fontSize {
		return r.cachedFormat
	}
	if r.cachedFormat != 0 {
		dwComRelease(r.cachedFormat)
		r.cachedFormat = 0
	}
	f, err := dwCreateTextFormat(dwriteSharedFactory, family, weight, fontSize)
	if err != nil {
		return 0
	}
	r.cachedFormat = f
	r.cachedFormatFamily = family
	r.cachedFormatWeight = weight
	r.cachedFormatSize = fontSize
	return f
}

func (r *DWriteRenderer) scaledFontSize(fontSize float64) float64 {
	s := fontSize * r.fontScale
	if s < 1 {
		s = 1
	}
	return s
}

// IsAvailable returns true if DirectWrite can be used.
func (r *DWriteRenderer) IsAvailable() bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.ensureInitLocked()
}

// SetFont sets the font by file path (resolved to a family name).
func (r *DWriteRenderer) SetFont(fontPath string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	name := FontPathToName(fontPath)
	if name == r.fontName {
		return
	}
	r.fontName = name
}

// SetGDIParams updates font weight and scale.
func (r *DWriteRenderer) SetGDIParams(weight int, scale float64) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if weight <= 0 {
		weight = dwriteFontWeightNormal
	}
	if scale <= 0 {
		scale = 1.0
	}
	r.fontWeight = weight
	r.fontScale = scale
}

// MeasureString measures text width for the given font size.
func (r *DWriteRenderer) MeasureString(text string, fontSize float64) float64 {
	if text == "" {
		return 0
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if !r.ensureInitLocked() {
		return 0
	}

	scaledSize := r.scaledFontSize(fontSize)
	family := r.fontName
	if containsSymbolChars(text) {
		family = dwriteSymbolFont
	}

	format := r.getFormatLocked(family, r.fontWeight, scaledSize)
	if format == 0 {
		return 0
	}

	layout, err := dwCreateTextLayout(dwriteSharedFactory, format, text)
	if err != nil || layout == 0 {
		return 0
	}
	defer dwComRelease(layout)

	metrics, err := dwGetTextMetrics(layout)
	if err != nil {
		return 0
	}
	return float64(metrics.WidthIncludingTrailingWhitespace)
}

// BeginDraw starts a batch drawing session on the given image.
func (r *DWriteRenderer) BeginDraw(img *image.RGBA) {
	if img == nil || len(img.Pix) == 0 {
		return
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	r.inDraw = false
	r.target = nil
	if !r.ensureInitLocked() {
		return
	}
	r.target = img
	r.inDraw = true
}

// DrawString draws text at the given baseline position.
func (r *DWriteRenderer) DrawString(text string, x, y float64, fontSize float64, clr color.Color) {
	if text == "" {
		return
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if !r.inDraw || r.target == nil {
		return
	}
	r.drawStringLocked(text, x, y, fontSize, clr, r.fontWeight)
}

// DrawStringWithWeight draws text with a specific font weight (100–900).
func (r *DWriteRenderer) DrawStringWithWeight(text string, x, y float64, fontSize float64, clr color.Color, weight int) {
	if text == "" || weight <= 0 {
		r.DrawString(text, x, y, fontSize, clr)
		return
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if !r.inDraw || r.target == nil {
		return
	}
	r.drawStringLocked(text, x, y, fontSize, clr, weight)
}

func (r *DWriteRenderer) drawStringLocked(text string, x, y float64, fontSize float64, clr color.Color, weight int) {
	scaledSize := r.scaledFontSize(fontSize)
	family := r.fontName
	if containsSymbolChars(text) {
		family = dwriteSymbolFont
	}

	// --- Measurement via DirectWrite ---

	// For DrawStringWithWeight the weight may differ from r.fontWeight.
	// Use a temporary format if that's the case to avoid polluting the cache.
	var format uintptr
	if weight == r.fontWeight {
		format = r.getFormatLocked(family, weight, scaledSize)
	} else {
		var err error
		format, err = dwCreateTextFormat(dwriteSharedFactory, family, weight, scaledSize)
		if err != nil {
			return
		}
		defer dwComRelease(format)
	}
	if format == 0 {
		return
	}

	layout, err := dwCreateTextLayout(dwriteSharedFactory, format, text)
	if err != nil || layout == 0 {
		return
	}
	defer dwComRelease(layout)

	metrics, err := dwGetTextMetrics(layout)
	if err != nil {
		return
	}

	var baseline float64
	if lines, _ := dwGetLineMetrics(layout); len(lines) > 0 {
		baseline = float64(lines[0].Baseline)
	}

	bmpW := int(math.Ceil(float64(metrics.WidthIncludingTrailingWhitespace))) + 4
	bmpH := int(math.Ceil(float64(metrics.Height))) + 4
	if bmpW <= 0 || bmpH <= 0 {
		return
	}

	// --- Rendering via DirectWrite (matches original C++ shim exactly) ---
	// 1. Copy background from target image into GDI bitmap (RGBA→BGRA)
	// 2. IDWriteTextLayout::Draw → goTextRenderer → IDWriteBitmapRenderTarget::DrawGlyphRun
	// 3. Copy result back (BGRA→RGB), preserving original alpha

	dstX := int(math.Round(x))
	dstY := int(math.Round(y - baseline))

	if err := r.backend.ensureSize(bmpW, bmpH); err != nil {
		return
	}
	r.backend.copyFromImage(r.target, dstX, dstY, bmpW, bmpH)

	// Set text color on the renderer: COLORREF = 0x00BBGGRR
	cr, cg, cb, _ := clr.RGBA()
	r.backend.renderer.textColor = uint32(cb>>8)<<16 | uint32(cg>>8)<<8 | uint32(cr>>8)

	// Render via DirectWrite: IDWriteTextLayout::Draw (vtable 58)
	dwComCall(layout, dwLayoutVtDraw,
		0, // clientDrawingContext (NULL)
		uintptr(unsafe.Pointer(r.backend.renderer)), // IDWriteTextRenderer
		uintptr(math.Float32bits(float32(0))),       // originX
		uintptr(math.Float32bits(float32(0))),       // originY
	)

	// Copy result back — only RGB channels, original alpha preserved.
	r.backend.copyToImageRGB(r.target, dstX, dstY, bmpW, bmpH)
}

// EndDraw finishes the drawing session.
func (r *DWriteRenderer) EndDraw() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.inDraw = false
	r.target = nil
}

// Close releases all resources held by this renderer.
func (r *DWriteRenderer) Close() {
	r.mu.Lock()
	defer r.mu.Unlock()

	r.inDraw = false
	r.target = nil

	if r.cachedFormat != 0 {
		dwComRelease(r.cachedFormat)
		r.cachedFormat = 0
	}
	if r.backend != nil {
		r.backend.close()
		r.backend = nil
	}
	if r.loaded {
		releaseDWriteHandle(r.component)
		r.loaded = false
	}
}
