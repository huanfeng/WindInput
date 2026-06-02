package ui

// 竖排候选窗的盒模型 View 树构建（v2.6 P1），以及横/竖共用的翻页区构建。

import "fmt"

// buildPager 构建翻页区的子节点（chevron 箭头 + 可选页码）。
// 返回子节点切片，以及可用时的上/下翻页按钮（供命中测试）。无翻页时返回空。
func (r *Renderer) buildPager(
	scale float64, sc func(float64) int, isTextIndex bool,
	page, totalPages int, hoverPageBtn string, rowH int,
) (children []*View, up, down *View) {
	cfg := &r.config
	absTotal := totalPages
	if absTotal < 0 {
		absTotal = -absTotal
	}
	if !(absTotal > 1 || cfg.AlwaysShowPager) {
		return nil, nil, nil
	}

	pageFS := maxF(12*scale, cfg.IndexFontSize)
	if isTextIndex {
		pageFS = maxF(14*scale, cfg.IndexFontSize+2*scale)
	}
	arrowSz := maxF(8*scale, pageFS*0.65)
	arrowW := int(arrowSz + 6*scale*2 + 0.5)
	lineW := 1.5 * scale
	canUp := page > 1
	canDown := page < absTotal

	mkBtn := func(glyph GlyphKind, enabled, hovered bool) *View {
		clr := cfg.IndexBgColor
		if !enabled {
			clr = cfg.InputTextColor
		}
		b := &View{
			FixedW: arrowW, FixedH: rowH,
			Glyph: glyph, GlyphColor: clr, GlyphSize: arrowSz, GlyphLineWidth: lineW,
		}
		if hovered && enabled {
			b.Background = Fill{Color: cfg.HoverBgColor}
			b.Border = Border{Radius: sc(4 * scale)}
		}
		return b
	}

	u := mkBtn(GlyphChevronLeft, canUp, hoverPageBtn == "up")
	children = append(children, u)
	if cfg.ShowPageNumber {
		txt := fmt.Sprintf(" %d/%d ", page, absTotal)
		if totalPages < 0 {
			txt = fmt.Sprintf(" %d/%d+ ", page, absTotal)
		}
		children = append(children, &View{
			Text:      txt,
			TextStyle: TextStyle{FontSize: pageFS, Color: cfg.InputTextColor},
		})
	}
	d := mkBtn(GlyphChevronRight, canDown, hoverPageBtn == "down")
	children = append(children, d)
	if canUp {
		up = u
	}
	if canDown {
		down = d
	}
	return children, up, down
}

// buildVerticalCandidateTree 构建竖排候选窗 View 树（每候选一行、全宽；翻页区在底部居中）。
// 当前覆盖核心；暂未覆盖：长候选省略号截断、ModeLabel、embedded、accent-glow。
func (r *Renderer) buildVerticalCandidateTree(
	candidates []Candidate,
	input string,
	cursorPos int,
	page, totalPages, selectedIndex, hoverIndex int,
	hoverPageBtn string,
) *candWindowTree {
	cfg := &r.config
	scale := GetDPIScale()
	sc := func(v float64) int { return int(v*scale + 0.5) }

	isTextIndex := cfg.IndexStyle == "text"
	padX := pickF(cfg.WindowPaddingX, cfg.Padding)
	padY := pickF(cfg.WindowPaddingY, cfg.Padding)
	indexMarginRight := pickF(cfg.IndexMarginRight*scale, 4*scale)
	commentMarginLeft := pickF(cfg.CommentMarginLeft*scale, 8*scale)
	itemPadR := pickF(cfg.ItemPaddingRight*scale, 8*scale)

	indexRadius := maxF(11*scale, (cfg.IndexFontSize+8*scale)/2)
	indexAreaW := int(2*indexRadius + 6*scale + 0.5)
	if isTextIndex {
		indexAreaW = sc(20 * scale)
	}
	commentSize := cfg.IndexFontSize
	if isTextIndex {
		commentSize = cfg.IndexFontSize + 2*scale
	}
	rowH := int(cfg.ItemHeight + 0.5)
	commentColor := r.getCommentColor()

	// 长候选钳制：预量算自然宽，计算截断预算 targetW ≤ VerticalMaxWidth（默认 600*scale）。
	maxItemW := pickF(cfg.VerticalMaxWidth, 600*scale)
	commentWidths := make([]float64, len(candidates))
	maxNatural := 0.0
	for i, cand := range candidates {
		lo := 8 * scale
		if cand.Index >= 0 {
			lo = float64(indexAreaW) + indexMarginRight
		}
		tw := r.textDrawer.MeasureString(candidateDisplayText(cand, cfg.CmdbarPrefix), cfg.FontSize)
		if cand.Comment != "" {
			commentWidths[i] = r.textDrawer.MeasureString(cand.Comment, commentSize)
		}
		nat := lo + tw + itemPadR
		if commentWidths[i] > 0 {
			nat += commentMarginLeft + commentWidths[i]
		}
		if nat > maxNatural {
			maxNatural = nat
		}
	}
	targetW := maxNatural
	if targetW > maxItemW {
		targetW = maxItemW
	}

	// ---- 候选项（每行全宽）----
	items := make([]*View, 0, len(candidates))
	for i, cand := range candidates {
		children := make([]*View, 0, 3)

		if cand.Index >= 0 {
			label := indexLabel(cfg.IndexLabels, cand.Index, cand.IndexLabel)
			if isTextIndex {
				children = append(children, &View{
					FixedW:    indexAreaW,
					Text:      label,
					TextStyle: TextStyle{FontSize: cfg.IndexFontSize, Weight: cfg.IndexFontWeight, Color: cfg.IndexColor},
				})
			} else {
				d := int(2*indexRadius + 0.5)
				leftM := sc(3 * scale)
				rightM := indexAreaW - d - leftM
				if rightM < 0 {
					rightM = 0
				}
				children = append(children, &View{
					FixedW:     d,
					FixedH:     d,
					Margin:     Edges{Left: leftM, Right: rightM},
					Background: Fill{Color: cfg.IndexBgColor},
					Border:     Border{Radius: d / 2},
					Layout:     LayoutStack,
					Children: []*View{{
						FixedW:    d,
						FixedH:    d,
						Text:      label,
						TextStyle: TextStyle{FontSize: cfg.IndexFontSize, Weight: cfg.IndexFontWeight, Color: cfg.IndexColor, Align: AlignCenter},
					}},
				})
			}
		}

		lo := 8 * scale
		if cand.Index >= 0 {
			lo = float64(indexAreaW) + indexMarginRight
		}
		availText := targetW - lo - itemPadR
		if commentWidths[i] > 0 {
			availText -= commentMarginLeft + commentWidths[i]
		}
		textChild := &View{
			Text:      r.truncateToWidth(candidateDisplayText(cand, cfg.CmdbarPrefix), cfg.FontSize, availText),
			TextStyle: TextStyle{FontSize: cfg.FontSize, Color: cfg.TextColor},
		}
		if len(children) > 0 {
			textChild.Margin = Edges{Left: sc(indexMarginRight)}
		} else {
			textChild.Margin = Edges{Left: sc(8 * scale)} // 无序号时靠左
		}
		children = append(children, textChild)

		if cand.Comment != "" {
			children = append(children, &View{
				Text:      cand.Comment,
				TextStyle: TextStyle{FontSize: commentSize, Color: commentColor},
				Margin:    Edges{Left: sc(commentMarginLeft)},
			})
		}

		item := &View{
			Layout:     LayoutRow,
			CrossAlign: AlignCenter,
			Stretch:    true, // 每行全宽
			FixedH:     rowH,
			Padding:    Edges{Right: sc(itemPadR)},
			Children:   children,
		}
		if i == selectedIndex {
			item.Background = Fill{Color: cfg.SelectedBgColor}
			if cfg.HasAccentBar && cfg.AccentBarColor != nil {
				barW := sc(3 * scale)
				item.Layers = []ImageLayer{{
					Color: cfg.AccentBarColor, Z: -1, Anchor: "left",
					OffsetX: sc(1 * scale), W: barW, H: int(cfg.ItemHeight*0.6 + 0.5), Radius: barW / 2,
				}}
			}
		} else if i == hoverIndex {
			item.Background = Fill{Color: cfg.HoverBgColor}
		}
		items = append(items, item)
	}
	list := &View{Layout: LayoutColumn, Stretch: true, Children: items}

	// ---- band 列表 ----
	bands := make([]*View, 0, 3)
	if (input != "" || cfg.ModeLabel != "") && !cfg.HidePreedit {
		inputH := int(maxF(30*scale, cfg.FontSize*1.5) + 0.5)
		bands = append(bands, r.buildPreeditBand(input, cursorPos, inputH, scale, sc))
	}
	bands = append(bands, list)

	// ---- 翻页区（底部居中行）----
	pagerChildren, pagerUp, pagerDown := r.buildPager(scale, sc, isTextIndex, page, totalPages, hoverPageBtn, rowH)
	if len(pagerChildren) > 0 {
		bands = append(bands, &View{
			Layout:     LayoutRow,
			CrossAlign: AlignCenter,
			Children:   pagerChildren,
		})
	}

	window := &View{
		Layout:     LayoutColumn,
		CrossAlign: AlignCenter, // 让底部翻页行水平居中
		Gap:        sc(4 * scale),
		Padding:    Edges{Top: sc(padY), Right: sc(padX), Bottom: sc(padY), Left: sc(padX)},
		Background: Fill{Color: cfg.BackgroundColor, Image: cfg.BackgroundImage, Mode: cfg.BackgroundMode, Slice: cfg.BackgroundSlice, Opacity: cfg.BackgroundOpacity},
		Border:     r.windowBorder(int(cfg.CornerRadius+0.5), sc, scale),
		Shadow:     &ViewShadow{OffsetX: sc(2 * scale), OffsetY: sc(2 * scale), Color: r.getShadowColor()},
		Children:   bands,
	}
	return &candWindowTree{root: window, items: items, pagerUp: pagerUp, pagerDown: pagerDown}
}
