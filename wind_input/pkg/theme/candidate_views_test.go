package theme

import (
	"image/color"
	"testing"
)

func testPalette() ResolvedPalette {
	return ResolvedPalette{
		Shadow: color.RGBA{1, 1, 1, 15},
		CandidateWindow: ResolvedCandidateWindowPalette{
			Background:  color.RGBA{255, 255, 255, 255},
			Border:      color.RGBA{200, 200, 200, 255},
			Text:        color.RGBA{30, 30, 30, 255},
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

func TestResolveCandidateViewColor(t *testing.T) {
	pal := testPalette()
	cases := map[string]color.Color{
		"${background}":  pal.CandidateWindow.Background,
		"${index_bg}":    pal.CandidateWindow.IndexBg,
		"${selected_bg}": pal.CandidateWindow.SelectedBg,
		"${comment}":     pal.CandidateWindow.Comment,
		"${accent}":      pal.CandidateWindow.AccentBar,
		"${shadow}":      pal.Shadow,
	}
	for tok, want := range cases {
		if got := resolveCandidateViewColor(tok, pal); got != want {
			t.Errorf("%s → got %v, want %v", tok, got, want)
		}
	}
	if got := resolveCandidateViewColor("#FF0000", pal); got == nil {
		t.Error("hex 应解析为非 nil")
	}
	if resolveCandidateViewColor("", pal) != nil {
		t.Error("空串应为 nil")
	}
	if resolveCandidateViewColor("${unknown}", pal) != nil {
		t.Error("未知 token 应为 nil")
	}
	// P7-C：transparent 字面量 → 全透明（位图皮肤让背景透出）
	if c := resolveCandidateViewColor("transparent", pal); c == nil {
		t.Error("transparent 应返回非 nil 的全透明色")
	} else if _, _, _, a := c.RGBA(); a != 0 {
		t.Errorf("transparent 的 alpha 应为 0, got %d", a)
	}
}

func TestResolveCandidateViews_GeometryAndColor(t *testing.T) {
	pal := testPalette()
	v := defaultViews()
	v.Window.Background = ViewFill{Color: "${background}"}
	rv := ResolveCandidateViews(v, pal)

	if rv.Window.PadLeft.Value != 8 || rv.Window.PadTop.Value != 8 {
		t.Errorf("Window padding 应为 8, got L=%v T=%v", rv.Window.PadLeft, rv.Window.PadTop)
	}
	if rv.Item.BorderRadius.Value != 4 {
		t.Errorf("Item radius 应为 4, got %v", rv.Item.BorderRadius)
	}
	if rv.ItemSpacing.Value != 12 || rv.WindowGap.Value != 2 || rv.ShadowOffset.Value != 2 {
		t.Errorf("metrics 顶层错: spacing=%v gap=%v shadow=%v", rv.ItemSpacing, rv.WindowGap, rv.ShadowOffset)
	}
	if rv.AccentBarWidth.Value != 3 || rv.AccentBarOffset.Value != 1 || rv.AccentBarHRatio != 0.6 {
		t.Errorf("accent metrics 错: w=%v off=%v hr=%v", rv.AccentBarWidth, rv.AccentBarOffset, rv.AccentBarHRatio)
	}
	if rv.Index.BgColor != pal.CandidateWindow.IndexBg {
		t.Errorf("Index.BgColor 默认应=palette.IndexBg, got %v", rv.Index.BgColor)
	}
	if rv.Text.TextColor != pal.CandidateWindow.Text {
		t.Errorf("Text.TextColor 默认应=palette.Text, got %v", rv.Text.TextColor)
	}
	// P7-D：item 三态升级为 RVState patch。selected 默认 palette SelectedBg+SelectedText，hover 默认 HoverBg。
	if rv.Item.Selected == nil || rv.Item.Selected.BgColor != pal.CandidateWindow.SelectedBg ||
		rv.Item.Selected.TextColor != pal.CandidateWindow.SelectedText {
		t.Errorf("Item selected 默认错: %+v", rv.Item.Selected)
	}
	if rv.Item.Hover == nil || rv.Item.Hover.BgColor != pal.CandidateWindow.HoverBg {
		t.Errorf("Item hover 默认错: %+v", rv.Item.Hover)
	}
	if rv.Item.Disabled != nil {
		t.Errorf("Item disabled 无 palette 默认、主题未配应为 nil, got %+v", rv.Item.Disabled)
	}
	if rv.Window.BgColor != pal.CandidateWindow.Background {
		t.Errorf("Window.BgColor token 覆盖错, got %v", rv.Window.BgColor)
	}
	if rv.ShadowColor != pal.Shadow {
		t.Errorf("ShadowColor 应=palette.Shadow, got %v", rv.ShadowColor)
	}
	if rv.Text.FontSize != 0 || rv.ItemHeight != 0 || rv.VerticalMaxWidth != 0 {
		t.Errorf("字号/行高/竖排max 本层应为零值（ui 回填）, got fs=%v ih=%v vmax=%v", rv.Text.FontSize, rv.ItemHeight, rv.VerticalMaxWidth)
	}
}

// TestResolveCandidateViews_States 验证 P7-D：item 三态解析为完整 RVState patch
// （底色 + 高亮位图 + 文字色 + 字重 + 边框色/宽），hover token 解析，disabled 预留可解析。
func TestResolveCandidateViews_States(t *testing.T) {
	pal := testPalette()
	pal.CandidateWindow.SelectedText = color.RGBA{255, 255, 255, 255}

	v := defaultViews()
	v.Item.Selected = &ViewNode{
		Background: ViewFill{Color: "#102030", Image: &ViewImage{Ref: "hl", Mode: "nine_slice"}},
		Color:      "#FFFFFF",
		FontWeight: intp(700),
		Border:     ViewBorder{Color: "#445566", Width: dimp(2)},
	}
	v.Item.Hover = &ViewNode{Background: ViewFill{Color: "${hover_bg}"}}
	v.Item.Disabled = &ViewNode{Color: "#999999"}
	rv := ResolveCandidateViews(v, pal)

	sel := rv.Item.Selected
	if sel == nil {
		t.Fatal("selected state 应非 nil")
	}
	if sel.BgColor == nil || sel.BgImage == nil || sel.BgImage.Ref != "hl" || sel.BgImage.Mode != "nine_slice" {
		t.Errorf("selected 底色/高亮位图解析错: %+v", sel)
	}
	if sel.TextColor == nil {
		t.Error("selected 文字色应被 #FFFFFF 覆盖")
	}
	if sel.FontWeight != 700 {
		t.Errorf("selected 字重应=700, got %d", sel.FontWeight)
	}
	if sel.BorderColor == nil || sel.BorderWidth == nil || sel.BorderWidth.Value != 2 {
		t.Errorf("selected 边框解析错: color=%v width=%v", sel.BorderColor, sel.BorderWidth)
	}

	if rv.Item.Hover == nil || rv.Item.Hover.BgColor != pal.CandidateWindow.HoverBg {
		t.Errorf("hover bg token 应解析为 palette.HoverBg: %+v", rv.Item.Hover)
	}
	if rv.Item.Disabled == nil || rv.Item.Disabled.TextColor == nil {
		t.Errorf("disabled patch 应可解析（schema 预留）: %+v", rv.Item.Disabled)
	}

	// 序号/注释各自独立支持选中态（View 模型对称）：未配置→nil（沿用基态）；配置→解析。
	if rv.Index.Selected != nil || rv.Comment.Selected != nil {
		t.Errorf("未配置 index/comment.selected 应为 nil（默认与普通态一致）: idx=%+v cmt=%+v", rv.Index.Selected, rv.Comment.Selected)
	}
	v.Index.Selected = &ViewNode{Color: "#FF8800", FontWeight: intp(700)}
	rv2 := ResolveCandidateViews(v, pal)
	if rv2.Index.Selected == nil || rv2.Index.Selected.TextColor == nil || rv2.Index.Selected.FontWeight != 700 {
		t.Errorf("views.index.selected 应独立解析进 rv.Index.Selected: %+v", rv2.Index.Selected)
	}
}

// TestResolveCandidateViews_FontFields 验证 P7-B：逐元素 font_size（逻辑像素绝对值）/ font_weight
// 被 ResolveCandidateViews 读入 RVNode；未写的元素保持 0（由 ui 回填运行时派生 + DPI scale）。
func TestResolveCandidateViews_FontFields(t *testing.T) {
	pal := testPalette()
	v := defaultViews()
	v.Text.FontSize = intp(15)
	v.Text.FontWeight = intp(700)
	v.Text.FontFamily = "KaiTi"
	v.Comment.FontSize = intp(11)
	v.Comment.FontFamily = "Arial"
	v.Index.FontWeight = intp(600)
	rv := ResolveCandidateViews(v, pal)

	if rv.Text.FontSize != 15 {
		t.Errorf("Text.FontSize 应读 views 显式值 15(逻辑px), got %v", rv.Text.FontSize)
	}
	if rv.Text.FontWeight != 700 {
		t.Errorf("Text.FontWeight 应为 700, got %d", rv.Text.FontWeight)
	}
	if rv.Text.FontFamily != "KaiTi" {
		t.Errorf("Text.FontFamily 应读 views 显式值 KaiTi, got %q", rv.Text.FontFamily)
	}
	if rv.Comment.FontFamily != "Arial" {
		t.Errorf("Comment.FontFamily 应为 Arial, got %q", rv.Comment.FontFamily)
	}
	// 未设 family 的元素保持空（继承全局）
	if rv.Index.FontFamily != "" {
		t.Errorf("未设的 Index.FontFamily 应为空, got %q", rv.Index.FontFamily)
	}
	if rv.Comment.FontSize != 11 {
		t.Errorf("Comment.FontSize 应为 11, got %v", rv.Comment.FontSize)
	}
	if rv.Index.FontWeight != 600 {
		t.Errorf("Index.FontWeight 应为 600, got %d", rv.Index.FontWeight)
	}
	// 未设字号的元素保持 0（由 ui 回填运行时派生）
	if rv.PreeditBar.FontSize != 0 {
		t.Errorf("未设的 PreeditBar.FontSize 应为 0, got %v", rv.PreeditBar.FontSize)
	}
}

// TestResolveCandidateViews_BackgroundImage 验证 P7-C：views.X.background.image 与 layers[] 被
// ResolveCandidateViews 转成 RVNode 的 BgImage/Layers spec（toRVImage：opacity nil→1.0、slice 指针→plain）。
func TestResolveCandidateViews_BackgroundImage(t *testing.T) {
	pal := testPalette()
	v := defaultViews()
	v.Window.Background.Image = &ViewImage{
		Ref:   "paper",
		Mode:  "nine_slice",
		Slice: ViewEdges{Top: dimp(4), Right: dimp(4), Bottom: dimp(4), Left: dimp(4)},
	}
	v.Item.Layers = []ViewImage{
		{Ref: "glow", Mode: "stretch", Z: -1, Anchor: "left", Offset: ViewImagePoint{X: 2, Y: 0}, Size: ViewImageSize{W: 8, H: 8}},
	}
	rv := ResolveCandidateViews(v, pal)

	if rv.Window.BgImage == nil {
		t.Fatal("Window.BgImage 应非 nil")
	}
	if rv.Window.BgImage.Ref != "paper" || rv.Window.BgImage.Mode != "nine_slice" {
		t.Errorf("BgImage ref/mode 错: %+v", rv.Window.BgImage)
	}
	if rv.Window.BgImage.Opacity != 1.0 { // nil opacity → 1.0
		t.Errorf("BgImage opacity nil 应解析为 1.0, got %v", rv.Window.BgImage.Opacity)
	}
	if rv.Window.BgImage.Slice.Top != 4 {
		t.Errorf("BgImage slice.top 应为 4, got %d", rv.Window.BgImage.Slice.Top)
	}
	if len(rv.Item.Layers) != 1 || rv.Item.Layers[0].Ref != "glow" || rv.Item.Layers[0].Z != -1 {
		t.Errorf("Item.Layers 错: %+v", rv.Item.Layers)
	}
	if rv.Item.Layers[0].OffsetX != 2 || rv.Item.Layers[0].W != 8 {
		t.Errorf("Layer offset/size 错: %+v", rv.Item.Layers[0])
	}
	// 未设图的元素保持 nil/空
	if rv.Text.BgImage != nil || len(rv.Text.Layers) != 0 {
		t.Error("未设图的 Text 应无 BgImage/Layers")
	}
}
