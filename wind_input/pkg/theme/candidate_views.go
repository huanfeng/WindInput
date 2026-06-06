package theme

import (
	"image/color"
	"strings"
)

// resolveColorToken 是 v3 统一的 views 颜色字段解析器（候选窗 + 其它窗口共用）：
//   - 空串 → nil（调用方据此保留默认）
//   - "transparent" → 全透明（位图皮肤让背景透出用，P0 ColorToken）
//   - "${name}" → pal.Tokens[name]（缺失返回 nil）
//   - "#RRGGBB[AA]" → 直解
//   - 其余/未知 → nil
func resolveColorToken(s string, pal ResolvedPalette) color.Color {
	if s == "" {
		return nil
	}
	if s == "transparent" {
		return color.RGBA{0, 0, 0, 0}
	}
	if strings.HasPrefix(s, "${") && strings.HasSuffix(s, "}") {
		if c, ok := pal.Tokens[s[2:len(s)-1]]; ok {
			return c
		}
		return nil
	}
	if c, err := ParseHexColor(s); err == nil {
		return c
	}
	return nil
}

// resolveViewNode 通用「ViewNode → RVNode」解析器（P8 切片0）：把单个盒模型 ViewNode 解析为
// 渲染消费形态。窗口无关——颜色 token 由 resolveColor 注入（候选窗=resolveCandidateViewColor(s,pal)，
// 其它窗口注入各自的 palette 语义色表）；defBg/defBorder/defText = 该节点 palette 默认色（nil=无默认）。
// 几何（margin/padding/border）逻辑像素直拷；颜色 = 默认 ⊕ token 覆盖（token 解析非 nil 才覆盖）；
// FontSize 存「相对主字号的有符号偏移」（0/未写=同主字体），由 ui 侧换算；
// 字重 0=继承全局；字体族名空=继承全局（未知名由平台文本引擎回退）；背景图/layers 转 RVImage spec（不解码位图）。
func resolveViewNode(n ViewNode, resolveColor func(ColorRef) color.Color, defBg, defBorder, defText color.Color) RVNode {
	out := RVNode{
		MarginTop:    dimOr(n.Margin.Top, Dimension{}),
		MarginRight:  dimOr(n.Margin.Right, Dimension{}),
		MarginBottom: dimOr(n.Margin.Bottom, Dimension{}),
		MarginLeft:   dimOr(n.Margin.Left, Dimension{}),
		PadTop:       dimOr(n.Padding.Top, Dimension{}),
		PadRight:     dimOr(n.Padding.Right, Dimension{}),
		PadBottom:    dimOr(n.Padding.Bottom, Dimension{}),
		PadLeft:      dimOr(n.Padding.Left, Dimension{}),
		BorderRadius: dimOr(n.Border.Radius, Dimension{}),
		BorderWidth:  dimOr(n.Border.Width, Dimension{}),
		BgColor:      defBg,
		BorderColor:  defBorder,
		TextColor:    defText,
		FontSize:     float64(edgeOr(n.FontSize, 0)),
		FontWeight:   edgeOr(n.FontWeight, 0),
		FontFamily:   n.FontFamily,
		LineSpacing:  dimOr(n.LineSpacing, Dimension{}),
		ColGap:       dimOr(n.ColGap, Dimension{}),
		TitleGap:     dimOr(n.TitleGap, Dimension{}),
	}
	if c := resolveColor(n.Background.Color); c != nil {
		out.BgColor = c
	}
	if c := resolveColor(n.Border.Color); c != nil {
		out.BorderColor = c
	}
	if c := resolveColor(n.Color); c != nil {
		out.TextColor = c
	}
	// P7-C：背景填充图 + 层级覆盖图 spec（不解码位图，ui 侧按 Ref 缓存解码）。
	if n.Background.Image != nil {
		im := toRVImage(*n.Background.Image, resolveColor)
		out.BgImage = &im
	}
	// P7-E 落地：背景渐变 spec（stop 颜色解析 + 按 Pos 排序）。
	if g := n.Background.Gradient; g != nil && len(g.Stops) > 0 {
		out.BgGradient = resolveGradient(g, resolveColor)
	}
	if len(n.Layers) > 0 {
		out.Layers = make([]RVImage, len(n.Layers))
		for i := range n.Layers {
			out.Layers[i] = toRVImage(n.Layers[i], resolveColor)
		}
	}
	// 仅 footer_bar 消费：上/下翻页箭头图与字符（其它节点不写即 nil/空）。
	if n.PrevImage != nil {
		im := toRVImage(*n.PrevImage, resolveColor)
		out.PrevImage = &im
	}
	if n.NextImage != nil {
		im := toRVImage(*n.NextImage, resolveColor)
		out.NextImage = &im
	}
	if n.PrevChar != nil {
		out.PrevChar = *n.PrevChar
	}
	if n.NextChar != nil {
		out.NextChar = *n.NextChar
	}
	return out
}

// ResolveCandidateViews 把候选窗 Views（已 merge defaultViews 基线，含 Metrics）+ palette
// 解析为渲染消费的 ResolvedViews（几何=逻辑像素、颜色=color.Color）。
// 颜色 = palette 默认 ⊕ views token 覆盖（views 颜色非空才覆盖）。
// 不设字号（Text/Index/PreeditBar.FontSize）、ItemHeight、VerticalMaxWidth——这些是运行时值，由 ui 回填。
func ResolveCandidateViews(views Views, pal ResolvedPalette) ResolvedViews {
	resolve := func(c ColorRef) color.Color { return resolveColorToken(c.Select(pal.IsDark), pal) }
	build := func(n ViewNode, defBg, defBorder, defText color.Color) RVNode {
		return resolveViewNode(n, resolve, defBg, defBorder, defText)
	}
	// v3：候选窗默认色从扁平语义 token 取（替代旧 candidate_window 组）。
	// 未配置的 token → nil（无默认），由 views 节点 token 覆盖。
	tk := func(name string) color.Color {
		if c, ok := pal.Tokens[name]; ok {
			return c
		}
		return nil
	}
	rv := ResolvedViews{
		Window:        build(views.Window, tk("bg"), tk("border"), nil),
		PreeditBar:    build(views.PreeditBar, tk("surface"), nil, tk("text_dim")),
		CandidateList: build(views.CandidateList, nil, nil, nil),
		Item:          build(views.Item, nil, nil, nil),
		Index:         build(views.Index, tk("accent"), nil, tk("on_accent")),
		Text:          build(views.Text, nil, nil, tk("text")),
		Comment:       build(views.Comment, nil, nil, tk("text_hint")),
		AccentBar:     build(views.AccentBar, tk("accent"), nil, nil),
		FooterBar:     build(views.FooterBar, nil, nil, nil),
		ModeLabel:     build(views.ModeLabel, nil, nil, tk("text_hint")), // 模式徽标默认文字色 = 候选注释色

		ShadowColor: pal.Shadow,
	}
	// P7-D：item 三态解析为完整 patch。selected 默认 selection/selection_text，
	// hover 默认 hover（文字沿用基态），disabled 无默认（schema 预留）。
	rv.Item.Selected = resolveState(views.Item.Selected, tk("selection"), tk("selection_text"), resolve)
	rv.Item.Hover = resolveState(views.Item.Hover, tk("hover"), nil, resolve)
	rv.Item.Disabled = resolveState(views.Item.Disabled, nil, nil, resolve)
	// 架构统一：候选文字/序号/注释各自支持选中/悬停态（View 模型对称，渲染统一经 effectiveNode 消费）。
	// 候选文字选中态默认文字色 = selection_text（与旧 item.selected 着色候选文字等价，零回归；消除
	// item→text 颜色耦合）；悬停态无默认。序号/注释无 palette 默认 → 未配即 nil、沿用基态。
	rv.Text.Selected = resolveState(views.Text.Selected, nil, tk("selection_text"), resolve)
	rv.Text.Hover = resolveState(views.Text.Hover, nil, nil, resolve)
	rv.Index.Selected = resolveState(views.Index.Selected, nil, nil, resolve)
	rv.Index.Hover = resolveState(views.Index.Hover, nil, nil, resolve)
	rv.Comment.Selected = resolveState(views.Comment.Selected, nil, nil, resolve)
	rv.Comment.Hover = resolveState(views.Comment.Hover, nil, nil, resolve)
	// V3-D 属性归位：列表级几何从对应节点读取（取代退役的 views.Metrics）。
	// candidate_list 容器：候选项间距 / band 间距。
	rv.ItemSpacing = dimOr(views.CandidateList.Gap, Dimension{})
	rv.WindowGap = dimOr(views.CandidateList.BandGap, Dimension{})
	rv.RowGap = dimOr(views.CandidateList.RowGap, Dimension{})
	// window 节点：投影偏移/颜色（offset_x/offset_y/color）。标量 ShadowOffset = X（X/Y 默认同值）。
	if sh := views.Window.Shadow; sh != nil {
		rv.ShadowOffsetX = dimOr(sh.OffsetX, Dimension{})
		rv.ShadowOffsetY = dimOr(sh.OffsetY, Dimension{})
		rv.ShadowOffset = rv.ShadowOffsetX
		if c := resolveColorToken(sh.Color.Select(pal.IsDark), pal); c != nil {
			rv.ShadowColor = c
		}
	}
	// accent_bar 节点：强调条几何（width/offset/height_ratio）。
	rv.AccentBarWidth = dimOr(views.AccentBar.Width, Dimension{})
	rv.AccentBarOffset = dimOr(views.AccentBar.Offset, Dimension{})
	if views.AccentBar.HeightRatio != nil {
		rv.AccentBarHRatio = *views.AccentBar.HeightRatio
	}
	return rv
}

// resolveState 把状态 patch ViewNode（selected/hover/disabled）解析为递归 RVNode（V3-D 决策 9）。
// 复用 resolveViewNode 做完整 ViewNode→RVNode 解析（含几何/边框/字体），再注入该态的 palette
// 默认底色/文字色（defBg/defText，nil=无默认）。全空且无 palette 默认 → 返回 nil（该态无覆盖）。
//
// nil-gating：仅当 patch 显式提供了 bg/bgImage/渐变/层/text/border 色/border 宽/字重，或存在
// palette 默认色时，才视为「有覆盖」并返回非 nil。
//
// 有意不看几何：padding/margin/font_size 不计入"有无覆盖"判定——状态态几何**刻意不渲染**
// （状态改几何会牵动行高/列宽致候选框跳动，capability `state_geometry`=unsupported）。
// 即只改 padding 的 selected 态会被视为空 patch 而丢弃；但只改渐变/层的会保留（已支持）。
func resolveState(node *ViewNode, defBg, defText color.Color, resolveColor func(ColorRef) color.Color) *RVNode {
	has := defBg != nil || defText != nil
	if node != nil {
		if resolveColor(node.Background.Color) != nil || node.Background.Image != nil ||
			(node.Background.Gradient != nil && len(node.Background.Gradient.Stops) > 0) || len(node.Layers) > 0 ||
			resolveColor(node.Color) != nil || resolveColor(node.Border.Color) != nil ||
			node.Border.Width != nil || node.FontWeight != nil {
			has = true
		}
	}
	if !has {
		return nil
	}
	var n ViewNode
	if node != nil {
		n = *node
	}
	rv := resolveViewNode(n, resolveColor, defBg, nil, defText)
	return &rv
}

// toRVImage 把 schema ViewImage 廉价转换为渲染消费形态 RVImage：
// slice 指针边距→plain Padding；opacity nil→1.0；其余字段直拷。不解码位图（ui 侧按 Ref 缓存）。
func toRVImage(im ViewImage, resolveColor func(ColorRef) color.Color) RVImage {
	op := 1.0
	if im.Opacity != nil {
		op = *im.Opacity
	}
	out := RVImage{
		Ref:  im.Ref,
		Mode: im.Mode,
		Slice: Padding{
			Top:    dimOr(im.Slice.Top, Dimension{}).Value,
			Right:  dimOr(im.Slice.Right, Dimension{}).Value,
			Bottom: dimOr(im.Slice.Bottom, Dimension{}).Value,
			Left:   dimOr(im.Slice.Left, Dimension{}).Value,
		},
		Opacity: op,
		Z:       im.Z,
		Anchor:  im.Anchor,
		W:       im.Size.W,
		H:       im.Size.H,
	}
	out.OffsetX, out.OffsetXPct = im.Offset.X.Split() // dp 或百分比分流（百分比 paint 阶段相对 host 换算）
	out.OffsetY, out.OffsetYPct = im.Offset.Y.Split()
	if resolveColor != nil {
		if !im.Tint.IsZero() {
			out.TintColor = resolveColor(im.Tint)
		}
		if !im.DisabledTint.IsZero() {
			out.DisabledTintColor = resolveColor(im.DisabledTint)
		}
	}
	return out
}
