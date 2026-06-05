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
// 这里只描述候选窗的运行时参数；候选窗外观（颜色/几何）已由 theme.ResolveCandidateViews
// 经 r.resolvedViews 承载（P6 阶段2e 删合成桥后，颜色/几何字段不再经 RenderConfig 中转）。
// 字体文件选择与 fallback 细节由 FontConfig 接管。
type RenderConfig struct {
	FontPath          string
	FontSize          float64                // 候选主文本字号（base，已乘 DPI scale）；其余元素字号 = base + 主题相对偏移
	ItemHeight        float64                // 行高（已乘 scale）；回填进 resolvedViews
	Layout            config.CandidateLayout // "horizontal" or "vertical"
	HidePreedit       bool                   // Hide preedit area when inline_preedit is enabled
	IndexStyle        string                 // "circle" (default) or "text" (plain text index)
	HasAccentBar      bool                   // Whether to draw accent bar
	VerticalMaxWidth  float64                // Vertical layout maximum width (scaled px), 0 = default 600
	AlwaysShowPager   bool                   // Always show page navigation (disable buttons when not navigable)
	ShowPageNumber    bool                   // Show page number text (e.g. "1/3")
	TextRenderMode    TextRenderMode         // "gdi" (Windows native) or "freetype" (original)
	ModeLabel         string                 // Temporary mode label (e.g. "临时拼音", "快捷输入"), empty = no label
	ModeAccentColor   color.Color            // Inner glow border color for special modes, nil = no glow
	PreeditMode       config.PreeditMode     // "top" (default) or "embedded" (inline before candidates); only effective when HidePreedit=false
	IndexLabels       string                 // 主题序号标签（来自 views.index.labels / 旧 layout）；空 = 默认 1-9,0
	GlobalIndexLabels string                 // 用户全局序号标签覆盖（config.UI.CandidateIndexLabels）；非空时覆盖 IndexLabels
	CmdbarPrefix      string                 // 副作用 cmdbar 候选 (Actions 含 ActionEffect) 的前缀符号; 空 = 不显示前缀

	// 候选窗背景图（nil = 仅纯色背景）
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
		FontPath:       "", // Will use system font
		FontSize:       18 * scale,
		ItemHeight:     32 * scale,
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
		delete(fc.faces, oldest) // delete 对不存在的键是 no-op，无需 ok 守卫（S1033）
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
	resolvedV3    *theme.ResolvedV3
	resolvedViews theme.ResolvedViews // 候选窗盒模型外观：每帧由 refreshResolvedViews 经 theme.ResolveCandidateViews 重建（几何+颜色）+ 运行时字号回填
	themeViews    *theme.Views        // 主题盒模型 views（已 merge defaultViews 基线）；来自 rv.Views
	// imgRes 位图解码缓存基础设施（P7-C；P8 切片6 抽为 imageResolver 与 status/tooltip/menu/toast 共享）：
	// 一次性解码、跨帧复用，SetTheme 换主题时 reset。resources 表按帧从 resolvedV3.Resources 传入。
	imgRes imageResolver
	TextBackendManager

	// Base (unscaled) values for DPI recalculation
	baseFontSize    float64 // 有效基准字号（= 跟随主题 ? themeFontSize : userFontSize），派生 FontSize/IndexFontSize/ItemHeight
	userFontSize    float64 // 用户全局字号（config.UI.FontSize），自定义模式下生效
	themeFontSize   float64 // 主题 behavior.font_size（来自 SetTheme），跟随模式下生效
	fontFollowTheme bool    // true=候选字号跟随主题 behavior.font_size；false=用 userFontSize
	lastDPI         int     // Last DPI used for scaling; 0 means not yet set

	// 主题 behavior 用户覆盖层（哲学Y，来自 config.UI）：每项 = FollowTheme ? 主题 behavior : 用户值。
	// 应用点在 SetTheme（pager/page_number）与 viewbox_render（vertical_max_width）。
	pagerFollowTheme            bool // true=always_show_pager 跟随主题；false=用 userAlwaysShowPager
	userAlwaysShowPager         bool
	pageNumberFollowTheme       bool // true=show_page_number 跟随主题；false=用 userShowPageNumber
	userShowPageNumber          bool
	verticalMaxWidthFollowTheme bool    // true=vertical_max_width 跟随主题；false=用 userVerticalMaxWidth
	userVerticalMaxWidth        float64 // 用户竖排最大宽（逻辑像素，未乘 scale）

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
		userFontSize:       18,
		// 主题 behavior 覆盖默认全部跟随主题（与未配置时的旧行为一致：直接用主题 behavior）。
		pagerFollowTheme:            true,
		pageNumberFollowTheme:       true,
		verticalMaxWidthFollowTheme: true,
		userShowPageNumber:          true,
		userVerticalMaxWidth:        600,
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

// applyFontDerivation 用当前 baseFontSize + DPI scale 重算字号派生：
// 主候选字体=base、行高=max(32, base*1.8)，均 ×scale。
// 序号/注释/页码/模式徽标字号一律由主题 views.<el>.font_size 相对 base 偏移（零派生魔法，见 refreshResolvedViews）。
// P7-5：候选窗行高恒由字号派生（不再支持主题级固定行高 themeRowHeight），自然高度=文字+item 内边距由盒模型决定。
func (r *Renderer) applyFontDerivation() {
	scale := GetDPIScale()
	base := r.baseFontSize
	if base <= 0 {
		base = 18
	}
	r.config.FontSize = base * scale
	r.config.ItemHeight = math.Max(32, base*1.8) * scale
}

// recomputeBaseFont 依「跟随主题/自定义」选定有效基准字号，再重算派生。
// 跟随且主题字号有效→主题 behavior.font_size；否则→用户全局字号。
func (r *Renderer) recomputeBaseFont() {
	if r.fontFollowTheme && r.themeFontSize > 0 {
		r.baseFontSize = r.themeFontSize
	} else if r.userFontSize > 0 {
		r.baseFontSize = r.userFontSize
	}
	r.applyFontDerivation()
}

// SetFontFollowTheme 设置候选字号是否跟随主题 behavior.font_size（来自 config.UI.FontSizeFollowTheme）。
func (r *Renderer) SetFontFollowTheme(follow bool) {
	r.fontFollowTheme = follow
	r.recomputeBaseFont()
}

// SetBehaviorOverrides 设置主题 behavior 的用户覆盖层（哲学Y，来自 config.UI）：
// always_show_pager / show_page_number / vertical_max_width 各自的「跟随主题」开关 + 用户值。
// 跟随=true 时用主题 behavior（SetTheme 写入），=false 时用此处用户值。
// 需在 SetTheme 之前或之后调用均可——applyBehaviorOverrides 在 SetTheme 末尾据当前
// 标志位与主题 behavior 计算最终值；verticalMaxWidth 在 viewbox_render 每帧据标志位选值。
func (r *Renderer) SetBehaviorOverrides(
	alwaysShowPager, alwaysShowPagerFollowTheme bool,
	showPageNumber, showPageNumberFollowTheme bool,
	verticalMaxWidth int, verticalMaxWidthFollowTheme bool,
) {
	r.pagerFollowTheme = alwaysShowPagerFollowTheme
	r.userAlwaysShowPager = alwaysShowPager
	r.pageNumberFollowTheme = showPageNumberFollowTheme
	r.userShowPageNumber = showPageNumber
	r.verticalMaxWidthFollowTheme = verticalMaxWidthFollowTheme
	if verticalMaxWidth > 0 {
		r.userVerticalMaxWidth = float64(verticalMaxWidth)
	}
	r.applyBehaviorOverrides()
}

// applyBehaviorOverrides 据「跟随主题/用户值」标志位把 pager/page_number 最终值写入
// r.config（vertical_max_width 不在此——它由 viewbox_render 每帧据标志位选值）。
// 跟随主题时用 r.resolvedV3.Behavior；用户自定义时用 r.user* 值。
// 注意：用户 PagerDisplayMode（applyPagerOverride）是更上层的独立强制覆盖，仍在其后生效。
func (r *Renderer) applyBehaviorOverrides() {
	if r.pagerFollowTheme {
		if r.resolvedV3 != nil {
			r.config.AlwaysShowPager = r.resolvedV3.Behavior.AlwaysShowPager
		}
	} else {
		r.config.AlwaysShowPager = r.userAlwaysShowPager
	}
	if r.pageNumberFollowTheme {
		if r.resolvedV3 != nil {
			r.config.ShowPageNumber = r.resolvedV3.Behavior.ShowPageNumber
		}
	} else {
		r.config.ShowPageNumber = r.userShowPageNumber
	}
}

// UpdateFont updates font settings
func (r *Renderer) UpdateFont(fontSize float64, fontFamily string) {
	if fontSize > 0 {
		r.userFontSize = fontSize
		r.recomputeBaseFont() // 跟随模式下忽略用户字号，用主题字号
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
// Called when the effective DPI changes (e.g., monitor switch). baseFontSize 已是有效值，
// 仅按新 scale 重算派生（窗口 padding/圆角等已于 P6 阶段2e 退役，不再 RenderConfig 中转）。
func (r *Renderer) RefreshDPIScale() {
	r.applyFontDerivation()
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
func (r *Renderer) SetTheme(rv *theme.ResolvedV3) {
	if rv == nil {
		return
	}
	r.resolvedV3 = rv
	r.themeViews = rv.Views // 主题盒模型 views（已 merge defaultViews 基线）
	// 候选窗颜色/几何已由 theme.ResolveCandidateViews 经 r.resolvedViews 承载（P6 阶段2e 删合成桥）；
	// 此处只搬运渲染运行时仍需的 RenderConfig 字段：IndexStyle / HasAccentBar / page 策略 / 序号标签。
	// P7-5/V3-D：这些已归口 views（序号样式=views.index.background.shape、强调条开关=views.accent_bar.enabled、
	// 标签=views.index.labels），不再来自 layout/metrics；行高恒由有效字号派生（themeRowHeight 退役）。
	// rv.Views 生产路径恒非 nil（resolver 保证）；此处 nil 防御供仅构造 Behavior/Palette 的单元测试。
	r.config.IndexStyle = "text"
	r.config.HasAccentBar = false
	var indexLabels []string
	if v := rv.Views; v != nil {
		if v.Index.Background.Shape == "circle" {
			r.config.IndexStyle = "circle"
		}
		if v.AccentBar.Enabled != nil {
			r.config.HasAccentBar = *v.AccentBar.Enabled
		}
		indexLabels = v.Index.Labels
	}
	// page 策略：哲学Y 双层覆盖——最终值 = FollowTheme ? 主题 behavior : config.UI 用户值
	// （applyBehaviorOverrides 据 r.*FollowTheme 标志位选源）。用户 PagerDisplayMode 是更上层的
	// 独立强制覆盖，在 applyPagerOverride 注入（Default=不覆盖，保留此处选定值）。
	r.applyBehaviorOverrides()
	// 字号跟随：记录主题 behavior.font_size，按「跟随/自定义」重算有效基准字号 + 派生（含行高）。
	r.themeFontSize = float64(rv.Behavior.FontSize)
	r.recomputeBaseFont()
	// 序号标签：nil（无 views）回退默认数字 1..9,0（与旧 layout 路径一致）。
	r.config.IndexLabels = theme.BuildIndexLabelsFromSlots(indexLabels)

	// 候选窗背景图/层级覆盖图（P7-C）：来源已归口 views（window.background.image / 各 View 的 layers），
	// 经 r.resolvedViews 的 BgImage/Layers spec 承载，build 时按 ref 经 imageForRef 取缓存位图。
	// 换主题清空位图缓存（ref 解码结果按主题失效）。
	r.imgRes.reset()
}

// resourcesSnapshot 返回当前主题的资源表（ref→path/dataURI），nil-safe。
func (r *Renderer) resourcesSnapshot() map[string]string {
	if r.resolvedV3 != nil {
		return r.resolvedV3.Resources
	}
	return nil
}

// imageForRef 把 ViewImage.ref 解码为位图（委派共享 imageResolver）。仅在 build 路径调用（单线程）。
func (r *Renderer) imageForRef(ref string) *image.RGBA {
	return r.imgRes.imageForRef(ref, r.resourcesSnapshot())
}

// fillFor 构建 View 背景填充：底色 + 可选背景图（委派共享 imageResolver）。
func (r *Renderer) fillFor(col color.Color, bg *theme.RVImage, grad *theme.RVGradient) Fill {
	return r.imgRes.fillFor(col, bg, grad, r.resourcesSnapshot())
}

// appendThemeLayers 把主题 RVImage 层级覆盖图（spec）解码后追加到 View.Layers（P7-C，D4；委派共享 imageResolver）。
// 与引擎内置层（accent rail / 光标）共存；offset/size 为逻辑像素经 sc 缩放，W/H=0 保持原图尺寸。
func (r *Renderer) appendThemeLayers(v *View, layers []theme.RVImage, sc func(float64) int) {
	r.imgRes.appendLayers(v, layers, r.resourcesSnapshot(), sc)
}

// getModeIndicatorColors returns mode indicator colors from theme or defaults
func (r *Renderer) getModeIndicatorColors() (bgColor, textColor color.Color) {
	if r.resolvedV3 != nil {
		t := r.resolvedV3.Palette.Tokens
		return t["toast_bg"], t["toast_text"]
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
