//go:build windows

package ui

// viewbox_tooltip.go — Tooltip（候选编码提示）的 View 树构建与颜色解析（P4-B）。
// 仅 Win：依赖 Windows 专属 TooltipWindow（darwin Tooltip 走原生 Swift）。
// 复用包级 Layout/PaintTree + newSharedDrawContext；颜色经 token 解析自 views.tooltip，
// 默认映射 Palette.Tooltip。多行 / \t 列对齐 / 行截断 / 行数上限逻辑在 build 内预处理。

import (
	"image/color"
	"strings"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// resolveTooltipNode 计算 Tooltip 盒模型 RVNode（P8 切片2：几何+border+font+颜色）。
// 几何（padding/margin/border/font 偏移）来自 views.tooltip；
// 颜色：views.tooltip token > Palette.Tooltip > 默认（Tooltip 无运行时 cfg 覆盖）。
func (w *TooltipWindow) resolveTooltipNode() theme.RVNode {
	node := theme.RVNode{
		BgColor:   color.RGBA{60, 60, 60, 240},
		TextColor: color.RGBA{255, 255, 255, 255},
	}
	if rv := w.resolvedV25; rv != nil {
		var tn *theme.ViewNode
		if rv.Views != nil {
			tn = rv.Views.Tooltip
		}
		node = theme.ResolveTooltipViews(tn, rv.Palette)
	}
	return node
}

// buildTooltipTree 构建 Tooltip View 树（LayoutColumn 行 + 多列 LayoutRow cell）。
// node 携带主题几何+颜色+border+font（来自 views.tooltip）。padding 四向未配（皆 0）兜底 6、
// radius 未配兜底 4（逻辑像素 × scale）；字号 = (14 + node.FontSize 偏移) × scale；字重/字体族随 node。
// 多列间距（lineSpacing 2 / colGap 16）保留 hardcode（非单节点盒模型字段）。maxContentWidth<=0 不限宽，无行返回 nil。
func buildTooltipTree(text string, maxContentWidth float64, node theme.RVNode, scale float64, m TextMeasurer) *View {
	fontSize := (14.0 + node.FontSize) * scale
	padT := node.PadTop.Scaled(scale)
	padR := node.PadRight.Scaled(scale)
	padB := node.PadBottom.Scaled(scale)
	padL := node.PadLeft.Scaled(scale)
	if padT == 0 && padR == 0 && padB == 0 && padL == 0 {
		p := int(6.0 * scale)
		padT, padR, padB, padL = p, p, p, p
	}
	radius := node.BorderRadius.Scaled(scale)
	if radius == 0 {
		radius = int(4.0 * scale)
	}
	lineSpacing := int(2.0 * scale)
	colGap := int(16.0 * scale)
	const maxLines = 20
	weight := node.FontWeight
	family := node.FontFamily

	lines := splitLines(text)
	if len(lines) == 0 {
		return nil
	}
	if len(lines) > maxLines {
		hidden := len(lines) - (maxLines - 1)
		kept := append([]string{}, lines[:maxLines-1]...)
		lines = append(kept, "… (+"+itoaCompact(hidden)+")")
	}

	innerMax := maxContentWidth - float64(padL+padR)

	// 拆列，求最大列数
	rows := make([][]string, len(lines))
	numCols := 1
	for i, line := range lines {
		cells := strings.Split(line, "\t")
		rows[i] = cells
		if len(cells) > numCols {
			numCols = len(cells)
		}
	}

	border := Border{Radius: radius}
	if node.BorderColor != nil {
		border.Color = node.BorderColor
		border.Width = node.BorderWidth.Scaled(scale)
		if border.Width == 0 {
			border.Width = int(1.0 * scale)
		}
	}
	root := &View{
		Layout:     LayoutColumn,
		Gap:        lineSpacing,
		Padding:    Edges{Top: padT, Right: padR, Bottom: padB, Left: padL},
		Background: Fill{Color: node.BgColor},
		Border:     border,
	}
	mkText := func(s string) *View {
		return &View{Text: s, TextStyle: TextStyle{FontSize: fontSize, Color: node.TextColor, Weight: weight, Family: family}}
	}

	// 单列路径：逐行截断
	if numCols == 1 {
		for _, line := range lines {
			if innerMax > 0 && measureText(m, line, fontSize, family) > innerMax {
				line = truncateLineToWidth(m, line, fontSize, innerMax)
			}
			root.Children = append(root.Children, mkText(line))
		}
		return root
	}

	// 多列路径：列宽 = 每列最大；总宽超 innerMax 则截断最后一列
	colWidth := make([]float64, numCols)
	for _, cells := range rows {
		for k := 0; k < numCols && k < len(cells); k++ {
			if lw := measureText(m, cells[k], fontSize, family); lw > colWidth[k] {
				colWidth[k] = lw
			}
		}
	}
	if innerMax > 0 {
		var fixed float64
		for k := 0; k < numCols-1; k++ {
			fixed += colWidth[k]
		}
		fixed += float64(numCols-1) * float64(colGap)
		lastBudget := innerMax - fixed
		if lastBudget < 0 {
			lastBudget = 0
		}
		if colWidth[numCols-1] > lastBudget {
			colWidth[numCols-1] = 0
			for i, cells := range rows {
				if len(cells) < numCols {
					continue
				}
				if measureText(m, cells[numCols-1], fontSize, family) > lastBudget {
					rows[i][numCols-1] = truncateLineToWidth(m, cells[numCols-1], fontSize, lastBudget)
				}
				if lw := measureText(m, rows[i][numCols-1], fontSize, family); lw > colWidth[numCols-1] {
					colWidth[numCols-1] = lw
				}
			}
		}
	}

	for _, cells := range rows {
		rowView := &View{Layout: LayoutRow, Gap: colGap}
		for k := 0; k < numCols; k++ {
			cell := &View{FixedW: int(colWidth[k] + 0.5)}
			if k < len(cells) {
				cell.Text = cells[k]
				cell.TextStyle = TextStyle{FontSize: fontSize, Color: node.TextColor, Weight: weight, Family: family}
			}
			rowView.Children = append(rowView.Children, cell)
		}
		root.Children = append(root.Children, rowView)
	}
	return root
}
