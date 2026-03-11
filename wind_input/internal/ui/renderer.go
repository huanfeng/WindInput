package ui

import (
	"errors"
	"image"
	"image/color"
	"sync"

	"github.com/fogleman/gg"
	"github.com/golang/freetype/truetype"
	"github.com/huanfeng/wind_input/pkg/theme"
	"golang.org/x/image/font"
)

// RenderConfig contains rendering configuration
type RenderConfig struct {
	FontPath        string
	FontSize        float64
	IndexFontSize   float64
	Padding         float64
	ItemHeight      float64
	CornerRadius    float64
	BackgroundColor color.Color
	TextColor       color.Color
	IndexColor      color.Color
	IndexBgColor    color.Color
	InputBgColor    color.Color
	InputTextColor  color.Color
	BorderColor     color.Color
	HoverBgColor    color.Color    // Background color for hovered candidate
	Layout          string         // "horizontal" or "vertical"
	HidePreedit     bool           // Hide preedit area when inline_preedit is enabled
	IndexStyle      string         // "circle" (default) or "text" (plain text index)
	AccentBarColor  color.Color    // Left accent bar color, nil = no bar
	HasAccentBar    bool           // Whether to draw accent bar
	TextRenderMode  TextRenderMode // "gdi" (Windows native) or "freetype" (original)
}

// DefaultRenderConfig returns default rendering configuration with DPI scaling
func DefaultRenderConfig() RenderConfig {
	// Get DPI scale factor
	scale := GetDPIScale()

	return RenderConfig{
		FontPath:        "", // Will use system font
		FontSize:        18 * scale,
		IndexFontSize:   14 * scale,
		Padding:         10 * scale,
		ItemHeight:      32 * scale,
		CornerRadius:    8 * scale,
		BackgroundColor: color.RGBA{255, 255, 255, 245}, // Slightly transparent white
		TextColor:       color.RGBA{30, 30, 30, 255},
		IndexColor:      color.RGBA{255, 255, 255, 255},
		IndexBgColor:    color.RGBA{66, 133, 244, 255}, // Blue
		InputBgColor:    color.RGBA{240, 240, 240, 255},
		InputTextColor:  color.RGBA{100, 100, 100, 255},
		BorderColor:     color.RGBA{200, 200, 200, 255},
		HoverBgColor:    color.RGBA{230, 240, 255, 255}, // Light blue for hover
		Layout:          "horizontal",                   // Default to horizontal layout
		HidePreedit:     false,
	}
}

// fontCache caches loaded fonts and font faces
// maxFontFaces limits the number of cached font.Face instances per fontCache.
// When exceeded, the least recently used face is closed and evicted.
const maxFontFaces = 16

type fontCache struct {
	mu        sync.RWMutex
	font      *truetype.Font
	fontPath  string
	faces     map[float64]font.Face // Cache font faces by size
	faceOrder []float64             // LRU order: most recently used at end
}

func newFontCache() *fontCache {
	return &fontCache{
		faces: make(map[float64]font.Face),
	}
}

// loadFont records the font path for lazy loading.
// The actual truetype.Font is NOT parsed here — it is deferred to getFace()
// on first use. This ensures GDI-mode components never load FreeType font data.
func (fc *fontCache) loadFont(path string) error {
	if fc.fontPath == path && fc.font != nil {
		return nil // Already loaded
	}
	// Close existing faces when switching fonts
	for _, face := range fc.faces {
		face.Close()
	}
	fc.faces = make(map[float64]font.Face)
	fc.faceOrder = nil
	fc.font = nil // Will be loaded lazily in getFace()
	fc.fontPath = path
	return nil
}

// ensureFontParsed loads the truetype.Font from the global registry on demand.
// Must be called with fc.mu held for writing.
func (fc *fontCache) ensureFontParsed() error {
	if fc.font != nil {
		return nil
	}
	if fc.fontPath == "" {
		return errors.New("no font path set")
	}
	f, err := GetSharedFont(fc.fontPath)
	if err != nil {
		return err
	}
	fc.font = f
	return nil
}

// getFace returns a cached font face for the given size, with LRU eviction.
func (fc *fontCache) getFace(size float64) font.Face {
	fc.mu.RLock()
	if face, ok := fc.faces[size]; ok {
		fc.mu.RUnlock()
		// Promote to most-recently-used (deferred to avoid write lock contention)
		fc.mu.Lock()
		fc.touchLRU(size)
		fc.mu.Unlock()
		return face
	}
	fc.mu.RUnlock()

	fc.mu.Lock()
	defer fc.mu.Unlock()

	// Double-check
	if face, ok := fc.faces[size]; ok {
		fc.touchLRU(size)
		return face
	}

	// Lazy load the truetype.Font on first getFace call
	if err := fc.ensureFontParsed(); err != nil {
		return nil
	}

	face := truetype.NewFace(fc.font, &truetype.Options{
		Size:    size,
		DPI:     72,
		Hinting: font.HintingFull,
	})

	// Evict LRU if at capacity
	if len(fc.faces) >= maxFontFaces && len(fc.faceOrder) > 0 {
		oldest := fc.faceOrder[0]
		fc.faceOrder = fc.faceOrder[1:]
		if oldFace, ok := fc.faces[oldest]; ok {
			oldFace.Close()
			delete(fc.faces, oldest)
		}
	}

	fc.faces[size] = face
	fc.faceOrder = append(fc.faceOrder, size)
	return face
}

// touchLRU moves size to the end of the LRU order. Must be called with fc.mu held.
func (fc *fontCache) touchLRU(size float64) {
	for i, s := range fc.faceOrder {
		if s == size {
			fc.faceOrder = append(fc.faceOrder[:i], fc.faceOrder[i+1:]...)
			fc.faceOrder = append(fc.faceOrder, size)
			return
		}
	}
	// Not found (shouldn't happen), add it
	fc.faceOrder = append(fc.faceOrder, size)
}

// Renderer renders candidate window content
type Renderer struct {
	config        RenderConfig
	fontPath      string
	fontCache     *fontCache
	fontReady     bool
	resolvedTheme *theme.ResolvedTheme
	textRenderer  *TextRenderer // GDI text renderer for native Windows text quality
	textDrawer    TextDrawer    // Active text drawing backend (GDI or FreeType)
	fontConfig    *FontConfig   // Centralized font configuration
}

// NewRenderer creates a new renderer
func NewRenderer(config RenderConfig) *Renderer {
	fontCfg := NewFontConfig()
	tr := NewTextRenderer()
	tr.SetGDIParams(fontCfg.GetEffectiveGDIWeight(), fontCfg.GetEffectiveGDIScale())

	r := &Renderer{
		config:       config,
		fontCache:    newFontCache(),
		textRenderer: tr,
		fontConfig:   fontCfg,
	}
	// Pre-load font on creation
	r.ensureFontLoaded()
	r.updateTextDrawer()
	return r
}

// updateTextDrawer creates the appropriate TextDrawer based on current render mode
func (r *Renderer) updateTextDrawer() {
	if r.config.TextRenderMode == TextRenderModeFreetype {
		r.textDrawer = newFreeTypeDrawer(r.fontCache, r.fontConfig)
	} else {
		r.textDrawer = newGDIDrawer(r.textRenderer)
	}
}

// SetGDIFontParams updates GDI font weight and scale for text rendering
func (r *Renderer) SetGDIFontParams(weight int, scale float64) {
	if r.textRenderer != nil {
		r.textRenderer.SetGDIParams(weight, scale)
	}
}

// SetTextRenderMode switches between GDI and FreeType text rendering
func (r *Renderer) SetTextRenderMode(mode TextRenderMode) {
	r.config.TextRenderMode = mode
	r.updateTextDrawer()
}

// GetTextRenderMode returns the current text rendering mode
func (r *Renderer) GetTextRenderMode() TextRenderMode {
	if r.config.TextRenderMode == TextRenderModeFreetype {
		return TextRenderModeFreetype
	}
	return TextRenderModeGDI
}

// SetFontPath sets the font path
func (r *Renderer) SetFontPath(path string) {
	r.fontPath = path
	r.fontReady = false
	r.ensureFontLoaded()
	r.textRenderer.SetFont(r.fontPath)
}

// UpdateFont updates font settings
func (r *Renderer) UpdateFont(fontSize float64, fontPath string) {
	scale := GetDPIScale()

	if fontSize > 0 {
		r.config.FontSize = fontSize * scale
		r.config.IndexFontSize = (fontSize - 4) * scale
	}

	if fontPath != "" && fontPath != r.fontPath {
		r.fontPath = fontPath
		r.fontReady = false
		r.ensureFontLoaded()
		r.textRenderer.SetFont(r.fontPath)
	}
}

// SetLayout sets the candidate layout mode
func (r *Renderer) SetLayout(layout string) {
	if layout == "horizontal" || layout == "vertical" {
		r.config.Layout = layout
	}
}

// SetHidePreedit sets whether to hide the preedit area
func (r *Renderer) SetHidePreedit(hide bool) {
	r.config.HidePreedit = hide
}

// SetTheme sets the theme for the renderer and updates colors
func (r *Renderer) SetTheme(resolved *theme.ResolvedTheme) {
	if resolved == nil {
		return
	}
	r.resolvedTheme = resolved
	// Update config colors from theme
	colors := resolved.CandidateWindow
	r.config.BackgroundColor = colors.BackgroundColor
	r.config.BorderColor = colors.BorderColor
	r.config.TextColor = colors.TextColor
	r.config.IndexColor = colors.IndexColor
	r.config.IndexBgColor = colors.IndexBgColor
	r.config.HoverBgColor = colors.HoverBgColor
	r.config.InputBgColor = colors.InputBgColor
	r.config.InputTextColor = colors.InputTextColor
	// Update style from theme
	r.config.IndexStyle = resolved.Style.IndexStyle
	r.config.AccentBarColor = resolved.Style.AccentBarColor
	r.config.HasAccentBar = resolved.Style.HasAccentBar
}

// getCommentColor returns the comment color from theme or default
func (r *Renderer) getCommentColor() color.Color {
	if r.resolvedTheme != nil {
		return r.resolvedTheme.CandidateWindow.CommentColor
	}
	return color.RGBA{150, 150, 150, 255}
}

// getShadowColor returns the shadow color from theme or default
func (r *Renderer) getShadowColor() color.Color {
	if r.resolvedTheme != nil {
		return r.resolvedTheme.CandidateWindow.ShadowColor
	}
	return color.RGBA{0, 0, 0, 15}
}

// getModeIndicatorColors returns mode indicator colors from theme or defaults
func (r *Renderer) getModeIndicatorColors() (bgColor, textColor color.Color) {
	if r.resolvedTheme != nil {
		return r.resolvedTheme.ModeIndicator.BackgroundColor, r.resolvedTheme.ModeIndicator.TextColor
	}
	return color.RGBA{50, 50, 50, 230}, color.RGBA{255, 255, 255, 255}
}

// GetLayout returns the current layout mode
func (r *Renderer) GetLayout() string {
	return r.config.Layout
}

// ensureFontLoaded resolves the primary font path and records it for lazy loading.
func (r *Renderer) ensureFontLoaded() {
	if r.fontReady && r.fontCache.fontPath != "" {
		return
	}

	r.fontCache.mu.Lock()
	defer r.fontCache.mu.Unlock()

	// Sync user-specified font to FontConfig
	if r.fontPath != "" {
		r.fontConfig.SetPrimaryFont(r.fontPath)
	}

	// Resolve the primary font from the centralized config
	resolved := r.fontConfig.ResolvePrimaryFont()
	if resolved != "" {
		if err := r.fontCache.loadFont(resolved); err == nil {
			r.fontPath = resolved
			r.fontReady = true
			r.textRenderer.SetFont(resolved)
			return
		}
	}
}

// drawChevronLeft draws a left-pointing chevron (‹) at the given center position
func (r *Renderer) drawChevronLeft(dc *gg.Context, cx, cy, size, lineWidth float64) {
	halfH := size / 2
	halfW := size * 0.35 // narrower for elegance
	dc.SetLineWidth(lineWidth)
	dc.SetLineCap(gg.LineCapRound)
	dc.SetLineJoin(gg.LineJoinRound)
	dc.MoveTo(cx+halfW, cy-halfH)
	dc.LineTo(cx-halfW, cy)
	dc.LineTo(cx+halfW, cy+halfH)
	dc.Stroke()
}

// drawChevronRight draws a right-pointing chevron (›) at the given center position
func (r *Renderer) drawChevronRight(dc *gg.Context, cx, cy, size, lineWidth float64) {
	halfH := size / 2
	halfW := size * 0.35
	dc.SetLineWidth(lineWidth)
	dc.SetLineCap(gg.LineCapRound)
	dc.SetLineJoin(gg.LineJoinRound)
	dc.MoveTo(cx-halfW, cy-halfH)
	dc.LineTo(cx+halfW, cy)
	dc.LineTo(cx-halfW, cy+halfH)
	dc.Stroke()
}

func (r *Renderer) drawRoundedRect(dc *gg.Context, x, y, w, h, radius float64) {
	dc.NewSubPath()
	dc.MoveTo(x+radius, y)
	dc.LineTo(x+w-radius, y)
	dc.DrawArc(x+w-radius, y+radius, radius, -gg.Radians(90), 0)
	dc.LineTo(x+w, y+h-radius)
	dc.DrawArc(x+w-radius, y+h-radius, radius, 0, gg.Radians(90))
	dc.LineTo(x+radius, y+h)
	dc.DrawArc(x+radius, y+h-radius, radius, gg.Radians(90), gg.Radians(180))
	dc.LineTo(x, y+radius)
	dc.DrawArc(x+radius, y+radius, radius, gg.Radians(180), gg.Radians(270))
	dc.ClosePath()
}

// RenderModeIndicator renders a mode indicator with adaptive width
func (r *Renderer) RenderModeIndicator(mode string) *image.RGBA {
	scale := GetDPIScale()
	td := r.textDrawer

	minWidth := 50.0 * scale
	height := 36.0 * scale
	fontSize := 20.0 * scale
	padding := 12.0 * scale

	// Measure text width
	textWidth := td.MeasureString(mode, fontSize)

	// Adaptive width: max(minWidth, textWidth + padding*2)
	width := textWidth + padding*2
	if width < minWidth {
		width = minWidth
	}

	dc := gg.NewContext(int(width), int(height))

	// Get colors from theme
	bgColor, textColor := r.getModeIndicatorColors()

	// Draw background shape
	dc.SetColor(bgColor)
	r.drawRoundedRect(dc, 2*scale, 2*scale, width-4*scale, height-4*scale, 6*scale)
	dc.Fill()

	// Draw mode text
	img := dc.Image().(*image.RGBA)
	td.BeginDraw(img)
	tw := td.MeasureString(mode, fontSize)
	td.DrawString(mode, width/2-tw/2, height/2+7*scale, fontSize, textColor)
	td.EndDraw()

	return img
}
