package ui

import (
	"bytes"
	"encoding/base64"
	"image"
	"image/color"
	"image/png"
	"testing"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// tinyPNGDataURI 运行时生成一张 2x2 PNG 的 data URI（避免硬编码 base64 损坏）。
func tinyPNGDataURI(t *testing.T) string {
	t.Helper()
	img := image.NewRGBA(image.Rect(0, 0, 2, 2))
	for i := range img.Pix {
		img.Pix[i] = 255
	}
	var buf bytes.Buffer
	if err := png.Encode(&buf, img); err != nil {
		t.Fatalf("encode png: %v", err)
	}
	return "data:image/png;base64," + base64.StdEncoding.EncodeToString(buf.Bytes())
}

// TestImageForRef_DecodeAndCache 验证 P7-C：ViewImage.ref → resources 表 → 解码位图 + 缓存；
// fillFor 把 RVImage spec 装配成带位图的 Fill；未知 ref 安全退化为纯底色。
func TestImageForRef_DecodeAndCache(t *testing.T) {
	r := NewRenderer(parityConfig())
	if r.TextDrawer() == nil {
		t.Skip("无可用文本后端")
	}
	r.resolvedV25 = &theme.ResolvedV25{
		Resources: map[string]string{"bg": tinyPNGDataURI(t)},
	}

	img := r.imageForRef("bg")
	if img == nil {
		t.Fatal("imageForRef(\"bg\") 应解码出位图")
	}
	if r.imageForRef("bg") != img {
		t.Error("第二次 imageForRef 应命中缓存返回同一位图")
	}

	// fillFor：带图 spec → Fill.Image 非空 + 参数透传
	f := r.fillFor(color.Black, &theme.RVImage{Ref: "bg", Mode: "stretch", Opacity: 0.5})
	if f.Image == nil {
		t.Error("fillFor 应填入解码位图")
	}
	if f.Opacity != 0.5 || f.Mode != "stretch" {
		t.Errorf("fillFor 参数透传错: mode=%q opacity=%v", f.Mode, f.Opacity)
	}

	// 未知 ref / nil spec → 纯底色退化，不崩
	if r.imageForRef("missing") != nil {
		t.Error("未知 ref 应返回 nil（已缓存失败）")
	}
	if got := r.fillFor(color.White, nil); got.Image != nil {
		t.Error("nil spec 应退化为纯底色")
	}
}

// TestWindowBackgroundImage_Rendered 验证 P7-C 完整渲染路径终点：themeViews 设了
// window.background.image → refreshResolvedViews → build → 窗口 View.Background.Image 非空。
func TestWindowBackgroundImage_Rendered(t *testing.T) {
	r := NewRenderer(parityConfig())
	if r.TextDrawer() == nil {
		t.Skip("无可用文本后端")
	}
	views := themePathViews(6, 8)
	views.Window.Background.Image = &theme.ViewImage{Ref: "panel", Mode: "nine_slice", Slice: theme.ViewEdges{Top: dip(8), Right: dip(8), Bottom: dip(8), Left: dip(8)}}
	r.resolvedV25 = &theme.ResolvedV25{
		Palette:   themePathPalette(),
		Behavior:  theme.ResolvedBehavior{FontSize: 18, ShowPageNumber: true, VerticalMaxWidth: 600},
		Resources: map[string]string{"panel": tinyPNGDataURI(t)},
	}
	r.themeViews = &views
	r.refreshResolvedViews()

	cands := []Candidate{{Text: "中文", Index: 1}}
	tree := r.buildHorizontalCandidateTree(cands, "", -1, 0, 1, 0, -1, "")
	if tree.root.Background.Image == nil {
		t.Fatal("窗口 View 应拿到背景图（views.window.background.image → fillFor）")
	}
	if tree.root.Background.Mode != "nine_slice" {
		t.Errorf("窗口背景 mode 应为 nine_slice, got %q", tree.root.Background.Mode)
	}
	if tree.root.Background.Slice.Top != 8 {
		t.Errorf("窗口背景 slice.top 应为 8, got %d", tree.root.Background.Slice.Top)
	}
}

// TestItemState_SelectedHighlightImage 验证 P7-D：item.selected.background.image → 选中项铺高亮位图
// （Fill.Image 非空），非选中项不受影响；选中文字色被 patch 覆盖（整行统一）。
func TestItemState_SelectedHighlightImage(t *testing.T) {
	r := NewRenderer(parityConfig())
	if r.TextDrawer() == nil {
		t.Skip("无可用文本后端")
	}
	views := themePathViews(6, 8)
	views.Item.Selected = &theme.ViewNode{
		Background: theme.ViewFill{Image: &theme.ViewImage{Ref: "hl", Mode: "stretch"}},
		Color:      "#FFFFFF",
		FontWeight: ip(700),
	}
	r.resolvedV25 = &theme.ResolvedV25{
		Palette:   themePathPalette(),
		Behavior:  theme.ResolvedBehavior{FontSize: 18, ShowPageNumber: true, VerticalMaxWidth: 600},
		Resources: map[string]string{"hl": tinyPNGDataURI(t)},
	}
	r.themeViews = &views
	r.refreshResolvedViews()

	cands := []Candidate{{Text: "甲", Index: 1}, {Text: "乙", Index: 2}}
	tree := r.buildHorizontalCandidateTree(cands, "", -1, 0, 1, 0, -1, "") // selectedIndex=0
	if len(tree.items) != 2 {
		t.Fatalf("应有 2 个候选项, got %d", len(tree.items))
	}
	if tree.items[0].Background.Image == nil {
		t.Error("选中项(items[0]) 应铺高亮位图（selected.background.image）")
	}
	if tree.items[1].Background.Image != nil {
		t.Error("非选中项(items[1]) 不应有高亮位图")
	}
}

// TestWindowLayer_Rendered 验证 P7-C2：views.window.layers[] → 解码后追加到窗口 View.Layers
// （z 层级覆盖图，如水印），offset 经 sc 缩放。
func TestWindowLayer_Rendered(t *testing.T) {
	r := NewRenderer(parityConfig())
	if r.TextDrawer() == nil {
		t.Skip("无可用文本后端")
	}
	views := themePathViews(6, 8)
	views.Window.Layers = []theme.ViewImage{
		{Ref: "mark", Z: 1, Anchor: "bottom-right", Offset: theme.ViewImagePoint{X: 4, Y: 4}, Size: theme.ViewImageSize{W: 12, H: 12}},
	}
	r.resolvedV25 = &theme.ResolvedV25{
		Palette:   themePathPalette(),
		Behavior:  theme.ResolvedBehavior{FontSize: 18, ShowPageNumber: true, VerticalMaxWidth: 600},
		Resources: map[string]string{"mark": tinyPNGDataURI(t)},
	}
	r.themeViews = &views
	r.refreshResolvedViews()

	tree := r.buildHorizontalCandidateTree([]Candidate{{Text: "中文", Index: 1}}, "", -1, 0, 1, 0, -1, "")
	var found *ImageLayer
	for i := range tree.root.Layers {
		if tree.root.Layers[i].Img != nil {
			found = &tree.root.Layers[i]
			break
		}
	}
	if found == nil {
		t.Fatal("窗口 layers 应包含一个解码后的图片层（水印）")
	}
	if found.Z != 1 || found.Anchor != "bottom-right" {
		t.Errorf("层 z/anchor 错: z=%d anchor=%q", found.Z, found.Anchor)
	}
}
