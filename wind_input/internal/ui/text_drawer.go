package ui

import (
	"image"
	"image/color"
	"unicode/utf8"

	"github.com/fogleman/gg"
	"github.com/golang/freetype/truetype"
	"golang.org/x/image/font"
)

// TextRenderMode defines the text rendering backend
type TextRenderMode string

const (
	TextRenderModeGDI      TextRenderMode = "gdi"      // Windows GDI native rendering
	TextRenderModeFreetype TextRenderMode = "freetype" // FreeType rendering (original)
)

// TextDrawer provides a unified interface for text measurement and drawing.
// Both FreeType and GDI backends implement this interface.
// The interface is designed to be engine-agnostic so that rendering backends
// (FreeType, GDI, DirectWrite, etc.) can be swapped transparently.
type TextDrawer interface {
	// SetFont sets the font by file path. The backend resolves it to the
	// appropriate internal representation (e.g., GDI family name, freetype font).
	SetFont(fontPath string)
	// MeasureString measures text width in pixels for the given font size.
	MeasureString(text string, fontSize float64) float64
	// BeginDraw prepares for text drawing on the given image.
	// Must be called before DrawString, and EndDraw must be called after.
	BeginDraw(img *image.RGBA)
	// DrawString draws text at baseline position (x, y), matching gg.DrawString coordinates.
	DrawString(text string, x, y float64, fontSize float64, clr color.Color)
	// EndDraw finalizes text drawing and flushes results to the image.
	EndDraw()
	// Close releases all resources held by this drawer.
	Close()
}

// --- FreeType (original) implementation with font fallback ---

// freeTypeDrawer wraps gg/freetype for text rendering with glyph-level font fallback.
// When the primary font lacks a glyph (e.g., ✓ or ▸), it automatically tries
// fallback fonts from the shared font pool until one is found that contains the glyph.
// Fallback fonts are shared across all freeTypeDrawer instances to minimize memory usage.
type freeTypeDrawer struct {
	cache          *fontCache
	dc             *gg.Context
	fontConfig     *FontConfig
	fallbackCaches []*fontCache        // Font face caches (one per shared fallback font)
	fallbackFonts  []fallbackFontEntry // References to shared font pool entries
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
	d.cache.loadFont(fontPath)
	// Reset fallback when primary font changes
	d.fallbackInited = false
	d.fallbackCaches = nil
	d.fallbackFonts = nil
}

// initFallbacks lazily initializes fallback using the shared font pool.
// The actual font files are loaded only once globally via GetSharedFallbackFonts.
// Each drawer only creates its own fontCache (for size-specific font.Face caching).
func (d *freeTypeDrawer) initFallbacks() {
	if d.fallbackInited {
		return
	}
	d.fallbackInited = true

	if d.fontConfig == nil {
		return
	}

	fallbackPaths := d.fontConfig.GetFallbackFonts()
	shared := GetSharedFallbackFonts(fallbackPaths)

	for _, entry := range shared {
		// Create a lightweight fontCache that references the shared parsed font.
		// Only the font.Face cache (per size) is per-drawer; the font data is shared.
		fc := newFontCache()
		fc.font = entry.font
		fc.fontPath = entry.path
		d.fallbackCaches = append(d.fallbackCaches, fc)
		d.fallbackFonts = append(d.fallbackFonts, entry)
	}
}

// hasGlyph checks if the given font contains a glyph for the rune.
func hasGlyph(f *truetype.Font, r rune) bool {
	if f == nil {
		return false
	}
	return f.Index(r) != 0
}

// fontSegment represents a contiguous run of text that uses the same font.
type fontSegment struct {
	text string
	face font.Face
}

// segmentByFont splits text into segments, each using the best available font.
// The primary font is preferred; fallback fonts are tried for missing glyphs.
func (d *freeTypeDrawer) segmentByFont(text string, fontSize float64) []fontSegment {
	d.initFallbacks()

	primaryFont := d.cache.font
	primaryFace := d.cache.getFace(fontSize)
	if primaryFace == nil {
		return nil
	}

	// Fast path: if no fallbacks, just use primary for everything
	if len(d.fallbackFonts) == 0 {
		return []fontSegment{{text: text, face: primaryFace}}
	}

	var segments []fontSegment
	var currentText []byte
	var currentFace font.Face

	for i := 0; i < len(text); {
		r, size := utf8.DecodeRuneInString(text[i:])
		if r == utf8.RuneError && size <= 1 {
			i++
			continue
		}

		// Determine which face to use for this rune
		var bestFace font.Face
		if hasGlyph(primaryFont, r) {
			bestFace = primaryFace
		} else {
			// Try fallback fonts from the shared pool
			for j, entry := range d.fallbackFonts {
				if hasGlyph(entry.font, r) {
					bestFace = d.fallbackCaches[j].getFace(fontSize)
					break
				}
			}
			if bestFace == nil {
				// No font has this glyph, use primary anyway
				bestFace = primaryFace
			}
		}

		if bestFace != currentFace {
			// Flush current segment
			if len(currentText) > 0 {
				segments = append(segments, fontSegment{text: string(currentText), face: currentFace})
				currentText = currentText[:0]
			}
			currentFace = bestFace
		}
		currentText = append(currentText, text[i:i+size]...)
		i += size
	}

	// Flush last segment
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

	var totalW float64
	for _, seg := range segments {
		dc.SetFontFace(seg.face)
		w, _ := dc.MeasureString(seg.text)
		totalW += w
	}
	return totalW
}

func (d *freeTypeDrawer) BeginDraw(img *image.RGBA) {
	d.dc = gg.NewContextForRGBA(img)
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
		d.dc.SetFontFace(seg.face)
		d.dc.DrawString(seg.text, drawX, y)
		w, _ := d.dc.MeasureString(seg.text)
		drawX += w
	}
}

func (d *freeTypeDrawer) EndDraw() {
	d.dc = nil
}

func (d *freeTypeDrawer) Close() {
	// fontCache is shared, not owned by this drawer
	d.dc = nil
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

func (d *gdiDrawer) EndDraw() {
	d.tr.EndDraw()
}

func (d *gdiDrawer) Close() {
	d.tr.Close()
}
