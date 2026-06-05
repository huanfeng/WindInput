package ui

// 竖排候选窗的盒模型 View 树构建（P1），以及横/竖共用的翻页区构建。

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

	fb := r.resolvedViews.FooterBar
	pageFS := fb.FontSize                 // 页码/翻页字号 = base + views.footer_bar.font_size 偏移（无派生魔法）
	arrowSz := maxF(8*scale, pageFS*0.65) // chevron 视觉尺寸按字号比例（绘制内禀几何，非主题字号配置）
	// 箭头左右 padding 来自 views.footer_bar.padding；未配兜底现状 6（默认逐像素零回归）。
	arrowPadL := 6.0 * scale
	if v := fb.PadLeft.Scaled(scale); v != 0 {
		arrowPadL = float64(v)
	}
	arrowPadR := 6.0 * scale
	if v := fb.PadRight.Scaled(scale); v != 0 {
		arrowPadR = float64(v)
	}
	arrowW := int(arrowSz + arrowPadL + arrowPadR + 0.5)
	lineW := 1.5 * scale
	canUp := page > 1
	canDown := page < absTotal

	// 翻页颜色可配（views.footer_bar.color）：页码 + 启用态箭头取 FooterBar.TextColor，
	// 未配则零回归（页码=PreeditBar.TextColor、启用箭头=Index.BgColor）；禁用箭头恒为暗色（PreeditBar.TextColor）。
	pageColor := r.resolvedViews.PreeditBar.TextColor
	if fb.TextColor != nil {
		pageColor = fb.TextColor
	}

	iconSz := int(arrowSz + 0.5)
	mkBtn := func(glyph GlyphKind, img *theme.RVImage, enabled, hovered bool) *View {
		clr := r.resolvedViews.Index.BgColor
		if fb.TextColor != nil {
			clr = fb.TextColor
		}
		if !enabled {
			clr = r.resolvedViews.PreeditBar.TextColor
		}
		b := &View{FixedW: arrowW, FixedH: rowH}
		var bg Fill
		if hovered && enabled {
			bg.Color = stateBg(r.resolvedViews.Item.Hover)
			b.Border = Border{Radius: sc(4 * scale)}
		}
		// 主题配了翻页箭头图（views.footer_bar.prev_image/next_image，SVG/PNG，可 tint 随主题变色）→
		// 按图标尺寸栅格化、居中绘制；解码失败回退内置矢量 chevron（零回归）。
		useGlyph := true
		if img != nil {
			// 禁用态（首/末页）用主题配的 disabled_tint（可引用 LightDark token 自动亮暗）；未配则不变化。
			tint := img.TintColor
			if !enabled && img.DisabledTintColor != nil {
				tint = img.DisabledTintColor
			}
			if decoded := r.imgRes.resolveImage(img.Ref, r.resourcesSnapshot(), iconSz, iconSz, tint); decoded != nil {
				bg.Image = decoded
				bg.Mode = "center"
				useGlyph = false
			}
		}
		if useGlyph {
			b.Glyph, b.GlyphColor, b.GlyphSize, b.GlyphLineWidth = glyph, clr, arrowSz, lineW
		}
		b.Background = bg
		return b
	}

	u := mkBtn(GlyphChevronLeft, fb.PrevImage, canUp, hoverPageBtn == "up")
	children = append(children, u)
	if cfg.ShowPageNumber {
		txt := fmt.Sprintf(" %d/%d ", page, absTotal)
		if totalPages < 0 {
			txt = fmt.Sprintf(" %d/%d+ ", page, absTotal)
		}
		children = append(children, &View{
			Text:      txt,
			TextStyle: TextStyle{FontSize: pageFS, Color: pageColor},
		})
	}
	d := mkBtn(GlyphChevronRight, fb.NextImage, canDown, hoverPageBtn == "down")
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

		// 架构统一：与横排同一套 effectiveNode + styleLeaf + buildIndexCircle。
		sel, hov := i == selectedIndex, i == hoverIndex
		effIdx := effectiveNode(rv.Index, sel, hov)
		effText := effectiveNode(rv.Text, sel, hov)
		effCmt := effectiveNode(rv.Comment, sel, hov)
		effItem := effectiveNode(rv.Item, sel, hov)

		if cand.Index >= 0 {
			label := indexLabel(r.effectiveIndexLabels(), cand.Index, cand.IndexLabel)
			if isTextIndex {
				// 文本序号：accent 底色是圆圈模式专属，文本模式无背景（保留 color/font/border）。
				effIdxText := effIdx
				effIdxText.BgColor, effIdxText.BgImage = nil, nil
				idx := r.styleLeaf(effIdxText, label, scale, AlignStart, Edges{})
				idx.FixedW = indexAreaW
				children = append(children, idx)
			} else {
				d := int(indexD + 0.5)
				leftM := sc(3 * scale)
				rightM := indexAreaW - d - leftM
				if rightM < 0 {
					rightM = 0
				}
				circle := r.buildIndexCircle(effIdx, label, d, scale)
				circle.Margin = Edges{Left: leftM, Right: rightM}
				children = append(children, circle)
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
		textMargin := Edges{Left: indexMarginRight}
		if len(children) == 0 {
			textMargin = Edges{Left: sc(8 * scale)} // 无序号时靠左
		}
		textStr := r.truncateToWidth(candidateDisplayText(cand, cfg.CmdbarPrefix), rv.Text.FontSize, availText, rv.Text.FontFamily)
		children = append(children, r.styleLeaf(effText, textStr, scale, AlignStart, textMargin))

		if cand.Comment != "" {
			children = append(children, r.styleLeaf(effCmt, cand.Comment, scale, AlignStart, Edges{Left: commentMarginLeft}))
		}

		// 强调条占位元素：rail 在所有行占据左留白（保持列对齐），仅选中行绘制强调条；
		// 内容（序号/文字）排在 rail 右侧。无强调条主题不加 rail（railFixedW=0，内容靠左）。
		itemChildren := children
		if rail := r.buildAccentRail(railW, sel, rowH, scale); rail != nil {
			itemChildren = append([]*View{rail}, children...)
		}
		item := &View{
			Layout:     LayoutRow,
			CrossAlign: AlignCenter,
			Stretch:    true, // 每行全宽
			FixedH:     rowH,
			// 上下 padding 真实生效（与横排一致）：对称时逐像素同旧版，非对称不再均摊。
			Padding:  Edges{Top: scD(rv.Item.PadTop), Right: itemPadR, Bottom: scD(rv.Item.PadBottom)},
			Children: itemChildren,
		}
		r.applyNodeBox(item, effItem, scale)          // 统一：item 行背景 + 边框（含选中/悬停态）
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
		Background: r.fillFor(r.resolvedViews.Window.BgColor, r.resolvedViews.Window.BgImage, r.resolvedViews.Window.BgGradient),                      // P7-C：背景图来自 views.window.background.image
		Border:     r.windowBorder(rv.Window.BorderRadius.Scaled(scale), sc, scale),
		Shadow:     &ViewShadow{OffsetX: rv.ShadowOffsetX.Scaled(scale), OffsetY: rv.ShadowOffsetY.Scaled(scale), Color: r.resolvedViews.ShadowColor},
		Children:   bands,
	}
	r.appendThemeLayers(window, rv.Window.Layers, sc) // P7-C：窗口装饰层（水印等）
	return &candWindowTree{root: window, items: items, pagerUp: pagerUp, pagerDown: pagerDown}
}
