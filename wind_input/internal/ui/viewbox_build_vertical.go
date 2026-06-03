package ui

// 竖排候选窗的盒模型 View 树构建（v2.6 P1），以及横/竖共用的翻页区构建。

import (
	"fmt"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// buildPager 构建翻页区的子节点（chevron 箭头 + 可选页码）。
// 返回子节点切片，以及可用时的上/下翻页按钮（供命中测试）。无翻页时返回空。
func (r *Renderer) buildPager(
	scale float64, sc func(float64) int,
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

	pageFS := r.resolvedViews.FooterBar.FontSize // 页码/翻页字号 = base + views.footer_bar.font_size 偏移（无派生魔法）
	arrowSz := maxF(8*scale, pageFS*0.65)        // chevron 视觉尺寸按字号比例（绘制内禀几何，非主题字号配置）
	arrowW := int(arrowSz + 6*scale*2 + 0.5)
	lineW := 1.5 * scale
	canUp := page > 1
	canDown := page < absTotal

	mkBtn := func(glyph GlyphKind, enabled, hovered bool) *View {
		clr := r.resolvedViews.Index.BgColor
		if !enabled {
			clr = r.resolvedViews.PreeditBar.TextColor
		}
		b := &View{
			FixedW: arrowW, FixedH: rowH,
			Glyph: glyph, GlyphColor: clr, GlyphSize: arrowSz, GlyphLineWidth: lineW,
		}
		if hovered && enabled {
			b.Background = Fill{Color: stateBg(r.resolvedViews.Item.Hover)}
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
			TextStyle: TextStyle{FontSize: pageFS, Color: r.resolvedViews.PreeditBar.TextColor},
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
	scD := func(d theme.Dimension) int { return d.Scaled(scale) } // 按单位换算为设备像素（dp 缩放 / px 不缩放）

	isTextIndex := cfg.IndexStyle == "text"
	// 外观取值改走 ResolvedViews，经 scD 按单位换算为设备像素（dp 缩放 / px 不缩放）。
	rv := &r.resolvedViews
	indexMarginRight := scD(rv.Text.MarginLeft)
	commentMarginLeft := scD(rv.Comment.MarginLeft)
	itemPadR := scD(rv.Item.PadRight)

	// 圆圈序号直径 = 序号字号 + index 上下 padding（盒模型，无 max(11/18) 尺寸下限魔法）。
	indexD := rv.Index.FontSize + maxF(float64(scD(rv.Index.PadTop)+scD(rv.Index.PadBottom)), float64(scD(rv.Index.PadLeft)+scD(rv.Index.PadRight))) // 四边 padding 都参与，保持正圆
	indexAreaW := int(indexD + 6*scale + 0.5)
	if isTextIndex {
		// 文本序号列宽按字形测量收紧：取最宽序号标签实际宽 + 小留白，紧凑且各行候选文字对齐。
		// （旧 sc(20*scale) 既偏宽、又重复乘 scale 致高 DPI 失真。）
		maxLabelW := 0.0
		for _, cand := range candidates {
			if cand.Index >= 0 {
				lw := measureText(r.textDrawer, indexLabel(r.effectiveIndexLabels(), cand.Index, cand.IndexLabel), rv.Index.FontSize, rv.Index.FontFamily)
				if lw > maxLabelW {
					maxLabelW = lw
				}
			}
		}
		indexAreaW = int(maxLabelW + float64(scD(rv.Index.PadLeft)+scD(rv.Index.PadRight)) + 0.5) // 文本序号列宽 = 最宽标签 + 左右 padding（取代 +4 魔法）
	}
	commentSize := rv.Comment.FontSize // 注释字号 = base + views.comment.font_size 偏移（无派生魔法）
	// 行高 = 行内容自然高 + item 上下内边距（全由主题 item.padding 控制，无 max(32) 魔法）。
	lineH := rv.Text.FontSize
	if commentSize > lineH {
		lineH = commentSize
	}
	if isTextIndex {
		if rv.Index.FontSize > lineH {
			lineH = rv.Index.FontSize
		}
	} else if indexD > lineH {
		lineH = indexD
	}
	rowH := int(lineH+0.5) + scD(rv.Item.PadTop) + scD(rv.Item.PadBottom)

	// 强调条占位 rail 宽度（逻辑像素）：与横排一致取 item 左内边距为左留白，承载强调条；
	// 不足以容纳强调条（offset+width）时取下限。无强调条时 railFixedW=0（不占位）。
	railW := 0 // 设备像素；0=无强调条不占位
	railFixedW := 0
	if cfg.HasAccentBar && rv.AccentBar.BgColor != nil {
		railW = scD(rv.Item.PadLeft)
		if minW := rv.AccentBarOffset.Scaled(scale) + rv.AccentBarWidth.Scaled(scale) + sc(2); railW < minW {
			railW = minW
		}
		railFixedW = railW
	}

	// 长候选钳制：预量算自然宽，计算截断预算 targetW ≤ VerticalMaxWidth（默认 600*scale）。
	maxItemW := rv.VerticalMaxWidth * scale
	commentWidths := make([]float64, len(candidates))
	maxNatural := 0.0
	for i, cand := range candidates {
		lo := float64(railFixedW) + 8*scale
		if cand.Index >= 0 {
			lo = float64(railFixedW) + float64(indexAreaW) + float64(indexMarginRight)
		}
		tw := measureText(r.textDrawer, candidateDisplayText(cand, cfg.CmdbarPrefix), rv.Text.FontSize, rv.Text.FontFamily)
		if cand.Comment != "" {
			commentWidths[i] = measureText(r.textDrawer, cand.Comment, commentSize, rv.Comment.FontFamily)
		}
		nat := lo + tw + float64(itemPadR)
		if commentWidths[i] > 0 {
			nat += float64(commentMarginLeft) + commentWidths[i]
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

		// P7-D：候选文字用 item 选中态着色/加粗；序号、注释各自独立支持选中态（与横排一致）。
		sel, hov := i == selectedIndex, i == hoverIndex
		st := itemStateFor(rv.Item, sel, hov)
		textColor, textWeight := rv.Text.TextColor, rv.Text.FontWeight
		if st != nil {
			if st.TextColor != nil {
				textColor = st.TextColor
			}
			if st.FontWeight != 0 {
				textWeight = st.FontWeight
			}
		}
		idxColor, idxWeight := elementTextState(rv.Index, sel, hov)
		cmtColor, cmtWeight := elementTextState(rv.Comment, sel, hov)

		if cand.Index >= 0 {
			label := indexLabel(r.effectiveIndexLabels(), cand.Index, cand.IndexLabel)
			if isTextIndex {
				children = append(children, &View{
					FixedW:    indexAreaW,
					Text:      label,
					TextStyle: TextStyle{FontSize: rv.Index.FontSize, Weight: idxWeight, Family: rv.Index.FontFamily, Color: idxColor},
				})
			} else {
				d := int(indexD + 0.5)
				leftM := sc(3 * scale)
				rightM := indexAreaW - d - leftM
				if rightM < 0 {
					rightM = 0
				}
				children = append(children, &View{
					FixedW:     d,
					FixedH:     d,
					Margin:     Edges{Left: leftM, Right: rightM},
					Background: r.elementFill(rv.Index, sel, hov), // P7-C/D：序号背景可带图、可随选中态变
					Border:     Border{Radius: d / 2},
					Layout:     LayoutStack,
					Children: []*View{{
						FixedW:    d,
						FixedH:    d,
						Text:      label,
						TextStyle: TextStyle{FontSize: rv.Index.FontSize, Weight: idxWeight, Family: rv.Index.FontFamily, Color: idxColor, Align: AlignCenter},
					}},
				})
			}
		}

		lo := float64(railFixedW) + 8*scale
		if cand.Index >= 0 {
			lo = float64(railFixedW) + float64(indexAreaW) + float64(indexMarginRight)
		}
		availText := targetW - lo - float64(itemPadR)
		if commentWidths[i] > 0 {
			availText -= float64(commentMarginLeft) + commentWidths[i]
		}
		textChild := &View{
			Text:      r.truncateToWidth(candidateDisplayText(cand, cfg.CmdbarPrefix), rv.Text.FontSize, availText, rv.Text.FontFamily),
			TextStyle: TextStyle{FontSize: rv.Text.FontSize, Weight: textWeight, Family: rv.Text.FontFamily, Color: textColor},
		}
		if len(children) > 0 {
			textChild.Margin = Edges{Left: indexMarginRight}
		} else {
			textChild.Margin = Edges{Left: sc(8 * scale)} // 无序号时靠左
		}
		children = append(children, textChild)

		if cand.Comment != "" {
			children = append(children, &View{
				Text:      cand.Comment,
				TextStyle: TextStyle{FontSize: commentSize, Weight: cmtWeight, Family: rv.Comment.FontFamily, Color: cmtColor},
				Margin:    Edges{Left: commentMarginLeft},
			})
		}

		// 强调条占位元素：rail 在所有行占据左留白（保持列对齐），仅选中行绘制强调条；
		// 内容（序号/文字）排在 rail 右侧。无强调条主题不加 rail（railFixedW=0，内容靠左）。
		itemChildren := children
		if rail := r.buildAccentRail(railW, i == selectedIndex, rowH, scale); rail != nil {
			itemChildren = append([]*View{rail}, children...)
		}
		item := &View{
			Layout:     LayoutRow,
			CrossAlign: AlignCenter,
			Stretch:    true, // 每行全宽
			FixedH:     rowH,
			// 上下 padding 真实生效（与横排一致）：对称时逐像素同旧版，非对称不再均摊。
			Padding: Edges{Top: scD(rv.Item.PadTop), Right: itemPadR, Bottom: scD(rv.Item.PadBottom)},
			// 候选项圆角（与横排一致；原竖排漏设导致横竖排圆角不同）。
			Border:   Border{Radius: rv.Item.BorderRadius.Scaled(scale)},
			Children: itemChildren,
		}
		r.applyItemState(item, st, scale)             // P7-D：选中/悬停态背景（高亮位图/底色）+ 边框
		r.appendThemeLayers(item, rv.Item.Layers, sc) // P7-C：候选项装饰层
		items = append(items, item)
	}
	list := &View{Layout: LayoutColumn, Stretch: true, Children: items}

	// ---- band 列表 ----
	bands := make([]*View, 0, 3)
	if (input != "" || cfg.ModeLabel != "") && !cfg.HidePreedit {
		inputH := int(rv.PreeditBar.FontSize+0.5) + scD(rv.PreeditBar.PadTop) + scD(rv.PreeditBar.PadBottom) // 条高=内容+preedit 上下 padding（无 max 魔法，横竖统一）
		bands = append(bands, r.buildPreeditBand(input, cursorPos, inputH, scale, sc))
	}
	bands = append(bands, list)

	// ---- 翻页区（底部居中行）----
	pagerChildren, pagerUp, pagerDown := r.buildPager(scale, sc, page, totalPages, hoverPageBtn, rowH)
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
		Gap:        rv.WindowGap.Scaled(scale),
		Padding:    Edges{Top: scD(rv.Window.PadTop), Right: scD(rv.Window.PadRight), Bottom: scD(rv.Window.PadBottom), Left: scD(rv.Window.PadLeft)}, // 完整遵循主题 window.padding 四边
		Background: r.fillFor(r.resolvedViews.Window.BgColor, r.resolvedViews.Window.BgImage),                                                         // P7-C：背景图来自 views.window.background.image
		Border:     r.windowBorder(rv.Window.BorderRadius.Scaled(scale), sc, scale),
		Shadow:     &ViewShadow{OffsetX: rv.ShadowOffsetX.Scaled(scale), OffsetY: rv.ShadowOffsetY.Scaled(scale), Color: r.resolvedViews.ShadowColor},
		Children:   bands,
	}
	r.appendThemeLayers(window, rv.Window.Layers, sc) // P7-C：窗口装饰层（水印等）
	return &candWindowTree{root: window, items: items, pagerUp: pagerUp, pagerDown: pagerDown}
}
