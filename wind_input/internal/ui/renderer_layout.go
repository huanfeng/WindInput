package ui

import (
	"fmt"
	"image"
	"image/color"
	"math"

	"github.com/gogpu/gg"
	"github.com/huanfeng/wind_input/pkg/config"
)

// pagerFontSize returns the font size for the pager indicator (e.g. "1/3").
// Scales with cfg.IndexFontSize so the pager grows together with the candidate
// font, while keeping historical 12/14 (* scale) values as a lower bound so
// small font configs do not become illegible.
func pagerFontSize(cfg RenderConfig, scale float64, isTextIndex bool) float64 {
	if isTextIndex {
		return math.Max(14*scale, cfg.IndexFontSize+2*scale)
	}
	return math.Max(12*scale, cfg.IndexFontSize)
}

// pagerArrowSize returns the chevron arrow visual size, scaled with the pager
// font so the arrows visually balance the page-number text.
func pagerArrowSize(pageFontSize, scale float64) float64 {
	return math.Max(8*scale, pageFontSize*0.65)
}

// pagerButtonHeight returns the clickable button height for pager arrows.
// Includes a small margin around the arrow / text.
func pagerButtonHeight(pageFontSize, scale float64) float64 {
	return math.Max(20*scale, pageFontSize+8*scale)
}

// indexLabel returns the display string for a candidate index.
// Priority: overrideLabel > IndexLabels config > default 1-9,0
func indexLabel(indexLabels string, index int, overrideLabel string) string {
	if overrideLabel != "" {
		return overrideLabel
	}
	if indexLabels != "" {
		labels := []rune(indexLabels)
		pos := index - 1
		if index == 0 {
			pos = 9
		}
		if pos >= 0 && pos < len(labels) {
			return string(labels[pos])
		}
	}
	return string(rune('0' + index))
}

// RenderCandidates renders candidates to an image
// hoverIndex: index of the hovered candidate (-1 for none)
// Returns the rendered image and candidate bounding rectangles for hit testing
func (r *Renderer) RenderCandidates(candidates []Candidate, input string, cursorPos int, page, totalPages int, hoverIndex int, hoverPageBtn string, selectedIndex int) (*image.RGBA, *RenderResult) {
	// Auto-refresh DPI-dependent config if DPI changed since last render
	r.refreshDPIIfNeeded()
	cfg := r.config

	if cfg.Layout == config.LayoutHorizontal {
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

	// 纵向布局宽度：基于候选内容动态计算
	width := 0.0

	// Measure input text width for dynamic width adjustment
	// ModeLabel（临时拼音、快捷输入等）显示在输入行右侧，需要一起计算宽度
	modeLabelWidth := 0.0
	if cfg.ModeLabel != "" {
		modeLabelWidth = td.MeasureString(cfg.ModeLabel, cfg.IndexFontSize) + 12*scale // label + 左右间距
	}
	if input != "" {
		inputTextWidth := td.MeasureString(input, cfg.FontSize)
		minInputWidth := inputTextWidth + padX*2 + 16*scale + modeLabelWidth
		if minInputWidth > width {
			width = minInputWidth
		}
	} else if modeLabelWidth > 0 {
		// 无输入但有 ModeLabel 时（如临时英文初始状态），确保窗口能容纳标签
		minLabelWidth := padX*2 + modeLabelWidth
		if minLabelWidth > width {
			width = minLabelWidth
		}
	}

	// Element spacing (from theme config)
	indexMarginRight := cfg.IndexMarginRight   // 序号与候选文本间距
	commentMarginLeft := cfg.CommentMarginLeft // 候选文本与编码提示间距

	// Index area width + text start position
	isTextIndex := cfg.IndexStyle == "text"
	// Circle radius scales with IndexFontSize so the number always fits inside.
	// diameter = max(22, IndexFontSize+8); layout: 3px left gap + diameter + 3px right gap
	indexRadius := math.Max(11*scale, (cfg.IndexFontSize+8*scale)/2)
	indexAreaWidth := 2*indexRadius + 6*scale // circle style
	if isTextIndex {
		indexAreaWidth = 20.0 * scale // text style: narrower
	}
	textStartX := padX + indexAreaWidth + indexMarginRight

	// 编码提示实际渲染字体大小（text 索引样式时更大）
	commentSizeForWidth := cfg.IndexFontSize
	if isTextIndex {
		commentSizeForWidth = cfg.IndexFontSize + 2*scale
	}

	// Item right padding
	itemPadR := 8.0 * scale
	if cfg.ItemPaddingRight > 0 {
		itemPadR = cfg.ItemPaddingRight * scale
	}

	// Width limits (from theme config, with defaults)
	maxCandWidth := 600.0 * scale
	if cfg.VerticalMaxWidth > 0 {
		maxCandWidth = cfg.VerticalMaxWidth
	}

	textMarginRight := cfg.TextMarginRight       // 候选文本右间距
	commentMarginRight := cfg.CommentMarginRight // 编码提示右间距

	for _, cand := range candidates {
		candTextWidth := td.MeasureString(cand.Text, cfg.FontSize) + textMarginRight
		if cand.Comment != "" {
			candTextWidth += commentMarginLeft + td.MeasureString(cand.Comment, commentSizeForWidth) + commentMarginRight
		}
		minCandWidth := textStartX + candTextWidth + itemPadR
		if minCandWidth > width && minCandWidth <= maxCandWidth {
			width = minCandWidth
		} else if minCandWidth > maxCandWidth {
			width = maxCandWidth
		}
	}

	// 处理分级加载：负值 totalPages 表示还有更多候选未加载
	absTotalPagesV := totalPages
	if absTotalPagesV < 0 {
		absTotalPagesV = -absTotalPagesV
	}

	// 确保页码指示器能完整显示
	showVerticalPager := absTotalPagesV > 1 || cfg.AlwaysShowPager
	if showVerticalPager && cfg.ShowPageNumber {
		pageFontSize := pagerFontSize(cfg, scale, isTextIndex)
		hasMoreV := totalPages < 0
		var pageText string
		if hasMoreV {
			pageText = fmt.Sprintf(" %d / %d+ ", page, absTotalPagesV)
		} else {
			pageText = fmt.Sprintf(" %d / %d ", page, absTotalPagesV)
		}
		pageW := td.MeasureString(pageText, pageFontSize)
		arrowSize := pagerArrowSize(pageFontSize, scale)
		arrowPad := 8.0 * scale
		arrowW := arrowSize + arrowPad*2
		pagerWidth := arrowW + pageW + arrowW + padX*2
		if pagerWidth > width {
			width = pagerWidth
		}
	}

	// 确保最小宽度不小于合理下限
	if cfg.VerticalMinWidth > 0 {
		// 主题配置了明确的最小宽度
		if width < cfg.VerticalMinWidth {
			width = cfg.VerticalMinWidth
		}
	} else {
		// 自动计算：索引区 + 一个汉字宽度 + 右侧边距
		autoMinWidth := textStartX + td.MeasureString("汉", cfg.FontSize) + itemPadR
		if width < autoMinWidth {
			width = autoMinWidth
		}
	}

	inputHeight := math.Max(30*scale, cfg.FontSize*1.5)
	if cfg.HidePreedit {
		inputHeight = 0
	}
	contentHeight := float64(candidateCount) * cfg.ItemHeight
	pageFontSize := pagerFontSize(cfg, scale, isTextIndex)
	pagerBtnH := pagerButtonHeight(pageFontSize, scale)
	pageInfoHeight := 0.0
	if showVerticalPager {
		if absTotalPagesV < 1 {
			absTotalPagesV = 1
			if totalPages < 0 {
				totalPages = -1
			} else {
				totalPages = 1
			}
		}
		if page < 1 {
			page = 1
		}
		pageInfoHeight = math.Max(24*scale, pagerBtnH+4*scale)
	}
	height := padY*2 + inputHeight + contentHeight + pageInfoHeight + 4*scale
	if cfg.HidePreedit {
		height = padY*2 + contentHeight + pageInfoHeight
	}

	// Font size variants (isTextIndex already computed above for dynamic width)
	indexTextSize := cfg.FontSize
	commentSize := cfg.IndexFontSize
	if isTextIndex {
		indexTextSize = cfg.FontSize + 2*scale
		commentSize = cfg.IndexFontSize + 2*scale
	}
	// Text layout constants (textStartX already computed above for dynamic width)

	// Candidate start Y (after input area)
	candStartY := padY
	if !cfg.HidePreedit {
		candStartY = padY + inputHeight + 4*scale
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
				x:    tx + candWidth + commentMarginLeft,
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
			hasMoreV2 := totalPages < 0
			if hasMoreV2 {
				pageText = fmt.Sprintf(" %d / %d+ ", page, absTotalPagesV)
			} else {
				pageText = fmt.Sprintf(" %d / %d ", page, absTotalPagesV)
			}
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

	// Border — accent 模式下用 accent 颜色替换默认边框
	if cfg.ModeAccentColor != nil {
		base := color.RGBAModel.Convert(cfg.ModeAccentColor).(color.RGBA)
		dc.SetColor(base)
		dc.SetLineWidth(2.5 * scale)
		r.drawRoundedRect(dc, 1, 1, width-4, height-4, cfg.CornerRadius)
		dc.Stroke()
	} else {
		dc.SetColor(cfg.BorderColor)
		dc.SetLineWidth(1)
		r.drawRoundedRect(dc, 0.5, 0.5, width-3, height-3, cfg.CornerRadius)
		dc.Stroke()
	}

	// Input area background and cursor line
	if !cfg.HidePreedit {
		dc.SetColor(cfg.InputBgColor)
		r.drawRoundedRect(dc, padX, padY, width-padX*2-2, inputHeight, 4*scale)
		dc.Fill()

		if cfg.ModeAccentColor != nil {
			base := color.RGBAModel.Convert(cfg.ModeAccentColor).(color.RGBA)
			dc.SetColor(color.RGBA{base.R, base.G, base.B, 35})
			r.drawRoundedRect(dc, padX, padY, width-padX*2-2, inputHeight, 4*scale)
			dc.Fill()
		}

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
			vIndexRadius := math.Max(11*scale, (cfg.IndexFontSize+8*scale)/2)
			vIndexCenterX := padX + vIndexRadius + 3*scale
			indexY := itemY + cfg.ItemHeight/2
			dc.SetColor(cfg.IndexBgColor)
			dc.DrawCircle(vIndexCenterX, indexY, vIndexRadius)
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
		arrowSize := pagerArrowSize(pageFontSize, scale)
		arrowPad := 8.0 * scale
		arrowW := arrowSize + arrowPad*2
		totalW := arrowW + pageW + arrowW
		startX := width/2 - totalW/2
		// Center the button vertically inside pageInfoHeight (which may exceed pagerBtnH)
		btnY := pageY + (pageInfoHeight-pagerBtnH)/2
		centerY := btnY + pagerBtnH/2

		// Page up button
		canPageUp := page > 1
		pageUpBtnRect := CandidateRect{X: startX, Y: btnY, W: arrowW, H: pagerBtnH}
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
		canPageDown := page < absTotalPagesV
		pageDownBtnRect := CandidateRect{X: startX + arrowW + pageW, Y: btnY, W: arrowW, H: pagerBtnH}
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

	// Mode label (e.g. "临时拼音", "快捷输入")
	if cfg.ModeLabel != "" {
		labelSize := cfg.IndexFontSize
		labelColor := r.getCommentColor()
		labelWidth := td.MeasureString(cfg.ModeLabel, labelSize)
		if cfg.HidePreedit {
			// Embedded mode: show on first candidate row only when no candidates
			if len(candidates) == 0 {
				labelX := width - padX - labelWidth - 4*scale
				labelY := candStartY + cfg.ItemHeight/2 + labelSize/3
				td.DrawString(cfg.ModeLabel, labelX, labelY, labelSize, labelColor)
			}
		} else {
			// Non-embedded mode: right-aligned inside the input buffer area
			labelX := width - padX - labelWidth - 8*scale
			labelY := padY + inputHeight/2 + labelSize/3
			td.DrawString(cfg.ModeLabel, labelX, labelY, labelSize, labelColor)
		}
	}

	// Index numbers
	vertIndexWeight := cfg.IndexFontWeight
	for i, cand := range candidates {
		itemY := candStartY + float64(i)*cfg.ItemHeight
		if cand.Index < 0 {
			continue // 负索引跳过绘制（如加词模式）
		}
		indexStr := indexLabel(cfg.IndexLabels, cand.Index, cand.IndexLabel)

		if isTextIndex {
			if vertIndexWeight > 0 {
				td.DrawStringWithWeight(indexStr, padX+4*scale, itemY+cfg.ItemHeight/2+indexTextSize/3, indexTextSize, cfg.IndexColor, vertIndexWeight)
			} else {
				td.DrawString(indexStr, padX+4*scale, itemY+cfg.ItemHeight/2+indexTextSize/3, indexTextSize, cfg.IndexColor)
			}
		} else {
			vIndexRadius := math.Max(11*scale, (cfg.IndexFontSize+8*scale)/2)
			vIndexCenterX := padX + vIndexRadius + 3*scale
			indexY := itemY + cfg.ItemHeight/2
			tw := td.MeasureString(indexStr, cfg.IndexFontSize)
			if vertIndexWeight > 0 {
				td.DrawStringWithWeight(indexStr, vIndexCenterX-tw/2, indexY+cfg.IndexFontSize/3, cfg.IndexFontSize, cfg.IndexColor, vertIndexWeight)
			} else {
				td.DrawString(indexStr, vIndexCenterX-tw/2, indexY+cfg.IndexFontSize/3, cfg.IndexFontSize, cfg.IndexColor)
			}
		}
	}

	// Candidate texts (with ellipsis truncation for long text)
	ellipsis := "…"
	ellipsisWidth := td.MeasureString(ellipsis, cfg.FontSize)
	borderPadding := itemPadR // 右侧边距复用 itemPadR
	for i, cand := range candidates {
		itemY := candStartY + float64(i)*cfg.ItemHeight
		tx := textStartX
		if cand.Index < 0 {
			tx = padX + 8*scale // 无序号时文本靠左
		}
		maxTextWidth := width - tx - borderPadding
		drawText := cand.Text
		if maxTextWidth > 0 {
			textW := td.MeasureString(drawText, cfg.FontSize)
			if textW > maxTextWidth {
				// 逐字符截断直到加上省略号后不超出
				runes := []rune(drawText)
				for len(runes) > 0 {
					runes = runes[:len(runes)-1]
					truncW := td.MeasureString(string(runes), cfg.FontSize) + ellipsisWidth
					if truncW <= maxTextWidth {
						drawText = string(runes) + ellipsis
						break
					}
				}
				if len(runes) == 0 {
					drawText = ellipsis
				}
			}
		}
		td.DrawString(drawText, tx, itemY+cfg.ItemHeight/2+cfg.FontSize/3, cfg.FontSize, cfg.TextColor)
	}

	// Comments
	commentColor := r.getCommentColor()
	for _, c := range comments {
		td.DrawString(c.text, c.x, c.y, commentSize, commentColor)
	}

	// Page text
	if showVerticalPager && showVerticalPageNumber && pageText != "" {
		pageY := candStartY + float64(len(candidates))*cfg.ItemHeight + 4*scale
		arrowSize := pagerArrowSize(pageFontSize, scale)
		arrowPad := 8.0 * scale
		arrowW := arrowSize + arrowPad*2
		totalW := arrowW + pageW + arrowW
		startX := width/2 - totalW/2
		btnY := pageY + (pageInfoHeight-pagerBtnH)/2
		centerY := btnY + pagerBtnH/2

		td.DrawString(pageText, startX+arrowW, centerY+pageFontSize/3, pageFontSize, cfg.InputTextColor)
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
	pageFontSize := pagerFontSize(cfg, scale, isTextIndex)

	// Measure all candidates to calculate total width
	type candMeasure struct {
		textWidth    float64
		commentWidth float64
		totalWidth   float64
	}
	measures := make([]candMeasure, len(candidates))

	indexSize := math.Max(18*scale, cfg.IndexFontSize+4*scale)
	indexMargin := cfg.IndexMarginRight // 从主题配置获取
	itemSpacing := 12.0 * scale

	if isTextIndex {
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
			indexStr := indexLabel(cfg.IndexLabels, cand.Index, cand.IndexLabel)
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
			measures[i].totalWidth = measures[i].textWidth + cfg.TextMarginRight
		} else if isTextIndex {
			measures[i].totalWidth = indexTextWidths[i] + indexMargin + measures[i].textWidth + cfg.TextMarginRight
		} else {
			measures[i].totalWidth = indexSize + indexMargin + measures[i].textWidth + cfg.TextMarginRight
		}
		if cand.Comment != "" {
			measures[i].totalWidth += cfg.CommentMarginLeft + measures[i].commentWidth + cfg.CommentMarginRight
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

	// 处理分级加载：负值 totalPages 表示还有更多候选未加载
	absTotalPagesH := totalPages
	if absTotalPagesH < 0 {
		absTotalPagesH = -absTotalPagesH
	}

	// Page info width
	arrowSize := pagerArrowSize(pageFontSize, scale)
	arrowPad := 6.0 * scale
	arrowW := arrowSize + arrowPad*2
	pageInfoWidth := 0.0
	var pageText string
	var pageW float64
	showPager := absTotalPagesH > 1 || cfg.AlwaysShowPager
	showPageNumber := cfg.ShowPageNumber
	if showPager {
		if absTotalPagesH < 1 {
			absTotalPagesH = 1
			if totalPages < 0 {
				totalPages = -1
			} else {
				totalPages = 1
			}
		}
		if page < 1 {
			page = 1
		}
		if showPageNumber {
			hasMoreH := totalPages < 0
			if hasMoreH {
				pageText = fmt.Sprintf(" %d/%d+ ", page, absTotalPagesH)
			} else {
				pageText = fmt.Sprintf(" %d/%d ", page, absTotalPagesH)
			}
			pageW = td.MeasureString(pageText, pageFontSize)
			pageInfoWidth = arrowW + pageW + arrowW + 8*scale
		} else {
			pageInfoWidth = arrowW + arrowW + 8*scale
		}
	}

	// Input area (preedit)
	// ModeLabel（临时拼音、快捷输入等）显示在输入行右侧，需要一起计算宽度
	modeLabelWidth := 0.0
	if cfg.ModeLabel != "" {
		modeLabelWidth = td.MeasureString(cfg.ModeLabel, cfg.IndexFontSize) + 12*scale
	}
	// embedded 模式：编码嵌入候选行前，不显示独立编码行
	isEmbedded := cfg.PreeditMode == config.PreeditEmbedded && !cfg.HidePreedit
	inputWidth := 0.0
	inputHeight := 0.0
	if !cfg.HidePreedit && !isEmbedded && input != "" {
		inputWidth = td.MeasureString(input, cfg.FontSize)
		inputWidth += 16*scale + modeLabelWidth
		inputHeight = math.Max(24*scale, cfg.FontSize*1.3)
	} else if !cfg.HidePreedit && !isEmbedded && modeLabelWidth > 0 {
		// 无输入但有 ModeLabel（如临时英文初始状态）
		inputWidth = modeLabelWidth + 16*scale
		inputHeight = math.Max(24*scale, cfg.FontSize*1.3)
	}

	// embedded 模式下，编码文本嵌入候选行前的宽度
	embeddedPreeditWidth := 0.0
	embeddedModeLabelWidth := 0.0
	embeddedSeparatorWidth := 16.0 * scale // 编码与候选之间的分隔间距
	if isEmbedded {
		if input != "" {
			embeddedPreeditWidth = td.MeasureString(input, cfg.FontSize) + embeddedSeparatorWidth
		}
		if cfg.ModeLabel != "" {
			embeddedModeLabelWidth = modeLabelWidth + 4*scale
		}
	}

	// Extra padding for accent bar
	accentBarExtra := 0.0
	if cfg.HasAccentBar && cfg.AccentBarColor != nil {
		accentBarExtra = 3.0*scale + 2*scale
	}

	// Total width
	minWidth := 60.0 * scale
	if cfg.HorizontalMinWidth > 0 {
		minWidth = cfg.HorizontalMinWidth
	}
	// embedded 模式：候选行宽度需包含编码前缀
	embeddedPrefix := embeddedPreeditWidth + embeddedModeLabelWidth
	contentWidth := padX*2 + accentBarExtra + embeddedPrefix + candidatesWidth + pageInfoWidth
	if inputWidth > 0 {
		contentWidth = padX*2 + accentBarExtra + inputWidth
		if accentBarExtra+embeddedPrefix+candidatesWidth+pageInfoWidth > accentBarExtra+inputWidth {
			contentWidth = padX*2 + accentBarExtra + embeddedPrefix + candidatesWidth + pageInfoWidth
		}
	}
	width := contentWidth
	if width < minWidth {
		width = minWidth
	}
	if cfg.HorizontalMaxWidth > 0 && width > cfg.HorizontalMaxWidth {
		width = cfg.HorizontalMaxWidth
	}
	// HidePreedit 模式无候选时 ModeLabel 右对齐在候选行，需确保宽度足够显示
	if cfg.HidePreedit && cfg.ModeLabel != "" && len(candidates) == 0 {
		rawLabelWidth := td.MeasureString(cfg.ModeLabel, cfg.IndexFontSize)
		minLabelW := padX*2 + accentBarExtra + rawLabelWidth + 4*scale
		if width < minLabelW {
			width = minLabelW
		}
	}

	// Height calculation
	candidateRowHeight := cfg.ItemHeight
	height := padY*2 + candidateRowHeight
	if inputHeight > 0 {
		height += inputHeight + 4*scale
	}

	// Pre-compute cursor X position
	var cursorDrawX float64
	hasCursor := false
	if !cfg.HidePreedit && !isEmbedded && input != "" && cursorPos >= 0 && cursorPos <= len(input) {
		cursorText := input[:cursorPos]
		preeditX := padX + accentBarExtra
		textX := preeditX + 8*scale
		cursorDrawX = textX + td.MeasureString(cursorText, cfg.FontSize)
		hasCursor = true
	}
	// Embedded mode cursor
	var embeddedCursorX float64
	hasEmbeddedCursor := false
	if isEmbedded && input != "" && cursorPos >= 0 && cursorPos <= len(input) {
		cursorText := input[:cursorPos]
		embedX := padX + accentBarExtra
		embeddedCursorX = embedX + td.MeasureString(cursorText, cfg.FontSize)
		hasEmbeddedCursor = true
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
	if !cfg.HidePreedit && !isEmbedded && input != "" {
		candStartY = padY + inputHeight + 4*scale
	}

	xPos := padX + accentBarOffset + embeddedPrefix + bgPadL
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

	// Border — accent 模式下用 accent 颜色替换默认边框
	if cfg.ModeAccentColor != nil {
		base := color.RGBAModel.Convert(cfg.ModeAccentColor).(color.RGBA)
		dc.SetColor(base)
		dc.SetLineWidth(2.5 * scale)
		r.drawRoundedRect(dc, 1, 1, width-4, height-4, cfg.CornerRadius)
		dc.Stroke()
	} else {
		dc.SetColor(cfg.BorderColor)
		dc.SetLineWidth(1)
		r.drawRoundedRect(dc, 0.5, 0.5, width-3, height-3, cfg.CornerRadius)
		dc.Stroke()
	}

	y := padY

	// Input area shapes (top mode only; embedded mode draws preedit inline with candidates)
	if !cfg.HidePreedit && !isEmbedded && input != "" {
		preeditX := padX + accentBarOffset
		dc.SetColor(cfg.InputBgColor)
		r.drawRoundedRect(dc, preeditX, y, width-preeditX-padX-2, inputHeight, 4*scale)
		dc.Fill()

		if cfg.ModeAccentColor != nil {
			base := color.RGBAModel.Convert(cfg.ModeAccentColor).(color.RGBA)
			dc.SetColor(color.RGBA{base.R, base.G, base.B, 35})
			r.drawRoundedRect(dc, preeditX, y, width-preeditX-padX-2, inputHeight, 4*scale)
			dc.Fill()
		}

		if hasCursor {
			cursorTopY := y + 3*scale
			cursorBottomY := y + inputHeight - 3*scale
			dc.SetColor(cfg.InputTextColor)
			dc.SetLineWidth(1.5 * scale)
			dc.DrawLine(cursorDrawX, cursorTopY, cursorDrawX, cursorBottomY)
			dc.Stroke()
		}
	}

	// Embedded mode: draw cursor in candidate row
	if hasEmbeddedCursor {
		cursorTopY := candStartY + candidateRowHeight*0.15
		cursorBottomY := candStartY + candidateRowHeight*0.85
		dc.SetColor(cfg.InputTextColor)
		dc.SetLineWidth(1.5 * scale)
		dc.DrawLine(embeddedCursorX, cursorTopY, embeddedCursorX, cursorBottomY)
		dc.Stroke()
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
		canPageDown := page < absTotalPagesH
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
	if !cfg.HidePreedit && !isEmbedded && input != "" {
		preeditX := padX + accentBarOffset
		textX := preeditX + 8*scale
		textY := padY + inputHeight/2 + cfg.FontSize/3
		td.DrawString(input, textX, textY, cfg.FontSize, cfg.InputTextColor)
	}

	// Embedded mode: draw preedit text and cursor inline before candidates
	if isEmbedded && (input != "" || cfg.ModeLabel != "") {
		embedY := candStartY + candidateRowHeight/2 + cfg.FontSize/3
		embedX := padX + accentBarOffset
		if input != "" {
			td.DrawString(input, embedX, embedY, cfg.FontSize, cfg.InputTextColor)
		}
		// ModeLabel in embedded mode: draw after preedit text
		if cfg.ModeLabel != "" {
			labelSize := cfg.IndexFontSize
			labelColor := r.getCommentColor()
			labelX := embedX + embeddedPreeditWidth
			labelY := candStartY + candidateRowHeight/2 + labelSize/3
			td.DrawString(cfg.ModeLabel, labelX, labelY, labelSize, labelColor)
		}
	}

	// Mode label (e.g. "临时拼音", "快捷输入") - top mode only
	if !isEmbedded && cfg.ModeLabel != "" {
		labelSize := cfg.IndexFontSize
		labelColor := r.getCommentColor()
		labelWidth := td.MeasureString(cfg.ModeLabel, labelSize)
		if cfg.HidePreedit {
			// HidePreedit mode: show on candidate row only when no candidates
			if len(candidates) == 0 {
				labelX := width - padX - labelWidth - 4*scale
				labelY := candStartY + candidateRowHeight/2 + labelSize/3
				td.DrawString(cfg.ModeLabel, labelX, labelY, labelSize, labelColor)
			}
		} else {
			// Non-embedded mode: right-aligned inside the input buffer area
			labelX := width - padX - labelWidth - 8*scale
			labelY := padY + inputHeight/2 + labelSize/3
			td.DrawString(cfg.ModeLabel, labelX, labelY, labelSize, labelColor)
		}
	}

	// Candidate text (index, text, comment)
	indexWeight := cfg.IndexFontWeight
	for i, cand := range candidates {
		px := positions[i].x

		// Index（负索引跳过绘制，如加词模式）
		if cand.Index >= 0 {
			if isTextIndex {
				indexStr := indexLabel(cfg.IndexLabels, cand.Index, cand.IndexLabel)
				if indexWeight > 0 {
					td.DrawStringWithWeight(indexStr, px, candY+indexTextSize/3, indexTextSize, cfg.IndexColor, indexWeight)
				} else {
					td.DrawString(indexStr, px, candY+indexTextSize/3, indexTextSize, cfg.IndexColor)
				}
			} else {
				indexX := px + indexSize/2
				indexStr := indexLabel(cfg.IndexLabels, cand.Index, cand.IndexLabel)
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
			commentX := positions[i].textX + measures[i].textWidth + cfg.CommentMarginLeft
			td.DrawString(cand.Comment, commentX, candY+commentSize/3, commentSize, r.getCommentColor())
		}
	}

	// Page text
	if showPager && showPageNumber && pageText != "" {
		totalW := arrowW + pageW + arrowW
		startX := width - padX - totalW
		td.DrawString(pageText, startX+arrowW, candY+pageFontSize/3, pageFontSize, cfg.InputTextColor)
	}

	td.EndDraw()

	DrawDebugBanner(img)
	return img, result
}
