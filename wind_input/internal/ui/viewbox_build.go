package ui

// 从 RenderConfig + 候选数据构建候选窗的盒模型 View 树（P1）。
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

// buildModeLabelView 构建模式徽标 View（临时拼音等）：字体 + margin/padding 全取自 views.mode_label。
// padding 提供与输入编码/候选的分隔（默认左右 8，主题可调），消除重叠。
func (r *Renderer) buildModeLabelView(scale float64) *View {
	m := &r.resolvedViews.ModeLabel
	return &View{
		Text:      r.config.ModeLabel,
		TextStyle: TextStyle{FontSize: m.FontSize, Weight: m.FontWeight, Family: m.FontFamily, Color: m.TextColor},
		Margin:    Edges{Top: m.MarginTop.Scaled(scale), Right: m.MarginRight.Scaled(scale), Bottom: m.MarginBottom.Scaled(scale), Left: m.MarginLeft.Scaled(scale)},
		Padding:   Edges{Top: m.PadTop.Scaled(scale), Right: m.PadRight.Scaled(scale), Bottom: m.PadBottom.Scaled(scale), Left: m.PadLeft.Scaled(scale)},
	}
}

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
		children = append(children, r.buildModeLabelView(scale)) // margin/padding 取自 views.mode_label
	}
	inline := &View{
		Layout: LayoutRow, CrossAlign: AlignCenter, FixedH: rowH,
		Margin:   Edges{Right: sc(16)},
		Children: children,
	}
	if input != "" && cursorPos >= 0 && cursorPos <= len(input) {
		cw := measureText(r.textDrawer, input[:cursorPos], pbFS, r.resolvedViews.PreeditBar.FontFamily)
		inline.Layers = append(inline.Layers, ImageLayer{
			Color: r.resolvedViews.PreeditBar.TextColor, Z: 1, Anchor: "left",
			OffsetX: int(cw + 0.5), W: maxInt(1, sc(1.5)), H: int(float64(rowH) * 0.7),
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
			&View{Grow: true},           // 弹性占位把标签推到右侧
			r.buildModeLabelView(scale), // margin/padding 取自 views.mode_label（与输入编码分隔）
		)
	}
	band := &View{
		Layout: LayoutRow, CrossAlign: AlignCenter, Stretch: true, FixedH: inputH,
		Padding:    Edges{Left: pb.PadLeft.Scaled(scale), Right: pb.PadRight.Scaled(scale)},
		Background: r.fillFor(bgColor, pb.BgImage, pb.BgGradient), // P7-C：preedit 背景可带图/渐变
		Border:     Border{Radius: pb.BorderRadius.Scaled(scale)},
		Children:   children,
	}
	r.appendThemeLayers(band, pb.Layers, sc)
	if input != "" && cursorPos >= 0 && cursorPos <= len(input) {
		cw := measureText(r.textDrawer, input[:cursorPos], pbFS, pb.FontFamily)
		band.Layers = append(band.Layers, ImageLayer{
			Color: pb.TextColor, Z: 1, Anchor: "left",
			OffsetX: pb.PadLeft.Scaled(scale) + int(cw+0.5), W: maxInt(1, sc(1.5)), H: int(pbFS + 0.5),
		})
	}
	return band
}

// windowBorder 返回窗口边框：accent 模式(ModeAccentColor 非空)用更宽的 accent 色 glow 边框。
func (r *Renderer) windowBorder(radius int, sc func(float64) int, scale float64) Border {
	cfg := &r.config
	if cfg.ModeAccentColor != nil {
		return Border{Width: maxInt(1, sc(2.5)), Color: cfg.ModeAccentColor, Radius: radius}
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
func itemStateFor(item theme.RVNode, selected, hover bool) *theme.RVNode {
	if selected {
		return item.Selected
	}
	if hover {
		return item.Hover
	}
	return nil
}

// stateBg 取状态 patch 的底色（nil-safe）；用于不需要位图/边框的轻量场景（如翻页按钮 hover）。
func stateBg(st *theme.RVNode) color.Color {
	if st != nil {
		return st.BgColor
	}
	return nil
}

// effectiveNode 是候选元素状态的**统一**扁平器（架构统一核心）：把基态 RVNode ⊕ 当前激活
// 状态（selected 优先 hover）合并为一个有效节点，供 index/text/comment/item 同一套消费
// （背景/边框/文字色/字重/字体族）。状态非零字段覆盖基态；未配状态=基态（零回归）。
// 字号不随状态变（决策：避免选中/悬停改变行高的几何连锁）。
func effectiveNode(n theme.RVNode, selected, hover bool) theme.RVNode {
	st := itemStateFor(n, selected, hover)
	if st == nil {
		return n
	}
	out := n
	if st.BgColor != nil {
		out.BgColor = st.BgColor
	}
	if st.BgImage != nil {
		out.BgImage = st.BgImage
	}
	if st.BorderColor != nil {
		out.BorderColor = st.BorderColor
	}
	if st.BorderWidth != (theme.Dimension{}) {
		out.BorderWidth = st.BorderWidth
	}
	if st.BorderRadius != (theme.Dimension{}) {
		out.BorderRadius = st.BorderRadius
	}
	if st.TextColor != nil {
		out.TextColor = st.TextColor
	}
	if st.FontWeight != 0 {
		out.FontWeight = st.FontWeight
	}
	if st.FontFamily != "" {
		out.FontFamily = st.FontFamily
	}
	return out
}

// applyNodeBox 把有效节点的背景 + 边框应用到 View——**仅配置了才设**，未配=不动（纯文本/无框，零回归）。
// index/text/comment/item 统一经此上盒模型；padding 由各自构建处控制（item 行内边距含 rail 逻辑，不在此覆盖）。
func (r *Renderer) applyNodeBox(v *View, eff theme.RVNode, scale float64) {
	if eff.BgColor != nil || eff.BgImage != nil {
		v.Background = r.fillFor(eff.BgColor, eff.BgImage, eff.BgGradient) // 高亮位图优先于底色
	}
	if eff.BorderColor != nil || eff.BorderWidth != (theme.Dimension{}) || eff.BorderRadius != (theme.Dimension{}) {
		v.Border = Border{Color: eff.BorderColor, Width: eff.BorderWidth.Scaled(scale), Radius: eff.BorderRadius.Scaled(scale)}
	}
}

// styleLeaf 用有效节点构建带样式的文本叶子（字号/字重/字体族/色 + 背景/边框/padding 统一来自 eff）。
// 候选文字、注释、文本序号走它；序号圆圈因形状特殊单独构建但样式同源（effectiveNode）。
func (r *Renderer) styleLeaf(eff theme.RVNode, text string, scale float64, align Align, margin Edges) *View {
	v := &View{
		Text:      text,
		TextStyle: TextStyle{FontSize: eff.FontSize, Weight: eff.FontWeight, Family: eff.FontFamily, Color: eff.TextColor, Align: align},
		Margin:    margin,
	}
	r.applyNodeBox(v, eff, scale)
	return v
}

// buildIndexCircle 构建圆圈序号 View（样式统一来自有效节点 eff）：直径 d 的圆（圆角默认 d/2，
// eff.BorderRadius 非零则覆盖），背景/边框来自 eff，居中序号文字用 eff 字体/色。两 builder 共用。
func (r *Renderer) buildIndexCircle(eff theme.RVNode, label string, d int, scale float64) *View {
	radius := d / 2
	if eff.BorderRadius != (theme.Dimension{}) {
		radius = eff.BorderRadius.Scaled(scale)
	}
	return &View{
		FixedW:     d,
		FixedH:     d,
		Background: r.fillFor(eff.BgColor, eff.BgImage, eff.BgGradient),
		Border:     Border{Radius: radius, Color: eff.BorderColor, Width: eff.BorderWidth.Scaled(scale)},
		Layout:     LayoutStack,
		Children: []*View{{
			FixedW:    d,
			FixedH:    d,
			Text:      label,
			TextStyle: TextStyle{FontSize: eff.FontSize, Weight: eff.FontWeight, Family: eff.FontFamily, Color: eff.TextColor, Align: AlignCenter},
		}},
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
			H: int(float64(rowH)*rv.AccentBarHRatio + 0.5), Radius: barW / 2,
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
	bgPadL := scD(rv.Item.PadLeft)
	bgPadR := scD(rv.Item.PadRight)
	// rail 宽（横竖统一）：取 item 左内边距；有强调条且不足以容纳其几何（offset+width）时取下限。
	railW := bgPadL
	if cfg.HasAccentBar && rv.AccentBar.BgColor != nil {
		if minW := rv.AccentBarOffset.Scaled(scale) + rv.AccentBarWidth.Scaled(scale) + sc(2); railW < minW {
			railW = minW
		}
	}
	indexMarginRight := scD(rv.Text.MarginLeft)
	commentMarginLeft := scD(rv.Comment.MarginLeft)

	itemSpacing := scD(rv.ItemSpacing) // 间距全由主题 metrics.item_spacing 决定（文本序号模式的旧 +4 magic 已下沉到 msime 主题）
	commentSize := rv.Comment.FontSize // 注释字号 = base + views.comment.font_size 偏移（无派生魔法）
	// 圆圈序号直径 = 序号字号 + index 上下 padding（盒模型：背景=内容+padding；无 max(18) 下限魔法，全由主题 index.padding 控制）
	// 圆直径 = 字号 + max(上下padding和, 左右padding和)：四边 padding 都参与（取较大轴），保持正圆
	indexSize := rv.Index.FontSize + maxF(float64(scD(rv.Index.PadTop)+scD(rv.Index.PadBottom)), float64(scD(rv.Index.PadLeft)+scD(rv.Index.PadRight)))
	// 行高 = 行内容自然高(最高元素：候选文字/序号/注释) + item 上下内边距。
	// 全由主题 item.padding 控制（无 max(32, base*1.8) 派生魔法）；不想要高度就把 item 上下 padding 配 0。
	lineH := rv.Text.FontSize
	if commentSize > lineH {
		lineH = commentSize
	}
	if isTextIndex {
		if rv.Index.FontSize > lineH {
			lineH = rv.Index.FontSize
		}
	} else if indexSize > lineH {
		lineH = indexSize
	}
	rowH := int(lineH+0.5) + scD(rv.Item.PadTop) + scD(rv.Item.PadBottom)

	// ---- 候选项 ----
	items := make([]*View, 0, len(candidates))
	for i, cand := range candidates {
		children := make([]*View, 0, 3)

		// 架构统一（默认/选中/悬停 × 背景/边框/字体/颜色）：每个元素经 effectiveNode 取
		// 「基态 ⊕ 当前激活状态」的有效外观，再统一经 styleLeaf/applyNodeBox 上盒模型。
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
				children = append(children, r.styleLeaf(effIdxText, label, scale, AlignStart, Edges{}))
			} else {
				children = append(children, r.buildIndexCircle(effIdx, label, int(indexSize+0.5), scale))
			}
		}

		// 候选文字（颜色/字重来自 text 自身状态：选中默认 selection_text，与旧 item.selected 等价）。
		textMargin := Edges{}
		if len(children) > 0 {
			textMargin = Edges{Left: indexMarginRight}
		}
		children = append(children, r.styleLeaf(effText, candidateDisplayText(cand, cfg.CmdbarPrefix), scale, AlignStart, textMargin))

		// 注释
		if cand.Comment != "" {
			children = append(children, r.styleLeaf(effCmt, cand.Comment, scale, AlignStart, Edges{Left: commentMarginLeft}))
		}

		// 强调条占位元素：rail 存在时占据原左内边距宽度（内容位置不变），并承载强调条；
		// 无强调条主题沿用左内边距。
		itemChildren := children
		itemPadLeft := bgPadL
		if rail := r.buildAccentRail(railW, sel, rowH, scale); rail != nil {
			itemPadLeft = 0
			itemChildren = append([]*View{rail}, children...)
		}
		item := &View{
			Layout:     LayoutRow,
			CrossAlign: AlignCenter,
			// 上下 padding 作为真实内边距生效：内容带从 y+PadTop 起、高 lineH，PadBottom 留在下方。
			// 配合 FixedH=rowH(=lineH+PadTop+PadBottom)，对称 padding 与旧版逐像素一致；
			// 非对称时上下不再被均摊（修复"改上等于上下同时变"）。
			Padding:  Edges{Top: scD(rv.Item.PadTop), Right: bgPadR, Bottom: scD(rv.Item.PadBottom), Left: itemPadLeft},
			FixedH:   rowH,
			Children: itemChildren,
		}
		r.applyNodeBox(item, effItem, scale)          // 统一：item 行背景 + 边框（含选中/悬停态）
		r.appendThemeLayers(item, rv.Item.Layers, sc) // P7-C：候选项装饰层（per-item 覆盖图）
		items = append(items, item)
	}

	// ---- 候选列表行：[内嵌预编辑?] + 候选项 + [翻页区?] ----
	pagerChildren, pagerUp, pagerDown := r.buildPager(scale, sc, page, totalPages, hoverPageBtn, rowH)
	listChildren := make([]*View, 0, len(items)+4)
	if isEmbedded {
		if inline := r.buildEmbeddedPreedit(input, cursorPos, rowH, scale, sc); inline != nil {
			listChildren = append(listChildren, inline)
		}
	}
	listChildren = append(listChildren, items...)
	if len(pagerChildren) > 0 {
		pagerChildren[0].Margin = Edges{Left: sc(8)} // 与候选列表的分隔
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
		inputH := int(rv.PreeditBar.FontSize+0.5) + scD(rv.PreeditBar.PadTop) + scD(rv.PreeditBar.PadBottom) // 条高=内容+preedit 上下 padding（无 max 魔法）
		bands = append(bands, r.buildPreeditBand(input, cursorPos, inputH, scale, sc))
	}
	bands = append(bands, list)

	window := &View{
		Layout:     LayoutColumn,
		Gap:        rv.WindowGap.Scaled(scale),
		Padding:    Edges{Top: scD(rv.Window.PadTop), Right: scD(rv.Window.PadRight), Bottom: scD(rv.Window.PadBottom), Left: scD(rv.Window.PadLeft)}, // 完整遵循主题 window.padding 四边
		Background: r.fillFor(rv.Window.BgColor, rv.Window.BgImage, rv.Window.BgGradient),                                                             // P7-C：背景图来自 views.window.background.image
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
