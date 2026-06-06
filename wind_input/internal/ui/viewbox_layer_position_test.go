package ui

import (
	"image"
	"image/color"
	"testing"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// solidLayerImg 造一张全不透明纯色图（预乘 alpha；纯色 alpha=255 时预乘=原值）。
func solidLayerImg(w, h int, c color.RGBA) *image.RGBA {
	img := image.NewRGBA(image.Rect(0, 0, w, h))
	for i := 0; i < len(img.Pix); i += 4 {
		img.Pix[i], img.Pix[i+1], img.Pix[i+2], img.Pix[i+3] = c.R, c.G, c.B, 255
	}
	return img
}

// fillOpaque 把画布填成不透明底色——blendOver 有「dst alpha=0 不绘制」的圆角保护门，
// 真实渲染里底色先铺满使 dst 不透明，单测须模拟此前置（否则透明画布上一律不画）。
func fillOpaque(img *image.RGBA) {
	for i := 0; i < len(img.Pix); i += 4 {
		img.Pix[i], img.Pix[i+1], img.Pix[i+2], img.Pix[i+3] = 0, 0, 0, 255
	}
}

// TestDrawLayer_PctOffsetAndClip 验证覆盖图层：百分比偏移相对 host 换算 + 超出 host 的部分被矩形硬裁。
func TestDrawLayer_PctOffsetAndClip(t *testing.T) {
	dc, img := newSharedDrawContext(120, 40)
	fillOpaque(img)
	host := image.Rect(0, 0, 100, 40) // host 比画布窄，用于验证裁到 host 而非画布
	red := solidLayerImg(20, 20, color.RGBA{255, 0, 0, 255})
	// top-left 锚点 + 水平偏移 = host 宽的 90%（=90px）→ 目标 (90..110)，溢出 host 右界 100 → 右侧裁掉。
	l := ImageLayer{Img: red, Anchor: "top-left", OffsetXPct: 90, W: 20, H: 20}
	drawLayer(dc, img, host, &l)

	if c := img.RGBAAt(95, 10); c.R != 255 {
		t.Errorf("host 内 (95,10) 应绘制为红，got %+v", c)
	}
	if c := img.RGBAAt(105, 10); c.R == 255 {
		t.Errorf("host 外 (105,10) 应被裁（不绘制），got %+v", c)
	}
}

// TestDrawLayer_DpOffset 验证 dp 偏移（已 ×scale）与 anchor 叠加定位正确。
func TestDrawLayer_DpOffset(t *testing.T) {
	dc, img := newSharedDrawContext(60, 60)
	fillOpaque(img)
	host := image.Rect(0, 0, 60, 60)
	blue := solidLayerImg(10, 10, color.RGBA{0, 0, 255, 255})
	// center 锚点（图落在 (25,25)-(35,35)）+ dp 偏移 (+10,+10) → (35,35)-(45,45)。
	l := ImageLayer{Img: blue, Anchor: "center", OffsetX: 10, OffsetY: 10, W: 10, H: 10}
	drawLayer(dc, img, host, &l)

	if c := img.RGBAAt(40, 40); c.B != 255 {
		t.Errorf("偏移后 (40,40) 应为蓝，got %+v", c)
	}
	if c := img.RGBAAt(28, 28); c.B == 255 {
		t.Errorf("原 center 位置 (28,28) 应已让出（偏移后空），got %+v", c)
	}
}

// TestFillFor_PositionedBackground 验证背景图配了定位字段 → Fill.Positioned + offset dp ×scale、百分比透传。
func TestFillFor_PositionedBackground(t *testing.T) {
	r := NewRenderer(parityConfig())
	if r.TextDrawer() == nil {
		t.Skip("无可用文本后端")
	}
	r.resolvedV3 = &theme.ResolvedV3{Resources: map[string]string{"bg": tinyPNGDataURI(t)}}

	bg := &theme.RVImage{
		Ref: "bg", Mode: "stretch", Opacity: 1,
		Anchor: "bottom-right", OffsetX: 4, OffsetYPct: -10, W: 16, H: 16,
	}
	f := r.fillFor(color.Black, bg, nil, 2.0) // scale=2 → dp 部分翻倍
	if !f.Positioned {
		t.Fatal("配了 anchor/offset/size 的背景图应标记 Positioned")
	}
	if f.Anchor != "bottom-right" {
		t.Errorf("anchor 应透传，got %q", f.Anchor)
	}
	if f.OffsetX != 8 {
		t.Errorf("OffsetX dp 4 @scale2 应=8，got %d", f.OffsetX)
	}
	if f.OffsetYPct != -10 {
		t.Errorf("OffsetYPct 百分比应透传 -10（不缩放），got %v", f.OffsetYPct)
	}
	if f.ImgW != 32 || f.ImgH != 32 {
		t.Errorf("size 16 @scale2 应=32，got (%d,%d)", f.ImgW, f.ImgH)
	}
}

// TestFillFor_FullCoverNoPositioned 验证未配定位字段的背景图维持全覆盖（Positioned=false，零回归）。
func TestFillFor_FullCoverNoPositioned(t *testing.T) {
	r := NewRenderer(parityConfig())
	if r.TextDrawer() == nil {
		t.Skip("无可用文本后端")
	}
	r.resolvedV3 = &theme.ResolvedV3{Resources: map[string]string{"bg": tinyPNGDataURI(t)}}

	f := r.fillFor(color.Black, &theme.RVImage{Ref: "bg", Mode: "nine_slice", Opacity: 1}, nil, 1.0)
	if f.Positioned {
		t.Error("未配 anchor/offset/size 的背景图应保持全覆盖（Positioned=false）")
	}
}
