//go:build windows

package ui

// viewbox_status.go — 状态泡（mode indicator bubble）的 View 树构建与颜色解析（P4-A）。
// 仅 Win：依赖 Windows 专属 StatusRenderer（darwin 状态泡走原生 Swift）。
// 状态泡复用包级 Layout/PaintTree 引擎核心；颜色经 token 解析自 views.status，
// 默认映射 Palette.Status（与 Toast/Tooltip 统一深灰底白字），运行时 StatusWindowConfig 自定义色优先。

import (
	"image/color"
	"strings"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// resolveTokenColor 通用颜色 token 解析：hex(#RRGGBB[AA]) 直解；${name} 交给 resolver；
// 空 / 未知 token / 解析失败返回 nil（调用方据此回退）。各窗口注入自己的 resolver。
func resolveTokenColor(s string, resolver func(name string) color.Color) color.Color {
	if s == "" {
		return nil
	}
	if strings.HasPrefix(s, "${") && strings.HasSuffix(s, "}") {
		return resolver(s[2 : len(s)-1])
	}
	if c, err := theme.ParseHexColor(s); err == nil {
		return c
	}
	return nil
}

// resolveStatusNode 计算状态泡盒模型 RVNode（P8 切片1：几何+border+font+颜色）。
// 几何（padding/margin/border/font 偏移）来自 views.status；
// 颜色优先级：自定义 cfg > views.status token > Palette.Status 默认。
func (r *StatusRenderer) resolveStatusNode(cfg StatusWindowConfig) theme.RVNode {
	// 无主题兜底：深灰底白字（与 P5-6 现状一致）；几何零值由 buildStatusTree 兜底为现状 hardcode。
	node := theme.RVNode{
		BgColor:   color.RGBA{60, 60, 60, 240},
		TextColor: color.RGBA{255, 255, 255, 255},
	}
	if rv := r.resolvedV3; rv != nil {
		var sn *theme.ViewNode
		if rv.Views != nil {
			sn = rv.Views.Status
		}
		node = theme.ResolveStatusViews(sn, rv.Palette)
	}

	// 自定义 cfg 颜色优先级最高
	if cfg.BackgroundColor != "" {
		if c, ok := parseHexColor(cfg.BackgroundColor); ok {
			node.BgColor = c
		}
	}
	if cfg.TextColor != "" {
		if c, ok := parseHexColor(cfg.TextColor); ok {
			node.TextColor = c
		}
	}
	return node
}

// buildStatusTree 构建状态泡 View 树（单文本节点：完整盒模型 padding/border/font + 居中文本）。
// node 携带主题几何+颜色+border+font（已叠加运行时 cfg 颜色覆盖）。
// fallbackPad/fallbackRadius 为现状兜底（逻辑像素）：node 未配 padding（四向皆 0）→ 兜底 fallbackPad；
// node 未配 radius（0）→ 兜底 fallbackRadius。字号 = (fontSize + node.FontSize 偏移) × scale。
// minWidth=32（逻辑）经 FixedW 钳制。
func buildStatusTree(text string, node theme.RVNode, fontSize, fallbackPad, fallbackRadius, scale float64, m TextMeasurer, ir *imageResolver, resources map[string]string) *View {
	fs := (fontSize + node.FontSize) * scale
	padT := node.PadTop.Scaled(scale)
	padR := node.PadRight.Scaled(scale)
	padB := node.PadBottom.Scaled(scale)
	padL := node.PadLeft.Scaled(scale)
	if padT == 0 && padR == 0 && padB == 0 && padL == 0 {
		p := int(fallbackPad * scale)
		padT, padR, padB, padL = p, p, p, p
	}
	radius := node.BorderRadius.Scaled(scale)
	if radius == 0 {
		radius = int(fallbackRadius * scale)
	}
	minW := int(32.0 * scale)

	tw := measureText(m, text, fs, node.FontFamily)
	w := max(int(tw)+padL+padR, minW)

	border := Border{Radius: radius}
	if node.BorderColor != nil {
		border.Color = node.BorderColor
		border.Width = node.BorderWidth.Scaled(scale)
		if border.Width == 0 {
			border.Width = int(1.0 * scale) // 配了边框色但未配宽 → 1px 发丝线
		}
	}

	root := &View{
		Text:       text,
		TextStyle:  TextStyle{FontSize: fs, Color: node.TextColor, Align: AlignCenter, Weight: node.FontWeight, Family: node.FontFamily},
		Padding:    Edges{Top: padT, Right: padR, Bottom: padB, Left: padL},
		Background: ir.fillFor(node.BgColor, node.BgImage, node.BgGradient, resources, scale), // P8 切片6：背景可带图/渐变
		Border:     border,
		FixedW:     w,
	}
	ir.appendLayers(root, node.Layers, resources, func(v float64) int { return int(v * scale) }) // P8 切片6：状态泡装饰层
	return root
}
