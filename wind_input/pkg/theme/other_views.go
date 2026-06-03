package theme

import (
	"image/color"
	"strings"
)

// other_views.go — P8：其它窗口（status/tooltip/menu/toolbar/toast）的盒模型 View 解析。
// 复用候选窗的通用 resolveViewNode（ViewNode→RVNode）+ resolveState + toRVImage（candidate_views.go），
// 各窗口仅注入自己的 palette 语义色表（makeColorResolver 的 tokenMap）。
// 几何/border/font/颜色由各 ResolveXxxViews 解析；background image/layers 经 RVNode.BgImage/Layers 承载，
// 由 ui 侧共享 imageResolver 消费（P8 切片6）。

// makeColorResolver 构造一个 ViewNode 颜色字段解析闭包：
//   - 空串 → nil（调用方据此保留默认）
//   - "transparent" → 全透明（位图皮肤让背景透出用，P0 ColorToken）
//   - "${name}" → tokenMap(name)（各窗口注入自己的语义名→palette 组件色映射）
//   - "#RRGGBB[AA]" → 直解
//   - 其余/未知 → nil
//
// 与候选窗 resolveCandidateViewColor 语义一致，差异仅在 token 表由各窗口注入。
func makeColorResolver(tokenMap func(name string) color.Color) func(string) color.Color {
	return func(s string) color.Color {
		switch {
		case s == "":
			return nil
		case s == "transparent":
			return color.RGBA{0, 0, 0, 0}
		case strings.HasPrefix(s, "${") && strings.HasSuffix(s, "}"):
			return tokenMap(s[2 : len(s)-1])
		}
		if c, err := ParseHexColor(s); err == nil {
			return c
		}
		return nil
	}
}

// ResolveStatusViews 解析 views.status 节点为渲染消费的 RVNode（P8 切片1）。
// 几何（margin/padding/border/font 偏移）来自 ViewNode；
// 颜色 token：${background}/${text} → Palette.Status；默认底色/文字 = Palette.Status（无 views 覆盖时）。
// node==nil（主题未配 views.status）时返回纯默认色 + 零几何，由 ui 侧按现状兜底 padding/radius。
// background image/layers 由 ui 侧 imageResolver 消费（P8 切片6：fillFor 装配 BgImage、appendLayers 追加 Layers）。
func ResolveStatusViews(node *ViewNode, pal ResolvedPalette) RVNode {
	resolve := makeColorResolver(func(name string) color.Color {
		switch name {
		case "background":
			return pal.Status.Background
		case "text":
			return pal.Status.Text
		}
		return nil
	})
	var n ViewNode
	if node != nil {
		n = *node
	}
	return resolveViewNode(n, resolve, pal.Status.Background, nil, pal.Status.Text)
}

// ResolveTooltipViews 解析 views.tooltip 节点为渲染消费的 RVNode（P8 切片2）。
// 几何（margin/padding/border/font 偏移）来自 ViewNode；
// 颜色 token：${background}/${text} → Palette.Tooltip；默认底色/文字 = Palette.Tooltip。
// node==nil（主题未配 views.tooltip）时返回纯默认色 + 零几何，由 ui 侧按现状兜底 padding/radius。
// background image/layers 由 ui 侧 imageResolver 消费（P8 切片6：fillFor 装配 BgImage、appendLayers 追加 Layers）。
func ResolveTooltipViews(node *ViewNode, pal ResolvedPalette) RVNode {
	resolve := makeColorResolver(func(name string) color.Color {
		switch name {
		case "background":
			return pal.Tooltip.Background
		case "text":
			return pal.Tooltip.Text
		}
		return nil
	})
	var n ViewNode
	if node != nil {
		n = *node
	}
	return resolveViewNode(n, resolve, pal.Tooltip.Background, nil, pal.Tooltip.Text)
}

// ResolveToastViews 解析 views.toast 节点为渲染消费的 RVNode（P8 切片5）。
// 颜色 token：${background}/${text} → Palette.Toast；默认底色/文字 = Palette.Toast。
// 几何（padding/border 圆角/字号偏移）由 ui 侧按现状兜底；bg 不透明化在 ui 侧 forceAlphaOpaque。
// background image/layers 由 ui 侧 imageResolver 消费（P8 切片6：fillFor 装配 BgImage、appendLayers 追加 Layers）。
func ResolveToastViews(node *ViewNode, pal ResolvedPalette) RVNode {
	resolve := makeColorResolver(func(name string) color.Color {
		switch name {
		case "background":
			return pal.Toast.Background
		case "text":
			return pal.Toast.Text
		}
		return nil
	})
	var n ViewNode
	if node != nil {
		n = *node
	}
	return resolveViewNode(n, resolve, pal.Toast.Background, nil, pal.Toast.Text)
}

// ResolveMenuViews 解析 views.menu（Root/Item/Separator）为渲染消费的 ResolvedMenuViews（P8 切片3）。
// 颜色 token → Palette.PopupMenu 语义色；item 的 hover/disabled 走 ViewNode states patch
// （hover 默认 HoverBg/HoverText、disabled 默认文字 Disabled）。
// mv==nil（主题未配 views.menu）时各节点取 palette 默认色 + 零几何，由 ui 侧按现状兜底布局尺寸。
// background image/layers 由 ui 侧 imageResolver 消费（P8 切片6：fillFor 装配 BgImage、appendLayers 追加 Layers）。
func ResolveMenuViews(mv *MenuViews, pal ResolvedPalette) ResolvedMenuViews {
	pm := pal.PopupMenu
	resolve := makeColorResolver(func(name string) color.Color {
		switch name {
		case "background":
			return pm.Background
		case "border":
			return pm.Border
		case "text":
			return pm.Text
		case "disabled":
			return pm.Disabled
		case "hover_bg":
			return pm.HoverBg
		case "hover_text":
			return pm.HoverText
		case "separator":
			return pm.Separator
		}
		return nil
	})
	var root, item, sep ViewNode
	if mv != nil {
		root, item, sep = mv.Root, mv.Item, mv.Separator
	}
	out := ResolvedMenuViews{
		Root:      resolveViewNode(root, resolve, pm.Background, pm.Border, nil),
		Item:      resolveViewNode(item, resolve, nil, nil, pm.Text),
		Separator: resolveViewNode(sep, resolve, pm.Separator, nil, nil),
	}
	out.Item.Hover = resolveState(item.Hover, pm.HoverBg, pm.HoverText, resolve)
	out.Item.Disabled = resolveState(item.Disabled, nil, pm.Disabled, resolve)
	return out
}
