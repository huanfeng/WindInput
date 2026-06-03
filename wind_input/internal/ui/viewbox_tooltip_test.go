//go:build windows

package ui

import (
	"image/color"
	"testing"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// TestResolveTooltipColors token 映射 Palette.Tooltip。
func TestResolveTooltipColors(t *testing.T) {
	bg := color.RGBA{11, 22, 33, 255}
	txt := color.RGBA{210, 210, 210, 255}
	rv := &theme.ResolvedV25{
		Palette: theme.ResolvedPalette{Tooltip: theme.ResolvedTooltipPalette{Background: bg, Text: txt}},
		Views:   &theme.Views{Tooltip: &theme.ViewNode{Background: theme.ViewFill{Color: "${background}"}, Color: "${text}"}},
	}
	w := &TooltipWindow{resolvedV25: rv}
	node := w.resolveTooltipNode()
	if node.BgColor != color.Color(bg) || node.TextColor != color.Color(txt) {
		t.Fatalf("tooltip 颜色应映射 Palette.Tooltip, got %+v", node)
	}
}

// TestBuildTooltipTree_SingleCol 单列多行：列布局，行高/总尺寸指纹。
func TestBuildTooltipTree_SingleCol(t *testing.T) {
	m := fixedMeasurer{charW: 10}
	node := theme.RVNode{BgColor: color.RGBA{1, 1, 1, 255}, TextColor: color.RGBA{2, 2, 2, 255}}
	root := buildTooltipTree("ab\ncde", 0, node, 1.0, m)
	Layout(root, 0, 0, m)
	r := root.Rect()
	// 行宽: "ab"=20, "cde"=30 → max 30; +padding 6*2 = 42
	if r.Dx() != 42 {
		t.Errorf("width 应 42, got %d", r.Dx())
	}
	// 高: fontSize 14*2 + lineSpacing 2*(2-1) + padding 12 = 28+2+12 = 42
	if r.Dy() != 42 {
		t.Errorf("height 应 42, got %d", r.Dy())
	}
	if len(root.Children) != 2 || root.Layout != LayoutColumn {
		t.Errorf("应为 2 行的列布局, got layout=%d children=%d", root.Layout, len(root.Children))
	}
	if root.Background.Color != (color.RGBA{1, 1, 1, 255}) {
		t.Error("bg 颜色指纹不符")
	}
}

// TestBuildTooltipTree_MultiCol 多列：每行 LayoutRow，列宽对齐 + 缺列空占位。
func TestBuildTooltipTree_MultiCol(t *testing.T) {
	m := fixedMeasurer{charW: 10}
	node := theme.RVNode{BgColor: color.RGBA{1, 1, 1, 255}, TextColor: color.RGBA{2, 2, 2, 255}}
	// 行1 两列 "a\tbb"，行2 一列 "ccc"（缺第2列）
	root := buildTooltipTree("a\tbb\nccc", 0, node, 1.0, m)
	Layout(root, 0, 0, m)
	if len(root.Children) != 2 {
		t.Fatalf("应 2 行, got %d", len(root.Children))
	}
	row0 := root.Children[0]
	if row0.Layout != LayoutRow || len(row0.Children) != 2 {
		t.Fatalf("行0 应为 2 列的 Row, got layout=%d cells=%d", row0.Layout, len(row0.Children))
	}
	// 列0 宽 = max("a"=10, "ccc"=30) = 30
	if row0.Children[0].FixedW != 30 {
		t.Errorf("列0 宽应 30（列对齐取最大）, got %d", row0.Children[0].FixedW)
	}
	row1 := root.Children[1]
	if len(row1.Children) != 2 || row1.Children[1].Text != "" {
		t.Error("行1 缺第2列应补空占位 cell")
	}
}
