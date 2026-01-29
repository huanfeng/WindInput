package ui

import (
	"fmt"
	"image"
	"image/color"

	"github.com/fogleman/gg"
)

// RenderConfig contains rendering configuration
type RenderConfig struct {
	FontPath       string
	FontSize       float64
	IndexFontSize  float64
	Padding        float64
	ItemHeight     float64
	CornerRadius   float64
	BackgroundColor color.Color
	TextColor       color.Color
	IndexColor      color.Color
	IndexBgColor    color.Color
	InputBgColor    color.Color
	InputTextColor  color.Color
	BorderColor     color.Color
}

// DefaultRenderConfig returns default rendering configuration with DPI scaling
func DefaultRenderConfig() RenderConfig {
	// Get DPI scale factor
	scale := GetDPIScale()

	return RenderConfig{
		FontPath:        "",  // Will use system font
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
	}
}

// Renderer renders candidate window content
type Renderer struct {
	config   RenderConfig
	fontPath string
}

// NewRenderer creates a new renderer
func NewRenderer(config RenderConfig) *Renderer {
	return &Renderer{
		config: config,
	}
}

// SetFontPath sets the font path
func (r *Renderer) SetFontPath(path string) {
	r.fontPath = path
}

// UpdateFont updates font settings
func (r *Renderer) UpdateFont(fontSize float64, fontPath string) {
	scale := GetDPIScale()

	if fontSize > 0 {
		r.config.FontSize = fontSize * scale
		r.config.IndexFontSize = (fontSize - 4) * scale
	}

	if fontPath != "" {
		r.fontPath = fontPath
	}
}

// RenderCandidates renders candidates to an image
func (r *Renderer) RenderCandidates(candidates []Candidate, input string, page, totalPages int) *image.RGBA {
	cfg := r.config
	scale := GetDPIScale()

	// Calculate dimensions with DPI scaling
	candidateCount := len(candidates)
	if candidateCount == 0 {
		candidateCount = 1 // Show at least input area
	}

	width := 280.0 * scale
	inputHeight := 30.0 * scale
	contentHeight := float64(candidateCount) * cfg.ItemHeight
	pageInfoHeight := 0.0
	if totalPages > 1 {
		pageInfoHeight = 24.0 * scale
	}
	height := cfg.Padding*2 + inputHeight + contentHeight + pageInfoHeight + 4*scale // gaps scaled

	// Create context
	dc := gg.NewContext(int(width), int(height))

	// Draw rounded rectangle background with shadow effect
	r.drawRoundedRectWithShadow(dc, 0, 0, width, height, cfg.CornerRadius)

	// Draw background
	dc.SetColor(cfg.BackgroundColor)
	r.drawRoundedRect(dc, 2, 2, width-4, height-4, cfg.CornerRadius-1)
	dc.Fill()

	// Draw border
	dc.SetColor(cfg.BorderColor)
	dc.SetLineWidth(1)
	r.drawRoundedRect(dc, 1, 1, width-2, height-2, cfg.CornerRadius)
	dc.Stroke()

	// Load font - avoid .ttc files as they can cause issues with gg library
	fontLoaded := false
	if r.fontPath != "" {
		if err := dc.LoadFontFace(r.fontPath, cfg.FontSize); err == nil {
			fontLoaded = true
		}
	}
	if !fontLoaded {
		// Try common Windows fonts (prefer .ttf over .ttc)
		fonts := []string{
			"C:/Windows/Fonts/simhei.ttf",  // SimHei
			"C:/Windows/Fonts/simsun.ttf",  // SimSun (ttf version)
			"C:/Windows/Fonts/msyh.ttf",    // Microsoft YaHei (ttf version)
			"C:/Windows/Fonts/arial.ttf",   // Arial fallback
			"C:/Windows/Fonts/segoeui.ttf", // Segoe UI
		}
		for _, f := range fonts {
			if err := dc.LoadFontFace(f, cfg.FontSize); err == nil {
				fontLoaded = true
				r.fontPath = f
				break
			}
		}
	}

	y := cfg.Padding

	// Draw input area
	dc.SetColor(cfg.InputBgColor)
	r.drawRoundedRect(dc, cfg.Padding, y, width-cfg.Padding*2, inputHeight, 4*scale)
	dc.Fill()

	dc.SetColor(cfg.InputTextColor)
	if fontLoaded {
		dc.DrawString(input, cfg.Padding+8*scale, y+inputHeight/2+cfg.FontSize/3)
	}
	y += inputHeight + 4*scale

	// Draw candidates
	for i, cand := range candidates {
		itemY := y + float64(i)*cfg.ItemHeight

		// Draw index circle
		indexX := cfg.Padding + 14*scale
		indexY := itemY + cfg.ItemHeight/2
		dc.SetColor(cfg.IndexBgColor)
		dc.DrawCircle(indexX, indexY, 11*scale)
		dc.Fill()

		// Draw index number
		dc.SetColor(cfg.IndexColor)
		if fontLoaded {
			dc.LoadFontFace(r.fontPath, cfg.IndexFontSize)
		}
		indexStr := string(rune('0' + cand.Index))
		tw, _ := dc.MeasureString(indexStr)
		dc.DrawString(indexStr, indexX-tw/2, indexY+cfg.IndexFontSize/3)

		// Draw candidate text
		dc.SetColor(cfg.TextColor)
		if fontLoaded {
			dc.LoadFontFace(r.fontPath, cfg.FontSize)
		}
		dc.DrawString(cand.Text, cfg.Padding+32*scale, itemY+cfg.ItemHeight/2+cfg.FontSize/3)

		// Draw comment/hint (e.g., wubi code) if present
		if cand.Comment != "" {
			// Measure candidate text width to position comment after it
			candWidth, _ := dc.MeasureString(cand.Text)
			commentX := cfg.Padding + 32*scale + candWidth + 8*scale

			// Draw comment in a lighter color
			dc.SetColor(color.RGBA{150, 150, 150, 255})
			if fontLoaded {
				dc.LoadFontFace(r.fontPath, cfg.IndexFontSize) // Use smaller font for comment
			}
			dc.DrawString(cand.Comment, commentX, itemY+cfg.ItemHeight/2+cfg.IndexFontSize/3)
		}
	}

	// Draw page info
	if totalPages > 1 {
		pageY := y + float64(len(candidates))*cfg.ItemHeight + 4*scale
		dc.SetColor(cfg.InputTextColor)
		if fontLoaded {
			dc.LoadFontFace(r.fontPath, 12*scale)
		}
		pageText := fmt.Sprintf("%d / %d  (← →)", page, totalPages)
		tw, _ := dc.MeasureString(pageText)
		dc.DrawString(pageText, width/2-tw/2, pageY+16*scale)
	}

	return dc.Image().(*image.RGBA)
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

func (r *Renderer) drawRoundedRectWithShadow(dc *gg.Context, x, y, w, h, radius float64) {
	// Draw shadow layers
	for i := 3; i >= 0; i-- {
		offset := float64(i)
		alpha := uint8(20 - i*5)
		dc.SetColor(color.RGBA{0, 0, 0, alpha})
		r.drawRoundedRect(dc, x+offset, y+offset, w, h, radius)
		dc.Fill()
	}
}

// RenderModeIndicator renders a mode indicator (中/En)
func (r *Renderer) RenderModeIndicator(mode string) *image.RGBA {
	scale := GetDPIScale()

	width := 50.0 * scale
	height := 36.0 * scale
	fontSize := 20.0 * scale

	dc := gg.NewContext(int(width), int(height))

	// Draw background
	dc.SetColor(color.RGBA{50, 50, 50, 230})
	r.drawRoundedRect(dc, 2*scale, 2*scale, width-4*scale, height-4*scale, 6*scale)
	dc.Fill()

	// Load font - avoid .ttc files as they can cause issues with gg library
	fontLoaded := false
	if r.fontPath != "" {
		if err := dc.LoadFontFace(r.fontPath, fontSize); err == nil {
			fontLoaded = true
		}
	}
	if !fontLoaded {
		fonts := []string{
			"C:/Windows/Fonts/simhei.ttf",  // SimHei
			"C:/Windows/Fonts/msyh.ttf",    // Microsoft YaHei (ttf version)
			"C:/Windows/Fonts/arial.ttf",   // Arial fallback
			"C:/Windows/Fonts/segoeui.ttf", // Segoe UI
		}
		for _, f := range fonts {
			if err := dc.LoadFontFace(f, fontSize); err == nil {
				r.fontPath = f
				break
			}
		}
	}

	// Draw mode text
	dc.SetColor(color.RGBA{255, 255, 255, 255})
	tw, _ := dc.MeasureString(mode)
	dc.DrawString(mode, width/2-tw/2, height/2+7*scale)

	return dc.Image().(*image.RGBA)
}
