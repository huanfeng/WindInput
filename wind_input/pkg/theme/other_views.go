package theme

import (
	"image/color"
	"strings"
)

// other_views.go — P8：其它窗口（status/tooltip/menu/toolbar/toast）的盒模型 View 解析。
// 复用候选窗的通用 resolveViewNode（ViewNode→RVNode）+ resolveState + toRVImage（candidate_views.go），
// 各窗口仅注入自己的 palette 语义色表（makeColorResolver 的 tokenMap）。
// 几何/border/font/颜色由各 ResolveXxxViews 解析；background image/layers 待 P8 切片6（共享位图基础设施）接入。

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
// background image/layers 本切片不消费（待 P8 切片6 共享位图基础设施）。
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
// background image/layers 本切片不消费（待 P8 切片6 共享位图基础设施）。
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
