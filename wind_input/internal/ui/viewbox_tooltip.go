package ui

// viewbox_tooltip.go — Tooltip（候选编码提示）的 View 树构建与颜色解析（P4-B）。
// 复用包级 Layout/PaintTree + newSharedDrawContext；颜色经 token 解析自 views.tooltip，
// 默认映射 Palette.Tooltip。多行 / \t 列对齐 / 行截断 / 行数上限逻辑在 build 内预处理。

import (
	"image/color"
	"strings"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// resolveTooltipColors 计算 Tooltip 颜色：views.tooltip token > Palette.Tooltip > 默认。
func (w *TooltipWindow) resolveTooltipColors() theme.ResolvedTooltipViews {
	bg := color.Color(color.RGBA{60, 60, 60, 240})
	text := color.Color(color.RGBA{255, 255, 255, 255})
	rv := w.resolvedV25
	if rv == nil {
		return theme.ResolvedTooltipViews{BgColor: bg, TextColor: text}
	}
	bg = rv.Palette.Tooltip.Background
	text = rv.Palette.Tooltip.Text
	if rv.Views != nil && rv.Views.Tooltip != nil {
		res := func(name string) color.Color {
			switch name {
			case "background":
				return rv.Palette.Tooltip.Background
			case "text":
				return rv.Palette.Tooltip.Text
			}
			return nil
		}
		if c := resolveTokenColor(rv.Views.Tooltip.Background.Color, res); c != nil {
			bg = c
		}
		if c := resolveTokenColor(rv.Views.Tooltip.Color, res); c != nil {
			text = c
		}
	}
	return theme.ResolvedTooltipViews{BgColor: bg, TextColor: text}
}

// buildTooltipTree 构建 Tooltip View 树（LayoutColumn 行 + 多列 LayoutRow cell）。
// 几何为 hardcode 逻辑像素 × scale。maxContentWidth<=0 不限宽。无行返回 nil。
func buildTooltipTree(text string, maxContentWidth float64, rtv theme.ResolvedTooltipViews, scale float64, m TextMeasurer) *View {
	fontSize := 14.0 * scale
	padding := int(6.0 * scale)
	lineSpacing := int(2.0 * scale)
	colGap := int(16.0 * scale)
	radius := int(4.0 * scale)
	const maxLines = 20

	lines := splitLines(text)
	if len(lines) == 0 {
		return nil
	}
	if len(lines) > maxLines {
		hidden := len(lines) - (maxLines - 1)
		kept := append([]string{}, lines[:maxLines-1]...)
		lines = append(kept, "… (+"+itoaCompact(hidden)+")")
	}

	innerMax := maxContentWidth - float64(padding*2)

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

	root := &View{
		Layout:     LayoutColumn,
		Gap:        lineSpacing,
		Padding:    Edges{Top: padding, Right: padding, Bottom: padding, Left: padding},
		Background: Fill{Color: rtv.BgColor},
		Border:     Border{Radius: radius},
	}
	mkText := func(s string) *View {
		return &View{Text: s, TextStyle: TextStyle{FontSize: fontSize, Color: rtv.TextColor}}
	}

	// 单列路径：逐行截断
	if numCols == 1 {
		for _, line := range lines {
			if innerMax > 0 && m.MeasureString(line, fontSize) > innerMax {
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
			if lw := m.MeasureString(cells[k], fontSize); lw > colWidth[k] {
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
				if m.MeasureString(cells[numCols-1], fontSize) > lastBudget {
					rows[i][numCols-1] = truncateLineToWidth(m, cells[numCols-1], fontSize, lastBudget)
				}
				if lw := m.MeasureString(rows[i][numCols-1], fontSize); lw > colWidth[numCols-1] {
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
				cell.TextStyle = TextStyle{FontSize: fontSize, Color: rtv.TextColor}
			}
			rowView.Children = append(rowView.Children, cell)
		}
		root.Children = append(root.Children, rowView)
	}
	return root
}
