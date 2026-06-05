package theme

import (
	"image/color"
	"testing"
)

// gradient_test.go — 守护渐变解析（resolveGradient）+ 栅格化（RasterizeGradient）。

func TestResolveGradient(t *testing.T) {
	pal := ResolvedPalette{Tokens: map[string]color.Color{"accent": color.RGBA{0, 0, 255, 255}}}
	resolve := func(c ColorRef) color.Color { return resolveColorToken(c.Select(false), pal) }

	g := &ViewGradient{
		Type:  "linear",
		Angle: 45,
		Stops: []ViewGradientStop{
			{Color: NewLightDark("#FF0000"), Pos: 1.0},   // 乱序：末尾先给
			{Color: NewLightDark("${accent}"), Pos: 0.0}, // token 解析
			{Color: NewLightDark(""), Pos: 0.5},          // 空 → 跳过
		},
	}
	rv := resolveGradient(g, resolve)
	if rv == nil {
		t.Fatal("resolveGradient 应返回非 nil")
	}
	if len(rv.Stops) != 2 {
		t.Fatalf("空 stop 应跳过，期望 2 个，got %d", len(rv.Stops))
	}
	if rv.Stops[0].Pos != 0.0 || rv.Stops[1].Pos != 1.0 {
		t.Errorf("stops 应按 Pos 升序，got %v, %v", rv.Stops[0].Pos, rv.Stops[1].Pos)
	}
	if rv.Stops[0].Color != color.Color(color.RGBA{0, 0, 255, 255}) {
		t.Errorf("Pos0 应解析为 accent 蓝，got %v", rv.Stops[0].Color)
	}
	if rv.Type != "linear" || rv.Angle != 45 {
		t.Errorf("type/angle 应透传，got %q %v", rv.Type, rv.Angle)
	}

	if resolveGradient(nil, resolve) != nil {
		t.Error("nil 应返回 nil")
	}
	if resolveGradient(&ViewGradient{Stops: []ViewGradientStop{{Color: NewLightDark("")}}}, resolve) != nil {
		t.Error("全空 stop 应返回 nil")
	}
}

func TestRasterizeGradient_Linear(t *testing.T) {
	red := color.RGBA{255, 0, 0, 255}
	blue := color.RGBA{0, 0, 255, 255}
	g := &RVGradient{Type: "linear", Angle: 0, Stops: []RVGradientStop{
		{Color: red, Pos: 0}, {Color: blue, Pos: 1},
	}}
	img := RasterizeGradient(g, 10, 4)
	if img == nil {
		t.Fatal("应返回位图")
	}
	// angle 0 = 左→右：最左偏红、最右偏蓝。
	left := img.RGBAAt(0, 2)
	right := img.RGBAAt(9, 2)
	if left.R < 200 || left.B > 60 {
		t.Errorf("最左应偏红，got %+v", left)
	}
	if right.B < 200 || right.R > 60 {
		t.Errorf("最右应偏蓝，got %+v", right)
	}
	if left.A != 255 {
		t.Errorf("不透明渐变 A 应=255，got %d", left.A)
	}
}

func TestRasterizeGradient_Radial(t *testing.T) {
	white := color.RGBA{255, 255, 255, 255}
	black := color.RGBA{0, 0, 0, 255}
	g := &RVGradient{Type: "radial", Stops: []RVGradientStop{
		{Color: white, Pos: 0}, {Color: black, Pos: 1},
	}}
	img := RasterizeGradient(g, 11, 11)
	if img == nil {
		t.Fatal("应返回位图")
	}
	center := img.RGBAAt(5, 5)
	corner := img.RGBAAt(0, 0)
	if center.R < 200 {
		t.Errorf("radial 中心应近 stop0 白，got %+v", center)
	}
	if corner.R > 80 {
		t.Errorf("radial 角落应近 stop_last 黑，got %+v", corner)
	}
}

func TestRasterizeGradient_Invalid(t *testing.T) {
	if RasterizeGradient(nil, 10, 10) != nil {
		t.Error("nil spec 应返回 nil")
	}
	if RasterizeGradient(&RVGradient{Stops: []RVGradientStop{{Color: color.Black, Pos: 0}}}, 0, 10) != nil {
		t.Error("尺寸非正应返回 nil")
	}
}

// 预乘正确性：半透明 stop 写入 Pix 应为预乘值（R*A/255）。
func TestRasterizeGradient_Premultiplied(t *testing.T) {
	half := color.RGBA{255, 0, 0, 128} // straight 红，alpha 128
	g := &RVGradient{Type: "linear", Angle: 0, Stops: []RVGradientStop{
		{Color: half, Pos: 0}, {Color: half, Pos: 1},
	}}
	img := RasterizeGradient(g, 4, 4)
	px := img.RGBAAt(1, 1)
	wantR := uint8(uint32(255) * 128 / 255) // 预乘
	if px.R != wantR || px.A != 128 {
		t.Errorf("预乘写入错：got R=%d A=%d，want R=%d A=128", px.R, px.A, wantR)
	}
}
