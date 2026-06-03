package theme

import (
	"image/color"
	"strings"
)

// resolveCandidateViewColor 解析候选窗 views 颜色字段：${name}→palette 候选窗语义色 /
// hex(#RRGGBB[AA]) 直解 / 空或未知 token → nil（调用方据此保留 palette 默认）。
// 从 ui 侧 resolveViewColor 下沉（P6 2b），token 映射表与之一致。
func resolveCandidateViewColor(s string, pal ResolvedPalette) color.Color {
	if s == "" {
		return nil
	}
	if s == "transparent" { // P0 ColorToken：全透明（位图皮肤让背景图透出用）
		return color.RGBA{0, 0, 0, 0}
	}
	cw := pal.CandidateWindow
	if strings.HasPrefix(s, "${") && strings.HasSuffix(s, "}") {
		switch s[2 : len(s)-1] {
		case "background":
			return cw.Background
		case "border":
			return cw.Border
		case "text":
			return cw.Text
		case "index_bg":
			return cw.IndexBg
		case "index_text":
			return cw.IndexText
		case "hover_bg":
			return cw.HoverBg
		case "selected_bg":
			return cw.SelectedBg
		case "preedit_bg":
			return cw.PreeditBg
		case "preedit_text":
			return cw.PreeditText
		case "comment":
			return cw.Comment
		case "accent":
			return cw.AccentBar
		case "shadow":
			return pal.Shadow
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
func resolveViewNode(n ViewNode, resolveColor func(string) color.Color, defBg, defBorder, defText color.Color) RVNode {
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
		im := toRVImage(*n.Background.Image)
		out.BgImage = &im
	}
	if len(n.Layers) > 0 {
		out.Layers = make([]RVImage, len(n.Layers))
		for i := range n.Layers {
			out.Layers[i] = toRVImage(n.Layers[i])
		}
	}
	return out
}

// ResolveCandidateViews 把候选窗 Views（已 merge defaultViews 基线，含 Metrics）+ palette
// 解析为渲染消费的 ResolvedViews（几何=逻辑像素、颜色=color.Color）。
// 颜色 = palette 默认 ⊕ views token 覆盖（views 颜色非空才覆盖）。
// 不设字号（Text/Index/PreeditBar.FontSize）、ItemHeight、VerticalMaxWidth——这些是运行时值，由 ui 回填。
func ResolveCandidateViews(views Views, pal ResolvedPalette) ResolvedViews {
	resolve := func(s string) color.Color { return resolveCandidateViewColor(s, pal) }
	build := func(n ViewNode, defBg, defBorder, defText color.Color) RVNode {
		return resolveViewNode(n, resolve, defBg, defBorder, defText)
	}
	cw := pal.CandidateWindow
	rv := ResolvedViews{
		Window:        build(views.Window, cw.Background, cw.Border, nil),
		PreeditBar:    build(views.PreeditBar, cw.PreeditBg, nil, cw.PreeditText),
		CandidateList: build(views.CandidateList, nil, nil, nil),
		Item:          build(views.Item, nil, nil, nil),
		Index:         build(views.Index, cw.IndexBg, nil, cw.IndexText),
		Text:          build(views.Text, nil, nil, cw.Text),
		Comment:       build(views.Comment, nil, nil, cw.Comment),
		AccentBar:     build(views.AccentBar, cw.AccentBar, nil, nil),
		FooterBar:     build(views.FooterBar, nil, nil, nil),
		ModeLabel:     build(views.ModeLabel, nil, nil, cw.Comment), // 模式徽标默认文字色 = 候选注释色

		ShadowColor: pal.Shadow,
	}
	// P7-D：item 三态解析为完整 patch。selected 默认 palette SelectedBg/SelectedText，
	// hover 默认 HoverBg（文字沿用基态），disabled 无 palette 默认（schema 预留）。
	rv.Item.Selected = resolveState(views.Item.Selected, cw.SelectedBg, cw.SelectedText, resolve)
	rv.Item.Hover = resolveState(views.Item.Hover, cw.HoverBg, nil, resolve)
	rv.Item.Disabled = resolveState(views.Item.Disabled, nil, nil, resolve)
	// P7-D：序号/注释也各自支持选中/悬停态（View 模型对称）。无 palette 默认 → 未配置即返回 nil，
	// 渲染沿用各元素基态（默认与普通态一致）；主题可用 views.index.selected / views.comment.selected 独立配。
	rv.Index.Selected = resolveState(views.Index.Selected, nil, nil, resolve)
	rv.Index.Hover = resolveState(views.Index.Hover, nil, nil, resolve)
	rv.Comment.Selected = resolveState(views.Comment.Selected, nil, nil, resolve)
	rv.Comment.Hover = resolveState(views.Comment.Hover, nil, nil, resolve)
	if m := views.Metrics; m != nil {
		rv.ItemSpacing = dimOr(m.ItemSpacing, Dimension{})
		rv.WindowGap = dimOr(m.BandGap, Dimension{})
		rv.ShadowOffset = dimOr(m.ShadowOffset, Dimension{})
		// P7-E：X/Y 默认 = 标量 shadow_offset；structured shadow 存在则覆盖（blur/spread 暂不消费）。
		rv.ShadowOffsetX, rv.ShadowOffsetY = rv.ShadowOffset, rv.ShadowOffset
		if m.Shadow != nil {
			if m.Shadow.OffsetX != nil {
				rv.ShadowOffsetX = *m.Shadow.OffsetX
			}
			if m.Shadow.OffsetY != nil {
				rv.ShadowOffsetY = *m.Shadow.OffsetY
			}
			if c := resolveCandidateViewColor(m.Shadow.Color, pal); c != nil {
				rv.ShadowColor = c
			}
		}
		if m.AccentBar != nil {
			rv.AccentBarWidth = dimOr(m.AccentBar.Width, Dimension{})
			rv.AccentBarOffset = dimOr(m.AccentBar.Offset, Dimension{})
			if m.AccentBar.HeightRatio != nil {
				rv.AccentBarHRatio = *m.AccentBar.HeightRatio
			}
		}
	}
	return rv
}

// resolveState 把状态 patch ViewNode（selected/hover/disabled）解析为 RVState（P7-D）。
// defBg/defText = 该态的 palette 默认底色/文字色（nil=无默认）；patch 提供对应字段才覆盖。
// 全空且无 palette 默认 → 返回 nil（该态无覆盖，渲染沿用基态）。
func resolveState(node *ViewNode, defBg, defText color.Color, resolveColor func(string) color.Color) *RVState {
	st := RVState{BgColor: defBg, TextColor: defText}
	has := defBg != nil || defText != nil
	if node != nil {
		if c := resolveColor(node.Background.Color); c != nil {
			st.BgColor = c
			has = true
		}
		if node.Background.Image != nil {
			im := toRVImage(*node.Background.Image)
			st.BgImage = &im
			has = true
		}
		if c := resolveColor(node.Color); c != nil {
			st.TextColor = c
			has = true
		}
		if c := resolveColor(node.Border.Color); c != nil {
			st.BorderColor = c
			has = true
		}
		if node.Border.Width != nil {
			w := *node.Border.Width
			st.BorderWidth = &w
			has = true
		}
		if node.FontWeight != nil {
			st.FontWeight = *node.FontWeight
			has = true
		}
	}
	if !has {
		return nil
	}
	return &st
}

// toRVImage 把 schema ViewImage 廉价转换为渲染消费形态 RVImage：
// slice 指针边距→plain Padding；opacity nil→1.0；其余字段直拷。不解码位图（ui 侧按 Ref 缓存）。
func toRVImage(im ViewImage) RVImage {
	op := 1.0
	if im.Opacity != nil {
		op = *im.Opacity
	}
	return RVImage{
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
		OffsetX: im.Offset.X,
		OffsetY: im.Offset.Y,
		W:       im.Size.W,
		H:       im.Size.H,
	}
}
