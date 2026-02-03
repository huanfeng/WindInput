// Package ui provides native Windows UI for input method
package ui

import (
	"image"
	"image/color"

	"github.com/fogleman/gg"
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
	fontPath string
}

// NewToolbarRenderer creates a new toolbar renderer
func NewToolbarRenderer() *ToolbarRenderer {
	return &ToolbarRenderer{}
}

// SetFontPath sets the font path for rendering
func (r *ToolbarRenderer) SetFontPath(path string) {
	r.fontPath = path
}

// Render renders the toolbar with the given state
func (r *ToolbarRenderer) Render(state ToolbarState) *image.RGBA {
	scale := GetDPIScale()

	width := int(float64(toolbarBaseWidth) * scale)
	height := int(float64(toolbarBaseHeight) * scale)

	dc := gg.NewContext(width, height)

	// Background with rounded corners - brighter, more modern look
	radius := 6.0 * scale
	dc.DrawRoundedRectangle(0, 0, float64(width), float64(height), radius)
	// Gradient-like effect: lighter top to slightly darker bottom
	dc.SetRGBA(1.0, 1.0, 1.0, 0.98) // Pure white, slightly transparent
	dc.Fill()

	// Border - subtle blue tint for a more modern look
	dc.DrawRoundedRectangle(0.5, 0.5, float64(width)-1, float64(height)-1, radius)
	dc.SetRGBA(0.78, 0.82, 0.88, 1) // Light blue-gray border
	dc.SetLineWidth(1)
	dc.Stroke()

	// Load font
	fontSize := 14.0 * scale
	if err := r.loadFont(dc, fontSize); err != nil {
		// Continue without text if font fails
	}

	// Draw grip handle (left side)
	r.drawGrip(dc, scale, height)

	// Draw buttons
	x := gripWidth * scale
	buttonW := buttonWidth * scale
	padding := buttonPadding * scale

	// Mode button (中/En/A)
	r.drawModeButton(dc, x+padding, padding, buttonW-padding*2, float64(height)-padding*2, state, scale)
	x += buttonW

	// Full-width button (全/半)
	r.drawWidthButton(dc, x+padding, padding, buttonW-padding*2, float64(height)-padding*2, state.FullWidth, scale)
	x += buttonW

	// Punctuation button (，/,)
	r.drawPunctButton(dc, x+padding, padding, buttonW-padding*2, float64(height)-padding*2, state.ChinesePunct, scale)
	x += buttonW

	// Settings button (gear icon)
	r.drawSettingsButton(dc, x+padding, padding, buttonW-padding*2, float64(height)-padding*2, scale)

	return dc.Image().(*image.RGBA)
}

// loadFont loads the font for rendering
func (r *ToolbarRenderer) loadFont(dc *gg.Context, fontSize float64) error {
	// Try custom font first
	if r.fontPath != "" {
		if err := dc.LoadFontFace(r.fontPath, fontSize); err == nil {
			return nil
		}
	}

	// Try system fonts
	systemFonts := []string{
		"C:\\Windows\\Fonts\\msyh.ttc",    // Microsoft YaHei
		"C:\\Windows\\Fonts\\simhei.ttf",  // SimHei
		"C:\\Windows\\Fonts\\simsun.ttc",  // SimSun
		"C:\\Windows\\Fonts\\segoeui.ttf", // Segoe UI
	}

	for _, font := range systemFonts {
		if err := dc.LoadFontFace(font, fontSize); err == nil {
			return nil
		}
	}

	return nil
}

// drawGrip draws the grip handle for dragging
func (r *ToolbarRenderer) drawGrip(dc *gg.Context, scale float64, height int) {
	gripW := gripWidth * scale
	dotSize := 2.0 * scale
	dotGap := 4.0 * scale

	// Modern subtle grip dots with blue tint
	dc.SetRGBA(0.60, 0.68, 0.78, 0.7) // Blue-gray dots (#99ADC7)

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

// drawModeButton draws the mode button (中/En/A)
func (r *ToolbarRenderer) drawModeButton(dc *gg.Context, x, y, w, h float64, state ToolbarState, scale float64) {
	// Background - vibrant colors
	if state.ChineseMode {
		dc.SetRGBA(0.20, 0.56, 0.96, 1) // Bright blue for Chinese (#339AF5)
	} else {
		dc.SetRGBA(0.45, 0.50, 0.58, 1) // Slate gray for English (#737F94)
	}
	radius := 4.0 * scale
	dc.DrawRoundedRectangle(x, y, w, h, radius)
	dc.Fill()

	// Text
	dc.SetRGBA(1, 1, 1, 1)
	var text string
	if state.ChineseMode {
		text = "中"
	} else if state.CapsLock {
		text = "A"
	} else {
		text = "a"
	}
	dc.DrawStringAnchored(text, x+w/2, y+h/2, 0.5, 0.5)
}

// drawWidthButton draws the full/half width button
func (r *ToolbarRenderer) drawWidthButton(dc *gg.Context, x, y, w, h float64, fullWidth bool, scale float64) {
	// Background - modern teal/green
	if fullWidth {
		dc.SetRGBA(0.18, 0.72, 0.60, 1) // Teal green for full-width (#2EB899)
	} else {
		dc.SetRGBA(0.90, 0.92, 0.94, 1) // Light cool gray for half-width (#E6EAEF)
	}
	radius := 4.0 * scale
	dc.DrawRoundedRectangle(x, y, w, h, radius)
	dc.Fill()

	// Text
	if fullWidth {
		dc.SetRGBA(1, 1, 1, 1)
	} else {
		dc.SetRGBA(0.35, 0.40, 0.48, 1) // Darker text for better contrast
	}
	text := "半"
	if fullWidth {
		text = "全"
	}
	dc.DrawStringAnchored(text, x+w/2, y+h/2, 0.5, 0.5)
}

// drawPunctButton draws the punctuation button
func (r *ToolbarRenderer) drawPunctButton(dc *gg.Context, x, y, w, h float64, chinesePunct bool, scale float64) {
	// Background - vibrant orange/coral
	if chinesePunct {
		dc.SetRGBA(0.96, 0.52, 0.26, 1) // Coral orange for Chinese punctuation (#F58543)
	} else {
		dc.SetRGBA(0.90, 0.92, 0.94, 1) // Light cool gray for English punctuation (#E6EAEF)
	}
	radius := 4.0 * scale
	dc.DrawRoundedRectangle(x, y, w, h, radius)
	dc.Fill()

	// Text (comma as indicator)
	if chinesePunct {
		dc.SetRGBA(1, 1, 1, 1)
	} else {
		dc.SetRGBA(0.35, 0.40, 0.48, 1) // Darker text for better contrast
	}
	text := ","
	if chinesePunct {
		text = "\uFF0C" // Chinese comma ，
	}
	dc.DrawStringAnchored(text, x+w/2, y+h/2, 0.5, 0.5)
}

// drawSettingsButton draws the settings button (gear icon)
func (r *ToolbarRenderer) drawSettingsButton(dc *gg.Context, x, y, w, h float64, scale float64) {
	// Background - subtle purple/violet tint
	dc.SetRGBA(0.90, 0.92, 0.94, 1) // Light cool gray (#E6EAEF)
	radius := 4.0 * scale
	dc.DrawRoundedRectangle(x, y, w, h, radius)
	dc.Fill()

	// Draw gear icon - modern purple/violet color
	centerX := x + w/2
	centerY := y + h/2
	outerR := 8.0 * scale
	innerR := 4.0 * scale
	toothHeight := 2.5 * scale

	dc.SetRGBA(0.48, 0.40, 0.72, 1) // Muted purple (#7A66B8)

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
	dc.SetRGBA(0.90, 0.92, 0.94, 1) // Match button background
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

	// Calculate text size
	fontSize := 12.0 * scale
	padding := 6.0 * scale

	// Create temporary context to measure text
	tmpDc := gg.NewContext(1, 1)
	if err := r.loadFont(tmpDc, fontSize); err == nil {
		// Font loaded successfully
	}
	textWidth, _ := tmpDc.MeasureString(text)

	width := int(textWidth + padding*2 + 2)
	height := int(fontSize + padding*2)

	dc := gg.NewContext(width, height)

	// Background - dark tooltip style
	radius := 4.0 * scale
	dc.DrawRoundedRectangle(0, 0, float64(width), float64(height), radius)
	dc.SetRGBA(0.15, 0.18, 0.22, 0.95) // Dark background (#262E38)
	dc.Fill()

	// Border
	dc.DrawRoundedRectangle(0.5, 0.5, float64(width)-1, float64(height)-1, radius)
	dc.SetRGBA(0.3, 0.35, 0.42, 1) // Subtle border
	dc.SetLineWidth(1)
	dc.Stroke()

	// Text
	if err := r.loadFont(dc, fontSize); err == nil {
		dc.SetRGBA(0.95, 0.95, 0.95, 1) // Light text
		dc.DrawStringAnchored(text, float64(width)/2, float64(height)/2, 0.5, 0.5)
	}

	return dc.Image().(*image.RGBA)
}
