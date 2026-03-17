package ui

import (
	"image"
	"image/color"
	"image/draw"
	"unicode/utf8"

	"github.com/gogpu/gg"
	ggtext "github.com/gogpu/gg/text"
)

// TextRenderMode defines the text rendering backend.
// 历史上配置值已经对外暴露，所以这里保留了 "freetype" 这个模式名。
type TextRenderMode string

const (
	TextRenderModeGDI         TextRenderMode = "gdi"         // Windows GDI native rendering
	TextRenderModeFreetype    TextRenderMode = "freetype"    // gogpu/gg text rendering
	TextRenderModeDirectWrite TextRenderMode = "directwrite" // DirectWrite + Direct2D rendering
)

// TextDrawer provides a unified interface for text measurement and drawing.
// Both FreeType and GDI backends implement this interface.
// The interface is designed to be engine-agnostic so that rendering backends
// (FreeType, GDI, DirectWrite, etc.) can be swapped transparently.
type TextDrawer interface {
	// SetFont sets the font by file path. The backend resolves it to the
	// appropriate internal representation (e.g., GDI family name, gg/text source).
	SetFont(fontPath string)
	// MeasureString measures text width in pixels for the given font size.
	MeasureString(text string, fontSize float64) float64
	// BeginDraw prepares for text drawing on the given image.
	// Must be called before DrawString, and EndDraw must be called after.
	BeginDraw(img *image.RGBA)
	// DrawString draws text at baseline position (x, y), matching gg.DrawString coordinates.
	DrawString(text string, x, y float64, fontSize float64, clr color.Color)
	// DrawStringWithWeight draws text with a specific font weight (100-900).
	// If weight <= 0, behaves like DrawString with global weight.
	DrawStringWithWeight(text string, x, y float64, fontSize float64, clr color.Color, weight int)
	// EndDraw finalizes text drawing and flushes results to the image.
	EndDraw()
	// Close releases all resources held by this drawer.
	Close()
}

// --- FreeType (original) implementation with font fallback ---

// freeTypeDrawer keeps the existing TextRenderMode name for config compatibility,
// but the implementation now uses gogpu/gg text faces instead of fogleman/gg.
// Fallback still happens at glyph granularity so symbols like ✓ and ▸ can be
// resolved independently of the primary font.
type freeTypeDrawer struct {
	cache          *fontCache
	dc             *gg.Context
	target         *image.RGBA
	fontConfig     *FontConfig
	fallbackCaches []*fontCache        // Font face caches (one per fallback font path)
	fallbackFonts  []fallbackFontEntry // References to fallback font entries
	fallbackInited bool                // Whether fallback has been initialized
}

func newFreeTypeDrawer(cache *fontCache, fontConfig *FontConfig) *freeTypeDrawer {
	return &freeTypeDrawer{
		cache:      cache,
		fontConfig: fontConfig,
	}
}

func (d *freeTypeDrawer) SetFont(fontPath string) {
	d.cache.mu.Lock()
	defer d.cache.mu.Unlock()
	_ = d.cache.loadFont(fontPath)
	// 主字体变化后，fallback 选择顺序也可能变化，所以一并重建。
	d.fallbackInited = false
	d.fallbackCaches = nil
	d.fallbackFonts = nil
}

// initFallbacks lazily initializes fallback metadata.
// The actual font files are not parsed here. Each drawer only creates its own
// lightweight fontCache descriptors, and the underlying FontSource is loaded on
// first glyph lookup through fontCache.getFace().
func (d *freeTypeDrawer) initFallbacks() {
	if d.fallbackInited {
		return
	}
	d.fallbackInited = true

	if d.fontConfig == nil {
		return
	}

	fallbackPaths := d.fontConfig.GetTextFallbackFonts()
	shared := GetSharedFallbackFonts(fallbackPaths)

	for _, entry := range shared {
		// Create a lightweight fontCache descriptor for each fallback path.
		// The font file itself is still loaded lazily in getFace().
		fc := newFontCache()
		fc.fontPath = entry.path
		d.fallbackCaches = append(d.fallbackCaches, fc)
		d.fallbackFonts = append(d.fallbackFonts, entry)
	}
}

// fontSegment represents a contiguous run of text that uses the same font.
type fontSegment struct {
	text string
	face ggtext.Face
}

// segmentByFont splits text into segments, each using the best available font.
// The primary font is preferred; fallback fonts are tried for missing glyphs.
func (d *freeTypeDrawer) segmentByFont(text string, fontSize float64) []fontSegment {
	d.initFallbacks()

	primaryFace := d.cache.getFace(fontSize)
	if primaryFace == nil {
		return nil
	}

	if len(d.fallbackCaches) == 0 {
		return []fontSegment{{text: text, face: primaryFace}}
	}

	var segments []fontSegment
	var currentText []byte
	var currentFace ggtext.Face

	for i := 0; i < len(text); {
		r, size := utf8.DecodeRuneInString(text[i:])
		if r == utf8.RuneError && size <= 1 {
			i++
			continue
		}

		// Determine which face to use for this rune.
		bestFace := primaryFace
		if !primaryFace.HasGlyph(r) {
			// Try fallback fonts in priority order. These faces are loaded lazily,
			// so common strings never pay the cost of parsing the whole chain.
			for j := range d.fallbackCaches {
				face := d.fallbackCaches[j].getFace(fontSize)
				if face != nil && face.HasGlyph(r) {
					bestFace = face
					break
				}
			}
		}

		if bestFace != currentFace {
			// Flush current segment. gg/text still works one face at a time, so
			// segmented runs preserve the old glyph-level fallback behavior.
			if len(currentText) > 0 {
				segments = append(segments, fontSegment{text: string(currentText), face: currentFace})
				currentText = currentText[:0]
			}
			currentFace = bestFace
		}
		currentText = append(currentText, text[i:i+size]...)
		i += size
	}

	if len(currentText) > 0 {
		segments = append(segments, fontSegment{text: string(currentText), face: currentFace})
	}

	return segments
}

func (d *freeTypeDrawer) MeasureString(text string, fontSize float64) float64 {
	if text == "" {
		return 0
	}

	segments := d.segmentByFont(text, fontSize)
	if len(segments) == 0 {
		return 0
	}

	dc := d.dc
	if dc == nil {
		dc = gg.NewContext(1, 1)
	}

	// gg.MeasureString works per current font, so segmented fallback text needs
	// to be measured run by run and summed manually.
	var totalW float64
	for _, seg := range segments {
		dc.SetFont(seg.face)
		w, _ := dc.MeasureString(seg.text)
		totalW += w
	}
	return totalW
}

func (d *freeTypeDrawer) BeginDraw(img *image.RGBA) {
	// TODO(gogpu/gg): NewContextForImage 的文本路径有 bug，DrawString 不会稳定落到目标位图上。
	// 等上游修复后可改为 NewContextForImage + overlay 复用，避免每帧分配新 RGBA 缓冲。
	// 当前 workaround：在独立 overlay 上绘字，EndDraw 阶段合成回原图。
	bounds := img.Bounds()
	d.target = img
	d.dc = gg.NewContext(bounds.Dx(), bounds.Dy())
}

func (d *freeTypeDrawer) DrawString(text string, x, y float64, fontSize float64, clr color.Color) {
	if d.dc == nil || text == "" {
		return
	}

	segments := d.segmentByFont(text, fontSize)
	if len(segments) == 0 {
		return
	}

	d.dc.SetColor(clr)
	drawX := x
	for _, seg := range segments {
		d.dc.SetFont(seg.face)
		d.dc.DrawString(seg.text, drawX, y)
		// Advance by the measured width of the segment so mixed-font runs keep the
		// same baseline flow as the old implementation.
		w, _ := d.dc.MeasureString(seg.text)
		drawX += w
	}
}

func (d *freeTypeDrawer) DrawStringWithWeight(text string, x, y float64, fontSize float64, clr color.Color, weight int) {
	// FreeType doesn't support per-draw weight, fall back to regular DrawString
	d.DrawString(text, x, y, fontSize, clr)
}

func (d *freeTypeDrawer) EndDraw() {
	if d.target != nil && d.dc != nil {
		if overlay, ok := d.dc.Image().(*image.RGBA); ok {
			draw.Draw(d.target, d.target.Bounds(), overlay, overlay.Bounds().Min, draw.Over)
		}
	}
	d.dc = nil
	d.target = nil
}

func (d *freeTypeDrawer) Close() {
	// d.cache 由外层 renderer 持有和复用，这里只释放当前 drawer 私有状态。
	d.dc = nil
	d.target = nil
	d.fallbackCaches = nil
	d.fallbackFonts = nil
}

// --- GDI implementation ---

// gdiDrawer wraps TextRenderer for Windows-native GDI text rendering.
type gdiDrawer struct {
	tr *TextRenderer
}

func newGDIDrawer(tr *TextRenderer) *gdiDrawer {
	return &gdiDrawer{tr: tr}
}

func (d *gdiDrawer) SetFont(fontPath string) {
	d.tr.SetFont(fontPath)
}

func (d *gdiDrawer) MeasureString(text string, fontSize float64) float64 {
	return d.tr.MeasureString(text, fontSize)
}

func (d *gdiDrawer) BeginDraw(img *image.RGBA) {
	d.tr.BeginDraw(img)
}

func (d *gdiDrawer) DrawString(text string, x, y float64, fontSize float64, clr color.Color) {
	d.tr.DrawString(text, x, y, fontSize, clr)
}

func (d *gdiDrawer) DrawStringWithWeight(text string, x, y float64, fontSize float64, clr color.Color, weight int) {
	d.tr.DrawStringWithWeight(text, x, y, fontSize, clr, weight)
}

func (d *gdiDrawer) EndDraw() {
	d.tr.EndDraw()
}

func (d *gdiDrawer) Close() {
	d.tr.Close()
}

// --- DirectWrite implementation ---

// directWriteDrawer wraps DWriteRenderer for DirectWrite + Direct2D text rendering.
type directWriteDrawer struct {
	tr *DWriteRenderer
}

func newDirectWriteDrawer(tr *DWriteRenderer) *directWriteDrawer {
	return &directWriteDrawer{tr: tr}
}

func (d *directWriteDrawer) SetFont(fontPath string) {
	d.tr.SetFont(fontPath)
}

func (d *directWriteDrawer) MeasureString(text string, fontSize float64) float64 {
	return d.tr.MeasureString(text, fontSize)
}

func (d *directWriteDrawer) BeginDraw(img *image.RGBA) {
	d.tr.BeginDraw(img)
}

func (d *directWriteDrawer) DrawString(text string, x, y float64, fontSize float64, clr color.Color) {
	d.tr.DrawString(text, x, y, fontSize, clr)
}

func (d *directWriteDrawer) DrawStringWithWeight(text string, x, y float64, fontSize float64, clr color.Color, weight int) {
	d.tr.DrawStringWithWeight(text, x, y, fontSize, clr, weight)
}

func (d *directWriteDrawer) EndDraw() {
	d.tr.EndDraw()
}

func (d *directWriteDrawer) Close() {
	d.tr.Close()
}
