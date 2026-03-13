package ui

import (
	"fmt"
	"image"

	"github.com/gogpu/gg"
)

// RenderCandidates renders candidates to an image
// hoverIndex: index of the hovered candidate (-1 for none)
// Returns the rendered image and candidate bounding rectangles for hit testing
func (r *Renderer) RenderCandidates(candidates []Candidate, input string, cursorPos int, page, totalPages int, hoverIndex int, hoverPageBtn string) (*image.RGBA, *RenderResult) {
	// Auto-refresh DPI-dependent config if DPI changed since last render
	r.refreshDPIIfNeeded()
	cfg := r.config

	if cfg.Layout == "horizontal" {
		return r.renderHorizontalCandidates(candidates, input, cursorPos, page, totalPages, hoverIndex, hoverPageBtn)
	}
	return r.renderVerticalCandidates(candidates, input, cursorPos, page, totalPages, hoverIndex, hoverPageBtn)
}

// renderVerticalCandidates renders candidates in vertical layout (traditional style)
func (r *Renderer) renderVerticalCandidates(candidates []Candidate, input string, cursorPos int, page, totalPages int, hoverIndex int, hoverPageBtn string) (*image.RGBA, *RenderResult) {
	cfg := r.config
	scale := GetDPIScale()
	td := r.textDrawer

	candidateCount := len(candidates)
	if candidateCount == 0 {
		candidateCount = 1
	}

	width := 280.0 * scale

	// Measure input text width for dynamic width adjustment
	if input != "" {
		inputTextWidth := td.MeasureString(input, cfg.FontSize)
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

	// Font size variants
	isTextIndex := cfg.IndexStyle == "text"
	indexTextSize := cfg.FontSize
	commentSize := cfg.IndexFontSize
	if isTextIndex {
		indexTextSize = cfg.FontSize + 2*scale
		commentSize = cfg.IndexFontSize + 2*scale
	}
	pageFontSize := 12.0 * scale
	if isTextIndex {
		pageFontSize = 14 * scale
	}

	// Text layout constants
	textStartX := cfg.Padding + 32*scale
	if cfg.IndexStyle == "text" {
		textStartX = cfg.Padding + 24*scale
	}

	// Candidate start Y (after input area)
	candStartY := cfg.Padding
	if !cfg.HidePreedit {
		candStartY += inputHeight + 4*scale
	}

	// Pre-compute cursor X position
	var cursorDrawX float64
	hasCursor := false
	if !cfg.HidePreedit && cursorPos >= 0 && cursorPos <= len(input) {
		cursorText := input[:cursorPos]
		textX := cfg.Padding + 8*scale
		cursorDrawX = textX + td.MeasureString(cursorText, cfg.FontSize)
		hasCursor = true
	}

	// Pre-compute comment positions (need candidate text widths)
	type commentInfo struct {
		text string
		x    float64
		y    float64
	}
	var comments []commentInfo
	for i, cand := range candidates {
		if cand.Comment != "" {
			itemY := candStartY + float64(i)*cfg.ItemHeight
			candWidth := td.MeasureString(cand.Text, cfg.FontSize)
			comments = append(comments, commentInfo{
				text: cand.Comment,
				x:    textStartX + candWidth + 8*scale,
				y:    itemY + cfg.ItemHeight/2 + commentSize/3,
			})
		}
	}

	// Pre-compute page text measurement
	var pageText string
	var pageW float64
	if totalPages > 1 {
		pageText = fmt.Sprintf(" %d / %d ", page, totalPages)
		pageW = td.MeasureString(pageText, pageFontSize)
	}

	// ===== PHASE 1: Draw all shapes with gg =====
	dc := gg.NewContext(int(width), int(height))

	// Shadow
	dc.SetColor(r.getShadowColor())
	r.drawRoundedRect(dc, 2, 2, width, height, cfg.CornerRadius)
	dc.Fill()

	// Background
	dc.SetColor(cfg.BackgroundColor)
	r.drawRoundedRect(dc, 0, 0, width-2, height-2, cfg.CornerRadius)
	dc.Fill()

	// Border
	dc.SetColor(cfg.BorderColor)
	dc.SetLineWidth(1)
	r.drawRoundedRect(dc, 0.5, 0.5, width-3, height-3, cfg.CornerRadius)
	dc.Stroke()

	// Input area background and cursor line
	if !cfg.HidePreedit {
		dc.SetColor(cfg.InputBgColor)
		r.drawRoundedRect(dc, cfg.Padding, cfg.Padding, width-cfg.Padding*2-2, inputHeight, 4*scale)
		dc.Fill()

		if hasCursor {
			cursorTopY := cfg.Padding + 4*scale
			cursorBottomY := cfg.Padding + inputHeight - 4*scale
			dc.SetColor(cfg.InputTextColor)
			dc.SetLineWidth(1.5 * scale)
			dc.DrawLine(cursorDrawX, cursorTopY, cursorDrawX, cursorBottomY)
			dc.Stroke()
		}
	}

	// Build candidate rectangles for hit testing
	result := &RenderResult{
		Rects: make([]CandidateRect, len(candidates)),
	}

	// Hover background
	if hoverIndex >= 0 && hoverIndex < len(candidates) {
		itemY := candStartY + float64(hoverIndex)*cfg.ItemHeight
		dc.SetColor(cfg.HoverBgColor)
		r.drawRoundedRect(dc, cfg.Padding-2, itemY, width-cfg.Padding*2+2, cfg.ItemHeight, 4*scale)
		dc.Fill()
	}

	// Accent bar
	if cfg.HasAccentBar && cfg.AccentBarColor != nil {
		barWidth := 3.0 * scale
		barMargin := height * 0.3
		if barMargin < cfg.CornerRadius+1 {
			barMargin = cfg.CornerRadius + 1
		}
		dc.SetColor(cfg.AccentBarColor)
		r.drawRoundedRect(dc, 1, barMargin, barWidth, height-barMargin*2, barWidth/2)
		dc.Fill()
	}

	// Index circles and bounding boxes
	for i := range candidates {
		itemY := candStartY + float64(i)*cfg.ItemHeight

		if cfg.IndexStyle != "text" {
			indexX := cfg.Padding + 14*scale
			indexY := itemY + cfg.ItemHeight/2
			dc.SetColor(cfg.IndexBgColor)
			dc.DrawCircle(indexX, indexY, 11*scale)
			dc.Fill()
		}

		result.Rects[i] = CandidateRect{
			Index: i,
			X:     cfg.Padding - 2,
			Y:     itemY,
			W:     width - cfg.Padding*2 + 2,
			H:     cfg.ItemHeight,
		}
	}

	// Page info chevrons (shapes only)
	if totalPages > 1 {
		pageY := candStartY + float64(len(candidates))*cfg.ItemHeight + 4*scale
		arrowSize := 8.0 * scale
		arrowPad := 8.0 * scale
		arrowW := arrowSize + arrowPad*2
		totalW := arrowW + pageW + arrowW
		startX := width/2 - totalW/2
		centerY := pageY + 10*scale

		// Page up button
		pageUpBtnRect := CandidateRect{X: startX, Y: pageY, W: arrowW, H: 20 * scale}
		if hoverPageBtn == "up" {
			dc.SetColor(cfg.HoverBgColor)
			r.drawRoundedRect(dc, pageUpBtnRect.X, pageUpBtnRect.Y, pageUpBtnRect.W, pageUpBtnRect.H, 4*scale)
			dc.Fill()
		}
		leftArrowColor := cfg.IndexBgColor
		if page <= 1 {
			leftArrowColor = cfg.InputTextColor
		}
		dc.SetColor(leftArrowColor)
		r.drawChevronLeft(dc, startX+arrowW/2, centerY, arrowSize, 1.5*scale)
		result.PageUpRect = &pageUpBtnRect

		// Page down button
		pageDownBtnRect := CandidateRect{X: startX + arrowW + pageW, Y: pageY, W: arrowW, H: 20 * scale}
		if hoverPageBtn == "down" {
			dc.SetColor(cfg.HoverBgColor)
			r.drawRoundedRect(dc, pageDownBtnRect.X, pageDownBtnRect.Y, pageDownBtnRect.W, pageDownBtnRect.H, 4*scale)
			dc.Fill()
		}
		rightArrowColor := cfg.IndexBgColor
		if page >= totalPages {
			rightArrowColor = cfg.InputTextColor
		}
		rightCenterX := startX + arrowW + pageW + arrowW/2
		dc.SetColor(rightArrowColor)
		r.drawChevronRight(dc, rightCenterX, centerY, arrowSize, 1.5*scale)
		result.PageDownRect = &pageDownBtnRect
	}

	// ===== PHASE 2: Draw all text =====
	img := dc.Image().(*image.RGBA)
	td.BeginDraw(img)

	// Input text
	if !cfg.HidePreedit && input != "" {
		textX := cfg.Padding + 8*scale
		textY := cfg.Padding + inputHeight/2 + cfg.FontSize/3
		td.DrawString(input, textX, textY, cfg.FontSize, cfg.InputTextColor)
	}

	// Index numbers
	for i, cand := range candidates {
		itemY := candStartY + float64(i)*cfg.ItemHeight
		indexStr := string(rune('0' + cand.Index))

		if isTextIndex {
			td.DrawString(indexStr, cfg.Padding+4*scale, itemY+cfg.ItemHeight/2+indexTextSize/3, indexTextSize, cfg.IndexColor)
		} else {
			indexX := cfg.Padding + 14*scale
			indexY := itemY + cfg.ItemHeight/2
			tw := td.MeasureString(indexStr, cfg.IndexFontSize)
			td.DrawString(indexStr, indexX-tw/2, indexY+cfg.IndexFontSize/3, cfg.IndexFontSize, cfg.IndexColor)
		}
	}

	// Candidate texts
	for i, cand := range candidates {
		itemY := candStartY + float64(i)*cfg.ItemHeight
		td.DrawString(cand.Text, textStartX, itemY+cfg.ItemHeight/2+cfg.FontSize/3, cfg.FontSize, cfg.TextColor)
	}

	// Comments
	commentColor := r.getCommentColor()
	for _, c := range comments {
		td.DrawString(c.text, c.x, c.y, commentSize, commentColor)
	}

	// Page text
	if totalPages > 1 {
		pageY := candStartY + float64(len(candidates))*cfg.ItemHeight + 4*scale
		arrowSize := 8.0 * scale
		arrowPad := 8.0 * scale
		arrowW := arrowSize + arrowPad*2
		totalW := arrowW + pageW + arrowW
		startX := width/2 - totalW/2
		centerY := pageY + 10*scale

		td.DrawString(pageText, startX+arrowW, centerY+4*scale, pageFontSize, cfg.InputTextColor)
	}

	td.EndDraw()

	return img, result
}

// renderHorizontalCandidates renders candidates in horizontal layout (modern style)
func (r *Renderer) renderHorizontalCandidates(candidates []Candidate, input string, cursorPos int, page, totalPages int, hoverIndex int, hoverPageBtn string) (*image.RGBA, *RenderResult) {
	cfg := r.config
	scale := GetDPIScale()
	td := r.textDrawer

	// Font size variants
	isTextIndex := cfg.IndexStyle == "text"
	indexTextSize := cfg.FontSize
	commentSize := cfg.IndexFontSize
	if isTextIndex {
		indexTextSize = cfg.FontSize + 2*scale
		commentSize = cfg.IndexFontSize + 2*scale
	}
	pageFontSize := 12.0 * scale
	if isTextIndex {
		pageFontSize = 14 * scale
	}

	// Measure all candidates to calculate total width
	type candMeasure struct {
		textWidth    float64
		commentWidth float64
		totalWidth   float64
	}
	measures := make([]candMeasure, len(candidates))

	indexSize := 18.0 * scale
	indexMargin := 4.0 * scale
	itemSpacing := 12.0 * scale

	if isTextIndex {
		indexMargin = 2.0 * scale
		itemSpacing = 16.0 * scale
	}

	indexTextWidths := make([]float64, len(candidates))

	// Measure index text widths for text style
	if isTextIndex {
		for i, cand := range candidates {
			indexStr := string(rune('0' + cand.Index))
			indexTextWidths[i] = td.MeasureString(indexStr, indexTextSize)
		}
	}

	// Measure candidate text widths
	for i, cand := range candidates {
		measures[i].textWidth = td.MeasureString(cand.Text, cfg.FontSize)
	}

	// Measure comment widths
	for i, cand := range candidates {
		if cand.Comment != "" {
			measures[i].commentWidth = td.MeasureString(cand.Comment, commentSize)
		}
	}

	// Calculate total width for each candidate
	for i, cand := range candidates {
		if isTextIndex {
			measures[i].totalWidth = indexTextWidths[i] + indexMargin + measures[i].textWidth
		} else {
			measures[i].totalWidth = indexSize + indexMargin + measures[i].textWidth
		}
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
	arrowSize := 8.0 * scale
	arrowPad := 6.0 * scale
	arrowW := arrowSize + arrowPad*2
	pageInfoWidth := 0.0
	var pageText string
	var pageW float64
	if totalPages > 1 {
		pageText = fmt.Sprintf(" %d/%d ", page, totalPages)
		pageW = td.MeasureString(pageText, pageFontSize)
		pageInfoWidth = arrowW + pageW + arrowW + 8*scale
	}

	// Input area (preedit)
	inputWidth := 0.0
	inputHeight := 0.0
	if !cfg.HidePreedit && input != "" {
		inputWidth = td.MeasureString(input, cfg.FontSize)
		inputWidth += 16 * scale
		inputHeight = 24 * scale
	}

	// Extra padding for accent bar
	accentBarExtra := 0.0
	if cfg.HasAccentBar && cfg.AccentBarColor != nil {
		accentBarExtra = 3.0*scale + 2*scale
	}

	// Total width
	minWidth := 200.0 * scale
	contentWidth := cfg.Padding*2 + accentBarExtra + candidatesWidth + pageInfoWidth
	if inputWidth > 0 {
		contentWidth = cfg.Padding*2 + accentBarExtra + inputWidth
		if accentBarExtra+candidatesWidth+pageInfoWidth > accentBarExtra+inputWidth {
			contentWidth = cfg.Padding*2 + accentBarExtra + candidatesWidth + pageInfoWidth
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

	// Pre-compute cursor X position
	var cursorDrawX float64
	hasCursor := false
	if !cfg.HidePreedit && input != "" && cursorPos >= 0 && cursorPos <= len(input) {
		cursorText := input[:cursorPos]
		preeditX := cfg.Padding + accentBarExtra
		textX := preeditX + 8*scale
		cursorDrawX = textX + td.MeasureString(cursorText, cfg.FontSize)
		hasCursor = true
	}

	// Pre-compute candidate X positions
	type candPosition struct {
		x     float64 // start X of this candidate
		textX float64 // X position for candidate text
	}
	positions := make([]candPosition, len(candidates))

	accentBarOffset := 0.0
	if cfg.HasAccentBar && cfg.AccentBarColor != nil {
		accentBarOffset = 3.0*scale + 2*scale
	}

	candStartY := cfg.Padding
	if !cfg.HidePreedit && input != "" {
		candStartY += inputHeight + 4*scale
	}

	xPos := cfg.Padding + accentBarOffset
	for i := range candidates {
		positions[i].x = xPos
		if isTextIndex {
			positions[i].textX = xPos + indexTextWidths[i] + indexMargin
		} else {
			positions[i].textX = xPos + indexSize + indexMargin
		}
		xPos += measures[i].totalWidth + itemSpacing
	}

	// ===== PHASE 1: Draw all shapes with gg =====
	dc := gg.NewContext(int(width), int(height))

	// Shadow
	dc.SetColor(r.getShadowColor())
	r.drawRoundedRect(dc, 2, 2, width, height, cfg.CornerRadius)
	dc.Fill()

	// Background
	dc.SetColor(cfg.BackgroundColor)
	r.drawRoundedRect(dc, 0, 0, width-2, height-2, cfg.CornerRadius)
	dc.Fill()

	// Border
	dc.SetColor(cfg.BorderColor)
	dc.SetLineWidth(1)
	r.drawRoundedRect(dc, 0.5, 0.5, width-3, height-3, cfg.CornerRadius)
	dc.Stroke()

	y := cfg.Padding

	// Accent bar
	if cfg.HasAccentBar && cfg.AccentBarColor != nil {
		barWidth := 3.0 * scale
		barMargin := height * 0.3
		if barMargin < cfg.CornerRadius+1 {
			barMargin = cfg.CornerRadius + 1
		}
		dc.SetColor(cfg.AccentBarColor)
		r.drawRoundedRect(dc, 1, barMargin, barWidth, height-barMargin*2, barWidth/2)
		dc.Fill()
	}

	// Input area shapes
	if !cfg.HidePreedit && input != "" {
		preeditX := cfg.Padding + accentBarOffset
		dc.SetColor(cfg.InputBgColor)
		r.drawRoundedRect(dc, preeditX, y, width-preeditX-cfg.Padding-2, inputHeight, 4*scale)
		dc.Fill()

		if hasCursor {
			cursorTopY := y + 3*scale
			cursorBottomY := y + inputHeight - 3*scale
			dc.SetColor(cfg.InputTextColor)
			dc.SetLineWidth(1.5 * scale)
			dc.DrawLine(cursorDrawX, cursorTopY, cursorDrawX, cursorBottomY)
			dc.Stroke()
		}
	}

	// Build candidate rectangles for hit testing
	result := &RenderResult{
		Rects: make([]CandidateRect, len(candidates)),
	}

	candY := candStartY + candidateRowHeight/2

	// Draw candidate shapes (hover bg, index circles)
	for i := range candidates {
		itemWidth := measures[i].totalWidth
		px := positions[i].x

		result.Rects[i] = CandidateRect{
			Index: i,
			X:     px - 4,
			Y:     candStartY,
			W:     itemWidth + 8,
			H:     candidateRowHeight,
		}

		// Hover background
		if i == hoverIndex {
			dc.SetColor(cfg.HoverBgColor)
			r.drawRoundedRect(dc, px-4, candStartY, itemWidth+8, candidateRowHeight, 4*scale)
			dc.Fill()
		}

		// Index circle (non-text style only)
		if !isTextIndex {
			indexX := px + indexSize/2
			dc.SetColor(cfg.IndexBgColor)
			dc.DrawCircle(indexX, candY, indexSize/2)
			dc.Fill()
		}
	}

	// Page info chevrons (shapes only)
	if totalPages > 1 {
		totalW := arrowW + pageW + arrowW
		startX := width - cfg.Padding - totalW

		// Page up button
		pageUpBtnRect := CandidateRect{X: startX, Y: candStartY, W: arrowW, H: candidateRowHeight}
		if hoverPageBtn == "up" {
			dc.SetColor(cfg.HoverBgColor)
			r.drawRoundedRect(dc, pageUpBtnRect.X, pageUpBtnRect.Y, pageUpBtnRect.W, pageUpBtnRect.H, 4*scale)
			dc.Fill()
		}
		leftArrowColor := cfg.IndexBgColor
		if page <= 1 {
			leftArrowColor = cfg.InputTextColor
		}
		dc.SetColor(leftArrowColor)
		r.drawChevronLeft(dc, startX+arrowW/2, candY, arrowSize, 1.5*scale)
		result.PageUpRect = &pageUpBtnRect

		// Page down button
		pageDownBtnRect := CandidateRect{X: startX + arrowW + pageW, Y: candStartY, W: arrowW, H: candidateRowHeight}
		if hoverPageBtn == "down" {
			dc.SetColor(cfg.HoverBgColor)
			r.drawRoundedRect(dc, pageDownBtnRect.X, pageDownBtnRect.Y, pageDownBtnRect.W, pageDownBtnRect.H, 4*scale)
			dc.Fill()
		}
		rightArrowColor := cfg.IndexBgColor
		if page >= totalPages {
			rightArrowColor = cfg.InputTextColor
		}
		rightCenterX := startX + arrowW + pageW + arrowW/2
		dc.SetColor(rightArrowColor)
		r.drawChevronRight(dc, rightCenterX, candY, arrowSize, 1.5*scale)
		result.PageDownRect = &pageDownBtnRect
	}

	// ===== PHASE 2: Draw all text =====
	img := dc.Image().(*image.RGBA)
	td.BeginDraw(img)

	// Input text
	if !cfg.HidePreedit && input != "" {
		preeditX := cfg.Padding + accentBarOffset
		textX := preeditX + 8*scale
		textY := cfg.Padding + inputHeight/2 + cfg.FontSize/3
		td.DrawString(input, textX, textY, cfg.FontSize, cfg.InputTextColor)
	}

	// Candidate text (index, text, comment)
	for i, cand := range candidates {
		px := positions[i].x

		// Index
		if isTextIndex {
			indexStr := string(rune('0' + cand.Index))
			td.DrawString(indexStr, px, candY+indexTextSize/3, indexTextSize, cfg.IndexColor)
		} else {
			indexX := px + indexSize/2
			indexStr := string(rune('0' + cand.Index))
			tw := td.MeasureString(indexStr, cfg.IndexFontSize)
			td.DrawString(indexStr, indexX-tw/2, candY+cfg.IndexFontSize/3, cfg.IndexFontSize, cfg.IndexColor)
		}

		// Candidate text
		td.DrawString(cand.Text, positions[i].textX, candY+cfg.FontSize/3, cfg.FontSize, cfg.TextColor)

		// Comment
		if cand.Comment != "" {
			commentX := positions[i].textX + measures[i].textWidth + 6*scale
			td.DrawString(cand.Comment, commentX, candY+commentSize/3, commentSize, r.getCommentColor())
		}
	}

	// Page text
	if totalPages > 1 {
		totalW := arrowW + pageW + arrowW
		startX := width - cfg.Padding - totalW
		td.DrawString(pageText, startX+arrowW, candY+6*scale, pageFontSize, cfg.InputTextColor)
	}

	td.EndDraw()

	return img, result
}
