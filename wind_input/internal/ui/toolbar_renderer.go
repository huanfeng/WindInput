// Package ui provides native Windows UI for input method
package ui

import (
	"image"
	"image/color"

	"github.com/fogleman/gg"
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
	fontPath       string
	resolvedTheme  *theme.ResolvedTheme
	fontCache      *fontCache
	textRenderer   *TextRenderer
	dwriteRenderer *DWriteRenderer
	textDrawer     TextDrawer
	fontConfig     *FontConfig
}

// NewToolbarRenderer creates a new toolbar renderer
func NewToolbarRenderer() *ToolbarRenderer {
	fc := NewFontConfig()
	r := &ToolbarRenderer{
		fontConfig: fc,
	}
	r.SetTextRenderMode(TextRenderModeGDI)
	return r
}

func (r *ToolbarRenderer) resolvePrimaryFontPath() string {
	if r.fontPath != "" {
		r.fontConfig.SetPrimaryFont(r.fontPath)
	}
	resolved := r.fontConfig.ResolvePrimaryFont()
	if resolved != "" {
		r.fontPath = resolved
	}
	return resolved
}

func (r *ToolbarRenderer) ensureTextRenderer() *TextRenderer {
	if r.textRenderer != nil {
		return r.textRenderer
	}
	tr := NewTextRenderer()
	tr.SetGDIParams(r.fontConfig.GetEffectiveGDIWeight(), r.fontConfig.GetEffectiveGDIScale())
	if resolved := r.resolvePrimaryFontPath(); resolved != "" {
		tr.SetFont(resolved)
	}
	r.textRenderer = tr
	return tr
}

func (r *ToolbarRenderer) ensureDWriteRenderer() *DWriteRenderer {
	if r.dwriteRenderer != nil {
		return r.dwriteRenderer
	}
	dwr := NewDWriteRenderer("toolbar")
	dwr.SetGDIParams(r.fontConfig.GetEffectiveGDIWeight(), r.fontConfig.GetEffectiveGDIScale())
	if resolved := r.resolvePrimaryFontPath(); resolved != "" {
		dwr.SetFont(resolved)
	}
	r.dwriteRenderer = dwr
	return dwr
}

func (r *ToolbarRenderer) ensureFontCache() *fontCache {
	if r.fontCache == nil {
		r.fontCache = newFontCache()
	}
	if resolved := r.resolvePrimaryFontPath(); resolved != "" {
		r.fontCache.mu.Lock()
		_ = r.fontCache.loadFont(resolved)
		r.fontCache.mu.Unlock()
	}
	return r.fontCache
}

func (r *ToolbarRenderer) releaseGDIBackend() {
	if r.textRenderer != nil {
		r.textRenderer.Close()
		r.textRenderer = nil
	}
}

func (r *ToolbarRenderer) releaseDWriteBackend() {
	if r.dwriteRenderer != nil {
		r.dwriteRenderer.Close()
		r.dwriteRenderer = nil
	}
}

func (r *ToolbarRenderer) releaseFreeTypeBackend() {
	if r.fontCache != nil {
		r.fontCache.Close()
		r.fontCache = nil
	}
}

// SetGDIFontParams updates GDI font weight and scale for text rendering
func (r *ToolbarRenderer) SetGDIFontParams(weight int, scale float64) {
	r.fontConfig.SetGDIFontWeight(weight)
	r.fontConfig.SetGDIFontScale(scale)
	if r.textRenderer != nil {
		r.textRenderer.SetGDIParams(weight, scale)
	}
	if r.dwriteRenderer != nil {
		r.dwriteRenderer.SetGDIParams(weight, scale)
	}
}

// SetTextRenderMode switches between GDI, FreeType, and DirectWrite text rendering
func (r *ToolbarRenderer) SetTextRenderMode(mode TextRenderMode) {
	switch mode {
	case TextRenderModeFreetype:
		fc := r.ensureFontCache()
		r.releaseGDIBackend()
		r.releaseDWriteBackend()
		r.textDrawer = newFreeTypeDrawer(fc, r.fontConfig)
	case TextRenderModeDirectWrite:
		dwr := r.ensureDWriteRenderer()
		if dwr != nil && dwr.IsAvailable() {
			r.releaseGDIBackend()
			r.releaseFreeTypeBackend()
			r.textDrawer = newDirectWriteDrawer(dwr)
			return
		}
		r.releaseDWriteBackend()
		tr := r.ensureTextRenderer()
		r.releaseFreeTypeBackend()
		r.textDrawer = newGDIDrawer(tr)
	default:
		tr := r.ensureTextRenderer()
		r.releaseDWriteBackend()
		r.releaseFreeTypeBackend()
		r.textDrawer = newGDIDrawer(tr)
	}
}

// SetFontPath sets the font path for rendering
func (r *ToolbarRenderer) SetFontPath(path string) {
	r.fontPath = path
	resolved := r.resolvePrimaryFontPath()
	if r.fontCache != nil && resolved != "" {
		r.fontCache.mu.Lock()
		_ = r.fontCache.loadFont(resolved)
		r.fontCache.mu.Unlock()
	}
	if r.textRenderer != nil && resolved != "" {
		r.textRenderer.SetFont(resolved)
	}
	if r.dwriteRenderer != nil && resolved != "" {
		r.dwriteRenderer.SetFont(resolved)
	}
}

// Close releases renderer-owned font and text resources.
func (r *ToolbarRenderer) Close() {
	r.releaseFreeTypeBackend()
	r.releaseGDIBackend()
	r.releaseDWriteBackend()
}

// SetTheme sets the theme for the toolbar renderer
func (r *ToolbarRenderer) SetTheme(resolved *theme.ResolvedTheme) {
	r.resolvedTheme = resolved
}

// getToolbarColors returns toolbar colors from theme or defaults
func (r *ToolbarRenderer) getToolbarColors() *theme.ResolvedToolbarColors {
	if r.resolvedTheme != nil {
		return &r.resolvedTheme.Toolbar
	}
	// Return default colors
	return &theme.ResolvedToolbarColors{
		BackgroundColor:     color.RGBA{255, 255, 255, 255},
		BorderColor:         color.RGBA{199, 209, 224, 255},
		GripColor:           color.RGBA{153, 173, 199, 179},
		ModeChineseBgColor:  color.RGBA{51, 154, 245, 255},
		ModeEnglishBgColor:  color.RGBA{115, 127, 148, 255},
		ModeTextColor:       color.RGBA{255, 255, 255, 255},
		FullWidthOnBgColor:  color.RGBA{46, 184, 153, 255},
		FullWidthOffBgColor: color.RGBA{230, 234, 239, 255},
		FullWidthOnColor:    color.RGBA{255, 255, 255, 255},
		FullWidthOffColor:   color.RGBA{89, 102, 122, 255},
		PunctChineseBgColor: color.RGBA{245, 133, 67, 255},
		PunctEnglishBgColor: color.RGBA{230, 234, 239, 255},
		PunctChineseColor:   color.RGBA{255, 255, 255, 255},
		PunctEnglishColor:   color.RGBA{89, 102, 122, 255},
		SettingsBgColor:     color.RGBA{230, 234, 239, 255},
		SettingsIconColor:   color.RGBA{122, 102, 184, 255},
		SettingsHoleColor:   color.RGBA{230, 234, 239, 255},
	}
}

// getTooltipColors returns tooltip colors from theme or defaults
func (r *ToolbarRenderer) getTooltipColors() (bgColor, textColor, borderColor color.Color) {
	if r.resolvedTheme != nil {
		return r.resolvedTheme.Tooltip.BackgroundColor, r.resolvedTheme.Tooltip.TextColor, color.RGBA{77, 89, 107, 255}
	}
	return color.RGBA{38, 46, 56, 242}, color.RGBA{242, 242, 242, 255}, color.RGBA{77, 89, 107, 255}
}

// Render renders the toolbar with the given state
func (r *ToolbarRenderer) Render(state ToolbarState) *image.RGBA {
	scale := GetDPIScale()
	colors := r.getToolbarColors()

	width := int(float64(toolbarBaseWidth) * scale)
	height := int(float64(toolbarBaseHeight) * scale)

	dc := gg.NewContext(width, height)
	fontSize := 14.0 * scale

	// ========== Phase 1: Draw all shapes with gg ==========

	// Background with rounded corners
	radius := 6.0 * scale
	dc.DrawRoundedRectangle(0, 0, float64(width), float64(height), radius)
	dc.SetColor(colors.BackgroundColor)
	dc.Fill()

	// Border
	dc.DrawRoundedRectangle(0.5, 0.5, float64(width)-1, float64(height)-1, radius)
	dc.SetColor(colors.BorderColor)
	dc.SetLineWidth(1)
	dc.Stroke()

	// Draw grip handle (left side)
	r.drawGrip(dc, scale, height, colors)

	// Draw button backgrounds
	x := gripWidth * scale
	buttonW := buttonWidth * scale
	padding := buttonPadding * scale

	// Mode button background
	modeBtnX, modeBtnY := x+padding, padding
	modeBtnW, modeBtnH := buttonW-padding*2, float64(height)-padding*2
	r.drawModeButton(dc, modeBtnX, modeBtnY, modeBtnW, modeBtnH, state, scale, colors)
	x += buttonW

	// Full-width button background
	widthBtnX, widthBtnY := x+padding, padding
	widthBtnW, widthBtnH := buttonW-padding*2, float64(height)-padding*2
	r.drawWidthButton(dc, widthBtnX, widthBtnY, widthBtnW, widthBtnH, state.FullWidth, scale, colors)
	x += buttonW

	// Punctuation button background
	punctBtnX, punctBtnY := x+padding, padding
	punctBtnW, punctBtnH := buttonW-padding*2, float64(height)-padding*2
	r.drawPunctButton(dc, punctBtnX, punctBtnY, punctBtnW, punctBtnH, state.ChinesePunct, scale, colors)
	x += buttonW

	// Settings button (shapes only, no text)
	r.drawSettingsButton(dc, x+padding, padding, buttonW-padding*2, float64(height)-padding*2, scale, colors)

	// ========== Phase 2: Draw all text with TextDrawer ==========
	img := dc.Image().(*image.RGBA)
	td := r.textDrawer
	td.BeginDraw(img)

	// Mode button text
	var modeText string
	if state.ChineseMode {
		modeText = "中"
	} else if state.CapsLock {
		modeText = "A"
	} else {
		modeText = "a"
	}
	tw := td.MeasureString(modeText, fontSize)
	td.DrawString(modeText, modeBtnX+modeBtnW/2-tw/2, modeBtnY+modeBtnH/2+fontSize*0.35, fontSize, colors.ModeTextColor)

	// Width button text
	var widthText string
	var widthTextColor color.Color
	if state.FullWidth {
		widthText = "全"
		widthTextColor = colors.FullWidthOnColor
	} else {
		widthText = "半"
		widthTextColor = colors.FullWidthOffColor
	}
	tw = td.MeasureString(widthText, fontSize)
	td.DrawString(widthText, widthBtnX+widthBtnW/2-tw/2, widthBtnY+widthBtnH/2+fontSize*0.35, fontSize, widthTextColor)

	// Punct button text
	var punctText string
	var punctTextColor color.Color
	if state.ChinesePunct {
		punctText = "\uFF0C" // Chinese comma ，
		punctTextColor = colors.PunctChineseColor
	} else {
		punctText = ","
		punctTextColor = colors.PunctEnglishColor
	}
	tw = td.MeasureString(punctText, fontSize)
	td.DrawString(punctText, punctBtnX+punctBtnW/2-tw/2, punctBtnY+punctBtnH/2+fontSize*0.35, fontSize, punctTextColor)

	td.EndDraw()
	return img
}

// drawGrip draws the grip handle for dragging
func (r *ToolbarRenderer) drawGrip(dc *gg.Context, scale float64, height int, colors *theme.ResolvedToolbarColors) {
	gripW := gripWidth * scale
	dotSize := 2.0 * scale
	dotGap := 4.0 * scale

	// Modern subtle grip dots
	dc.SetColor(colors.GripColor)

	// Draw dots pattern
	startY := float64(height)/2 - dotGap
	for row := 0; row < 3; row++ {
		y := startY + float64(row)*dotGap
		for col := 0; col < 2; col++ {
			x := gripW/2 - dotGap/2 + float64(col)*dotGap
			dc.DrawCircle(x, y, dotSize/2)
			dc.Fill()
		}
	}
}

// drawModeButton draws the mode button background (text drawn separately in Phase 2)
func (r *ToolbarRenderer) drawModeButton(dc *gg.Context, x, y, w, h float64, state ToolbarState, scale float64, colors *theme.ResolvedToolbarColors) {
	if state.ChineseMode {
		dc.SetColor(colors.ModeChineseBgColor)
	} else {
		dc.SetColor(colors.ModeEnglishBgColor)
	}
	radius := 4.0 * scale
	dc.DrawRoundedRectangle(x, y, w, h, radius)
	dc.Fill()
}

// drawWidthButton draws the full/half width button background (text drawn separately in Phase 2)
func (r *ToolbarRenderer) drawWidthButton(dc *gg.Context, x, y, w, h float64, fullWidth bool, scale float64, colors *theme.ResolvedToolbarColors) {
	if fullWidth {
		dc.SetColor(colors.FullWidthOnBgColor)
	} else {
		dc.SetColor(colors.FullWidthOffBgColor)
	}
	radius := 4.0 * scale
	dc.DrawRoundedRectangle(x, y, w, h, radius)
	dc.Fill()
}

// drawPunctButton draws the punctuation button background (text drawn separately in Phase 2)
func (r *ToolbarRenderer) drawPunctButton(dc *gg.Context, x, y, w, h float64, chinesePunct bool, scale float64, colors *theme.ResolvedToolbarColors) {
	if chinesePunct {
		dc.SetColor(colors.PunctChineseBgColor)
	} else {
		dc.SetColor(colors.PunctEnglishBgColor)
	}
	radius := 4.0 * scale
	dc.DrawRoundedRectangle(x, y, w, h, radius)
	dc.Fill()
}

// drawSettingsButton draws the settings button (gear icon)
func (r *ToolbarRenderer) drawSettingsButton(dc *gg.Context, x, y, w, h float64, scale float64, colors *theme.ResolvedToolbarColors) {
	// Background
	dc.SetColor(colors.SettingsBgColor)
	radius := 4.0 * scale
	dc.DrawRoundedRectangle(x, y, w, h, radius)
	dc.Fill()

	// Draw gear icon
	centerX := x + w/2
	centerY := y + h/2
	outerR := 8.0 * scale
	innerR := 4.0 * scale
	toothHeight := 2.5 * scale

	dc.SetColor(colors.SettingsIconColor)

	// Draw gear teeth
	teeth := 8
	for i := 0; i < teeth; i++ {
		angle := float64(i) * 360.0 / float64(teeth)
		dc.Push()
		dc.RotateAbout(gg.Radians(angle), centerX, centerY)
		dc.DrawRectangle(centerX-toothHeight/2, centerY-outerR, toothHeight, toothHeight)
		dc.Fill()
		dc.Pop()
	}

	// Draw outer circle
	dc.DrawCircle(centerX, centerY, outerR-toothHeight)
	dc.Fill()

	// Draw inner circle (hole)
	dc.SetColor(colors.SettingsHoleColor)
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
	td := r.textDrawer

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

	return img
}
