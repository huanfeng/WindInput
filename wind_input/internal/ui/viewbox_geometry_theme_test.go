package ui

import (
	"fmt"
	"image/color"
	"math"
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
	v := theme.Views{
		Window:     theme.ViewNode{Padding: theme.ViewEdges{Top: dip(winPad), Right: dip(winPad), Bottom: dip(winPad), Left: dip(winPad)}, Border: theme.ViewBorder{Radius: dip(8)}},
		PreeditBar: theme.ViewNode{Padding: theme.ViewEdges{Top: dip(3), Right: dip(8), Bottom: dip(3), Left: dip(8)}, Border: theme.ViewBorder{Radius: dip(4)}},             // 上下 3：条高=内容18+6=24
		Item:       theme.ViewNode{Padding: theme.ViewEdges{Top: dip(7), Right: dip(itemPad), Bottom: dip(7), Left: dip(itemPad)}, Border: theme.ViewBorder{Radius: dip(4)}}, // 上下 7：行高=内容18+14=32（行高现由 item 上下 padding 决定）
		Index:      theme.ViewNode{FontSize: ip(-4), Padding: theme.ViewEdges{Top: dip(2), Bottom: dip(2), Left: dip(2), Right: dip(2)}},                                     // 圆圈：字号 base-4，直径=字号+上下padding
		Text:       theme.ViewNode{Margin: theme.ViewEdges{Left: dip(4)}},
		Comment:    theme.ViewNode{FontSize: ip(-4), Margin: theme.ViewEdges{Left: dip(8)}},
		// V3-D：列表级几何归位到节点（candidate_list.gap/band_gap、window.shadow、accent_bar 几何）。
		CandidateList: theme.ViewNode{Gap: dip(12), BandGap: dip(4)},
		AccentBar:     theme.ViewNode{Width: dip(3), Offset: dip(1), HeightRatio: fp(0.6)},
		FooterBar:     theme.ViewNode{FontSize: ip(-4)},
	}
	v.Window.Shadow = &theme.ViewShadowSpec{OffsetX: dip(2), OffsetY: dip(2)}
	return v
}

// themePathPalette 返回内联候选窗调色板（与 parityConfig 颜色一致，便于对照）。
// v3：颜色全部扁平进 Tokens（候选窗节点经 ${token} 引用消费）。
func themePathPalette() theme.ResolvedPalette {
	tokens := map[string]color.Color{
		"bg":             color.RGBA{255, 255, 255, 255},
		"border":         color.RGBA{194, 198, 203, 255},
		"text":           color.RGBA{31, 31, 31, 255},
		"text_hint":      color.RGBA{150, 150, 150, 255},
		"text_dim":       color.RGBA{100, 100, 100, 255},
		"surface":        color.RGBA{240, 240, 240, 255},
		"accent":         color.RGBA{66, 133, 244, 255},
		"on_accent":      color.RGBA{255, 255, 255, 255},
		"selection":      color.RGBA{210, 228, 255, 255},
		"selection_text": color.RGBA{31, 31, 31, 255},
		"hover":          color.RGBA{230, 240, 255, 255},
	}
	return theme.ResolvedPalette{
		Shadow:   color.RGBA{0, 0, 0, 15},
		Tokens:   tokens,
		Bg:       tokens["bg"],
		Surface:  tokens["surface"],
		Border:   tokens["border"],
		Text:     tokens["text"],
		TextDim:  tokens["text_dim"],
		TextHint: tokens["text_hint"],
		Accent:   tokens["accent"],
		OnAccent: tokens["on_accent"],
	}
}

// applyThemePath 给 renderer 注入完整主题(views + palette + behavior)，驱动真实消费路径
// （refreshResolvedViews→ResolveCandidateViews+回填）。供 fingerprint / hittest 等共用。
func applyThemePath(r *Renderer, winPad, itemPad int) {
	v := themePathViews(winPad, itemPad)
	r.resolvedV3 = &theme.ResolvedV3{Palette: themePathPalette(), Behavior: theme.ResolvedBehavior{FontSize: 18, ShowPageNumber: true, VerticalMaxWidth: 600}}
	r.themeViews = &v
}

// themePathFontProbe / themePathFontBaseline：字体度量一致性闸门用探针。
// 基准值为生成下方 golden 指纹时的开发机实测宽度（"中文zhong" @ 字号 18）。
// CI runner 等异构字体环境实测会偏离，据此跳过精确几何断言。
const (
	themePathFontProbe    = "中文zhong"
	themePathFontBaseline = 81.0
)

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
	applyThemePath(r, 6, 8)
	// 字体度量一致性闸门：下方 golden 指纹锁的是基准开发机的字形宽度，而 CI runner
	// 字体后端度量不同会令整窗几何整体漂移（连 ASCII 串都偏移）。实测探针偏离基准即视为
	// 字体环境不同，跳过精确断言，避免字体差异被误报为几何回归（同款字体下仍真跑守护）。
	if w := r.TextDrawer().MeasureString(themePathFontProbe, 18); math.Abs(w-themePathFontBaseline) > 0.5 {
		t.Skipf("字体度量与 golden 基准不一致 (probe=%.2f, want≈%.0f)，跳过几何指纹断言", w, themePathFontBaseline)
	}
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
	wantVThemeGeometry = []string{"0,0,117,236|bg=ffffffff|bd=c2c6cbff|tx=-", "6,6,105,24|bg=f0f0f0ff|bd=-|tx=-", "14,9,45,18|bg=-|bd=-|tx=646464ff", "6,34,105,160|bg=-|bd=-|tx=-", "6,34,105,32|bg=d2e4ffff|bd=-|tx=-", "6,34,8,32|bg=-|bd=-|tx=-", "17,41,18,18|bg=4285f4ff|bd=-|tx=-", "17,41,18,18|bg=-|bd=-|tx=ffffffff", "42,41,36,18|bg=-|bd=-|tx=1f1f1fff", "6,66,105,32|bg=e6f0ffff|bd=-|tx=-", "6,66,8,32|bg=-|bd=-|tx=-", "17,73,18,18|bg=4285f4ff|bd=-|tx=-", "17,73,18,18|bg=-|bd=-|tx=ffffffff", "42,73,18,18|bg=-|bd=-|tx=1f1f1fff", "68,75,35,14|bg=-|bd=-|tx=969696ff", "6,98,105,32|bg=-|bd=-|tx=-", "6,98,8,32|bg=-|bd=-|tx=-", "17,105,18,18|bg=4285f4ff|bd=-|tx=-", "17,105,18,18|bg=-|bd=-|tx=ffffffff", "42,105,18,18|bg=-|bd=-|tx=1f1f1fff", "6,130,105,32|bg=-|bd=-|tx=-", "6,130,8,32|bg=-|bd=-|tx=-", "17,137,18,18|bg=4285f4ff|bd=-|tx=-", "17,137,18,18|bg=-|bd=-|tx=ffffffff", "42,137,18,18|bg=-|bd=-|tx=1f1f1fff", "6,162,105,32|bg=-|bd=-|tx=-", "6,162,8,32|bg=-|bd=-|tx=-", "17,169,18,18|bg=4285f4ff|bd=-|tx=-", "17,169,18,18|bg=-|bd=-|tx=ffffffff", "42,169,18,18|bg=-|bd=-|tx=1f1f1fff", "20,198,77,32|bg=-|bd=-|tx=-", "20,198,21,32|bg=-|bd=-|tx=-", "41,207,35,14|bg=-|bd=-|tx=646464ff", "76,198,21,32|bg=-|bd=-|tx=-"}
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
var wantVTextThemeGeometry = []string{"0,0,104,236|bg=ffffffff|bd=c2c6cbff|tx=-", "6,6,92,24|bg=f0f0f0ff|bd=-|tx=-", "14,9,45,18|bg=-|bd=-|tx=646464ff", "6,34,92,160|bg=-|bd=-|tx=-", "6,34,92,32|bg=d2e4ffff|bd=-|tx=-", "6,34,8,32|bg=-|bd=-|tx=-", "14,43,11,14|bg=-|bd=-|tx=ffffffff", "29,41,36,18|bg=-|bd=-|tx=1f1f1fff", "6,66,92,32|bg=e6f0ffff|bd=-|tx=-", "6,66,8,32|bg=-|bd=-|tx=-", "14,75,11,14|bg=-|bd=-|tx=ffffffff", "29,73,18,18|bg=-|bd=-|tx=1f1f1fff", "55,75,35,14|bg=-|bd=-|tx=969696ff", "6,98,92,32|bg=-|bd=-|tx=-", "6,98,8,32|bg=-|bd=-|tx=-", "14,107,11,14|bg=-|bd=-|tx=ffffffff", "29,105,18,18|bg=-|bd=-|tx=1f1f1fff", "6,130,92,32|bg=-|bd=-|tx=-", "6,130,8,32|bg=-|bd=-|tx=-", "14,139,11,14|bg=-|bd=-|tx=ffffffff", "29,137,18,18|bg=-|bd=-|tx=1f1f1fff", "6,162,92,32|bg=-|bd=-|tx=-", "6,162,8,32|bg=-|bd=-|tx=-", "14,171,11,14|bg=-|bd=-|tx=ffffffff", "29,169,18,18|bg=-|bd=-|tx=1f1f1fff", "13,198,77,32|bg=-|bd=-|tx=-", "13,198,21,32|bg=-|bd=-|tx=-", "34,207,35,14|bg=-|bd=-|tx=646464ff", "69,198,21,32|bg=-|bd=-|tx=-"}

// TestGeometryFingerprint_ThemePathVerticalText 竖排文本序号（msime 同款）几何零回归：
// 守护强调条 rail 占位（序号排在 rail 右侧不重叠）+ 序号列宽测量收紧。
func TestGeometryFingerprint_ThemePathVerticalText(t *testing.T) {
	got := themePathFingerprint(t, config.LayoutVertical, "text")
	if !reflect.DeepEqual(got, wantVTextThemeGeometry) {
		t.Errorf("竖排文本序号主题路径几何+颜色漂移:\n got (%d): %#v", len(got), got)
	}
}

// TestVerticalPaddingAndRowGap 守护两项一致性修正（accent 关闭路径，现有指纹测试只覆盖 accent 路径）：
//  1. item.padding.left 在竖排也生效（无 accent 时内容右移 padding.left，与横排一致）；
//  2. candidate_list.row_gap 产生竖排纵向行间距。
//
// 几何不依赖文本宽度（圆圈 FixedW + 行 Y 坐标），故无需字体度量闸门。
func TestVerticalPaddingAndRowGap(t *testing.T) {
	cfg := parityConfig()
	cfg.Layout = config.LayoutVertical
	cfg.IndexStyle = "circle"
	cfg.HasAccentBar = false // 关闭 accent：验证 padding.left 在竖排作内边距生效（而非被 rail 顶替）
	r := NewRenderer(cfg)
	if r.TextDrawer() == nil {
		t.Skip("无可用文本后端")
	}
	v := themePathViews(6, 8)        // itemPad=8 → bgPadL=8
	v.CandidateList.RowGap = dip(10) // 纵向行间距 10
	r.resolvedV3 = &theme.ResolvedV3{Palette: themePathPalette(), Behavior: theme.ResolvedBehavior{FontSize: 18, ShowPageNumber: true, VerticalMaxWidth: 600}}
	r.themeViews = &v
	r.refreshResolvedViews()

	cands := []Candidate{{Text: "中", Index: 1}, {Text: "文", Index: 2}}
	tree := r.buildVerticalCandidateTree(cands, "", -1, 1, 1, 0, -1, "")
	Layout(tree.root, 0, 0, r.textDrawer)
	if len(tree.items) < 2 {
		t.Fatalf("应 2 个候选项, got %d", len(tree.items))
	}

	// row_gap=10：相邻候选行的垂直间隙 = LayoutColumn Gap = 10。
	if gap := tree.items[1].Rect().Min.Y - tree.items[0].Rect().Max.Y; gap != 10 {
		t.Errorf("竖排 row_gap 应=10，相邻行间距 got %d", gap)
	}

	// padding.left=8 生效（无 accent，无 rail 顶替）：序号圆圈左偏移 = padding.left(8) + circle margin.left(3) = 11。
	// 旧逻辑（竖排丢弃 padding.left）下圆圈左偏移仅 = margin.left(3)，<8。
	circle := tree.items[0].Children[0]
	if off := circle.Rect().Min.X - tree.items[0].Rect().Min.X; off < 8 {
		t.Errorf("无 accent 时 item.padding.left(8) 应在竖排生效，序号左偏移 got %d (<8)", off)
	}
}
