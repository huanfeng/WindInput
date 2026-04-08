package ui

import (
	"fmt"
	"image"

	"github.com/gogpu/gg"
)

// RenderCandidates renders candidates to an image
// hoverIndex: index of the hovered candidate (-1 for none)
// Returns the rendered image and candidate bounding rectangles for hit testing
func (r *Renderer) RenderCandidates(candidates []Candidate, input string, cursorPos int, page, totalPages int, hoverIndex int, hoverPageBtn string, selectedIndex int) (*image.RGBA, *RenderResult) {
	// Auto-refresh DPI-dependent config if DPI changed since last render
	r.refreshDPIIfNeeded()
	cfg := r.config

	if cfg.Layout == "horizontal" {
		return r.renderHorizontalCandidates(candidates, input, cursorPos, page, totalPages, hoverIndex, hoverPageBtn, selectedIndex)
	}
	return r.renderVerticalCandidates(candidates, input, cursorPos, page, totalPages, hoverIndex, hoverPageBtn, selectedIndex)
}

// renderVerticalCandidates renders candidates in vertical layout (traditional style)
func (r *Renderer) renderVerticalCandidates(candidates []Candidate, input string, cursorPos int, page, totalPages int, hoverIndex int, hoverPageBtn string, selectedIndex int) (*image.RGBA, *RenderResult) {
	cfg := r.config
	scale := GetDPIScale()
	td := r.textDrawer

	// Effective window padding (theme-configurable)
	padX := cfg.Padding
	padY := cfg.Padding
	if cfg.WindowPaddingX > 0 {
		padX = cfg.WindowPaddingX
	}
	if cfg.WindowPaddingY > 0 {
		padY = cfg.WindowPaddingY
	}

	candidateCount := len(candidates)
	if candidateCount == 0 {
		candidateCount = 1
	}

	width := 280.0 * scale

	// Measure input text width for dynamic width adjustment
	if input != "" {
		inputTextWidth := td.MeasureString(input, cfg.FontSize)
		minInputWidth := inputTextWidth + padX*2 + 16*scale
		if minInputWidth > width {
			width = minInputWidth
		}
	}

	inputHeight := 30.0 * scale
	if cfg.HidePreedit {
		inputHeight = 0
	}
	contentHeight := float64(candidateCount) * cfg.ItemHeight
	showVerticalPager := totalPages > 1 || cfg.AlwaysShowPager
	pageInfoHeight := 0.0
	if showVerticalPager {
		if totalPages < 1 {
			totalPages = 1
		}
		if page < 1 {
			page = 1
		}
		pageInfoHeight = 24.0 * scale
	}
	height := padY*2 + inputHeight + contentHeight + pageInfoHeight + 4*scale
	if cfg.HidePreedit {
		height = padY*2 + contentHeight + pageInfoHeight
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
	textStartX := padX + 32*scale
	if cfg.IndexStyle == "text" {
		textStartX = padX + 24*scale
	}

	// Candidate start Y (after input area)
	candStartY := padY
	if !cfg.HidePreedit {
		candStartY += inputHeight + 4*scale
	}

	// Pre-compute cursor X position
	var cursorDrawX float64
	hasCursor := false
	if !cfg.HidePreedit && cursorPos >= 0 && cursorPos <= len(input) {
		cursorText := input[:cursorPos]
		textX := padX + 8*scale
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
			tx := textStartX
			if cand.Index < 0 {
				tx = padX + 8*scale
			}
			comments = append(comments, commentInfo{
				text: cand.Comment,
				x:    tx + candWidth + 8*scale,
				y:    itemY + cfg.ItemHeight/2 + commentSize/3,
			})
		}
	}

	// Pre-compute page text measurement
	var pageText string
	var pageW float64
	showVerticalPageNumber := cfg.ShowPageNumber
	if showVerticalPager {
		if showVerticalPageNumber {
			pageText = fmt.Sprintf(" %d / %d ", page, totalPages)
			pageW = td.MeasureString(pageText, pageFontSize)
		}
	}

	// ===== PHASE 1: Draw all shapes with gg =====
	dc := gg.NewContext(int(width), int(height))

	// Shadow (same size as background, offset 2px to bottom-right)
	dc.SetColor(r.getShadowColor())
	r.drawRoundedRect(dc, 2, 2, width-2, height-2, cfg.CornerRadius)
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
		r.drawRoundedRect(dc, padX, padY, width-padX*2-2, inputHeight, 4*scale)
		dc.Fill()

		if hasCursor {
			cursorTopY := padY + 4*scale
			cursorBottomY := padY + inputHeight - 4*scale
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

	// Selected background (keyboard selection via up/down arrows)
	if selectedIndex >= 0 && selectedIndex < len(candidates) {
		itemY := candStartY + float64(selectedIndex)*cfg.ItemHeight
		dc.SetColor(cfg.SelectedBgColor)
		r.drawRoundedRect(dc, padX-2, itemY, width-padX*2+2, cfg.ItemHeight, 4*scale)
		dc.Fill()

		// Accent bar — drawn inside the selected highlight box
		if cfg.HasAccentBar && cfg.AccentBarColor != nil {
			barWidth := 3.0 * scale
			barMarginY := cfg.ItemHeight * 0.2 // 竖条上下各留 20%，条高约 60%
			dc.SetColor(cfg.AccentBarColor)
			r.drawRoundedRect(dc, padX-1, itemY+barMarginY, barWidth, cfg.ItemHeight-barMarginY*2, barWidth/2)
			dc.Fill()
		}
	}

	// Hover background (mouse hover, takes visual precedence over selected)
	if hoverIndex >= 0 && hoverIndex < len(candidates) && hoverIndex != selectedIndex {
		itemY := candStartY + float64(hoverIndex)*cfg.ItemHeight
		dc.SetColor(cfg.HoverBgColor)
		r.drawRoundedRect(dc, padX-2, itemY, width-padX*2+2, cfg.ItemHeight, 4*scale)
		dc.Fill()
	}

	// Index circles and bounding boxes
	for i := range candidates {
		itemY := candStartY + float64(i)*cfg.ItemHeight

		if cfg.IndexStyle != "text" && candidates[i].Index >= 0 {
			indexX := padX + 14*scale
			indexY := itemY + cfg.ItemHeight/2
			dc.SetColor(cfg.IndexBgColor)
			dc.DrawCircle(indexX, indexY, 11*scale)
			dc.Fill()
		}

		result.Rects[i] = CandidateRect{
			Index: i,
			X:     padX - 2,
			Y:     itemY,
			W:     width - padX*2 + 2,
			H:     cfg.ItemHeight,
		}
	}

	// Page info chevrons (shapes only)
	if showVerticalPager {
		pageY := candStartY + float64(len(candidates))*cfg.ItemHeight + 4*scale
		arrowSize := 8.0 * scale
		arrowPad := 8.0 * scale
		arrowW := arrowSize + arrowPad*2
		totalW := arrowW + pageW + arrowW
		startX := width/2 - totalW/2
		centerY := pageY + 10*scale

		// Page up button
		canPageUp := page > 1
		pageUpBtnRect := CandidateRect{X: startX, Y: pageY, W: arrowW, H: 20 * scale}
		if canPageUp && hoverPageBtn == "up" {
			dc.SetColor(cfg.HoverBgColor)
			r.drawRoundedRect(dc, pageUpBtnRect.X, pageUpBtnRect.Y, pageUpBtnRect.W, pageUpBtnRect.H, 4*scale)
			dc.Fill()
		}
		leftArrowColor := cfg.IndexBgColor
		if !canPageUp {
			leftArrowColor = cfg.InputTextColor
		}
		dc.SetColor(leftArrowColor)
		r.drawChevronLeft(dc, startX+arrowW/2, centerY, arrowSize, 1.5*scale)
		if canPageUp {
			result.PageUpRect = &pageUpBtnRect
		}

		// Page down button
		canPageDown := page < totalPages
		pageDownBtnRect := CandidateRect{X: startX + arrowW + pageW, Y: pageY, W: arrowW, H: 20 * scale}
		if canPageDown && hoverPageBtn == "down" {
			dc.SetColor(cfg.HoverBgColor)
			r.drawRoundedRect(dc, pageDownBtnRect.X, pageDownBtnRect.Y, pageDownBtnRect.W, pageDownBtnRect.H, 4*scale)
			dc.Fill()
		}
		rightArrowColor := cfg.IndexBgColor
		if !canPageDown {
			rightArrowColor = cfg.InputTextColor
		}
		rightCenterX := startX + arrowW + pageW + arrowW/2
		dc.SetColor(rightArrowColor)
		r.drawChevronRight(dc, rightCenterX, centerY, arrowSize, 1.5*scale)
		if canPageDown {
			result.PageDownRect = &pageDownBtnRect
		}
	}

	// ===== PHASE 2: Draw all text =====
	img := dc.Image().(*image.RGBA)
	td.BeginDraw(img)

	// Input text
	if !cfg.HidePreedit && input != "" {
		textX := padX + 8*scale
		textY := padY + inputHeight/2 + cfg.FontSize/3
		td.DrawString(input, textX, textY, cfg.FontSize, cfg.InputTextColor)
	}

	// Index numbers
	vertIndexWeight := cfg.IndexFontWeight
	for i, cand := range candidates {
		itemY := candStartY + float64(i)*cfg.ItemHeight
		if cand.Index < 0 {
			continue // 负索引跳过绘制（如加词模式）
		}
		indexStr := string(rune('0' + cand.Index))

		if isTextIndex {
			if vertIndexWeight > 0 {
				td.DrawStringWithWeight(indexStr, padX+4*scale, itemY+cfg.ItemHeight/2+indexTextSize/3, indexTextSize, cfg.IndexColor, vertIndexWeight)
			} else {
				td.DrawString(indexStr, padX+4*scale, itemY+cfg.ItemHeight/2+indexTextSize/3, indexTextSize, cfg.IndexColor)
			}
		} else {
			indexX := padX + 14*scale
			indexY := itemY + cfg.ItemHeight/2
			tw := td.MeasureString(indexStr, cfg.IndexFontSize)
			if vertIndexWeight > 0 {
				td.DrawStringWithWeight(indexStr, indexX-tw/2, indexY+cfg.IndexFontSize/3, cfg.IndexFontSize, cfg.IndexColor, vertIndexWeight)
			} else {
				td.DrawString(indexStr, indexX-tw/2, indexY+cfg.IndexFontSize/3, cfg.IndexFontSize, cfg.IndexColor)
			}
		}
	}

	// Candidate texts
	for i, cand := range candidates {
		itemY := candStartY + float64(i)*cfg.ItemHeight
		tx := textStartX
		if cand.Index < 0 {
			tx = padX + 8*scale // 无序号时文本靠左
		}
		td.DrawString(cand.Text, tx, itemY+cfg.ItemHeight/2+cfg.FontSize/3, cfg.FontSize, cfg.TextColor)
	}

	// Comments
	commentColor := r.getCommentColor()
	for _, c := range comments {
		td.DrawString(c.text, c.x, c.y, commentSize, commentColor)
	}

	// Page text
	if showVerticalPager && showVerticalPageNumber && pageText != "" {
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

	DrawDebugBanner(img)
	return img, result
}

// renderHorizontalCandidates renders candidates in horizontal layout (modern style)
func (r *Renderer) renderHorizontalCandidates(candidates []Candidate, input string, cursorPos int, page, totalPages int, hoverIndex int, hoverPageBtn string, selectedIndex int) (*image.RGBA, *RenderResult) {
	cfg := r.config
	scale := GetDPIScale()
	td := r.textDrawer

	// Effective window padding (theme-configurable)
	padX := cfg.Padding
	padY := cfg.Padding
	if cfg.WindowPaddingX > 0 {
		padX = cfg.WindowPaddingX
	}
	if cfg.WindowPaddingY > 0 {
		padY = cfg.WindowPaddingY
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
			if cand.Index < 0 {
				indexTextWidths[i] = 0
				continue
			}
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
		if cand.Index < 0 {
			// 无序号候选：不包含 index 宽度
			measures[i].totalWidth = measures[i].textWidth
		} else if isTextIndex {
			measures[i].totalWidth = indexTextWidths[i] + indexMargin + measures[i].textWidth
		} else {
			measures[i].totalWidth = indexSize + indexMargin + measures[i].textWidth
		}
		if cand.Comment != "" {
			measures[i].totalWidth += 6*scale + measures[i].commentWidth
		}
	}

	// Item padding (left/right can be set separately)
	bgPadL := 8.0 * scale // default left padding
	bgPadR := 8.0 * scale // default right padding
	if cfg.ItemPaddingLeft > 0 {
		bgPadL = cfg.ItemPaddingLeft * scale
	}
	if cfg.ItemPaddingRight > 0 {
		bgPadR = cfg.ItemPaddingRight * scale
	}

	// Calculate total candidates width (including padding)
	// Effective spacing = right pad of prev item + left pad of next item
	effectiveSpacingForWidth := bgPadR + bgPadL
	if itemSpacing > effectiveSpacingForWidth {
		effectiveSpacingForWidth = itemSpacing
	}
	candidatesWidth := bgPadL // leading padding for first item
	for i := range candidates {
		candidatesWidth += measures[i].totalWidth
		if i < len(candidates)-1 {
			candidatesWidth += effectiveSpacingForWidth
		}
	}
	candidatesWidth += bgPadR // trailing padding for last item

	// Page info width
	arrowSize := 8.0 * scale
	arrowPad := 6.0 * scale
	arrowW := arrowSize + arrowPad*2
	pageInfoWidth := 0.0
	var pageText string
	var pageW float64
	showPager := totalPages > 1 || cfg.AlwaysShowPager
	showPageNumber := cfg.ShowPageNumber
	if showPager {
		if totalPages < 1 {
			totalPages = 1
		}
		if page < 1 {
			page = 1
		}
		if showPageNumber {
			pageText = fmt.Sprintf(" %d/%d ", page, totalPages)
			pageW = td.MeasureString(pageText, pageFontSize)
			pageInfoWidth = arrowW + pageW + arrowW + 8*scale
		} else {
			pageInfoWidth = arrowW + arrowW + 8*scale
		}
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
	contentWidth := padX*2 + accentBarExtra + candidatesWidth + pageInfoWidth
	if inputWidth > 0 {
		contentWidth = padX*2 + accentBarExtra + inputWidth
		if accentBarExtra+candidatesWidth+pageInfoWidth > accentBarExtra+inputWidth {
			contentWidth = padX*2 + accentBarExtra + candidatesWidth + pageInfoWidth
		}
	}
	width := contentWidth
	if width < minWidth {
		width = minWidth
	}

	// Height calculation
	candidateRowHeight := 32.0 * scale
	height := padY*2 + candidateRowHeight
	if inputHeight > 0 {
		height += inputHeight + 4*scale
	}

	// Pre-compute cursor X position
	var cursorDrawX float64
	hasCursor := false
	if !cfg.HidePreedit && input != "" && cursorPos >= 0 && cursorPos <= len(input) {
		cursorText := input[:cursorPos]
		preeditX := padX + accentBarExtra
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

	candStartY := padY
	if !cfg.HidePreedit && input != "" {
		candStartY += inputHeight + 4*scale
	}

	xPos := padX + accentBarOffset + bgPadL
	for i := range candidates {
		positions[i].x = xPos
		if candidates[i].Index < 0 {
			positions[i].textX = xPos // 无序号：文本直接从起始位置开始
		} else if isTextIndex {
			positions[i].textX = xPos + indexTextWidths[i] + indexMargin
		} else {
			positions[i].textX = xPos + indexSize + indexMargin
		}
		xPos += measures[i].totalWidth + effectiveSpacingForWidth
	}

	// ===== PHASE 1: Draw all shapes with gg =====
	dc := gg.NewContext(int(width), int(height))

	// Shadow (same size as background, offset 2px to bottom-right)
	dc.SetColor(r.getShadowColor())
	r.drawRoundedRect(dc, 2, 2, width-2, height-2, cfg.CornerRadius)
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

	y := padY

	// Input area shapes
	if !cfg.HidePreedit && input != "" {
		preeditX := padX + accentBarOffset
		dc.SetColor(cfg.InputBgColor)
		r.drawRoundedRect(dc, preeditX, y, width-preeditX-padX-2, inputHeight, 4*scale)
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

	// Draw candidate shapes (selected bg, hover bg, accent bar, index circles)
	for i := range candidates {
		itemWidth := measures[i].totalWidth
		px := positions[i].x

		bgX := px - bgPadL
		bgW := bgPadL + itemWidth + bgPadR

		result.Rects[i] = CandidateRect{
			Index: i,
			X:     bgX,
			Y:     candStartY,
			W:     bgW,
			H:     candidateRowHeight,
		}

		// Selected background (keyboard selection via up/down arrows)
		if i == selectedIndex {
			dc.SetColor(cfg.SelectedBgColor)
			r.drawRoundedRect(dc, bgX, candStartY, bgW, candidateRowHeight, 4*scale)
			dc.Fill()

			// Accent bar — drawn inside the selected highlight box
			if cfg.HasAccentBar && cfg.AccentBarColor != nil {
				barWidth := 3.0 * scale
				barMarginY := candidateRowHeight * 0.2 // 竖条上下各留 20%，条高约 60%
				dc.SetColor(cfg.AccentBarColor)
				r.drawRoundedRect(dc, bgX+1, candStartY+barMarginY, barWidth, candidateRowHeight-barMarginY*2, barWidth/2)
				dc.Fill()
			}
		}

		// Hover background (mouse hover, takes visual precedence)
		if i == hoverIndex && i != selectedIndex {
			dc.SetColor(cfg.HoverBgColor)
			r.drawRoundedRect(dc, bgX, candStartY, bgW, candidateRowHeight, 4*scale)
			dc.Fill()
		}

		// Index circle (non-text style only, skip for negative index)
		if !isTextIndex && candidates[i].Index >= 0 {
			indexX := px + indexSize/2
			dc.SetColor(cfg.IndexBgColor)
			dc.DrawCircle(indexX, candY, indexSize/2)
			dc.Fill()
		}
	}

	// Page info chevrons (shapes only)
	if showPager {
		totalW := arrowW + pageW + arrowW
		startX := width - padX - totalW

		// Page up button
		canPageUp := page > 1
		pageUpBtnRect := CandidateRect{X: startX, Y: candStartY, W: arrowW, H: candidateRowHeight}
		if canPageUp && hoverPageBtn == "up" {
			dc.SetColor(cfg.HoverBgColor)
			r.drawRoundedRect(dc, pageUpBtnRect.X, pageUpBtnRect.Y, pageUpBtnRect.W, pageUpBtnRect.H, 4*scale)
			dc.Fill()
		}
		leftArrowColor := cfg.IndexBgColor
		if !canPageUp {
			leftArrowColor = cfg.InputTextColor
		}
		dc.SetColor(leftArrowColor)
		r.drawChevronLeft(dc, startX+arrowW/2, candY, arrowSize, 1.5*scale)
		if canPageUp {
			result.PageUpRect = &pageUpBtnRect
		}

		// Page down button
		canPageDown := page < totalPages
		pageDownBtnRect := CandidateRect{X: startX + arrowW + pageW, Y: candStartY, W: arrowW, H: candidateRowHeight}
		if canPageDown && hoverPageBtn == "down" {
			dc.SetColor(cfg.HoverBgColor)
			r.drawRoundedRect(dc, pageDownBtnRect.X, pageDownBtnRect.Y, pageDownBtnRect.W, pageDownBtnRect.H, 4*scale)
			dc.Fill()
		}
		rightArrowColor := cfg.IndexBgColor
		if !canPageDown {
			rightArrowColor = cfg.InputTextColor
		}
		rightCenterX := startX + arrowW + pageW + arrowW/2
		dc.SetColor(rightArrowColor)
		r.drawChevronRight(dc, rightCenterX, candY, arrowSize, 1.5*scale)
		if canPageDown {
			result.PageDownRect = &pageDownBtnRect
		}
	}

	// ===== PHASE 2: Draw all text =====
	img := dc.Image().(*image.RGBA)
	td.BeginDraw(img)

	// Input text
	if !cfg.HidePreedit && input != "" {
		preeditX := padX + accentBarOffset
		textX := preeditX + 8*scale
		textY := padY + inputHeight/2 + cfg.FontSize/3
		td.DrawString(input, textX, textY, cfg.FontSize, cfg.InputTextColor)
	}

	// Candidate text (index, text, comment)
	indexWeight := cfg.IndexFontWeight
	for i, cand := range candidates {
		px := positions[i].x

		// Index（负索引跳过绘制，如加词模式）
		if cand.Index >= 0 {
			if isTextIndex {
				indexStr := string(rune('0' + cand.Index))
				if indexWeight > 0 {
					td.DrawStringWithWeight(indexStr, px, candY+indexTextSize/3, indexTextSize, cfg.IndexColor, indexWeight)
				} else {
					td.DrawString(indexStr, px, candY+indexTextSize/3, indexTextSize, cfg.IndexColor)
				}
			} else {
				indexX := px + indexSize/2
				indexStr := string(rune('0' + cand.Index))
				tw := td.MeasureString(indexStr, cfg.IndexFontSize)
				if indexWeight > 0 {
					td.DrawStringWithWeight(indexStr, indexX-tw/2, candY+cfg.IndexFontSize/3, cfg.IndexFontSize, cfg.IndexColor, indexWeight)
				} else {
					td.DrawString(indexStr, indexX-tw/2, candY+cfg.IndexFontSize/3, cfg.IndexFontSize, cfg.IndexColor)
				}
			}
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
	if showPager && showPageNumber && pageText != "" {
		totalW := arrowW + pageW + arrowW
		startX := width - padX - totalW
		td.DrawString(pageText, startX+arrowW, candY+6*scale, pageFontSize, cfg.InputTextColor)
	}

	td.EndDraw()

	DrawDebugBanner(img)
	return img, result
}
