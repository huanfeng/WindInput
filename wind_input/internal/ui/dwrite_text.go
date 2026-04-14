package ui

// Pure Go DirectWrite renderer — calls system dwrite.dll via COM vtable,
// no custom wind_dwrite.dll dependency.
// Reference: github.com/huanfeng/go-wui/platform/windows/dwrite.go

import (
	"fmt"
	"image"
	"image/color"
	"log/slog"
	"math"
	"runtime"
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

// IID for IDWriteFactory2 (Windows 8.1+, color emoji / COLR glyph support).
var dwIIDFactory2 = dwGUID{
	0x0439fc60, 0xca44, 0x4994,
	[8]byte{0x8d, 0xee, 0x3a, 0x9a, 0xf7, 0xb7, 0x32, 0xec},
}

// dwComCall invokes a COM method by vtable index.
// obj is an unsafe.Pointer to the COM object (avoids uintptr→Pointer vet warnings).
func dwComCall(obj unsafe.Pointer, vtableIndex int, args ...uintptr) (uintptr, error) {
	vtbl := *(*unsafe.Pointer)(obj)
	methodPtr := *(*uintptr)(unsafe.Add(vtbl, unsafe.Sizeof(uintptr(0))*uintptr(vtableIndex)))
	allArgs := make([]uintptr, 0, 1+len(args))
	allArgs = append(allArgs, uintptr(obj))
	allArgs = append(allArgs, args...)
	ret, _, _ := syscall.SyscallN(methodPtr, allArgs...)
	if int32(ret) < 0 {
		return ret, syscall.Errno(ret)
	}
	return ret, nil
}

// dwComRelease calls IUnknown::Release (vtable index 2).
func dwComRelease(obj unsafe.Pointer) {
	if obj != nil {
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

// IDWriteFactory2 vtable index (extends IDWriteFactory1 which extends IDWriteFactory).
// Factory(3-23) + Factory1(24-25) + Factory2(26-30).
const (
	dwFactory2VtGetSystemFontFallback     = 26
	dwFactory2VtCreateFontFallbackBuilder = 27
	dwFactory2VtTranslateColorGlyphRun    = 28
)

// IDWriteFontFallbackBuilder vtable indices.
// IUnknown(0-2) + AddMapping(3) + AddMappings(4) + CreateFontFallback(5).
const (
	dwFallbackBuilderVtAddMapping         = 3
	dwFallbackBuilderVtAddMappings        = 4
	dwFallbackBuilderVtCreateFontFallback = 5
)

// dwUnicodeRange matches DWRITE_UNICODE_RANGE.
type dwUnicodeRange struct {
	first, last uint32
}

// IDWriteTextLayout2 vtable index for SetFontFallback.
// IDWriteTextFormat(0-27) + IDWriteTextLayout(28-66) + IDWriteTextLayout1(67-70) + IDWriteTextLayout2(71-79).
const dwLayout2VtSetFontFallback = 78

// IID for IDWriteTextLayout2 (Windows 8.1+).
var dwIIDTextLayout2 = dwGUID{
	0x1093c18f, 0x8d5e, 0x43f0,
	[8]byte{0xb0, 0x64, 0x09, 0x17, 0x31, 0x1b, 0x52, 0x5e},
}

// IDWriteColorGlyphRunEnumerator vtable indices.
// IUnknown(0-2) + MoveNext(3) + GetCurrentRun(4).
const (
	dwColorEnumVtMoveNext      = 3
	dwColorEnumVtGetCurrentRun = 4
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

// dwColorGlyphRun maps DWRITE_COLOR_GLYPH_RUN (Windows 8.1+).
// The struct starts with an embedded DWRITE_GLYPH_RUN so the pointer
// can be passed directly to IDWriteBitmapRenderTarget::DrawGlyphRun.
type dwColorGlyphRun struct {
	// DWRITE_GLYPH_RUN (48 bytes on x64)
	FontFace      unsafe.Pointer // IDWriteFontFace*
	FontEmSize    float32
	GlyphCount    uint32
	GlyphIndices  unsafe.Pointer // const UINT16*
	GlyphAdvances unsafe.Pointer // const FLOAT*
	GlyphOffsets  unsafe.Pointer // const DWRITE_GLYPH_OFFSET*
	IsSideways    int32          // BOOL
	BidiLevel     uint32
	// DWRITE_COLOR_GLYPH_RUN additional fields
	GlyphRunDescription unsafe.Pointer
	BaselineOriginX     float32
	BaselineOriginY     float32
	RunColorR           float32
	RunColorG           float32
	RunColorB           float32
	RunColorA           float32
	PaletteIndex        uint16
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
	bitmapTarget unsafe.Pointer // IDWriteBitmapRenderTarget*
	renderParams unsafe.Pointer // IDWriteRenderingParams*
	textColor    uint32         // COLORREF (0x00BBGGRR)
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
// Parameters use unsafe.Pointer (not uintptr) to satisfy go vet's unsafeptr check.
// syscall.NewCallback supports pointer parameter types.

const dwSOK = 0

func dwTR_QueryInterface(this unsafe.Pointer, riid unsafe.Pointer, ppvObject unsafe.Pointer) uintptr {
	if ppvObject == nil {
		return 0x80004003 // E_POINTER
	}
	iid := (*dwGUID)(riid)
	if dwGUIDEqual(iid, &dwIIDUnknown) ||
		dwGUIDEqual(iid, &dwIIDTextRenderer) ||
		dwGUIDEqual(iid, &dwIIDPixelSnapping) {
		*(*unsafe.Pointer)(ppvObject) = this
		dwTR_AddRef(this)
		return dwSOK
	}
	*(*unsafe.Pointer)(ppvObject) = nil
	return 0x80004002 // E_NOINTERFACE
}

func dwTR_AddRef(this unsafe.Pointer) uintptr {
	tr := (*goTextRenderer)(this)
	return uintptr(atomic.AddInt32(&tr.refCount, 1))
}

func dwTR_Release(this unsafe.Pointer) uintptr {
	tr := (*goTextRenderer)(this)
	return uintptr(atomic.AddInt32(&tr.refCount, -1))
}

func dwTR_IsPixelSnappingDisabled(this unsafe.Pointer, clientDrawingContext unsafe.Pointer, isDisabled unsafe.Pointer) uintptr {
	if isDisabled != nil {
		*(*int32)(isDisabled) = 0 // FALSE — pixel snapping enabled
	}
	return dwSOK
}

func dwTR_GetCurrentTransform(this unsafe.Pointer, clientDrawingContext unsafe.Pointer, transform unsafe.Pointer) uintptr {
	if transform != nil {
		m := (*dwriteMatrix)(transform)
		*m = dwriteMatrix{M11: 1.0, M22: 1.0} // identity
	}
	return dwSOK
}

func dwTR_GetPixelsPerDip(this unsafe.Pointer, clientDrawingContext unsafe.Pointer, pixelsPerDip unsafe.Pointer) uintptr {
	if pixelsPerDip != nil {
		*(*float32)(pixelsPerDip) = 1.0
	}
	return dwSOK
}

// dwTR_DrawGlyphRun is NOT used — DrawGlyphRun is handled by the CGO bridge
// (dwrite_cgo_windows.go) which correctly receives float parameters from XMM
// registers via the C trampoline. The vtable entry points to dwCGODrawGlyphRunCallback().

func dwTR_DrawUnderline(this, clientDrawingContext unsafe.Pointer, baselineOriginX, baselineOriginY uintptr, underline, clientDrawingEffect unsafe.Pointer) uintptr {
	return dwSOK
}

func dwTR_DrawStrikethrough(this, clientDrawingContext unsafe.Pointer, baselineOriginX, baselineOriginY uintptr, strikethrough, clientDrawingEffect unsafe.Pointer) uintptr {
	return dwSOK
}

func dwTR_DrawInlineObject(this, clientDrawingContext unsafe.Pointer, originX, originY uintptr, inlineObject unsafe.Pointer, isSideways, isRightToLeft uintptr, clientDrawingEffect unsafe.Pointer) uintptr {
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

	dwriteSharedFactory      unsafe.Pointer
	dwriteSharedFactory2     unsafe.Pointer // IDWriteFactory2*, nil if color glyphs unavailable
	dwriteSystemFontFallback unsafe.Pointer // IDWriteFontFallback* for rare character fallback
	dwriteInitOnce           sync.Once
	dwriteInitErr            error
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

		var factory unsafe.Pointer
		hr, _, _ := procDWriteCreateFactory.Call(
			dwFactoryTypeShared,
			uintptr(unsafe.Pointer(&dwIIDFactory)),
			uintptr(unsafe.Pointer(&factory)),
		)
		if int32(hr) < 0 || factory == nil {
			dwriteInitErr = syscall.Errno(hr)
			return
		}
		dwriteSharedFactory = factory

		// Try to get IDWriteFactory2 for color emoji support (COLR/CPAL fonts).
		// Optional — if unavailable, emoji renders as monochrome silhouettes.
		var factory2 unsafe.Pointer
		if ret, _ := dwComCall(factory, 0, // IUnknown::QueryInterface
			uintptr(unsafe.Pointer(&dwIIDFactory2)),
			uintptr(unsafe.Pointer(&factory2)),
		); ret == 0 && factory2 != nil {
			dwriteSharedFactory2 = factory2
			slog.Info("DirectWrite color glyph support available (IDWriteFactory2)")

			// Build custom font fallback with explicit CJK font mappings + system fallback as base.
			// This ensures rare CJK characters (e.g., U+9FB6) fall back to SimSun/MingLiU
			// even if the system default fallback doesn't map to them.
			if customFallback := dwBuildCustomFontFallback(factory2); customFallback != nil {
				dwriteSystemFontFallback = customFallback
				slog.Info("DirectWrite custom font fallback built (CJK + system)")
			} else {
				// Fall back to system font fallback if custom build fails
				var fontFallback unsafe.Pointer
				if ret2, _ := dwComCall(factory2, dwFactory2VtGetSystemFontFallback,
					uintptr(unsafe.Pointer(&fontFallback)),
				); ret2 == 0 && fontFallback != nil {
					dwriteSystemFontFallback = fontFallback
					slog.Info("DirectWrite system font fallback available (fallback path)")
				}
			}
		}
	})
	return dwriteInitErr
}

// ═══════════════════════════════════════════════════════════════
// GDI-interop backend (bitmap render target for pixel readback)
// ═══════════════════════════════════════════════════════════════

type dwBackend struct {
	gdiInterop    unsafe.Pointer // IDWriteGdiInterop*
	bitmapTarget  unsafe.Pointer // IDWriteBitmapRenderTarget*
	renderParams  unsafe.Pointer // IDWriteRenderingParams*
	renderer      *goTextRenderer
	width, height int
}

func newDWBackend(factory unsafe.Pointer) (*dwBackend, error) {
	b := &dwBackend{}

	// IDWriteFactory::GetGdiInterop (vtable 17)
	var gdiInterop unsafe.Pointer
	_, err := dwComCall(factory, dwVtGetGdiInterop, uintptr(unsafe.Pointer(&gdiInterop)))
	if err != nil {
		return nil, err
	}
	b.gdiInterop = gdiInterop

	// IDWriteFactory::CreateRenderingParams (vtable 10)
	var renderParams unsafe.Pointer
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
	if w == b.width && h == b.height && b.bitmapTarget != nil {
		return nil
	}
	if b.bitmapTarget != nil {
		dwComRelease(b.bitmapTarget)
		b.bitmapTarget = nil
	}
	var target unsafe.Pointer
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
	if b.bitmapTarget == nil {
		return 0
	}
	dc, _ := dwComCall(b.bitmapTarget, dwBmpVtGetMemoryDC)
	return dc
}

// copyFromImage copies an RGBA region from the Go image into the GDI bitmap
// as BGRA, so that DirectWrite text rendering blends with the real background.
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
		}
	}
}

func (b *dwBackend) close() {
	b.renderer = nil
	if b.bitmapTarget != nil {
		dwComRelease(b.bitmapTarget)
		b.bitmapTarget = nil
	}
	if b.renderParams != nil {
		dwComRelease(b.renderParams)
		b.renderParams = nil
	}
	if b.gdiInterop != nil {
		dwComRelease(b.gdiInterop)
		b.gdiInterop = nil
	}
}

// ═══════════════════════════════════════════════════════════════
// Text format / layout helpers
// ═══════════════════════════════════════════════════════════════

func dwCreateTextFormat(factory unsafe.Pointer, family string, weight int, fontSize float64) (unsafe.Pointer, error) {
	familyW, _ := syscall.UTF16PtrFromString(family)
	localeW, _ := syscall.UTF16PtrFromString("en-us")
	var textFormat unsafe.Pointer
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
		return nil, err
	}
	return textFormat, nil
}

func dwCreateTextLayout(factory unsafe.Pointer, textFormat unsafe.Pointer, text string) (unsafe.Pointer, error) {
	textUTF16, _ := syscall.UTF16FromString(text)
	var layout unsafe.Pointer
	_, err := dwComCall(factory, dwVtCreateTextLayout,
		uintptr(unsafe.Pointer(&textUTF16[0])),
		uintptr(uint32(len(textUTF16)-1)), // exclude NUL
		uintptr(unsafe.Pointer(textFormat)),
		uintptr(math.Float32bits(float32(100000))), // maxWidth (huge → single line)
		uintptr(math.Float32bits(float32(100000))), // maxHeight
		uintptr(unsafe.Pointer(&layout)),
	)
	if err != nil {
		return nil, err
	}

	// Set system font fallback on the layout for rare character support
	// (CJK Extended B-H, etc.). Requires IDWriteTextLayout2 (Windows 8.1+).
	dwSetSystemFontFallback(layout)

	return layout, nil
}

// dwBuildCustomFontFallback creates a font fallback that maps CJK Unicode ranges
// to comprehensive CJK fonts (SimSun, MingLiU, MS Gothic) and includes the system
// fallback as a base. This ensures characters like U+9FB6 (龶) render correctly
// even when the primary font (e.g., Microsoft YaHei) lacks the glyph.
func dwBuildCustomFontFallback(factory2 unsafe.Pointer) unsafe.Pointer {
	// Create font fallback builder
	var builder unsafe.Pointer
	if ret, _ := dwComCall(factory2, dwFactory2VtCreateFontFallbackBuilder,
		uintptr(unsafe.Pointer(&builder)),
	); int32(ret) < 0 || builder == nil {
		slog.Warn("DirectWrite: CreateFontFallbackBuilder failed", "hr", fmt.Sprintf("0x%08X", ret))
		return nil
	}
	defer dwComRelease(builder)

	// CJK Unicode ranges that need fallback coverage
	ranges := []dwUnicodeRange{
		{0x2E80, 0x33FF},   // CJK Radicals, Kangxi Radicals, CJK Symbols
		{0x3400, 0x9FFF},   // CJK Extension A + CJK Unified Ideographs
		{0xE000, 0xF8FF},   // Private Use Area (Wubi radicals etc.)
		{0xF900, 0xFAFF},   // CJK Compatibility Ideographs
		{0x20000, 0x323AF}, // CJK Extensions B-G
	}

	// Target CJK font families (order = priority)
	familyNames := []string{"SimSun", "MingLiU", "MS Gothic", "Microsoft YaHei"}
	familyPtrs := make([]*uint16, len(familyNames))
	for i, name := range familyNames {
		familyPtrs[i], _ = syscall.UTF16PtrFromString(name)
	}

	// AddMapping(ranges, rangesCount, targetFamilyNames, targetFamilyNamesCount,
	//            fontCollection, localeName, baseFamilyName, scale)
	ret, _ := dwComCall(builder, dwFallbackBuilderVtAddMapping,
		uintptr(unsafe.Pointer(&ranges[0])),
		uintptr(uint32(len(ranges))),
		uintptr(unsafe.Pointer(&familyPtrs[0])),
		uintptr(uint32(len(familyPtrs))),
		0,                              // fontCollection = NULL (system)
		0,                              // localeName = NULL
		0,                              // baseFamilyName = NULL
		uintptr(math.Float32bits(1.0)), // scale = 1.0
	)
	if int32(ret) < 0 {
		slog.Warn("DirectWrite: AddMapping for CJK failed", "hr", fmt.Sprintf("0x%08X", ret))
		return nil
	}

	// Add system fallback mappings as base (for non-CJK characters)
	var sysFallback unsafe.Pointer
	if ret2, _ := dwComCall(factory2, dwFactory2VtGetSystemFontFallback,
		uintptr(unsafe.Pointer(&sysFallback)),
	); ret2 == 0 && sysFallback != nil {
		dwComCall(builder, dwFallbackBuilderVtAddMappings,
			uintptr(sysFallback),
		)
		dwComRelease(sysFallback)
	}

	// Build the final fallback object
	var fallback unsafe.Pointer
	if ret3, _ := dwComCall(builder, dwFallbackBuilderVtCreateFontFallback,
		uintptr(unsafe.Pointer(&fallback)),
	); int32(ret3) < 0 || fallback == nil {
		slog.Warn("DirectWrite: CreateFontFallback failed", "hr", fmt.Sprintf("0x%08X", ret3))
		return nil
	}

	runtime.KeepAlive(ranges)
	runtime.KeepAlive(familyPtrs)
	return fallback
}

// dwSetSystemFontFallback sets the system font fallback on a text layout
// so that characters missing from the primary font fall back to system fonts.
var dwFontFallbackLogOnce sync.Once

func dwSetSystemFontFallback(layout unsafe.Pointer) {
	if dwriteSystemFontFallback == nil || layout == nil {
		return
	}
	// QueryInterface for IDWriteTextLayout2
	var layout2 unsafe.Pointer
	if ret, _ := dwComCall(layout, 0, // IUnknown::QueryInterface
		uintptr(unsafe.Pointer(&dwIIDTextLayout2)),
		uintptr(unsafe.Pointer(&layout2)),
	); ret != 0 || layout2 == nil {
		dwFontFallbackLogOnce.Do(func() {
			slog.Warn("DirectWrite font fallback: IDWriteTextLayout2 not available", "hr", fmt.Sprintf("0x%08X", ret))
		})
		return
	}
	defer dwComRelease(layout2)

	// IDWriteTextLayout2::SetFontFallback(IDWriteFontFallback*)
	ret, _ := dwComCall(layout2, dwLayout2VtSetFontFallback,
		uintptr(dwriteSystemFontFallback),
	)
	dwFontFallbackLogOnce.Do(func() {
		if int32(ret) < 0 {
			slog.Warn("DirectWrite SetFontFallback failed", "hr", fmt.Sprintf("0x%08X", ret))
		} else {
			slog.Info("DirectWrite system font fallback applied to text layout")
		}
	})
}

func dwGetTextMetrics(layout unsafe.Pointer) (dwTextMetrics, error) {
	var m dwTextMetrics
	_, err := dwComCall(layout, dwLayoutVtGetMetrics, uintptr(unsafe.Pointer(&m)))
	return m, err
}

func dwGetLineMetrics(layout unsafe.Pointer) ([]dwLineMetrics, error) {
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
	cachedFormat       unsafe.Pointer
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
func (r *DWriteRenderer) getFormatLocked(family string, weight int, fontSize float64) unsafe.Pointer {
	if r.cachedFormat != nil &&
		r.cachedFormatFamily == family &&
		r.cachedFormatWeight == weight &&
		r.cachedFormatSize == fontSize {
		return r.cachedFormat
	}
	if r.cachedFormat != nil {
		dwComRelease(r.cachedFormat)
		r.cachedFormat = nil
	}
	f, err := dwCreateTextFormat(dwriteSharedFactory, family, weight, fontSize)
	if err != nil {
		return nil
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

// SetFont sets the preferred system font family for DirectWrite.
func (r *DWriteRenderer) SetFont(font string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	name := FontSpecToName(font)
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
	if format == nil {
		return 0
	}

	layout, err := dwCreateTextLayout(dwriteSharedFactory, format, text)
	if err != nil || layout == nil {
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

	var format unsafe.Pointer
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
	if format == nil {
		return
	}

	layout, err := dwCreateTextLayout(dwriteSharedFactory, format, text)
	if err != nil || layout == nil {
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
	// renderer 是 Go 堆对象，通过 uintptr 传给 COM 后在回调中传回，
	// 需要确保 COM 回调期间 GC 不会回收它。
	runtime.KeepAlive(r.backend.renderer)

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

	if r.cachedFormat != nil {
		dwComRelease(r.cachedFormat)
		r.cachedFormat = nil
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

// ═══════════════════════════════════════════════════════════════
// Color glyph run support (COLR/CPAL emoji)
// ═══════════════════════════════════════════════════════════════

// dwDrawColorGlyphRun attempts to render a glyph run as colored layers using
// IDWriteFactory2::TranslateColorGlyphRun. Returns true if color rendering
// succeeded, false if the glyph run has no color information (caller should
// fall back to monochrome rendering).
func dwDrawColorGlyphRun(
	bitmapTarget, renderParams unsafe.Pointer,
	textColor uint32,
	baseX, baseY float32,
	measuringMode, glyphRun, glyphRunDesc uintptr,
) bool {
	if dwriteSharedFactory2 == nil {
		return false
	}

	// IDWriteFactory2::TranslateColorGlyphRun (vtable 28)
	// Returns S_OK with color layers, or DWRITE_E_NOCOLOR for non-color glyphs.
	f2Vtbl := *(*unsafe.Pointer)(dwriteSharedFactory2)
	translateMethod := *(*uintptr)(unsafe.Add(f2Vtbl, unsafe.Sizeof(uintptr(0))*uintptr(dwFactory2VtTranslateColorGlyphRun)))

	var colorEnum unsafe.Pointer
	ret, _, _ := syscall.SyscallN(translateMethod,
		uintptr(dwriteSharedFactory2),
		uintptr(math.Float32bits(baseX)),
		uintptr(math.Float32bits(baseY)),
		glyphRun,
		glyphRunDesc,
		measuringMode,
		0, // worldToDeviceTransform = NULL
		0, // colorPaletteIndex = 0
		uintptr(unsafe.Pointer(&colorEnum)),
	)
	if int32(ret) < 0 || colorEnum == nil {
		return false
	}
	defer dwComRelease(colorEnum)

	// Cache DrawGlyphRun method pointer for the loop.
	bmpVtbl := *(*unsafe.Pointer)(bitmapTarget)
	drawMethod := *(*uintptr)(unsafe.Add(bmpVtbl, unsafe.Sizeof(uintptr(0))*uintptr(dwBmpVtDrawGlyphRun)))

	for {
		var hasRun int32
		dwComCall(colorEnum, dwColorEnumVtMoveNext, uintptr(unsafe.Pointer(&hasRun)))
		if hasRun == 0 {
			break
		}

		var runPtr unsafe.Pointer
		dwComCall(colorEnum, dwColorEnumVtGetCurrentRun, uintptr(unsafe.Pointer(&runPtr)))
		if runPtr == nil {
			continue
		}

		colorRun := (*dwColorGlyphRun)(runPtr)

		// PaletteIndex 0xFFFF means "use the text color specified by the caller".
		var colorRef uint32
		if colorRun.PaletteIndex == 0xFFFF {
			colorRef = textColor
		} else {
			r := byte(colorRun.RunColorR * 255)
			g := byte(colorRun.RunColorG * 255)
			b := byte(colorRun.RunColorB * 255)
			colorRef = uint32(b)<<16 | uint32(g)<<8 | uint32(r)
		}

		// Draw this color layer. The DWRITE_GLYPH_RUN is at offset 0 of
		// DWRITE_COLOR_GLYPH_RUN, so runPtr can be passed directly.
		var blackBoxRect RECT
		syscall.SyscallN(drawMethod,
			uintptr(bitmapTarget),
			uintptr(math.Float32bits(colorRun.BaselineOriginX)),
			uintptr(math.Float32bits(colorRun.BaselineOriginY)),
			measuringMode,
			uintptr(runPtr), // &DWRITE_GLYPH_RUN at offset 0
			uintptr(renderParams),
			uintptr(colorRef),
			uintptr(unsafe.Pointer(&blackBoxRect)),
		)
	}

	return true
}
