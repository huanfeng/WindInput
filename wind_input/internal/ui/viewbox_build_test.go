package ui

import (
	"image"
	"image/color"
	"image/png"
	"os"
	"path/filepath"
	"testing"

	"github.com/gogpu/gg"
)

// TestBuildHorizontalCandidateTree_DumpPNG 端到端验证盒模型引擎：
// 构建横排候选窗 View 树 → Layout → PaintTree → 落 PNG 供视觉检查。
// 用 freetype 文本后端（默认系统 CJK 字体），不依赖窗口/GDI。
func TestBuildHorizontalCandidateTree_DumpPNG(t *testing.T) {
	cfg := RenderConfig{
		TextRenderMode:  TextRenderModeFreetype,
		FontSize:        18,
		IndexFontSize:   14,
		ItemHeight:      32,
		CornerRadius:    8,
		Padding:         8,
		IndexStyle:      "circle",
		BackgroundColor: color.RGBA{255, 255, 255, 255},
		TextColor:       color.RGBA{31, 31, 31, 255},
		IndexColor:      color.RGBA{255, 255, 255, 255},
		IndexBgColor:    color.RGBA{66, 133, 244, 255},
		InputBgColor:    color.RGBA{240, 240, 240, 255},
		InputTextColor:  color.RGBA{100, 100, 100, 255},
		BorderColor:     color.RGBA{194, 198, 203, 255},
		HoverBgColor:    color.RGBA{230, 240, 255, 255},
		SelectedBgColor: color.RGBA{210, 228, 255, 255},
	}
	r := NewRenderer(cfg)
	td := r.TextDrawer()
	if td == nil {
		t.Skip("无可用文本后端（freetype 字体未解析）")
	}

	cands := []Candidate{
		{Text: "中文", Index: 1},
		{Text: "中", Index: 2, Comment: "zhōng"},
		{Text: "众", Index: 3},
		{Text: "种", Index: 4},
	}

	const pad = 14
	tree := r.buildHorizontalCandidateTree(cands, "zhong", 5, 1, 1, 0, 2, "")
	root := tree.root
	Layout(root, pad, pad, td)

	W := root.Rect().Max.X + pad
	H := root.Rect().Max.Y + pad
	if W <= 0 || H <= 0 {
		t.Fatalf("非法画布尺寸 %dx%d", W, H)
	}

	// dc 与 img 必须共享同一像素缓冲，否则 gg 矢量层与 theme.DrawBackground/文字层会分离
	buf := make([]byte, W*H*4)
	pm := gg.NewPixmapFromBuffer(buf, W, H)
	dc := gg.NewContextForPixmap(pm)
	img := pm.ImageView()
	PaintTree(root, dc, img, td)

	out := filepath.Join(os.TempDir(), "wind_viewtree_h.png")
	f, err := os.Create(out)
	if err != nil {
		t.Fatalf("create png: %v", err)
	}
	if err := png.Encode(f, img); err != nil {
		f.Close()
		t.Fatalf("encode png: %v", err)
	}
	f.Close()
	t.Logf("候选窗 View 树渲染输出: %s (画布 %dx%d)", out, W, H)

	// 像素采样：确认形状层是否真的画上（区分"缩略图看不清"与"真没画"）
	bands := root.Children
	list := bands[len(bands)-1]
	item0 := list.Children[0]
	sample := func(name string, rc image.Rectangle) {
		cx := (rc.Min.X + rc.Max.X) / 2
		cy := (rc.Min.Y + rc.Max.Y) / 2
		r8, g8, b8, a8 := img.At(cx, cy).RGBA()
		t.Logf("%s 中心(%d,%d) = RGBA(%d,%d,%d,%d)", name, cx, cy, r8>>8, g8>>8, b8>>8, a8>>8)
	}
	sample("item0(选中框)", item0.Rect())
	if len(item0.Children) > 0 {
		sample("item0.index(蓝圈)", item0.Children[0].Rect())
	}
}
