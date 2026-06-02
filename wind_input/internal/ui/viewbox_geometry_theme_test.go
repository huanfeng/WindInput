package ui

import (
	"fmt"
	"image/color"
	"reflect"
	"testing"

	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/theme"
)

// colorHex 把 color.Color 转为 RGBA 十六进制；nil 返回 "-"。
func colorHex(c color.Color) string {
	if c == nil {
		return "-"
	}
	r, g, b, a := c.RGBA()
	return fmt.Sprintf("%02x%02x%02x%02x", uint8(r>>8), uint8(g>>8), uint8(b>>8), uint8(a>>8))
}

// flattenNodes 深度优先收集 View 树所有节点的「几何 + 颜色」指纹。
// 每节点记录 Rect + Background/Border/TextStyle 颜色；重构前后一致即视为几何+颜色对齐。
func flattenNodes(v *View) []string {
	if v == nil {
		return nil
	}
	r := v.Rect()
	out := []string{fmt.Sprintf("%d,%d,%d,%d|bg=%s|bd=%s|tx=%s",
		r.Min.X, r.Min.Y, r.Dx(), r.Dy(),
		colorHex(v.Background.Color), colorHex(v.Border.Color), colorHex(v.TextStyle.Color))}
	for _, c := range v.Children {
		out = append(out, flattenNodes(c)...)
	}
	return out
}

// 本文件补齐合成桥几何指纹（viewbox_geometry_test.go，parityConfig padding=8）的盲区：
// 候选窗生产路径实际走 SetTheme→refreshResolvedViews→theme.ResolveCandidateViews，
// 且真实主题窗口 padding≠8。此处用内联完整 views（padding=6）驱动真实消费路径并锁 golden，
// 守护 P6 阶段2c「合成桥→ResolveCandidateViews」切换的几何+颜色零回归。

func ip(v int) *int              { return &v }                   // 字号/字重等 *int 字段
func dip(v int) *theme.Dimension { d := theme.Dp(v); return &d } // 几何字段=dp 尺寸
func fp(v float64) *float64      { return &v }

// themePathViews 返回一份完整候选窗 views（= defaultViews 量级），winPad/itemPad 可调
// 以覆盖不同 padding（默认指纹用 winPad=6 覆盖 parityConfig padding=8 盲区）。
func themePathViews(winPad, itemPad int) theme.Views {
	return theme.Views{
		Window:     theme.ViewNode{Padding: theme.ViewEdges{Top: dip(winPad), Right: dip(winPad), Bottom: dip(winPad), Left: dip(winPad)}, Border: theme.ViewBorder{Radius: dip(8)}},
		PreeditBar: theme.ViewNode{Padding: theme.ViewEdges{Right: dip(8), Left: dip(8)}, Border: theme.ViewBorder{Radius: dip(4)}},
		Item:       theme.ViewNode{Padding: theme.ViewEdges{Right: dip(itemPad), Left: dip(itemPad)}, Border: theme.ViewBorder{Radius: dip(4)}},
		Index:      theme.ViewNode{},
		Text:       theme.ViewNode{Margin: theme.ViewEdges{Left: dip(4)}},
		Comment:    theme.ViewNode{Margin: theme.ViewEdges{Left: dip(8)}},
		AccentBar:  theme.ViewNode{},
		Metrics: &theme.ViewMetrics{
			ItemSpacing: dip(12), BandGap: dip(4), ShadowOffset: dip(2),
			AccentBar: &theme.AccentBarMetrics{Width: dip(3), Offset: dip(1), HeightRatio: fp(0.6)},
		},
	}
}

// themePathPalette 返回内联候选窗调色板（与 parityConfig 颜色一致，便于对照）。
func themePathPalette() theme.ResolvedPalette {
	return theme.ResolvedPalette{
		Shadow: color.RGBA{0, 0, 0, 15},
		CandidateWindow: theme.ResolvedCandidateWindowPalette{
			Background:  color.RGBA{255, 255, 255, 255},
			Border:      color.RGBA{194, 198, 203, 255},
			Text:        color.RGBA{31, 31, 31, 255},
			Comment:     color.RGBA{150, 150, 150, 255},
			IndexBg:     color.RGBA{66, 133, 244, 255},
			IndexText:   color.RGBA{255, 255, 255, 255},
			HoverBg:     color.RGBA{230, 240, 255, 255},
			SelectedBg:  color.RGBA{210, 228, 255, 255},
			PreeditBg:   color.RGBA{240, 240, 240, 255},
			PreeditText: color.RGBA{100, 100, 100, 255},
			AccentBar:   color.RGBA{0, 120, 212, 255},
		},
	}
}

// themePathFingerprint 走真实消费路径（refreshResolvedViews→ResolveCandidateViews+回填），
// 返回候选窗 View 树几何+颜色指纹。
func themePathFingerprint(t *testing.T, layout config.CandidateLayout, indexStyle string) []string {
	t.Helper()
	cfg := parityConfig()
	cfg.Layout = layout
	cfg.IndexStyle = indexStyle
	r := NewRenderer(cfg)
	if r.TextDrawer() == nil {
		t.Skip("无可用文本后端")
	}
	views := themePathViews(6, 8)
	r.resolvedV25 = &theme.ResolvedV25{Palette: themePathPalette(), Behavior: theme.ResolvedBehavior{FontSize: 18, ShowPageNumber: true, VerticalMaxWidth: 600}}
	r.themeViews = &views
	r.refreshResolvedViews() // 真实生产路径：ResolveCandidateViews + 运行时回填
	cands := []Candidate{
		{Text: "中文", Index: 1},
		{Text: "中", Index: 2, Comment: "zhōng"},
		{Text: "众", Index: 3},
		{Text: "种", Index: 4},
		{Text: "重", Index: 5},
	}
	var tree *candWindowTree
	if layout == config.LayoutHorizontal {
		tree = r.buildHorizontalCandidateTree(cands, "zhong", 5, 2, 3, 0, 1, "")
	} else {
		tree = r.buildVerticalCandidateTree(cands, "zhong", 5, 2, 3, 0, 1, "")
	}
	Layout(tree.root, 0, 0, r.textDrawer)
	return flattenNodes(tree.root)
}

// 主题路径几何+颜色基准（window padding=6，DPI scale=1）。后续重构须保持不变。
var (
	wantHThemeGeometry = []string{"0,0,438,72|bg=ffffffff|bd=c2c6cbff|tx=-", "6,6,426,24|bg=f0f0f0ff|bd=-|tx=-", "14,9,45,18|bg=-|bd=-|tx=646464ff", "6,34,426,32|bg=-|bd=-|tx=-", "6,34,74,32|bg=d2e4ffff|bd=-|tx=-", "6,34,8,32|bg=-|bd=-|tx=-", "14,41,18,18|bg=4285f4ff|bd=-|tx=-", "14,41,18,18|bg=-|bd=-|tx=ffffffff", "36,41,36,18|bg=-|bd=-|tx=1f1f1fff", "80,34,99,32|bg=e6f0ffff|bd=-|tx=-", "80,34,8,32|bg=-|bd=-|tx=-", "88,41,18,18|bg=4285f4ff|bd=-|tx=-", "88,41,18,18|bg=-|bd=-|tx=ffffffff", "110,41,18,18|bg=-|bd=-|tx=1f1f1fff", "136,43,35,14|bg=-|bd=-|tx=969696ff", "179,34,56,32|bg=-|bd=-|tx=-", "179,34,8,32|bg=-|bd=-|tx=-", "187,41,18,18|bg=4285f4ff|bd=-|tx=-", "187,41,18,18|bg=-|bd=-|tx=ffffffff", "209,41,18,18|bg=-|bd=-|tx=1f1f1fff", "235,34,56,32|bg=-|bd=-|tx=-", "235,34,8,32|bg=-|bd=-|tx=-", "243,41,18,18|bg=4285f4ff|bd=-|tx=-", "243,41,18,18|bg=-|bd=-|tx=ffffffff", "265,41,18,18|bg=-|bd=-|tx=1f1f1fff", "291,34,56,32|bg=-|bd=-|tx=-", "291,34,8,32|bg=-|bd=-|tx=-", "299,41,18,18|bg=4285f4ff|bd=-|tx=-", "299,41,18,18|bg=-|bd=-|tx=ffffffff", "321,41,18,18|bg=-|bd=-|tx=1f1f1fff", "355,34,21,32|bg=-|bd=-|tx=-", "376,43,35,14|bg=-|bd=-|tx=646464ff", "411,34,21,32|bg=-|bd=-|tx=-"}
	wantVThemeGeometry = []string{"0,0,121,242|bg=ffffffff|bd=c2c6cbff|tx=-", "6,6,109,30|bg=f0f0f0ff|bd=-|tx=-", "14,12,45,18|bg=-|bd=-|tx=646464ff", "6,40,109,160|bg=-|bd=-|tx=-", "6,40,109,32|bg=d2e4ffff|bd=-|tx=-", "6,40,8,32|bg=-|bd=-|tx=-", "17,45,22,22|bg=4285f4ff|bd=-|tx=-", "17,45,22,22|bg=-|bd=-|tx=ffffffff", "46,47,36,18|bg=-|bd=-|tx=1f1f1fff", "6,72,109,32|bg=e6f0ffff|bd=-|tx=-", "6,72,8,32|bg=-|bd=-|tx=-", "17,77,22,22|bg=4285f4ff|bd=-|tx=-", "17,77,22,22|bg=-|bd=-|tx=ffffffff", "46,79,18,18|bg=-|bd=-|tx=1f1f1fff", "72,81,35,14|bg=-|bd=-|tx=969696ff", "6,104,109,32|bg=-|bd=-|tx=-", "6,104,8,32|bg=-|bd=-|tx=-", "17,109,22,22|bg=4285f4ff|bd=-|tx=-", "17,109,22,22|bg=-|bd=-|tx=ffffffff", "46,111,18,18|bg=-|bd=-|tx=1f1f1fff", "6,136,109,32|bg=-|bd=-|tx=-", "6,136,8,32|bg=-|bd=-|tx=-", "17,141,22,22|bg=4285f4ff|bd=-|tx=-", "17,141,22,22|bg=-|bd=-|tx=ffffffff", "46,143,18,18|bg=-|bd=-|tx=1f1f1fff", "6,168,109,32|bg=-|bd=-|tx=-", "6,168,8,32|bg=-|bd=-|tx=-", "17,173,22,22|bg=4285f4ff|bd=-|tx=-", "17,173,22,22|bg=-|bd=-|tx=ffffffff", "46,175,18,18|bg=-|bd=-|tx=1f1f1fff", "22,204,77,32|bg=-|bd=-|tx=-", "22,204,21,32|bg=-|bd=-|tx=-", "43,213,35,14|bg=-|bd=-|tx=646464ff", "78,204,21,32|bg=-|bd=-|tx=-"}
)

// TestGeometryFingerprint_ThemePathHorizontal 横排真实主题路径几何+颜色零回归（圆点序号）。
func TestGeometryFingerprint_ThemePathHorizontal(t *testing.T) {
	got := themePathFingerprint(t, config.LayoutHorizontal, "circle")
	if !reflect.DeepEqual(got, wantHThemeGeometry) {
		t.Errorf("横排主题路径几何+颜色漂移:\n got (%d): %#v", len(got), got)
	}
}

// TestGeometryFingerprint_ThemePathVertical 竖排真实主题路径几何+颜色零回归（圆点序号）。
func TestGeometryFingerprint_ThemePathVertical(t *testing.T) {
	got := themePathFingerprint(t, config.LayoutVertical, "circle")
	if !reflect.DeepEqual(got, wantVThemeGeometry) {
		t.Errorf("竖排主题路径几何+颜色漂移:\n got (%d): %#v", len(got), got)
	}
}

// wantVTextThemeGeometry 竖排文本序号真实主题路径基准（强调条 rail 占位 + 序号列宽按字形收紧，DPI scale=1）。
var wantVTextThemeGeometry = []string{"0,0,109,242|bg=ffffffff|bd=c2c6cbff|tx=-", "6,6,97,30|bg=f0f0f0ff|bd=-|tx=-", "14,12,45,18|bg=-|bd=-|tx=646464ff", "6,40,97,160|bg=-|bd=-|tx=-", "6,40,97,32|bg=d2e4ffff|bd=-|tx=-", "6,40,8,32|bg=-|bd=-|tx=-", "14,49,11,14|bg=-|bd=-|tx=ffffffff", "29,47,36,18|bg=-|bd=-|tx=1f1f1fff", "6,72,97,32|bg=e6f0ffff|bd=-|tx=-", "6,72,8,32|bg=-|bd=-|tx=-", "14,81,11,14|bg=-|bd=-|tx=ffffffff", "29,79,18,18|bg=-|bd=-|tx=1f1f1fff", "55,80,40,16|bg=-|bd=-|tx=969696ff", "6,104,97,32|bg=-|bd=-|tx=-", "6,104,8,32|bg=-|bd=-|tx=-", "14,113,11,14|bg=-|bd=-|tx=ffffffff", "29,111,18,18|bg=-|bd=-|tx=1f1f1fff", "6,136,97,32|bg=-|bd=-|tx=-", "6,136,8,32|bg=-|bd=-|tx=-", "14,145,11,14|bg=-|bd=-|tx=ffffffff", "29,143,18,18|bg=-|bd=-|tx=1f1f1fff", "6,168,97,32|bg=-|bd=-|tx=-", "6,168,8,32|bg=-|bd=-|tx=-", "14,177,11,14|bg=-|bd=-|tx=ffffffff", "29,175,18,18|bg=-|bd=-|tx=1f1f1fff", "12,204,84,32|bg=-|bd=-|tx=-", "12,204,22,32|bg=-|bd=-|tx=-", "34,212,40,16|bg=-|bd=-|tx=646464ff", "74,204,22,32|bg=-|bd=-|tx=-"}

// TestGeometryFingerprint_ThemePathVerticalText 竖排文本序号（msime 同款）几何零回归：
// 守护强调条 rail 占位（序号排在 rail 右侧不重叠）+ 序号列宽测量收紧。
func TestGeometryFingerprint_ThemePathVerticalText(t *testing.T) {
	got := themePathFingerprint(t, config.LayoutVertical, "text")
	if !reflect.DeepEqual(got, wantVTextThemeGeometry) {
		t.Errorf("竖排文本序号主题路径几何+颜色漂移:\n got (%d): %#v", len(got), got)
	}
}
