//go:build windows

// Package ui provides native Windows UI for input method
package ui

import (
	"image"
	"image/color"

	"github.com/gogpu/gg"
	"github.com/huanfeng/wind_input/pkg/theme"
)

// Toolbar layout constants (will be scaled for DPI)
const (
	toolbarBaseWidth  = 116 // gripWidth + 4 * buttonWidth + 2 = 10 + 104 + 2 = 116
	toolbarBaseHeight = 30
	gripWidth         = 10
	buttonWidth       = 26
	buttonPadding     = 2
)

// ToolbarRenderer renders the toolbar UI
type ToolbarRenderer struct {
	resolvedV25 *theme.ResolvedV25
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

// SetTheme sets the theme for the toolbar renderer（P5：吃 ResolvedV25）
func (r *ToolbarRenderer) SetTheme(rv *theme.ResolvedV25) {
	r.resolvedV25 = rv
}

// getTooltipColors returns tooltip colors from theme or defaults（工具栏内悬停提示，复用 Palette.Tooltip）
func (r *ToolbarRenderer) getTooltipColors() (bgColor, textColor, borderColor color.Color) {
	if r.resolvedV25 != nil {
		return r.resolvedV25.Palette.Tooltip.Background, r.resolvedV25.Palette.Tooltip.Text, color.RGBA{77, 89, 107, 255}
	}
	return color.RGBA{38, 46, 56, 242}, color.RGBA{242, 242, 242, 255}, color.RGBA{77, 89, 107, 255}
}

// Render renders the toolbar with the given state（盒模型 View 引擎）。
// View 承载整条背景/边框 + 4 按钮框 + mode 文字；grip/全半角/标点/齿轮矢量符号后处理。
func (r *ToolbarRenderer) Render(state ToolbarState) *image.RGBA {
	scale := GetDPIScale()
	rtv := r.resolveToolbarViews()
	td := r.TextDrawer()

	tt := buildToolbarTree(state, rtv, scale)
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
	for row := 0; row < 3; row++ {
		y := startY + float64(row)*dotGap
		for col := 0; col < 2; col++ {
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
	for i := 0; i < teeth; i++ {
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

// HitTest determines which part of the toolbar was clicked
func (r *ToolbarRenderer) HitTest(x, y, width, height int) ToolbarHitResult {
	scale := GetDPIScale()

	// Check grip area
	gripW := int(gripWidth * scale)
	if x < gripW {
		return HitGrip
	}

	// Check buttons
	buttonW := int(buttonWidth * scale)
	buttonX := gripW

	// Mode button
	if x >= buttonX && x < buttonX+buttonW {
		return HitModeButton
	}
	buttonX += buttonW

	// Width button
	if x >= buttonX && x < buttonX+buttonW {
		return HitWidthButton
	}
	buttonX += buttonW

	// Punctuation button
	if x >= buttonX && x < buttonX+buttonW {
		return HitPunctButton
	}
	buttonX += buttonW

	// Settings button
	if x >= buttonX && x < buttonX+buttonW {
		return HitSettingsButton
	}

	return HitNone
}

// GetButtonBounds returns the bounds of a specific button
func (r *ToolbarRenderer) GetButtonBounds(button ToolbarHitResult) (x, y, w, h int) {
	scale := GetDPIScale()
	height := int(toolbarBaseHeight * scale)
	gripW := int(gripWidth * scale)
	buttonW := int(buttonWidth * scale)
	padding := int(buttonPadding * scale)

	switch button {
	case HitGrip:
		return 0, 0, gripW, height
	case HitModeButton:
		return gripW + padding, padding, buttonW - padding*2, height - padding*2
	case HitWidthButton:
		return gripW + buttonW + padding, padding, buttonW - padding*2, height - padding*2
	case HitPunctButton:
		return gripW + buttonW*2 + padding, padding, buttonW - padding*2, height - padding*2
	case HitSettingsButton:
		return gripW + buttonW*3 + padding, padding, buttonW - padding*2, height - padding*2
	}
	return 0, 0, 0, 0
}

// GetToolbarSize returns the toolbar size
func (r *ToolbarRenderer) GetToolbarSize() (width, height int) {
	scale := GetDPIScale()
	return int(toolbarBaseWidth * scale), int(toolbarBaseHeight * scale)
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
