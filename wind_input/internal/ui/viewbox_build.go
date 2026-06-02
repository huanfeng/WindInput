package ui

// 从 RenderConfig + 候选数据构建候选窗的盒模型 View 树（v2.6 P1）。
//
// 本文件是"固定骨架 + 统一 View"思路的落地：旧渲染器里逐元素硬编码的 magic number，
// 在这里被翻译成各 View 的 margin/padding/border/fixed-size。引擎（viewbox.go/_paint.go）
// 只负责 measure/arrange/paint，对"候选窗"语义无感知。
//
// 当前覆盖横排核心：window / preedit_bar / candidate_list / item / index / text / comment
// 以及 selected/hover 背景、accent bar、pager、preedit 光标、ModeLabel、accent-glow。
// 暂未覆盖（后续迭代）：embedded preedit、竖排长候选省略号截断。

import (
	"image/color"

	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/theme"
)

// buildEmbeddedPreedit 构建内嵌预编辑（PreeditEmbedded 模式）：编码 + ModeLabel 内嵌到候选行首，
// 与首个候选间留 16*scale 分隔；含内嵌光标。无内容返回 nil。
func (r *Renderer) buildEmbeddedPreedit(input string, cursorPos, rowH int, scale float64, sc func(float64) int) *View {
	cfg := &r.config
	if input == "" && cfg.ModeLabel == "" {
		return nil
	}
	pbFS := r.resolvedViews.PreeditBar.FontSize // P7-B：预编辑字号（views 显式或运行时回填）
	children := make([]*View, 0, 2)
	if input != "" {
		children = append(children, &View{Text: input, TextStyle: TextStyle{FontSize: pbFS, Weight: r.resolvedViews.PreeditBar.FontWeight, Family: r.resolvedViews.PreeditBar.FontFamily, Color: r.resolvedViews.PreeditBar.TextColor}})
	}
	if cfg.ModeLabel != "" {
		ml := &View{Text: cfg.ModeLabel, TextStyle: TextStyle{FontSize: cfg.IndexFontSize, Color: r.getCommentColor()}}
		if len(children) > 0 {
			ml.Margin = Edges{Left: sc(4 * scale)}
		}
		children = append(children, ml)
	}
	inline := &View{
		Layout: LayoutRow, CrossAlign: AlignCenter, FixedH: rowH,
		Margin:   Edges{Right: sc(16 * scale)},
		Children: children,
	}
	if input != "" && cursorPos >= 0 && cursorPos <= len(input) {
		cw := measureText(r.textDrawer, input[:cursorPos], pbFS, r.resolvedViews.PreeditBar.FontFamily)
		inline.Layers = append(inline.Layers, ImageLayer{
			Color: r.resolvedViews.PreeditBar.TextColor, Z: 1, Anchor: "left",
			OffsetX: int(cw + 0.5), W: maxInt(1, sc(1.5*scale)), H: int(float64(rowH) * 0.7),
		})
	}
	return inline
}

// buildPreeditBand 构建预编辑条（横竖排共用）：输入文本 + 光标 + 右对齐 ModeLabel + accent-glow 底色。
// inputH 为条高（横/竖排不同）。
func (r *Renderer) buildPreeditBand(input string, cursorPos, inputH int, scale float64, sc func(float64) int) *View {
	cfg := &r.config
	bgColor := r.resolvedViews.PreeditBar.BgColor
	if cfg.ModeAccentColor != nil {
		bgColor = blendColor(r.resolvedViews.PreeditBar.BgColor, cfg.ModeAccentColor, 35) // 临时拼音等模式：input 区半透 accent 叠加
	}
	pb := &r.resolvedViews.PreeditBar
	pbFS := pb.FontSize // P7-B：预编辑字号（views 显式或运行时回填）
	children := []*View{{
		Text:      input,
		TextStyle: TextStyle{FontSize: pbFS, Weight: pb.FontWeight, Family: pb.FontFamily, Color: pb.TextColor},
	}}
	if cfg.ModeLabel != "" {
		children = append(children,
			&View{Grow: true}, // 弹性占位把标签推到右侧
			&View{Text: cfg.ModeLabel, TextStyle: TextStyle{FontSize: cfg.IndexFontSize, Color: r.getCommentColor()}},
		)
	}
	band := &View{
		Layout: LayoutRow, CrossAlign: AlignCenter, Stretch: true, FixedH: inputH,
		Padding:    Edges{Left: pb.PadLeft.Scaled(scale), Right: pb.PadRight.Scaled(scale)},
		Background: r.fillFor(bgColor, pb.BgImage), // P7-C：preedit 背景可带图
		Border:     Border{Radius: pb.BorderRadius.Scaled(scale)},
		Children:   children,
	}
	r.appendThemeLayers(band, pb.Layers, sc)
	if input != "" && cursorPos >= 0 && cursorPos <= len(input) {
		cw := measureText(r.textDrawer, input[:cursorPos], pbFS, pb.FontFamily)
		band.Layers = append(band.Layers, ImageLayer{
			Color: pb.TextColor, Z: 1, Anchor: "left",
			OffsetX: pb.PadLeft.Scaled(scale) + int(cw+0.5), W: maxInt(1, sc(1.5*scale)), H: int(pbFS + 0.5),
		})
	}
	return band
}

// windowBorder 返回窗口边框：accent 模式(ModeAccentColor 非空)用更宽的 accent 色 glow 边框。
func (r *Renderer) windowBorder(radius int, sc func(float64) int, scale float64) Border {
	cfg := &r.config
	if cfg.ModeAccentColor != nil {
		return Border{Width: maxInt(1, sc(2.5*scale)), Color: cfg.ModeAccentColor, Radius: radius}
	}
	// 非 accent：边框宽来自 views.window.border.width（逻辑像素，经 sc 缩放）；0=无边框。
	return Border{Width: r.resolvedViews.Window.BorderWidth.Scaled(scale), Color: r.resolvedViews.Window.BorderColor, Radius: radius}
}

// truncateToWidth 把 text 截断到不超过 avail 像素宽，超出时尾部加省略号。
// family 用于按元素字体度量（P7-B），空=全局字体。
func (r *Renderer) truncateToWidth(text string, fontSize, avail float64, family string) string {
	if avail <= 0 || measureText(r.textDrawer, text, fontSize, family) <= avail {
		return text
	}
	const ell = "…"
	ellW := measureText(r.textDrawer, ell, fontSize, family)
	runes := []rune(text)
	for len(runes) > 0 {
		runes = runes[:len(runes)-1]
		if measureText(r.textDrawer, string(runes), fontSize, family)+ellW <= avail {
			return string(runes) + ell
		}
	}
	return ell
}

// effectiveIndexLabels 返回生效的序号标签：用户全局覆盖（config）优先于主题 labels。
// 运行时 per-候选 IndexLabel 优先级更高，由 indexLabel() 内部处理（构成四层优先级）。
func (r *Renderer) effectiveIndexLabels() string {
	if r.config.GlobalIndexLabels != "" {
		return r.config.GlobalIndexLabels
	}
	return r.config.IndexLabels
}

// blendColor 把 over 以 overAlpha/255 透明度叠加到 base 上，返回不透明结果。
func blendColor(base, over color.Color, overAlpha uint32) color.Color {
	br, bg, bb, _ := base.RGBA()
	or, og, ob, _ := over.RGBA()
	inv := 255 - overAlpha
	mix := func(b, o uint32) uint8 { return uint8(((o>>8)*overAlpha + (b>>8)*inv) / 255) }
	return color.RGBA{mix(br, or), mix(bg, og), mix(bb, ob), 255}
}

// itemStateFor 选取候选项当前要应用的状态 patch（P7-D）：selected 优先于 hover；
// 均不命中返回 nil（沿用基态）。disabled 候选项暂无运行时触发器，不在此选取（schema 预留）。
func itemStateFor(item theme.RVNode, selected, hover bool) *theme.RVState {
	if selected {
		return item.Selected
	}
	if hover {
		return item.Hover
	}
	return nil
}

// stateBg 取状态 patch 的底色（nil-safe）；用于不需要位图/边框的轻量场景（如翻页按钮 hover）。
func stateBg(st *theme.RVState) color.Color {
	if st != nil {
		return st.BgColor
	}
	return nil
}

// elementTextState 取元素在当前选中/悬停下的有效文字色与字重（P7-D）：
// 元素自身 selected/hover patch 提供则覆盖，否则用基态（默认与普通态一致）。
func elementTextState(n theme.RVNode, selected, hover bool) (color.Color, int) {
	c, w := n.TextColor, n.FontWeight
	if st := itemStateFor(n, selected, hover); st != nil {
		if st.TextColor != nil {
			c = st.TextColor
		}
		if st.FontWeight != 0 {
			w = st.FontWeight
		}
	}
	return c, w
}

// elementFill 取元素在当前选中/悬停下的有效背景填充（P7-D）：state 的底色/位图覆盖基态。
func (r *Renderer) elementFill(n theme.RVNode, selected, hover bool) Fill {
	bg, img := n.BgColor, n.BgImage
	if st := itemStateFor(n, selected, hover); st != nil {
		if st.BgColor != nil {
			bg = st.BgColor
		}
		if st.BgImage != nil {
			img = st.BgImage
		}
	}
	return r.fillFor(bg, img)
}

// applyItemState 把状态 patch 应用到候选项 View：背景（高亮位图优先于底色）+ 边框覆盖（P7-D）。
// 文字色/字重在行内构建文本 cell 时单独应用（整行统一），不在此处理。
func (r *Renderer) applyItemState(item *View, st *theme.RVState, scale float64) {
	if st == nil {
		return
	}
	item.Background = r.fillFor(st.BgColor, st.BgImage) // 高亮位图（Fill.Image）优先于底色
	if st.BorderColor != nil {
		item.Border.Color = st.BorderColor
	}
	if st.BorderWidth != nil {
		item.Border.Width = st.BorderWidth.Scaled(scale)
	}
}

// buildAccentRail 构建强调条占位元素：作为候选项行的前导 View，FixedW=railW 在**所有行**
// 占位以保持序号/文字列对齐，仅 selected 行绘制强调条（z<0 纯色层，竖直居中、左缘偏移）。
// 主题无强调条（HasAccentBar=false 或无颜色）时返回 nil，调用方据此不加 rail（沿用原内边距/无留白）。
// 替代旧的 item.Layers 覆盖层写法：强调条从此参与盒模型布局，内容自然排在其右，不再依赖左内边距兜位。
func (r *Renderer) buildAccentRail(railW int, selected bool, rowH int, scale float64) *View {
	rv := &r.resolvedViews
	if !r.config.HasAccentBar || rv.AccentBar.BgColor == nil {
		return nil
	}
	rail := &View{FixedW: railW, FixedH: rowH}
	if selected {
		barW := rv.AccentBarWidth.Scaled(scale)
		rail.Layers = []ImageLayer{{
			Color: rv.AccentBar.BgColor, Z: -1, Anchor: "left",
			OffsetX: rv.AccentBarOffset.Scaled(scale), W: barW,
			H: int(rv.ItemHeight*rv.AccentBarHRatio + 0.5), Radius: barW / 2,
		}}
	}
	return rail
}

// buildHorizontalCandidateTree 构建横排候选窗 View 树。
// candWindowTree 是构建结果：窗口根 + 命中测试所需的关键 View。
type candWindowTree struct {
	root      *View
	items     []*View // 与 candidates 一一对应
	pagerUp   *View   // nil = 无翻页上键 / 不可用
	pagerDown *View
}

// (x,y) 由调用方在 Layout 时给定；本函数只描述结构与样式。
func (r *Renderer) buildHorizontalCandidateTree(
	candidates []Candidate,
	input string,
	cursorPos int,
	page, totalPages, selectedIndex, hoverIndex int,
	hoverPageBtn string,
) *candWindowTree {
	cfg := &r.config
	scale := GetDPIScale()
	sc := func(v float64) int { return int(v*scale + 0.5) }
	// scD 按单位换算几何尺寸为设备像素：dp ×scale 四舍五入，px 原样（发丝线不随 DPI 加粗）。
	scD := func(d theme.Dimension) int { return d.Scaled(scale) }

	isTextIndex := cfg.IndexStyle == "text"
	isEmbedded := cfg.PreeditMode == config.PreeditEmbedded && !cfg.HidePreedit

	// 外观取值改走 ResolvedViews，经 scD 按单位换算为设备像素（dp 缩放 / px 不缩放）。
	rv := &r.resolvedViews
	padX := scD(rv.Window.PadLeft)
	padY := scD(rv.Window.PadTop)
	bgPadL := scD(rv.Item.PadLeft)
	bgPadR := scD(rv.Item.PadRight)
	indexMarginRight := scD(rv.Text.MarginLeft)
	commentMarginLeft := scD(rv.Comment.MarginLeft)

	itemSpacing := scD(rv.ItemSpacing) // 间距全由主题 metrics.item_spacing 决定（文本序号模式的旧 +4 magic 已下沉到 msime 主题）
	commentSize := rv.Index.FontSize
	if isTextIndex {
		commentSize = rv.Index.FontSize + 2*scale
	}
	if rv.Comment.FontSize > 0 { // P7-B：views.comment.font_size 显式则绝对覆盖派生值
		commentSize = rv.Comment.FontSize
	}
	indexSize := maxF(18*scale, rv.Index.FontSize+4*scale)
	rowH := int(rv.ItemHeight + 0.5)

	// ---- 候选项 ----
	items := make([]*View, 0, len(candidates))
	for i, cand := range candidates {
		children := make([]*View, 0, 3)

		// P7-D：选中/悬停态只重着色/加粗**候选文字**；序号、注释各用自身配色（独立，避免误伤蓝圆白数字序号）。
		st := itemStateFor(rv.Item, i == selectedIndex, i == hoverIndex)
		textColor, textWeight := rv.Text.TextColor, rv.Text.FontWeight
		if st != nil {
			if st.TextColor != nil {
				textColor = st.TextColor
			}
			if st.FontWeight != 0 {
				textWeight = st.FontWeight
			}
		}

		// 序号/注释各自的选中态（独立于候选文字；未配置=与普通态一致）
		sel, hov := i == selectedIndex, i == hoverIndex
		idxColor, idxWeight := elementTextState(rv.Index, sel, hov)
		cmtColor, cmtWeight := elementTextState(rv.Comment, sel, hov)

		if cand.Index >= 0 {
			label := indexLabel(r.effectiveIndexLabels(), cand.Index, cand.IndexLabel)
			if isTextIndex {
				children = append(children, &View{
					Text:      label,
					TextStyle: TextStyle{FontSize: rv.Index.FontSize, Weight: idxWeight, Family: rv.Index.FontFamily, Color: idxColor},
				})
			} else {
				d := int(indexSize + 0.5)
				children = append(children, &View{
					FixedW:     d,
					FixedH:     d,
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

		// 候选文字
		textChild := &View{
			Text:      candidateDisplayText(cand, cfg.CmdbarPrefix),
			TextStyle: TextStyle{FontSize: rv.Text.FontSize, Weight: textWeight, Family: rv.Text.FontFamily, Color: textColor},
		}
		if len(children) > 0 {
			textChild.Margin = Edges{Left: indexMarginRight}
		}
		children = append(children, textChild)

		// 注释
		if cand.Comment != "" {
			children = append(children, &View{
				Text:      cand.Comment,
				TextStyle: TextStyle{FontSize: commentSize, Weight: cmtWeight, Family: rv.Comment.FontFamily, Color: cmtColor},
				Margin:    Edges{Left: commentMarginLeft},
			})
		}

		// 强调条占位元素：rail 存在时占据原左内边距宽度（内容位置不变），并承载强调条；
		// 无强调条主题沿用左内边距。
		itemChildren := children
		itemPadLeft := bgPadL
		if rail := r.buildAccentRail(bgPadL, i == selectedIndex, rowH, scale); rail != nil {
			itemPadLeft = 0
			itemChildren = append([]*View{rail}, children...)
		}
		item := &View{
			Layout:     LayoutRow,
			CrossAlign: AlignCenter,
			Padding:    Edges{Left: itemPadLeft, Right: bgPadR},
			FixedH:     rowH,
			Border:     Border{Radius: rv.Item.BorderRadius.Scaled(scale)},
			Children:   itemChildren,
		}
		r.applyItemState(item, st, scale)             // P7-D：选中/悬停态背景（高亮位图/底色）+ 边框
		r.appendThemeLayers(item, rv.Item.Layers, sc) // P7-C：候选项装饰层（per-item 覆盖图）
		items = append(items, item)
	}

	// ---- 候选列表行：[内嵌预编辑?] + 候选项 + [翻页区?] ----
	pagerChildren, pagerUp, pagerDown := r.buildPager(scale, sc, isTextIndex, page, totalPages, hoverPageBtn, rowH)
	listChildren := make([]*View, 0, len(items)+4)
	if isEmbedded {
		if inline := r.buildEmbeddedPreedit(input, cursorPos, rowH, scale, sc); inline != nil {
			listChildren = append(listChildren, inline)
		}
	}
	listChildren = append(listChildren, items...)
	if len(pagerChildren) > 0 {
		pagerChildren[0].Margin = Edges{Left: sc(8 * scale)} // 与候选列表的分隔
		listChildren = append(listChildren, pagerChildren...)
	}

	// 候选框间隙：旧渲染器 effectiveSpacing=max(padL+padR, itemSpacing)，扣掉左右内边距后
	// 即相邻框之间的真实间隙（通常为 0，框相邻）。
	boxGap := maxInt(itemSpacing-bgPadL-bgPadR, 0)
	list := &View{
		Layout:     LayoutRow,
		CrossAlign: AlignCenter, // 页码文本/箭头按钮在行内垂直居中
		Gap:        boxGap,
		Children:   listChildren,
	}

	// ---- band 列表（preedit + 候选列表）----
	bands := make([]*View, 0, 2)
	if (input != "" || cfg.ModeLabel != "") && !cfg.HidePreedit && !isEmbedded {
		inputH := int(maxF(24*scale, rv.PreeditBar.FontSize*1.3) + 0.5)
		bands = append(bands, r.buildPreeditBand(input, cursorPos, inputH, scale, sc))
	}
	bands = append(bands, list)

	window := &View{
		Layout:     LayoutColumn,
		Gap:        rv.WindowGap.Scaled(scale),
		Padding:    Edges{Top: padY, Right: padX, Bottom: padY, Left: padX},
		Background: r.fillFor(rv.Window.BgColor, rv.Window.BgImage), // P7-C：背景图来自 views.window.background.image
		Border:     r.windowBorder(rv.Window.BorderRadius.Scaled(scale), sc, scale),
		Shadow:     &ViewShadow{OffsetX: rv.ShadowOffsetX.Scaled(scale), OffsetY: rv.ShadowOffsetY.Scaled(scale), Color: rv.ShadowColor},
		Children:   bands,
	}
	r.appendThemeLayers(window, rv.Window.Layers, sc) // P7-C：窗口装饰层（水印等）
	return &candWindowTree{root: window, items: items, pagerUp: pagerUp, pagerDown: pagerDown}
}

func pickF(primary, fallback float64) float64 {
	if primary > 0 {
		return primary
	}
	return fallback
}

func maxF(a, b float64) float64 {
	if a > b {
		return a
	}
	return b
}
