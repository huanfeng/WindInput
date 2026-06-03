//go:build windows

package ui

import (
	"image"
	"image/color"
	"math"
	"sync"
	"syscall"
	"unsafe"
)

// GDI text rendering API bindings
var (
	procCreateFontW           = gdi32.NewProc("CreateFontW")
	procSetTextColor          = gdi32.NewProc("SetTextColor")
	procSetBkMode             = gdi32.NewProc("SetBkMode")
	procTextOutW              = gdi32.NewProc("TextOutW")
	procGetTextExtentPoint32W = gdi32.NewProc("GetTextExtentPoint32W")
	procGetTextMetricsW       = gdi32.NewProc("GetTextMetricsW")
)

// GDI constants for text rendering
const (
	gdiTransparent     = 1
	fwNormal           = 400
	fwBold             = 700
	defaultCharset     = 1
	outTTOnlyPrecis    = 7
	clipDefaultPrecis  = 0
	antialiasedQuality = 4
	defaultPitch       = 0
)

// TEXTMETRICW contains basic font metrics from GDI
type TEXTMETRICW struct {
	TmHeight           int32
	TmAscent           int32
	TmDescent          int32
	TmInternalLeading  int32
	TmExternalLeading  int32
	TmAveCharWidth     int32
	TmMaxCharWidth     int32
	TmWeight           int32
	TmOverhang         int32
	TmDigitizedAspectX int32
	TmDigitizedAspectY int32
	TmFirstChar        uint16
	TmLastChar         uint16
	TmDefaultChar      uint16
	TmBreakChar        uint16
	TmItalic           byte
	TmUnderlined       byte
	TmStruckOut        byte
	TmPitchAndFamily   byte
	TmCharSet          byte
}

// isUIChromeSymbolRune reports whether a single rune is a UI-chrome symbol that
// CJK fonts (like Microsoft YaHei) cover poorly and that we prefer to render via
// Segoe UI Symbol for consistent metrics.
//
// Scope is deliberately narrow:
//   - Geometric Shapes (U+25A0–U+25FF): UI uses ▶ ▸ ● ◑ ■ etc. These are
//     monochrome shapes by nature, so forcing a symbol font is safe.
//   - Dingbats (U+2700–U+27BF) whitelist: only ✓ (U+2713) and ✗ (U+2717),
//     the menu check/cross marks. The rest of the Dingbats block contains
//     emoji base characters (✂ ✈ ✉ ✊✋✌ ✏ ✨ ❄ ❤ …) that should be left
//     to the normal emoji font-fallback chain so they can render in color
//     and participate in ZWJ sequences (e.g. ❤️‍🔥).
func isUIChromeSymbolRune(r rune) bool {
	if r >= 0x25A0 && r <= 0x25FF {
		return true
	}
	if r == 0x2713 || r == 0x2717 {
		return true
	}
	return false
}

// isPureSymbolText reports whether text consists solely of UI-chrome symbol
// characters (spaces ignored) with at least one such symbol. Only then is it safe
// to force the whole run onto Segoe UI Symbol.
//
// Mixed strings — e.g. the status bubble's merged "中 英 ◐" that combines CJK mode
// labels with the full/half-width geometric glyph — return false, so they keep the
// primary CJK font (Microsoft YaHei). Otherwise the lone geometric symbol would drag
// the entire run onto Segoe UI Symbol, which lacks CJK glyphs and forces the Chinese
// characters into the SimSun fallback (the reported 雅黑→宋体 regression).
func isPureSymbolText(text string) bool {
	hasSymbol := false
	for _, r := range text {
		if r == ' ' || r == '\t' {
			continue
		}
		if isUIChromeSymbolRune(r) {
			hasSymbol = true
			continue
		}
		return false
	}
	return hasSymbol
}

type gdiFontKey struct {
	size   int
	bold   bool
	symbol bool   // true = use Segoe UI Symbol instead of primary font
	family string // 空=主字体 tr.fontName；非空=逐元素字体族名（P7-B）
}

// TextRenderer provides text drawing and measurement using Windows GDI.
// It produces text rendering that matches Windows native quality.
type TextRenderer struct {
	fontMu   sync.Mutex
	fontName string
	fonts    map[gdiFontKey]uintptr      // HFONT cache by size+bold
	metrics  map[gdiFontKey]*TEXTMETRICW // Cached text metrics

	// GDI rendering parameters (from FontConfig)
	gdiFontWeight int     // lfWeight for CreateFontW (default: 400)
	gdiFontScale  float64 // size multiplier (default: 1.0)

	// Drawing session state (single-threaded, no lock needed)
	inDraw     bool
	drawImg    *image.RGBA
	drawDC     uintptr
	drawBitmap uintptr
	drawBits   unsafe.Pointer
	drawOldBmp uintptr
	drawWidth  int
	drawHeight int
}

// NewTextRenderer creates a new TextRenderer with GDI backend
func NewTextRenderer() *TextRenderer {
	return &TextRenderer{
		fontName:      "Microsoft YaHei",
		fonts:         make(map[gdiFontKey]uintptr),
		metrics:       make(map[gdiFontKey]*TEXTMETRICW),
		gdiFontWeight: fwNormal, // 400
		gdiFontScale:  1.0,
	}
}

// SetGDIParams updates the GDI font weight and scale from FontConfig.
// Clears cached fonts so new parameters take effect.
func (tr *TextRenderer) SetGDIParams(weight int, scale float64) {
	tr.fontMu.Lock()
	defer tr.fontMu.Unlock()

	if weight <= 0 {
		weight = fwNormal
	}
	if scale <= 0 {
		scale = 1.0
	}

	if weight == tr.gdiFontWeight && scale == tr.gdiFontScale {
		return
	}

	// Clear font cache when parameters change
	for k, hFont := range tr.fonts {
		procDeleteObject.Call(hFont)
		delete(tr.fonts, k)
	}
	tr.metrics = make(map[gdiFontKey]*TEXTMETRICW)

	tr.gdiFontWeight = weight
	tr.gdiFontScale = scale
}

// SetFont sets the font family used by GDI rendering.
func (tr *TextRenderer) SetFont(font string) {
	tr.fontMu.Lock()
	defer tr.fontMu.Unlock()

	name := FontSpecToName(font)
	if name == tr.fontName {
		return
	}
	// Clear caches when font changes
	for k, hFont := range tr.fonts {
		procDeleteObject.Call(hFont)
		delete(tr.fonts, k)
	}
	tr.metrics = make(map[gdiFontKey]*TEXTMETRICW)
	tr.fontName = name
}

// symbolFontName is the font used for geometric shapes and dingbats
// that are typically missing from CJK fonts.
const symbolFontName = "Segoe UI Symbol"

// getFont returns a cached HFONT for the given size (caller must hold fontMu or be in single-threaded context)
func (tr *TextRenderer) getFont(size int, bold bool) uintptr {
	return tr.getFontInternal(size, bold, false, "")
}

// getSymbolFont returns a cached HFONT using Segoe UI Symbol for symbol characters
func (tr *TextRenderer) getSymbolFont(size int) uintptr {
	return tr.getFontInternal(size, false, true, "")
}

// getFontFamily returns a cached HFONT for an explicit platform font family (P7-B 逐元素字体)。
// family 为空时等价主字体；未知族名由 GDI 自行替换为默认字体。
func (tr *TextRenderer) getFontFamily(size int, bold bool, family string) uintptr {
	return tr.getFontInternal(size, bold, false, family)
}

// getFontInternal creates or returns a cached HFONT.
// When symbol=true, uses Segoe UI Symbol instead of the primary font.
// family 非空时用该族名（经 FontSpecToName 归一），优先级低于 symbol、独立于主字体缓存。
func (tr *TextRenderer) getFontInternal(size int, bold bool, symbol bool, family string) uintptr {
	key := gdiFontKey{size: size, bold: bold, symbol: symbol, family: family}
	if hFont, ok := tr.fonts[key]; ok {
		return hFont
	}

	// Apply GDI font scale
	scaledSize := size
	if tr.gdiFontScale > 0 && tr.gdiFontScale != 1.0 {
		scaledSize = int(math.Round(float64(size) * tr.gdiFontScale))
	}

	// Apply GDI font weight (bold overrides configured weight)
	weight := uintptr(tr.gdiFontWeight)
	if bold {
		weight = uintptr(fwBold)
	}

	// Choose font family：symbol > 逐元素 family > 主字体。
	name := tr.fontName
	if family != "" {
		name = FontSpecToName(family)
	}
	if symbol {
		name = symbolFontName
	}

	faceName, _ := syscall.UTF16PtrFromString(name)
	hFont, _, _ := procCreateFontW.Call(
		uintptr(int32(-scaledSize)),
		0, 0, 0,
		weight,
		0, 0, 0,
		uintptr(defaultCharset),
		uintptr(outTTOnlyPrecis),
		uintptr(clipDefaultPrecis),
		uintptr(antialiasedQuality),
		uintptr(defaultPitch),
		uintptr(unsafe.Pointer(faceName)),
	)

	if hFont != 0 {
		tr.fonts[key] = hFont
	}
	return hFont
}

// getMetrics returns cached text metrics for the given font size + family。
// 调用方须先把对应字体 select 进 hdc；family 仅用于缓存键避免跨族名碰撞。
func (tr *TextRenderer) getMetrics(hdc uintptr, size int, bold bool, family string) *TEXTMETRICW {
	key := gdiFontKey{size: size, bold: bold, family: family}
	if tm, ok := tr.metrics[key]; ok {
		return tm
	}
	var tm TEXTMETRICW
	procGetTextMetricsW.Call(hdc, uintptr(unsafe.Pointer(&tm)))
	tr.metrics[key] = &tm
	return &tm
}

// measureOnDC measures text width using an existing DC
func (tr *TextRenderer) measureOnDC(hdc uintptr, text string) float64 {
	textW, _ := syscall.UTF16FromString(text)
	var sz SIZE
	procGetTextExtentPoint32W.Call(
		hdc,
		uintptr(unsafe.Pointer(&textW[0])),
		uintptr(len(textW)-1),
		uintptr(unsafe.Pointer(&sz)),
	)
	return float64(sz.Cx)
}

// MeasureString measures text width for the given font size.
// Returns width in pixels, compatible with gg.MeasureString usage.
// For symbol characters, uses Segoe UI Symbol font for accurate measurement.
func (tr *TextRenderer) MeasureString(text string, fontSize float64) float64 {
	if text == "" {
		return 0
	}

	size := int(math.Round(fontSize))
	useSymbol := isPureSymbolText(text)

	// Use session DC if available (avoids creating temp DC)
	if tr.inDraw && tr.drawDC != 0 {
		var hFont uintptr
		if useSymbol {
			hFont = tr.getSymbolFont(size)
		} else {
			hFont = tr.getFont(size, false)
		}
		if hFont != 0 {
			procSelectObject.Call(tr.drawDC, hFont)
		}
		return tr.measureOnDC(tr.drawDC, text)
	}

	// Create temporary DC for measurement
	hdcScreen, _, _ := procGetDC.Call(0)
	if hdcScreen == 0 {
		return 0
	}
	defer procReleaseDC.Call(0, hdcScreen)

	hdc, _, _ := procCreateCompatibleDC.Call(hdcScreen)
	if hdc == 0 {
		return 0
	}
	defer procDeleteDC.Call(hdc)

	var hFont uintptr
	if useSymbol {
		hFont = tr.getSymbolFont(size)
	} else {
		hFont = tr.getFont(size, false)
	}
	if hFont != 0 {
		procSelectObject.Call(hdc, hFont)
	}
	return tr.measureOnDC(hdc, text)
}

// BeginDraw starts a batch drawing session on the given image.
// All subsequent DrawString calls will draw on this image efficiently.
// Must call EndDraw() when done to copy results back.
func (tr *TextRenderer) BeginDraw(img *image.RGBA) {
	if tr.inDraw {
		tr.endDrawInternal()
	}

	bounds := img.Bounds()
	width := bounds.Dx()
	height := bounds.Dy()

	hdcScreen, _, _ := procGetDC.Call(0)
	if hdcScreen == 0 {
		return
	}
	defer procReleaseDC.Call(0, hdcScreen)

	hdc, _, _ := procCreateCompatibleDC.Call(hdcScreen)
	if hdc == 0 {
		return
	}

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
		hdc,
		uintptr(unsafe.Pointer(&bi)),
		DIB_RGB_COLORS,
		uintptr(unsafe.Pointer(&bits)),
		0, 0,
	)
	if hBitmap == 0 {
		procDeleteDC.Call(hdc)
		return
	}

	oldBmp, _, _ := procSelectObject.Call(hdc, hBitmap)

	// Copy image pixels to DIB (RGBA → BGRA)
	// Set alpha to 255 so GDI text antialiasing works against the correct background
	pixelCount := width * height
	dstSlice := unsafe.Slice((*byte)(bits), pixelCount*4)
	for i := 0; i < pixelCount; i++ {
		si := i * 4
		dstSlice[si+0] = img.Pix[si+2] // B
		dstSlice[si+1] = img.Pix[si+1] // G
		dstSlice[si+2] = img.Pix[si+0] // R
		dstSlice[si+3] = 255           // Force opaque for GDI
	}

	procSetBkMode.Call(hdc, uintptr(gdiTransparent))

	tr.inDraw = true
	tr.drawImg = img
	tr.drawDC = hdc
	tr.drawBitmap = hBitmap
	tr.drawBits = bits
	tr.drawOldBmp = oldBmp
	tr.drawWidth = width
	tr.drawHeight = height
}

// DrawString draws text at the given baseline position (like gg.DrawString).
// Must be called between BeginDraw and EndDraw.
// For geometric shapes and dingbats (▸, ✓, etc.), automatically falls back
// to Segoe UI Symbol font since most CJK fonts lack these glyphs.
func (tr *TextRenderer) DrawString(text string, x, y float64, fontSize float64, clr color.Color) {
	if !tr.inDraw || text == "" {
		return
	}

	size := int(math.Round(fontSize))
	var hFont uintptr
	if isPureSymbolText(text) {
		hFont = tr.getSymbolFont(size)
	} else {
		hFont = tr.getFont(size, false)
	}
	if hFont == 0 {
		return
	}
	procSelectObject.Call(tr.drawDC, hFont)

	// Set text color (COLORREF = 0x00BBGGRR)
	cr, cg, cb, _ := clr.RGBA()
	colorRef := uint32(byte(cr>>8)) | uint32(byte(cg>>8))<<8 | uint32(byte(cb>>8))<<16
	procSetTextColor.Call(tr.drawDC, uintptr(colorRef))

	// Convert baseline Y to top-left Y for GDI
	tm := tr.getMetrics(tr.drawDC, size, false, "")
	drawX := int(math.Round(x))
	drawY := int(math.Round(y)) - int(tm.TmAscent)

	textW, _ := syscall.UTF16FromString(text)
	procTextOutW.Call(
		tr.drawDC,
		uintptr(drawX),
		uintptr(drawY),
		uintptr(unsafe.Pointer(&textW[0])),
		uintptr(len(textW)-1),
	)
}

// DrawStringWithWeight draws text with a specific font weight (100-900).
// Weight >= 600 uses bold font, otherwise uses normal font.
func (tr *TextRenderer) DrawStringWithWeight(text string, x, y float64, fontSize float64, clr color.Color, weight int) {
	if !tr.inDraw || text == "" {
		return
	}

	size := int(math.Round(fontSize))
	bold := weight >= 600
	hFont := tr.getFont(size, bold)
	if hFont == 0 {
		return
	}
	procSelectObject.Call(tr.drawDC, hFont)

	cr, cg, cb, _ := clr.RGBA()
	colorRef := uint32(byte(cr>>8)) | uint32(byte(cg>>8))<<8 | uint32(byte(cb>>8))<<16
	procSetTextColor.Call(tr.drawDC, uintptr(colorRef))

	tm := tr.getMetrics(tr.drawDC, size, bold, "")
	drawX := int(math.Round(x))
	drawY := int(math.Round(y)) - int(tm.TmAscent)

	textW, _ := syscall.UTF16FromString(text)
	procTextOutW.Call(
		tr.drawDC,
		uintptr(drawX),
		uintptr(drawY),
		uintptr(unsafe.Pointer(&textW[0])),
		uintptr(len(textW)-1),
	)
}

// MeasureStringFont measures text width using an explicit platform font family (P7-B)。
// family 为空回退主字体度量；非空时按该族名 select 字体后量算（不走 symbol 特判）。
func (tr *TextRenderer) MeasureStringFont(text string, fontSize float64, family string) float64 {
	if family == "" {
		return tr.MeasureString(text, fontSize)
	}
	if text == "" {
		return 0
	}
	size := int(math.Round(fontSize))
	if tr.inDraw && tr.drawDC != 0 {
		if hFont := tr.getFontFamily(size, false, family); hFont != 0 {
			procSelectObject.Call(tr.drawDC, hFont)
		}
		return tr.measureOnDC(tr.drawDC, text)
	}
	hdcScreen, _, _ := procGetDC.Call(0)
	if hdcScreen == 0 {
		return 0
	}
	defer procReleaseDC.Call(0, hdcScreen)
	hdc, _, _ := procCreateCompatibleDC.Call(hdcScreen)
	if hdc == 0 {
		return 0
	}
	defer procDeleteDC.Call(hdc)
	if hFont := tr.getFontFamily(size, false, family); hFont != 0 {
		procSelectObject.Call(hdc, hFont)
	}
	return tr.measureOnDC(hdc, text)
}

// DrawStringFull draws text with explicit weight + platform font family (P7-B)。
// family 为空回退按字重绘制（DrawStringWithWeight/DrawString）；非空时按族名 select 字体绘制。
func (tr *TextRenderer) DrawStringFull(text string, x, y float64, fontSize float64, clr color.Color, weight int, family string) {
	if family == "" {
		if weight > 0 {
			tr.DrawStringWithWeight(text, x, y, fontSize, clr, weight)
		} else {
			tr.DrawString(text, x, y, fontSize, clr)
		}
		return
	}
	if !tr.inDraw || text == "" {
		return
	}
	size := int(math.Round(fontSize))
	bold := weight >= 600
	hFont := tr.getFontFamily(size, bold, family)
	if hFont == 0 {
		return
	}
	procSelectObject.Call(tr.drawDC, hFont)

	cr, cg, cb, _ := clr.RGBA()
	colorRef := uint32(byte(cr>>8)) | uint32(byte(cg>>8))<<8 | uint32(byte(cb>>8))<<16
	procSetTextColor.Call(tr.drawDC, uintptr(colorRef))

	tm := tr.getMetrics(tr.drawDC, size, bold, family)
	drawX := int(math.Round(x))
	drawY := int(math.Round(y)) - int(tm.TmAscent)

	textW, _ := syscall.UTF16FromString(text)
	procTextOutW.Call(
		tr.drawDC,
		uintptr(drawX),
		uintptr(drawY),
		uintptr(unsafe.Pointer(&textW[0])),
		uintptr(len(textW)-1),
	)
}

// EndDraw finishes the drawing session and copies GDI-rendered text back to the image.
// Alpha channel from the original image is preserved.
func (tr *TextRenderer) EndDraw() {
	tr.endDrawInternal()
}

func (tr *TextRenderer) endDrawInternal() {
	if !tr.inDraw {
		return
	}

	// Copy pixels back (BGRA → RGBA).
	// GDI 在不透明 DIB 上以直通色绘制文字（BeginDraw 已把 DIB alpha 强制为 255）。
	// 文字像素（RGB 被改过的）写回前按原 alpha 预乘 (R'=R×A/255)，使其成为合法预乘
	// 像素，从而与背景共享同一透明度——与 DWrite 后端 copyToImageRGB 行为一致。
	// 背景像素（RGB 未变）保持原值，本就是 gg 输出的合法预乘值，不可重复预乘。
	pixelCount := tr.drawWidth * tr.drawHeight
	srcSlice := unsafe.Slice((*byte)(tr.drawBits), pixelCount*4)
	for i := 0; i < pixelCount; i++ {
		si := i * 4
		newR := srcSlice[si+2]
		newG := srcSlice[si+1]
		newB := srcSlice[si+0]
		oldR := tr.drawImg.Pix[si+0]
		oldG := tr.drawImg.Pix[si+1]
		oldB := tr.drawImg.Pix[si+2]

		if newR == oldR && newG == oldG && newB == oldB {
			continue // 背景像素未被字形覆盖，保持原预乘值
		}
		a := uint32(tr.drawImg.Pix[si+3])
		tr.drawImg.Pix[si+0] = uint8(uint32(newR) * a / 255)
		tr.drawImg.Pix[si+1] = uint8(uint32(newG) * a / 255)
		tr.drawImg.Pix[si+2] = uint8(uint32(newB) * a / 255)
	}

	// Cleanup GDI resources
	procSelectObject.Call(tr.drawDC, tr.drawOldBmp)
	procDeleteObject.Call(tr.drawBitmap)
	procDeleteDC.Call(tr.drawDC)

	tr.inDraw = false
	tr.drawImg = nil
	tr.drawDC = 0
	tr.drawBitmap = 0
	tr.drawBits = nil
	tr.drawOldBmp = 0
}

// Close releases all cached GDI resources
func (tr *TextRenderer) Close() {
	if tr.inDraw {
		tr.endDrawInternal()
	}

	tr.fontMu.Lock()
	defer tr.fontMu.Unlock()

	for k, hFont := range tr.fonts {
		procDeleteObject.Call(hFont)
		delete(tr.fonts, k)
	}
}
