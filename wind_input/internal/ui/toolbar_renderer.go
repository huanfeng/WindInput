//go:build windows

// Package ui provides native Windows UI for input method
package ui

import (
	"image"
	"image/color"

	"github.com/gogpu/gg"
	"github.com/huanfeng/wind_input/pkg/theme"
)

// Toolbar 默认几何（逻辑像素 dp；随 DPI 缩放，主题可经 views.toolbar 逐项覆盖）。
// 整条宽度不是常量，由 grip + 4×button 槽位 measure 汇总 = 10 + 4×26 = 114。
const (
	tbDefaultHeight       = 30
	tbDefaultGripWidth    = 10
	tbDefaultButtonWidth  = 26 // 按钮槽位宽（含 padding），可见框 = 槽位 - 2×padding
	tbDefaultButtonPad    = 2  // 按钮四周间距/竖向内缩（margin）
	tbDefaultButtonRadius = 4
	tbDefaultBarRadius    = 6
	tbDefaultBarBorderW   = 1 // 整条边框宽（px 发丝线，不缩放）
)

// toolbarGeom 工具栏解析后的几何（设备像素）：views.toolbar 的 *Dimension×scale，缺省=内置默认。
type toolbarGeom struct {
	height, gripWidth, buttonWidth, buttonPadding, buttonRadius int
	barRadius, barBorderW                                       int
	fontSize                                                    float64
}

// resolveToolbarGeom 把主题 views.toolbar 的几何 *Dimension 按 scale 换算为设备像素，缺省回退内置默认。
func resolveToolbarGeom(rv *theme.ResolvedV3, scale float64) toolbarGeom {
	g := toolbarGeom{
		height:        theme.Dp(tbDefaultHeight).Scaled(scale),
		gripWidth:     theme.Dp(tbDefaultGripWidth).Scaled(scale),
		buttonWidth:   theme.Dp(tbDefaultButtonWidth).Scaled(scale),
		buttonPadding: theme.Dp(tbDefaultButtonPad).Scaled(scale),
		buttonRadius:  theme.Dp(tbDefaultButtonRadius).Scaled(scale),
		barRadius:     theme.Dp(tbDefaultBarRadius).Scaled(scale),
		barBorderW:    tbDefaultBarBorderW,
		fontSize:      14.0 * scale,
	}
	if rv == nil || rv.Views == nil || rv.Views.Toolbar == nil {
		return g
	}
	t := rv.Views.Toolbar
	dim := func(d *theme.Dimension, def int) int {
		if d != nil {
			return d.Scaled(scale)
		}
		return def
	}
	g.height = dim(t.Height, g.height)
	g.gripWidth = dim(t.GripWidth, g.gripWidth)
	g.buttonWidth = dim(t.ButtonWidth, g.buttonWidth)
	g.buttonPadding = dim(t.ButtonPadding, g.buttonPadding)
	g.buttonRadius = dim(t.ButtonRadius, g.buttonRadius)
	return g
}

// zeroMeasurer 是几何专用文本度量桩：工具栏尺寸全由 FixedW/FixedH 决定，文本度量不影响布局，
// 故几何查询（computeGeometry）用它即可，免依赖文本后端——HitTest/GetToolbarSize 可在窗口创建早期安全调用。
type zeroMeasurer struct{}

func (zeroMeasurer) MeasureString(string, float64) float64 { return 0 }

// ToolbarRenderer renders the toolbar UI
type ToolbarRenderer struct {
	resolvedV3 *theme.ResolvedV3
	TextBackendManager
}

// NewToolbarRenderer creates a new toolbar renderer
func NewToolbarRenderer() *ToolbarRenderer {
	r := &ToolbarRenderer{
		TextBackendManager: NewTextBackendManager("toolbar"),
	}
	r.SetTextRenderMode(TextRenderModeDirectWrite)
	return r
}

// SetTheme sets the theme for the toolbar renderer（P5：吃 ResolvedV3）
func (r *ToolbarRenderer) SetTheme(rv *theme.ResolvedV3) {
	r.resolvedV3 = rv
}

// getTooltipColors returns tooltip colors from theme or defaults（工具栏内悬停提示，复用 Palette.Tooltip）
func (r *ToolbarRenderer) getTooltipColors() (bgColor, textColor, borderColor color.Color) {
	if r.resolvedV3 != nil {
		t := r.resolvedV3.Palette.Tokens
		return t["tooltip_bg"], t["tooltip_text"], color.RGBA{77, 89, 107, 255}
	}
	return color.RGBA{38, 46, 56, 242}, color.RGBA{242, 242, 242, 255}, color.RGBA{77, 89, 107, 255}
}

// Render renders the toolbar with the given state（盒模型 View 引擎）。
// View 承载整条背景/边框 + 4 按钮框 + mode 文字；grip/全半角/标点/齿轮矢量符号后处理。
func (r *ToolbarRenderer) Render(state ToolbarState) *image.RGBA {
	scale := GetDPIScale()
	rtv := r.resolveToolbarViews()
	geom := resolveToolbarGeom(r.resolvedV3, scale)
	td := r.TextDrawer()

	tt := buildToolbarTree(state, rtv, geom)
	Layout(tt.root, 0, 0, td)
	dc, img := newSharedDrawContext(tt.root.Rect().Dx(), tt.root.Rect().Dy())
	PaintTree(tt.root, dc, img, td)

	// 后处理矢量符号（坐标用 Layout 后各按钮 Rect()）
	r.paintGrip(dc, tt.grip.Rect(), scale, rtv)
	r.paintWidthSymbol(dc, tt.width.Rect(), state.FullWidth, scale, rtv)
	r.paintPunctSymbols(img, td, tt.punct.Rect(), state.ChinesePunct, scale, rtv)
	r.paintGear(dc, tt.settings.Rect(), scale, rtv)

	DrawDebugBanner(img)
	return img
}

// paintGrip 在 grip 区域 rect 内绘制拖动点阵（后处理，复刻原 drawGrip 几何）。
func (r *ToolbarRenderer) paintGrip(dc *gg.Context, rect image.Rectangle, scale float64, rtv theme.ResolvedToolbarViews) {
	dotSize := 2.0 * scale
	dotGap := 4.0 * scale
	dc.SetColor(rtv.Grip)
	cx := float64(rect.Min.X) + float64(rect.Dx())/2
	cy := float64(rect.Min.Y) + float64(rect.Dy())/2
	startY := cy - dotGap
	for row := range 3 {
		y := startY + float64(row)*dotGap
		for col := range 2 {
			x := cx - dotGap/2 + float64(col)*dotGap
			dc.DrawCircle(x, y, dotSize/2)
			dc.Fill()
		}
	}
}

// paintWidthSymbol 在 width 按钮 rect 内绘制全角实心圆 / 半角月牙（色用 ButtonText/ButtonBg）。
func (r *ToolbarRenderer) paintWidthSymbol(dc *gg.Context, rect image.Rectangle, fullWidth bool, scale float64, rtv theme.ResolvedToolbarViews) {
	cx := float64(rect.Min.X) + float64(rect.Dx())/2
	cy := float64(rect.Min.Y) + float64(rect.Dy())/2
	radius := 6.5 * scale

	if fullWidth {
		dc.SetColor(rtv.ButtonText)
		dc.DrawCircle(cx, cy, radius)
		dc.Fill()
	} else {
		dc.SetColor(rtv.ButtonText)
		dc.DrawCircle(cx, cy, radius)
		dc.Fill()
		// 挖掉左上一块形成月牙（用按钮底色覆盖）
		offset := radius * 0.5
		dc.SetColor(rtv.ButtonBg)
		dc.DrawCircle(cx-offset, cy-offset, radius*0.95)
		dc.Fill()
	}
}

// paintPunctSymbols 在 punct 按钮 rect 内绘制中/英标点双符号（带全角 nudge 补偿；色用 ButtonText）。
func (r *ToolbarRenderer) paintPunctSymbols(img *image.RGBA, td TextDrawer, rect image.Rectangle, chinesePunct bool, scale float64, rtv theme.ResolvedToolbarViews) {
	punctFontSize := 13.0 * scale
	x := float64(rect.Min.X)
	w := float64(rect.Dx())
	punctY := float64(rect.Min.Y) + float64(rect.Dy())/2 + punctFontSize*0.35
	leftAnchor := x + w*0.33
	rightAnchor := x + w*0.72
	td.BeginDraw(img)
	if chinesePunct {
		periodText := "。"
		lwP := td.MeasureString(periodText, punctFontSize)
		nudge := 2.0 * scale
		td.DrawString(periodText, leftAnchor-lwP/2+nudge, punctY, punctFontSize, rtv.ButtonText)
		commaText := "，"
		rwP := td.MeasureString(commaText, punctFontSize)
		td.DrawString(commaText, rightAnchor-rwP/2+nudge, punctY, punctFontSize, rtv.ButtonText)
	} else {
		dotText := "."
		lwP := td.MeasureString(dotText, punctFontSize)
		td.DrawString(dotText, leftAnchor-lwP/2, punctY, punctFontSize, rtv.ButtonText)
		commaText := ","
		rwP := td.MeasureString(commaText, punctFontSize)
		td.DrawString(commaText, rightAnchor-rwP/2, punctY, punctFontSize, rtv.ButtonText)
	}
	td.EndDraw()
}

// paintGear 在 settings 按钮 rect 内绘制齿轮矢量（背景框已由 View 画；色用 SettingsIcon/Hole）。
func (r *ToolbarRenderer) paintGear(dc *gg.Context, rect image.Rectangle, scale float64, rtv theme.ResolvedToolbarViews) {
	centerX := float64(rect.Min.X) + float64(rect.Dx())/2
	centerY := float64(rect.Min.Y) + float64(rect.Dy())/2
	outerR := 8.0 * scale
	innerR := 4.0 * scale
	toothHeight := 2.5 * scale

	dc.SetColor(rtv.SettingsIcon)
	teeth := 8
	for i := range teeth {
		angle := float64(i) * 360.0 / float64(teeth)
		dc.Push()
		dc.RotateAbout(radians(angle), centerX, centerY)
		dc.DrawRectangle(centerX-toothHeight/2, centerY-outerR, toothHeight, toothHeight)
		dc.Fill()
		dc.Pop()
	}
	dc.DrawCircle(centerX, centerY, outerR-toothHeight)
	dc.Fill()
	dc.SetColor(rtv.SettingsHole)
	dc.DrawCircle(centerX, centerY, innerR)
	dc.Fill()
}

// toolbarHit 是一个按钮的命中带（kind + margin 盒；LayoutRow 使各 margin 盒首尾相接，平铺整条满高）。
type toolbarHit struct {
	kind ToolbarHitResult
	rect image.Rectangle
}

// toolbarGeometry 是工具栏几何的单一真相源：一次 buildToolbarTree+Layout 的派生结果。
type toolbarGeometry struct {
	size   image.Point                          // 整条尺寸（GetToolbarSize）
	bounds map[ToolbarHitResult]image.Rectangle // 各按钮 content 矩形（GetButtonBounds）
	hits   []toolbarHit                         // 各按钮 margin 盒，按 x 顺序（HitTest）
}

// viewOuterRect 返回 View 的 margin 盒（content 矩形外扩自身 Margin）。
func viewOuterRect(v *View) image.Rectangle {
	r := v.Rect()
	return image.Rect(
		r.Min.X-v.Margin.Left, r.Min.Y-v.Margin.Top,
		r.Max.X+v.Margin.Right, r.Max.Y+v.Margin.Bottom,
	)
}

// computeGeometry 用零 state/零色构建工具栏 View 树并 Layout，派生几何——命中/边界/尺寸的唯一来源。
// 几何与 state/颜色无关（按钮 FixedW 固定、mode 文字不影响布局），故按需计算、无需缓存。
func (r *ToolbarRenderer) computeGeometry() toolbarGeometry {
	geom := resolveToolbarGeom(r.resolvedV3, GetDPIScale())
	tt := buildToolbarTree(ToolbarState{}, theme.ResolvedToolbarViews{}, geom)
	Layout(tt.root, 0, 0, zeroMeasurer{})
	return toolbarGeometry{
		size: tt.root.Rect().Size(),
		bounds: map[ToolbarHitResult]image.Rectangle{
			HitGrip:           tt.grip.Rect(),
			HitModeButton:     tt.mode.Rect(),
			HitWidthButton:    tt.width.Rect(),
			HitPunctButton:    tt.punct.Rect(),
			HitSettingsButton: tt.settings.Rect(),
		},
		hits: []toolbarHit{
			{HitGrip, viewOuterRect(tt.grip)},
			{HitModeButton, viewOuterRect(tt.mode)},
			{HitWidthButton, viewOuterRect(tt.width)},
			{HitPunctButton, viewOuterRect(tt.punct)},
			{HitSettingsButton, viewOuterRect(tt.settings)},
		},
	}
}

// HitTest determines which part of the toolbar was clicked（查 Layout 派生的命中带，无独立公式）。
func (r *ToolbarRenderer) HitTest(x, y, width, height int) ToolbarHitResult {
	pt := image.Pt(x, y)
	for _, h := range r.computeGeometry().hits {
		if pt.In(h.rect) {
			return h.kind
		}
	}
	return HitNone
}

// GetButtonBounds returns the bounds of a specific button（查 Layout 派生的 content 矩形）。
func (r *ToolbarRenderer) GetButtonBounds(button ToolbarHitResult) (x, y, w, h int) {
	rect, ok := r.computeGeometry().bounds[button]
	if !ok {
		return 0, 0, 0, 0
	}
	return rect.Min.X, rect.Min.Y, rect.Dx(), rect.Dy()
}

// GetToolbarSize returns the toolbar size（Layout 后 root 尺寸）。
func (r *ToolbarRenderer) GetToolbarSize() (width, height int) {
	sz := r.computeGeometry().size
	return sz.X, sz.Y
}

// CreateModeIndicatorColor returns the color for mode indicator
func CreateModeIndicatorColor(chineseMode bool) color.RGBA {
	if chineseMode {
		return color.RGBA{R: 66, G: 133, B: 244, A: 255} // Blue
	}
	return color.RGBA{R: 128, G: 128, B: 128, A: 255} // Gray
}

// RenderTooltip renders a tooltip with the given text
func (r *ToolbarRenderer) RenderTooltip(text string) *image.RGBA {
	scale := GetDPIScale()
	bgColor, textColor, borderColor := r.getTooltipColors()
	td := r.TextDrawer()

	fontSize := 12.0 * scale
	padding := 6.0 * scale

	// Measure text width
	textWidth := td.MeasureString(text, fontSize)

	width := int(textWidth + padding*2 + 2)
	height := int(fontSize + padding*2)

	dc := gg.NewContext(width, height)

	// Phase 1: Draw shapes with gg
	radius := 4.0 * scale
	dc.DrawRoundedRectangle(0, 0, float64(width), float64(height), radius)
	dc.SetColor(bgColor)
	dc.Fill()

	dc.DrawRoundedRectangle(0.5, 0.5, float64(width)-1, float64(height)-1, radius)
	dc.SetColor(borderColor)
	dc.SetLineWidth(1)
	dc.Stroke()

	// Phase 2: Draw text with TextDrawer
	img := dc.Image().(*image.RGBA)
	td.BeginDraw(img)
	tw := td.MeasureString(text, fontSize)
	td.DrawString(text, float64(width)/2-tw/2, float64(height)/2+fontSize*0.35, fontSize, textColor)
	td.EndDraw()

	DrawDebugBanner(img)
	return img
}
