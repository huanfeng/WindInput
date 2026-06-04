package theme

import (
	"image/color"
)

// other_views.go — P8：其它窗口（status/tooltip/menu/toolbar/toast）的盒模型 View 解析。
// 复用候选窗的通用 resolveViewNode（ViewNode→RVNode）+ resolveState + toRVImage（candidate_views.go），
// 各窗口仅注入自己的 palette 语义色表（makeColorResolver 的 tokenMap）。
// 几何/border/font/颜色由各 ResolveXxxViews 解析；background image/layers 经 RVNode.BgImage/Layers 承载，
// 由 ui 侧共享 imageResolver 消费（P8 切片6）。

// tk 取 pal.Tokens[name]（缺失返回 nil），各窗口默认色/兜底用。
func tk(pal ResolvedPalette, name string) color.Color {
	if c, ok := pal.Tokens[name]; ok {
		return c
	}
	return nil
}

// ResolveStatusViews 解析 views.status 节点为渲染消费的 RVNode（P8 切片1）。
// 几何（margin/padding/border/font 偏移）来自 ViewNode；
// 颜色 token 统一查 pal.Tokens（${status_bg}/${status_text}/${accent}…）；默认底色/文字 = status_bg/status_text。
// node==nil（主题未配 views.status）时返回纯默认色 + 零几何，由 ui 侧按现状兜底 padding/radius。
func ResolveStatusViews(node *ViewNode, pal ResolvedPalette) RVNode {
	resolve := func(s string) color.Color { return resolveColorToken(s, pal) }
	var n ViewNode
	if node != nil {
		n = *node
	}
	return resolveViewNode(n, resolve, tk(pal, "status_bg"), nil, tk(pal, "status_text"))
}

// ResolveTooltipViews 解析 views.tooltip 节点为渲染消费的 RVNode（P8 切片2）。
// 颜色 token 查 pal.Tokens（${tooltip_bg}/${tooltip_text}…）；默认底色/文字 = tooltip_bg/tooltip_text。
func ResolveTooltipViews(node *ViewNode, pal ResolvedPalette) RVNode {
	resolve := func(s string) color.Color { return resolveColorToken(s, pal) }
	var n ViewNode
	if node != nil {
		n = *node
	}
	return resolveViewNode(n, resolve, tk(pal, "tooltip_bg"), nil, tk(pal, "tooltip_text"))
}

// ResolveToastViews 解析 views.toast 节点为渲染消费的 RVNode（P8 切片5）。
// 颜色 token 查 pal.Tokens（${toast_bg}/${toast_text}…）；默认底色/文字 = toast_bg/toast_text。
func ResolveToastViews(node *ViewNode, pal ResolvedPalette) RVNode {
	resolve := func(s string) color.Color { return resolveColorToken(s, pal) }
	var n ViewNode
	if node != nil {
		n = *node
	}
	return resolveViewNode(n, resolve, tk(pal, "toast_bg"), nil, tk(pal, "toast_text"))
}

// ResolveMenuViews 解析 views.menu（Root/Item/Separator）为渲染消费的 ResolvedMenuViews（P8 切片3）。
// 颜色 token 查 pal.Tokens；item 的 hover/disabled 走 ViewNode states patch
// （hover 默认 menu_hover_bg/menu_hover_text、disabled 默认文字 menu_disabled）。
// mv==nil（主题未配 views.menu）时各节点取默认色 + 零几何，由 ui 侧按现状兜底布局尺寸。
func ResolveMenuViews(mv *MenuViews, pal ResolvedPalette) ResolvedMenuViews {
	resolve := func(s string) color.Color { return resolveColorToken(s, pal) }
	var root, item, sep ViewNode
	if mv != nil {
		root, item, sep = mv.Root, mv.Item, mv.Separator
	}
	out := ResolvedMenuViews{
		Root:      resolveViewNode(root, resolve, tk(pal, "menu_bg"), tk(pal, "menu_border"), nil),
		Item:      resolveViewNode(item, resolve, nil, nil, tk(pal, "menu_text")),
		Separator: resolveViewNode(sep, resolve, tk(pal, "menu_separator"), nil, nil),
	}
	out.Item.Hover = resolveState(item.Hover, tk(pal, "menu_hover_bg"), tk(pal, "menu_hover_text"), resolve)
	out.Item.Disabled = resolveState(item.Disabled, nil, tk(pal, "menu_disabled"), resolve)
	return out
}
