package ui

import (
	"image"
	"image/color"
	"math"
	"sync"

	"github.com/gogpu/gg"
	ggtext "github.com/gogpu/gg/text"
	"github.com/huanfeng/wind_input/pkg/buildvariant"
	"github.com/huanfeng/wind_input/pkg/theme"
)

// RenderConfig contains rendering configuration.
// 这里只描述候选窗的视觉参数；字体文件选择与 fallback 细节由 FontConfig 接管。
type RenderConfig struct {
	FontPath         string
	FontSize         float64
	IndexFontSize    float64
	Padding          float64
	ItemHeight       float64
	CornerRadius     float64
	BackgroundColor  color.Color
	TextColor        color.Color
	IndexColor       color.Color
	IndexBgColor     color.Color
	InputBgColor     color.Color
	InputTextColor   color.Color
	BorderColor      color.Color
	HoverBgColor     color.Color    // Background color for hovered candidate
	SelectedBgColor  color.Color    // Background color for keyboard-selected candidate
	Layout           string         // "horizontal" or "vertical"
	HidePreedit      bool           // Hide preedit area when inline_preedit is enabled
	IndexStyle       string         // "circle" (default) or "text" (plain text index)
	AccentBarColor   color.Color    // Left accent bar color, nil = no bar
	HasAccentBar     bool           // Whether to draw accent bar
	IndexFontWeight  int            // Index number font weight (100-900), 0 = use global weight
	ItemPaddingLeft  float64        // Left padding of each candidate item (px), 0 = default 8
	ItemPaddingRight float64        // Right padding of each candidate item (px), 0 = default 8
	WindowPaddingX   float64        // Horizontal window padding (px), 0 = default (use Padding)
	WindowPaddingY   float64        // Vertical window padding (px), 0 = default (use Padding)
	AlwaysShowPager  bool           // Always show page navigation (disable buttons when not navigable)
	ShowPageNumber   bool           // Show page number text (e.g. "1/3")
	TextRenderMode   TextRenderMode // "gdi" (Windows native) or "freetype" (original)
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
		BackgroundColor: color.RGBA{255, 255, 255, 255}, // Opaque white
		TextColor:       color.RGBA{30, 30, 30, 255},
		IndexColor:      color.RGBA{255, 255, 255, 255},
		IndexBgColor:    color.RGBA{66, 133, 244, 255}, // Blue
		InputBgColor:    color.RGBA{240, 240, 240, 255},
		InputTextColor:  color.RGBA{100, 100, 100, 255},
		BorderColor:     color.RGBA{200, 200, 200, 255},
		HoverBgColor:    color.RGBA{230, 240, 255, 255}, // Light blue for hover
		Layout:          "horizontal",                   // Default to horizontal layout
		HidePreedit:     false,
		ShowPageNumber:  true,
	}
}

// fontCache caches loaded gg/text FontSource instances and per-size faces.
// The shared FontSource is global; this struct only tracks the small face cache
// that varies by requested font size inside one renderer.
// maxFontFaces limits the number of cached ggtext.Face instances per fontCache.
// When exceeded, the least recently used face is closed and evicted.
const maxFontFaces = 16

type fontCache struct {
	mu        sync.RWMutex
	source    *ggtext.FontSource
	fontPath  string
	faces     map[float64]ggtext.Face // Cache font faces by size
	faceOrder []float64               // LRU order: most recently used at end
}

// newFontCache creates an empty per-renderer face cache.
func newFontCache() *fontCache {
	return &fontCache{
		faces: make(map[float64]ggtext.Face),
	}
}

// loadFont records the font path for lazy loading.
func (fc *fontCache) loadFont(path string) error {
	if fc.fontPath == path && fc.source != nil {
		return nil
	}
	// Switching fonts invalidates all per-size faces because gg/text Face objects
	// are derived from the FontSource and size together.
	fc.faces = make(map[float64]ggtext.Face)
	fc.faceOrder = nil
	fc.source = nil
	fc.fontPath = path
	return nil
}

// ensureFontSource loads the gg/text FontSource from the global registry on demand.
// Must be called with fc.mu held for writing.
func (fc *fontCache) ensureFontSource() error {
	if fc.source != nil {
		return nil
	}
	if fc.fontPath == "" {
		return nil
	}
	source, err := GetSharedFontSource(fc.fontPath)
	if err != nil {
		return err
	}
	fc.source = source
	return nil
}

// getFace returns a cached gg/text face for the given size, with LRU eviction.
func (fc *fontCache) getFace(size float64) ggtext.Face {
	fc.mu.RLock()
	if face, ok := fc.faces[size]; ok {
		fc.mu.RUnlock()
		fc.mu.Lock()
		fc.touchLRU(size)
		fc.mu.Unlock()
		return face
	}
	fc.mu.RUnlock()

	fc.mu.Lock()
	defer fc.mu.Unlock()

	if face, ok := fc.faces[size]; ok {
		fc.touchLRU(size)
		return face
	}

	if err := fc.ensureFontSource(); err != nil || fc.source == nil {
		return nil
	}

	// gg/text Face is lightweight, so creating it lazily and caching by size keeps
	// repeated measurements and draws cheap without duplicating font file data.
	face := fc.source.Face(size)

	if len(fc.faces) >= maxFontFaces && len(fc.faceOrder) > 0 {
		oldest := fc.faceOrder[0]
		fc.faceOrder = fc.faceOrder[1:]
		if _, ok := fc.faces[oldest]; ok {
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
	fc.faceOrder = append(fc.faceOrder, size)
}

// Close releases per-instance face references. FontSource instances are shared
// globally and intentionally stay alive for the process lifetime.
func (fc *fontCache) Close() {
	fc.mu.Lock()
	defer fc.mu.Unlock()
	fc.faces = make(map[float64]ggtext.Face)
	fc.faceOrder = nil
	fc.source = nil
}

// Renderer renders candidate window content
type Renderer struct {
	config        RenderConfig
	resolvedTheme *theme.ResolvedTheme
	TextBackendManager

	// Base (unscaled) values for DPI recalculation
	baseFontSize float64
	lastDPI      int // Last DPI used for scaling; 0 means not yet set
}

// NewRenderer creates a new renderer
func NewRenderer(config RenderConfig) *Renderer {
	r := &Renderer{
		config:             config,
		TextBackendManager: NewTextBackendManager("candidate"),
		baseFontSize:       18, // Default base font size (unscaled)
	}
	r.SetTextRenderMode(config.TextRenderMode)
	return r
}

// SetTextRenderMode switches between GDI, gg/text, and DirectWrite rendering.
func (r *Renderer) SetTextRenderMode(mode TextRenderMode) {
	r.config.TextRenderMode = mode
	r.TextBackendManager.SetTextRenderMode(mode)
}

// GetTextRenderMode returns the current text rendering mode
func (r *Renderer) GetTextRenderMode() TextRenderMode {
	return r.config.TextRenderMode
}

// UpdateFont updates font settings
func (r *Renderer) UpdateFont(fontSize float64, fontPath string) {
	scale := GetDPIScale()

	if fontSize > 0 {
		r.baseFontSize = fontSize
		r.config.FontSize = fontSize * scale
		r.config.IndexFontSize = (fontSize - 4) * scale
	}

	if fontPath != "" && fontPath != r.FontPath() {
		r.SetFontPath(fontPath)
	}
}

// refreshDPIIfNeeded checks if DPI has changed since last render and recalculates if needed.
func (r *Renderer) refreshDPIIfNeeded() {
	currentDPI := GetEffectiveDPI()
	if r.lastDPI != currentDPI {
		r.lastDPI = currentDPI
		r.RefreshDPIScale()
	}
}

// RefreshDPIScale recalculates all DPI-dependent config values.
// Called when the effective DPI changes (e.g., monitor switch).
func (r *Renderer) RefreshDPIScale() {
	scale := GetDPIScale()
	baseFontSize := r.baseFontSize
	if baseFontSize <= 0 {
		baseFontSize = 18
	}
	r.config.FontSize = baseFontSize * scale
	r.config.IndexFontSize = (baseFontSize - 4) * scale
	r.config.Padding = 10 * scale
	r.config.ItemHeight = 32 * scale
	r.config.CornerRadius = 8 * scale
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
	r.config.SelectedBgColor = colors.SelectedBgColor
	r.config.InputBgColor = colors.InputBgColor
	r.config.InputTextColor = colors.InputTextColor
	// Update style from theme
	r.config.IndexStyle = resolved.Style.IndexStyle
	r.config.AccentBarColor = resolved.Style.AccentBarColor
	r.config.HasAccentBar = resolved.Style.HasAccentBar
	r.config.IndexFontWeight = resolved.Style.IndexFontWeight
	r.config.ItemPaddingLeft = resolved.Style.ItemPaddingLeft
	r.config.ItemPaddingRight = resolved.Style.ItemPaddingRight
	r.config.AlwaysShowPager = resolved.Style.AlwaysShowPager
	r.config.ShowPageNumber = resolved.Style.ShowPageNumber
	// Apply window padding from theme (override base Padding)
	if resolved.Style.WindowPaddingX > 0 || resolved.Style.WindowPaddingY > 0 {
		scale := GetDPIScale()
		if resolved.Style.WindowPaddingX > 0 {
			r.config.WindowPaddingX = resolved.Style.WindowPaddingX * scale
		}
		if resolved.Style.WindowPaddingY > 0 {
			r.config.WindowPaddingY = resolved.Style.WindowPaddingY * scale
		}
	}
	if resolved.Style.CornerRadius > 0 {
		r.config.CornerRadius = resolved.Style.CornerRadius * GetDPIScale()
	}
	if resolved.Style.RowHeight > 0 {
		r.config.ItemHeight = resolved.Style.RowHeight * GetDPIScale()
	}
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

func radians(degrees float64) float64 {
	return degrees * math.Pi / 180
}

func (r *Renderer) drawRoundedRect(dc *gg.Context, x, y, w, h, radius float64) {
	dc.DrawRoundedRectangle(x, y, w, h, radius)
}

// RenderModeIndicator renders a mode indicator with adaptive width
func (r *Renderer) RenderModeIndicator(mode string) *image.RGBA {
	scale := GetDPIScale()
	td := r.TextDrawer()

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

// DrawDebugBanner draws a small anti-aliased red dot in the top-right area (debug variant only)
// Inset from the corner to stay within rounded rectangle bounds.
func DrawDebugBanner(img *image.RGBA) {
	if !buildvariant.IsDebug() {
		return
	}

	bounds := img.Bounds()
	w := bounds.Dx()
	h := bounds.Dy()
	if w < 20 || h < 20 {
		return
	}

	scale := GetDPIScale()
	radius := 4.0 * scale // 红点半径
	inset := 8.0 * scale  // 从边缘内缩，避开圆角

	// 红点中心（右上角内缩）
	cxf := float64(w) - inset
	cyf := inset

	// 扫描范围：圆心 ± (radius+1)，+1 为抗锯齿过渡带
	ri := int(math.Ceil(radius)) + 1
	for dy := -ri; dy <= ri; dy++ {
		for dx := -ri; dx <= ri; dx++ {
			dist := math.Sqrt(float64(dx)*float64(dx) + float64(dy)*float64(dy))
			if dist > radius+1 {
				continue
			}

			// 抗锯齿：边缘 1px 过渡带内线性插值 alpha
			var alpha float64
			if dist <= radius-0.5 {
				alpha = 1.0
			} else if dist >= radius+0.5 {
				alpha = 0.0
			} else {
				alpha = 1.0 - (dist - (radius - 0.5))
			}
			if alpha <= 0 {
				continue
			}

			px := bounds.Min.X + int(cxf) + dx
			py := bounds.Min.Y + int(cyf) + dy
			if px < bounds.Min.X || px >= bounds.Max.X || py < bounds.Min.Y || py >= bounds.Max.Y {
				continue
			}

			// Alpha 混合：将红点与背景像素混合
			a := uint8(alpha * 255)
			bg := img.RGBAAt(px, py)
			invA := 255 - uint16(a)
			nr := uint8((uint16(220)*uint16(a) + uint16(bg.R)*invA) / 255)
			ng := uint8((uint16(40)*uint16(a) + uint16(bg.G)*invA) / 255)
			nb := uint8((uint16(40)*uint16(a) + uint16(bg.B)*invA) / 255)
			na := uint8(math.Min(float64(uint16(a)+uint16(bg.A)), 255))
			img.SetRGBA(px, py, color.RGBA{nr, ng, nb, na})
		}
	}
}
