package ui

import (
	"fmt"
	"image"
	"image/color"
	"os"
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
	HoverBgColor    color.Color // Background color for hovered candidate
	Layout          string      // "horizontal" or "vertical"
	HidePreedit     bool        // Hide preedit area when inline_preedit is enabled
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
type fontCache struct {
	mu       sync.RWMutex
	font     *truetype.Font
	fontPath string
	faces    map[float64]font.Face // Cache font faces by size
}

func newFontCache() *fontCache {
	return &fontCache{
		faces: make(map[float64]font.Face),
	}
}

// loadFont loads a TTF font from the given path
func (fc *fontCache) loadFont(path string) error {
	data, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	f, err := truetype.Parse(data)
	if err != nil {
		return err
	}
	fc.font = f
	fc.fontPath = path
	fc.faces = make(map[float64]font.Face) // Clear cached faces
	return nil
}

// getFace returns a cached font face for the given size
func (fc *fontCache) getFace(size float64) font.Face {
	fc.mu.RLock()
	if face, ok := fc.faces[size]; ok {
		fc.mu.RUnlock()
		return face
	}
	fc.mu.RUnlock()

	fc.mu.Lock()
	defer fc.mu.Unlock()

	// Double-check
	if face, ok := fc.faces[size]; ok {
		return face
	}

	if fc.font == nil {
		return nil
	}

	face := truetype.NewFace(fc.font, &truetype.Options{
		Size:    size,
		DPI:     72,
		Hinting: font.HintingFull,
	})
	fc.faces[size] = face
	return face
}

// Renderer renders candidate window content
type Renderer struct {
	config        RenderConfig
	fontPath      string
	fontCache     *fontCache
	fontReady     bool
	resolvedTheme *theme.ResolvedTheme
}

// NewRenderer creates a new renderer
func NewRenderer(config RenderConfig) *Renderer {
	r := &Renderer{
		config:    config,
		fontCache: newFontCache(),
	}
	// Pre-load font on creation
	r.ensureFontLoaded()
	return r
}

// SetFontPath sets the font path
func (r *Renderer) SetFontPath(path string) {
	r.fontPath = path
	r.fontReady = false
	r.ensureFontLoaded()
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

// ensureFontLoaded loads the font if not already cached
func (r *Renderer) ensureFontLoaded() {
	if r.fontReady && r.fontCache.font != nil {
		return
	}

	r.fontCache.mu.Lock()
	defer r.fontCache.mu.Unlock()

	// Try user-specified font first
	if r.fontPath != "" {
		if err := r.fontCache.loadFont(r.fontPath); err == nil {
			r.fontReady = true
			return
		}
	}

	// Try common Windows fonts (prefer .ttf over .ttc)
	fonts := []string{
		"C:/Windows/Fonts/simhei.ttf",  // SimHei
		"C:/Windows/Fonts/simsun.ttf",  // SimSun (ttf version)
		"C:/Windows/Fonts/msyh.ttf",    // Microsoft YaHei (ttf version)
		"C:/Windows/Fonts/arial.ttf",   // Arial fallback
		"C:/Windows/Fonts/segoeui.ttf", // Segoe UI
	}
	for _, path := range fonts {
		if err := r.fontCache.loadFont(path); err == nil {
			r.fontPath = path
			r.fontReady = true
			return
		}
	}
}

// RenderCandidates renders candidates to an image
// Optimized to minimize font loading operations
// hoverIndex: index of the hovered candidate (-1 for none)
// Returns the rendered image and candidate bounding rectangles for hit testing
func (r *Renderer) RenderCandidates(candidates []Candidate, input string, cursorPos int, page, totalPages int, hoverIndex int) (*image.RGBA, *RenderResult) {
	cfg := r.config

	// Choose layout based on config
	if cfg.Layout == "horizontal" {
		return r.renderHorizontalCandidates(candidates, input, cursorPos, page, totalPages, hoverIndex)
	}
	return r.renderVerticalCandidates(candidates, input, cursorPos, page, totalPages, hoverIndex)
}

// renderVerticalCandidates renders candidates in vertical layout (traditional style)
func (r *Renderer) renderVerticalCandidates(candidates []Candidate, input string, cursorPos int, page, totalPages int, hoverIndex int) (*image.RGBA, *RenderResult) {
	cfg := r.config
	scale := GetDPIScale()

	// Calculate dimensions with DPI scaling
	candidateCount := len(candidates)
	if candidateCount == 0 {
		candidateCount = 1 // Show at least input area
	}

	width := 280.0 * scale

	// 动态调整宽度以适应长输入文本
	mainFace := r.fontCache.getFace(cfg.FontSize)
	if mainFace != nil && input != "" {
		tmpDc := gg.NewContext(1, 1)
		tmpDc.SetFontFace(mainFace)
		inputTextWidth, _ := tmpDc.MeasureString(input)
		minInputWidth := inputTextWidth + cfg.Padding*2 + 16*scale
		if minInputWidth > width {
			width = minInputWidth
		}
	}

	inputHeight := 30.0 * scale
	if cfg.HidePreedit {
		inputHeight = 0
	}
	contentHeight := float64(candidateCount) * cfg.ItemHeight
	pageInfoHeight := 0.0
	if totalPages > 1 {
		pageInfoHeight = 24.0 * scale
	}
	height := cfg.Padding*2 + inputHeight + contentHeight + pageInfoHeight + 4*scale
	if cfg.HidePreedit {
		height = cfg.Padding*2 + contentHeight + pageInfoHeight
	}

	// Create context
	dc := gg.NewContext(int(width), int(height))

	// Draw shadow (simplified - just 1 layer instead of 4)
	dc.SetColor(r.getShadowColor())
	r.drawRoundedRect(dc, 2, 2, width, height, cfg.CornerRadius)
	dc.Fill()

	// Draw background
	dc.SetColor(cfg.BackgroundColor)
	r.drawRoundedRect(dc, 0, 0, width-2, height-2, cfg.CornerRadius)
	dc.Fill()

	// Draw border
	dc.SetColor(cfg.BorderColor)
	dc.SetLineWidth(1)
	r.drawRoundedRect(dc, 0.5, 0.5, width-3, height-3, cfg.CornerRadius)
	dc.Stroke()

	// Get cached font faces (mainFace already obtained above for width calculation)
	if mainFace == nil {
		mainFace = r.fontCache.getFace(cfg.FontSize)
	}
	smallFace := r.fontCache.getFace(cfg.IndexFontSize)
	pageFace := r.fontCache.getFace(12 * scale)

	y := cfg.Padding

	// Draw input area (if not hidden)
	if !cfg.HidePreedit {
		dc.SetColor(cfg.InputBgColor)
		r.drawRoundedRect(dc, cfg.Padding, y, width-cfg.Padding*2-2, inputHeight, 4*scale)
		dc.Fill()

		// Draw input text and cursor
		if mainFace != nil {
			dc.SetFontFace(mainFace)
			textX := cfg.Padding + 8*scale
			textY := y + inputHeight/2 + cfg.FontSize/3

			dc.SetColor(cfg.InputTextColor)
			dc.DrawString(input, textX, textY)

			// Draw cursor indicator
			if cursorPos >= 0 && cursorPos <= len(input) {
				cursorText := input[:cursorPos]
				cursorX, _ := dc.MeasureString(cursorText)
				cursorDrawX := textX + cursorX
				cursorTopY := y + 4*scale
				cursorBottomY := y + inputHeight - 4*scale
				dc.SetColor(cfg.InputTextColor)
				dc.SetLineWidth(1.5 * scale)
				dc.DrawLine(cursorDrawX, cursorTopY, cursorDrawX, cursorBottomY)
				dc.Stroke()
			}
		}
		y += inputHeight + 4*scale
	}

	// Build candidate rectangles for hit testing
	result := &RenderResult{
		Rects: make([]CandidateRect, len(candidates)),
	}

	// Draw hover background first (if any)
	if hoverIndex >= 0 && hoverIndex < len(candidates) {
		itemY := y + float64(hoverIndex)*cfg.ItemHeight
		dc.SetColor(cfg.HoverBgColor)
		r.drawRoundedRect(dc, cfg.Padding-2, itemY, width-cfg.Padding*2+2, cfg.ItemHeight, 4*scale)
		dc.Fill()
	}

	// First pass: draw all index circles and record bounding boxes
	for i := range candidates {
		itemY := y + float64(i)*cfg.ItemHeight
		indexX := cfg.Padding + 14*scale
		indexY := itemY + cfg.ItemHeight/2
		dc.SetColor(cfg.IndexBgColor)
		dc.DrawCircle(indexX, indexY, 11*scale)
		dc.Fill()

		// Record bounding rectangle
		result.Rects[i] = CandidateRect{
			Index: i,
			X:     cfg.Padding - 2,
			Y:     itemY,
			W:     width - cfg.Padding*2 + 2,
			H:     cfg.ItemHeight,
		}
	}

	// Second pass: draw all index numbers (small font)
	if smallFace != nil {
		dc.SetFontFace(smallFace)
		dc.SetColor(cfg.IndexColor)
		for i, cand := range candidates {
			itemY := y + float64(i)*cfg.ItemHeight
			indexX := cfg.Padding + 14*scale
			indexY := itemY + cfg.ItemHeight/2
			indexStr := string(rune('0' + cand.Index))
			tw, _ := dc.MeasureString(indexStr)
			dc.DrawString(indexStr, indexX-tw/2, indexY+cfg.IndexFontSize/3)
		}
	}

	// Third pass: draw all candidate texts (main font)
	type commentInfo struct {
		text string
		x    float64
		y    float64
	}
	var comments []commentInfo

	if mainFace != nil {
		dc.SetFontFace(mainFace)
		dc.SetColor(cfg.TextColor)
		for i, cand := range candidates {
			itemY := y + float64(i)*cfg.ItemHeight
			dc.DrawString(cand.Text, cfg.Padding+32*scale, itemY+cfg.ItemHeight/2+cfg.FontSize/3)

			if cand.Comment != "" {
				candWidth, _ := dc.MeasureString(cand.Text)
				comments = append(comments, commentInfo{
					text: cand.Comment,
					x:    cfg.Padding + 32*scale + candWidth + 8*scale,
					y:    itemY + cfg.ItemHeight/2 + cfg.IndexFontSize/3,
				})
			}
		}
	}

	// Fourth pass: draw all comments (small font)
	if len(comments) > 0 && smallFace != nil {
		dc.SetFontFace(smallFace)
		dc.SetColor(r.getCommentColor())
		for _, c := range comments {
			dc.DrawString(c.text, c.x, c.y)
		}
	}

	// Draw page info
	if totalPages > 1 && pageFace != nil {
		pageY := y + float64(len(candidates))*cfg.ItemHeight + 4*scale
		dc.SetFontFace(pageFace)
		dc.SetColor(cfg.InputTextColor)
		pageText := fmt.Sprintf("%d / %d  (← →)", page, totalPages)
		tw, _ := dc.MeasureString(pageText)
		dc.DrawString(pageText, width/2-tw/2, pageY+16*scale)
	}

	return dc.Image().(*image.RGBA), result
}

// renderHorizontalCandidates renders candidates in horizontal layout (modern style)
func (r *Renderer) renderHorizontalCandidates(candidates []Candidate, input string, cursorPos int, page, totalPages int, hoverIndex int) (*image.RGBA, *RenderResult) {
	cfg := r.config
	scale := GetDPIScale()

	// Get cached font faces
	mainFace := r.fontCache.getFace(cfg.FontSize)
	smallFace := r.fontCache.getFace(cfg.IndexFontSize)
	pageFace := r.fontCache.getFace(12 * scale)

	// Measure all candidates to calculate total width
	type candMeasure struct {
		textWidth    float64
		commentWidth float64
		totalWidth   float64 // Total width including index, text, comment
	}
	measures := make([]candMeasure, len(candidates))

	indexSize := 18.0 * scale   // Index circle size
	indexMargin := 4.0 * scale  // Margin between index and text
	itemSpacing := 12.0 * scale // Space between items

	tmpDc := gg.NewContext(1, 1)
	if mainFace != nil {
		tmpDc.SetFontFace(mainFace)
		for i, cand := range candidates {
			tw, _ := tmpDc.MeasureString(cand.Text)
			measures[i].textWidth = tw
		}
	}
	if smallFace != nil {
		tmpDc.SetFontFace(smallFace)
		for i, cand := range candidates {
			if cand.Comment != "" {
				cw, _ := tmpDc.MeasureString(cand.Comment)
				measures[i].commentWidth = cw
			}
		}
	}

	// Calculate total width for each candidate
	for i, cand := range candidates {
		measures[i].totalWidth = indexSize + indexMargin + measures[i].textWidth
		if cand.Comment != "" {
			measures[i].totalWidth += 6*scale + measures[i].commentWidth
		}
	}

	// Calculate total candidates width
	candidatesWidth := 0.0
	for i := range candidates {
		candidatesWidth += measures[i].totalWidth
		if i < len(candidates)-1 {
			candidatesWidth += itemSpacing
		}
	}

	// Page info width
	pageInfoWidth := 0.0
	if totalPages > 1 && pageFace != nil {
		tmpDc.SetFontFace(pageFace)
		pageText := fmt.Sprintf("%d/%d", page, totalPages)
		pageInfoWidth, _ = tmpDc.MeasureString(pageText)
		pageInfoWidth += 20 * scale // padding
	}

	// Input area (preedit)
	inputWidth := 0.0
	inputHeight := 0.0
	if !cfg.HidePreedit && input != "" {
		if mainFace != nil {
			tmpDc.SetFontFace(mainFace)
			inputWidth, _ = tmpDc.MeasureString(input)
			inputWidth += 16 * scale // padding
		}
		inputHeight = 24 * scale
	}

	// Total width
	minWidth := 200.0 * scale
	contentWidth := cfg.Padding*2 + candidatesWidth + pageInfoWidth
	if inputWidth > 0 {
		contentWidth = cfg.Padding*2 + inputWidth
		if candidatesWidth+pageInfoWidth > inputWidth {
			contentWidth = cfg.Padding*2 + candidatesWidth + pageInfoWidth
		}
	}
	width := contentWidth
	if width < minWidth {
		width = minWidth
	}

	// Height calculation
	candidateRowHeight := 32.0 * scale
	height := cfg.Padding*2 + candidateRowHeight
	if inputHeight > 0 {
		height += inputHeight + 4*scale
	}

	// Create context
	dc := gg.NewContext(int(width), int(height))

	// Draw shadow
	dc.SetColor(r.getShadowColor())
	r.drawRoundedRect(dc, 2, 2, width, height, cfg.CornerRadius)
	dc.Fill()

	// Draw background
	dc.SetColor(cfg.BackgroundColor)
	r.drawRoundedRect(dc, 0, 0, width-2, height-2, cfg.CornerRadius)
	dc.Fill()

	// Draw border
	dc.SetColor(cfg.BorderColor)
	dc.SetLineWidth(1)
	r.drawRoundedRect(dc, 0.5, 0.5, width-3, height-3, cfg.CornerRadius)
	dc.Stroke()

	y := cfg.Padding

	// Draw input area (preedit) - if not hidden
	if !cfg.HidePreedit && input != "" {
		dc.SetColor(cfg.InputBgColor)
		r.drawRoundedRect(dc, cfg.Padding, y, width-cfg.Padding*2-2, inputHeight, 4*scale)
		dc.Fill()

		if mainFace != nil {
			dc.SetFontFace(mainFace)
			textX := cfg.Padding + 8*scale
			textY := y + inputHeight/2 + cfg.FontSize/3

			dc.SetColor(cfg.InputTextColor)
			dc.DrawString(input, textX, textY)

			// Draw cursor indicator
			if cursorPos >= 0 && cursorPos <= len(input) {
				cursorText := input[:cursorPos]
				cursorXOffset, _ := dc.MeasureString(cursorText)
				cursorDrawX := textX + cursorXOffset
				cursorTopY := y + 3*scale
				cursorBottomY := y + inputHeight - 3*scale
				dc.SetColor(cfg.InputTextColor)
				dc.SetLineWidth(1.5 * scale)
				dc.DrawLine(cursorDrawX, cursorTopY, cursorDrawX, cursorBottomY)
				dc.Stroke()
			}
		}
		y += inputHeight + 4*scale
	}

	// Build candidate rectangles for hit testing
	result := &RenderResult{
		Rects: make([]CandidateRect, len(candidates)),
	}

	// Draw candidates horizontally
	x := cfg.Padding
	candY := y + candidateRowHeight/2

	for i, cand := range candidates {
		itemWidth := measures[i].totalWidth

		// Record bounding rectangle (before drawing)
		result.Rects[i] = CandidateRect{
			Index: i,
			X:     x - 4,
			Y:     y,
			W:     itemWidth + 8,
			H:     candidateRowHeight,
		}

		// Draw hover background if this is the hovered item
		if i == hoverIndex {
			dc.SetColor(cfg.HoverBgColor)
			r.drawRoundedRect(dc, x-4, y, itemWidth+8, candidateRowHeight, 4*scale)
			dc.Fill()
		}

		// Draw index circle
		indexX := x + indexSize/2
		dc.SetColor(cfg.IndexBgColor)
		dc.DrawCircle(indexX, candY, indexSize/2)
		dc.Fill()

		// Draw index number
		if smallFace != nil {
			dc.SetFontFace(smallFace)
			dc.SetColor(cfg.IndexColor)
			indexStr := string(rune('0' + cand.Index))
			tw, _ := dc.MeasureString(indexStr)
			dc.DrawString(indexStr, indexX-tw/2, candY+cfg.IndexFontSize/3)
		}

		// Draw candidate text
		textX := x + indexSize + indexMargin
		if mainFace != nil {
			dc.SetFontFace(mainFace)
			dc.SetColor(cfg.TextColor)
			dc.DrawString(cand.Text, textX, candY+cfg.FontSize/3)
		}

		// Draw comment if present
		if cand.Comment != "" && smallFace != nil {
			commentX := textX + measures[i].textWidth + 6*scale
			dc.SetFontFace(smallFace)
			dc.SetColor(r.getCommentColor())
			dc.DrawString(cand.Comment, commentX, candY+cfg.IndexFontSize/3)
		}

		// Move to next item
		x += itemWidth + itemSpacing
	}

	// Draw page info (right aligned)
	if totalPages > 1 && pageFace != nil {
		dc.SetFontFace(pageFace)
		dc.SetColor(cfg.InputTextColor)
		pageText := fmt.Sprintf("%d/%d", page, totalPages)
		tw, _ := dc.MeasureString(pageText)
		dc.DrawString(pageText, width-cfg.Padding-tw, candY+6*scale)
	}

	return dc.Image().(*image.RGBA), result
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

// RenderModeIndicator renders a mode indicator (中/En)
func (r *Renderer) RenderModeIndicator(mode string) *image.RGBA {
	scale := GetDPIScale()

	width := 50.0 * scale
	height := 36.0 * scale
	fontSize := 20.0 * scale

	dc := gg.NewContext(int(width), int(height))

	// Get colors from theme
	bgColor, textColor := r.getModeIndicatorColors()

	// Draw background
	dc.SetColor(bgColor)
	r.drawRoundedRect(dc, 2*scale, 2*scale, width-4*scale, height-4*scale, 6*scale)
	dc.Fill()

	// Use cached font face
	face := r.fontCache.getFace(fontSize)
	if face != nil {
		dc.SetFontFace(face)
	}

	// Draw mode text
	dc.SetColor(textColor)
	tw, _ := dc.MeasureString(mode)
	dc.DrawString(mode, width/2-tw/2, height/2+7*scale)

	return dc.Image().(*image.RGBA)
}
