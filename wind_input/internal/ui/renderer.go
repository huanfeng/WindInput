package ui

import (
	"image"
	"image/color"
	"math"
	"sync"

	"github.com/gogpu/gg"
	ggtext "github.com/gogpu/gg/text"
	"github.com/huanfeng/wind_input/pkg/buildvariant"
	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/theme"
)

// RenderConfig contains rendering configuration.
// 这里只描述候选窗的视觉参数；字体文件选择与 fallback 细节由 FontConfig 接管。
type RenderConfig struct {
	FontPath           string
	FontSize           float64
	IndexFontSize      float64
	Padding            float64
	ItemHeight         float64
	CornerRadius       float64
	BackgroundColor    color.Color
	TextColor          color.Color
	IndexColor         color.Color
	IndexBgColor       color.Color
	InputBgColor       color.Color
	InputTextColor     color.Color
	BorderColor        color.Color
	HoverBgColor       color.Color            // Background color for hovered candidate
	SelectedBgColor    color.Color            // Background color for keyboard-selected candidate
	Layout             config.CandidateLayout // "horizontal" or "vertical"
	HidePreedit        bool                   // Hide preedit area when inline_preedit is enabled
	IndexStyle         string                 // "circle" (default) or "text" (plain text index)
	AccentBarColor     color.Color            // Left accent bar color, nil = no bar
	HasAccentBar       bool                   // Whether to draw accent bar
	IndexFontWeight    int                    // Index number font weight (100-900), 0 = use global weight
	ItemPaddingLeft    float64                // Left padding of each candidate item (px), 0 = default 8
	ItemPaddingRight   float64                // Right padding of each candidate item (px), 0 = default 8
	ItemRadius         float64                // Candidate item corner radius (px), 0 = default 4
	WindowPaddingX     float64                // Horizontal window padding (px), 0 = default (use Padding)
	WindowPaddingY     float64                // Vertical window padding (px), 0 = default (use Padding)
	IndexMarginRight   float64                // Gap between index and candidate text (scaled px)
	TextMarginRight    float64                // Gap after candidate text (scaled px)
	CommentMarginLeft  float64                // Gap between candidate text and comment (scaled px)
	CommentMarginRight float64                // Gap after comment to item right edge (scaled px)
	VerticalMinWidth   float64                // Vertical layout minimum width (scaled px), 0 = auto
	VerticalMaxWidth   float64                // Vertical layout maximum width (scaled px), 0 = default 600
	HorizontalMinWidth float64                // Horizontal layout minimum width (scaled px), 0 = default 60
	HorizontalMaxWidth float64                // Horizontal layout maximum width (scaled px), 0 = no limit
	AlwaysShowPager    bool                   // Always show page navigation (disable buttons when not navigable)
	ShowPageNumber     bool                   // Show page number text (e.g. "1/3")
	TextRenderMode     TextRenderMode         // "gdi" (Windows native) or "freetype" (original)
	ModeLabel          string                 // Temporary mode label (e.g. "临时拼音", "快捷输入"), empty = no label
	ModeAccentColor    color.Color            // Inner glow border color for special modes, nil = no glow
	PreeditMode        config.PreeditMode     // "top" (default) or "embedded" (inline before candidates); only effective when HidePreedit=false
	IndexLabels        string                 // 主题序号标签（来自 views.index.labels / 旧 layout）；空 = 默认 1-9,0
	GlobalIndexLabels  string                 // 用户全局序号标签覆盖（config.UI.CandidateIndexLabels）；非空时覆盖 IndexLabels
	CmdbarPrefix       string                 // 副作用 cmdbar 候选 (Actions 含 ActionEffect) 的前缀符号; 空 = 不显示前缀

	// v2.5 候选窗背景图（nil = 仅纯色背景）
	BackgroundImage   *image.RGBA
	BackgroundMode    string // nine_slice | stretch | tile | center
	BackgroundSlice   theme.Padding
	BackgroundOpacity float64
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
		SelectedBgColor: color.RGBA{204, 228, 255, 255}, // 键盘选中高亮 (默认值; 主题 SetTheme 会覆盖)。
		// 不设默认会在无主题渲染 (如 darwin forwarder) 选中项走 dc.SetColor(nil) 崩溃。
		Layout:         config.LayoutHorizontal, // Default to horizontal layout
		HidePreedit:    false,
		ShowPageNumber: true,
		CmdbarPrefix:   DefaultCmdbarCandidatePrefix,
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
	resolvedV25   *theme.ResolvedV25
	resolvedViews theme.ResolvedViews // 候选窗盒模型外观（P2 切片-0）：默认来自合成桥，主题提供 views 时来自 YAML
	themeViews    *theme.Views        // 主题 YAML 提供的 views（已 merge 基线）；nil=用合成桥
	TextBackendManager

	// Base (unscaled) values for DPI recalculation
	baseFontSize   float64
	themeRowHeight float64 // unscaled row height from theme; 0 = auto-compute from font size
	lastDPI        int     // Last DPI used for scaling; 0 means not yet set

	// 候选框绘制缓冲. 跨帧复用以避免 gg.NewPixmap + dc.Image() 的双倍分配
	// (旧 pprof 中合计 ~2.3 GB 累计). RenderCandidates 在 UI 单线程调用,
	// UpdateLayeredWindow 同步消费 img, 之后下一帧才会写入 — 无并发竞争.
	scratchPix []byte
}

// acquireDrawContext 返回一个 gg.Context 与对应的 *image.RGBA, 二者共享
// Renderer.scratchPix 底层数组. 容量不足时一次性扩张, 内容预清零 (gg 期望
// 透明背景起步). 调用方拿到的 img 在下一次 acquireDrawContext 前都有效.
func (r *Renderer) acquireDrawContext(w, h int) (*gg.Context, *image.RGBA) {
	need := w * h * 4
	if cap(r.scratchPix) < need {
		r.scratchPix = make([]byte, need)
	} else {
		r.scratchPix = r.scratchPix[:need]
		clear(r.scratchPix)
	}
	pm := gg.NewPixmapFromBuffer(r.scratchPix, w, h)
	return gg.NewContextForPixmap(pm), pm.ImageView()
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
func (r *Renderer) UpdateFont(fontSize float64, fontFamily string) {
	scale := GetDPIScale()

	if fontSize > 0 {
		r.baseFontSize = fontSize
		r.config.FontSize = fontSize * scale
		r.config.IndexFontSize = (fontSize - 4) * scale
		if r.themeRowHeight == 0 {
			r.config.ItemHeight = math.Max(32, fontSize*1.8) * scale
		}
	}

	if fontFamily != r.FontFamily() {
		r.SetFontFamily(fontFamily)
	}
}

// refreshDPIIfNeeded checks if DPI has changed since last render and recalculates if needed.
func (r *Renderer) refreshDPIIfNeeded() {
	// 用跨平台 GetDPIScale * 96 还原成 int dpi, 保持原 lastDPI int 字段语义不变。
	// Win 端 GetDPIScale() = GetEffectiveDPI()/96, 反推回 currentDPI = GetEffectiveDPI()。
	// darwin 端 GetDPIScale() 默认 1.0, currentDPI = 96 恒定 (无 per-monitor 切换)。
	currentDPI := int(GetDPIScale() * 96.0)
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
	r.config.CornerRadius = 8 * scale
	if r.themeRowHeight > 0 {
		r.config.ItemHeight = r.themeRowHeight * scale
	} else {
		r.config.ItemHeight = math.Max(32, baseFontSize*1.8) * scale
	}
}

// SetLayout sets the candidate layout mode
func (r *Renderer) SetLayout(layout config.CandidateLayout) {
	if layout.Valid() {
		r.config.Layout = layout
	}
}

// SetHidePreedit sets whether to hide the preedit area
func (r *Renderer) SetHidePreedit(hide bool) {
	r.config.HidePreedit = hide
}

// SetPreeditMode sets the preedit display mode ("top" or "embedded")
func (r *Renderer) SetPreeditMode(mode config.PreeditMode) {
	r.config.PreeditMode = mode
}

// SetCmdbarPrefix 设置副作用 cmdbar 候选的渲染前缀符号。空字符串=不显示前缀。
func (r *Renderer) SetCmdbarPrefix(prefix string) {
	r.config.CmdbarPrefix = prefix
}

// SetAlwaysShowPager 设置是否在单页时也显示翻页区域
func (r *Renderer) SetAlwaysShowPager(v bool) {
	r.config.AlwaysShowPager = v
}

// SetShowPageNumber 设置是否在翻页区域显示页码文字（如 "1/3"）
func (r *Renderer) SetShowPageNumber(v bool) {
	r.config.ShowPageNumber = v
}

// SetModeLabel sets the temporary mode label for display
func (r *Renderer) SetModeLabel(label string) {
	r.config.ModeLabel = label
}

// SetModeAccentColor sets the inner glow border color for the current mode, nil = no glow
func (r *Renderer) SetModeAccentColor(c color.Color) {
	r.config.ModeAccentColor = c
}

// SetTheme sets the theme for the renderer and updates colors
func (r *Renderer) SetTheme(rv *theme.ResolvedV25) {
	if rv == nil {
		return
	}
	r.resolvedV25 = rv
	r.themeViews = rv.Views // 主题盒模型 views（nil=用合成桥）
	// Update config colors from theme
	colors := rv.Palette.CandidateWindow
	r.config.BackgroundColor = colors.Background
	r.config.BorderColor = colors.Border
	r.config.TextColor = colors.Text
	r.config.IndexColor = colors.IndexText
	r.config.IndexBgColor = colors.IndexBg
	r.config.HoverBgColor = colors.HoverBg
	r.config.SelectedBgColor = colors.SelectedBg
	r.config.InputBgColor = colors.PreeditBg
	r.config.InputTextColor = colors.PreeditText
	// Update style from theme（原 adapter 从 rv.Layout 派生的 Style 字段，现直接读 Layout）
	lay := rv.Layout.CandidateWindow
	idx := lay.CandidateList.Index
	if idx.Circle {
		r.config.IndexStyle = "circle"
	} else {
		r.config.IndexStyle = "text"
	}
	r.config.AccentBarColor = rv.Palette.CandidateWindow.AccentBar
	r.config.HasAccentBar = lay.CandidateList.AccentBar.Enabled
	r.config.IndexFontWeight = 0
	r.config.ItemPaddingLeft = float64(lay.CandidateList.ItemPadding.Left)
	r.config.ItemRadius = float64(lay.CandidateList.ItemRadius)
	r.config.ItemPaddingRight = float64(lay.CandidateList.ItemPadding.Right)
	r.config.AlwaysShowPager = false
	r.config.ShowPageNumber = true
	// Apply window padding from theme (override base Padding)
	scale := GetDPIScale()
	if wpx := float64(lay.WindowPadding.Left); wpx > 0 {
		r.config.WindowPaddingX = wpx * scale
	}
	if wpy := float64(lay.WindowPadding.Top); wpy > 0 {
		r.config.WindowPaddingY = wpy * scale
	}
	if cr := float64(lay.BorderRadius); cr > 0 {
		r.config.CornerRadius = cr * scale
	}
	if rh := float64(lay.CandidateList.ItemHeight); rh > 0 {
		r.themeRowHeight = rh
		r.config.ItemHeight = rh * scale
	} else {
		r.themeRowHeight = 0
		baseFontSize := r.baseFontSize
		if baseFontSize <= 0 {
			baseFontSize = 18
		}
		r.config.ItemHeight = math.Max(32, baseFontSize*1.8) * scale
	}
	// Apply element spacing from theme
	if g := float64(lay.CandidateList.Index.Gap); g > 0 {
		r.config.IndexMarginRight = g * scale
	} else {
		r.config.IndexMarginRight = 4 * scale // default
	}
	// TextMarginRight：原 adapter Style 未映射（恒 0），renderer 走 else 分支取默认值
	r.config.TextMarginRight = 4 * scale // default
	if g := float64(lay.CandidateList.Comment.Gap); g > 0 {
		r.config.CommentMarginLeft = g * scale
	} else {
		r.config.CommentMarginLeft = 8 * scale // default
	}
	// CommentMarginRight：原 adapter Style 未映射（恒 0），renderer 走 else 分支取默认值
	r.config.CommentMarginRight = 4 * scale // default
	// 宽度限制（Vertical/Horizontal Min/Max）原 adapter Style 未映射（恒 0），
	// 原 renderer 仅 if>0 设置、无 else，故 rv 路径不设置，保留 config 既有值（零回归）。
	r.config.IndexLabels = theme.BuildIndexLabelsFromSlots(idx.Labels)

	// 候选窗背景图：ResolvedV25 不支持解码后的背景图（种子主题无图），直接清空（零回归）
	r.config.BackgroundImage = nil
	r.config.BackgroundMode = ""
	r.config.BackgroundOpacity = 0
}

// getCommentColor returns the comment color from theme or default
func (r *Renderer) getCommentColor() color.Color {
	if r.resolvedV25 != nil {
		return r.resolvedV25.Palette.CandidateWindow.Comment
	}
	return color.RGBA{150, 150, 150, 255}
}

// getShadowColor returns the shadow color from theme or default
func (r *Renderer) getShadowColor() color.Color {
	if r.resolvedV25 != nil {
		return r.resolvedV25.Palette.Shadow
	}
	return color.RGBA{0, 0, 0, 15}
}

// getModeIndicatorColors returns mode indicator colors from theme or defaults
func (r *Renderer) getModeIndicatorColors() (bgColor, textColor color.Color) {
	if r.resolvedV25 != nil {
		return r.resolvedV25.Palette.Toast.Background, r.resolvedV25.Palette.Toast.Text
	}
	return color.RGBA{50, 50, 50, 230}, color.RGBA{255, 255, 255, 255}
}

// GetLayout returns the current layout mode
func (r *Renderer) GetLayout() config.CandidateLayout {
	return r.config.Layout
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

// SetGlobalIndexLabels 设置用户全局序号标签覆盖（config.UI.CandidateIndexLabels）。
// 非空时在 build 中优先于主题 IndexLabels（见 effectiveIndexLabels）。
func (r *Renderer) SetGlobalIndexLabels(labels string) {
	r.config.GlobalIndexLabels = labels
}

// DrawDebugBanner draws a small anti-aliased green dot in the top-right area (debug variant only)
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
	radius := 4.0 * scale // 圆点半径
	inset := 8.0 * scale  // 从边缘内缩，避开圆角

	// 调试圆点统一为绿色（盒模型 View 引擎是唯一渲染路径）。
	dotR, dotG, dotB := uint16(40), uint16(180), uint16(70)

	// 圆点中心（右上角内缩）
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
			nr := uint8((dotR*uint16(a) + uint16(bg.R)*invA) / 255)
			ng := uint8((dotG*uint16(a) + uint16(bg.G)*invA) / 255)
			nb := uint8((dotB*uint16(a) + uint16(bg.B)*invA) / 255)
			na := uint8(math.Min(float64(uint16(a)+uint16(bg.A)), 255))
			img.SetRGBA(px, py, color.RGBA{nr, ng, nb, na})
		}
	}
}
