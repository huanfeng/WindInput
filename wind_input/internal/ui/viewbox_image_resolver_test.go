//go:build windows

package ui

import (
	"encoding/base64"
	"image"
	"image/color"
	"testing"
)

const tstSVG = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><rect width="100" height="100" fill="#000"/></svg>`

func tstSVGDataURI() string {
	return "data:image/svg+xml;base64," + base64.StdEncoding.EncodeToString([]byte(tstSVG))
}

// TestImageResolver_SVGSized：SVG 按目标尺寸栅格化、同尺寸命中缓存、异尺寸独立。
func TestImageResolver_SVGSized(t *testing.T) {
	ir := &imageResolver{}
	uri := tstSVGDataURI()
	img := ir.resolveImage(uri, nil, 24, 24, nil)
	if img == nil || img.Bounds().Dx() != 24 || img.Bounds().Dy() != 24 {
		t.Fatalf("SVG 应栅格化为 24x24, got %v", img)
	}
	if ir.resolveImage(uri, nil, 24, 24, nil) != img {
		t.Error("同尺寸应命中缓存返回同实例")
	}
	if img2 := ir.resolveImage(uri, nil, 12, 12, nil); img2 == img {
		t.Error("异尺寸应独立栅格化")
	}
}

// TestImageResolver_Tint：tint 把图当 alpha mask 用主题色填充（保留 alpha、RGB 换成 tint）。
func TestImageResolver_Tint(t *testing.T) {
	ir := &imageResolver{}
	img := ir.resolveImage(tstSVGDataURI(), nil, 16, 16, color.RGBA{200, 0, 0, 255})
	if img == nil {
		t.Fatal("tint SVG 应非 nil")
	}
	// 实心矩形中心：alpha=255 → R 应≈200、G/B≈0
	r, g, b, a := img.At(8, 8).RGBA()
	if a>>8 == 0 {
		t.Fatal("中心应不透明")
	}
	if r>>8 < 180 || g>>8 > 20 || b>>8 > 20 {
		t.Errorf("tint 后中心应为红色, got R=%d G=%d B=%d", r>>8, g>>8, b>>8)
	}
	// tint 色不同 → 缓存键不同（不同实例）
	if img2 := ir.resolveImage(tstSVGDataURI(), nil, 16, 16, color.RGBA{0, 0, 200, 255}); img2 == img {
		t.Error("不同 tint 色应独立缓存")
	}
}

// TestTintMask：直接校验 alpha 保留 + RGB 替换（预乘）。
func TestTintMask(t *testing.T) {
	src := image.NewRGBA(image.Rect(0, 0, 2, 1))
	// 像素0 alpha=255；像素1 alpha=128（预乘灰）
	src.Pix[0], src.Pix[1], src.Pix[2], src.Pix[3] = 255, 255, 255, 255
	src.Pix[4], src.Pix[5], src.Pix[6], src.Pix[7] = 128, 128, 128, 128
	out := tintMask(src, color.RGBA{200, 0, 0, 255})
	if out.Pix[0] != 200 || out.Pix[3] != 255 {
		t.Errorf("像素0 应 R=200 A=255, got R=%d A=%d", out.Pix[0], out.Pix[3])
	}
	wantR := uint8(200 * 128 / 255)
	if out.Pix[4] != wantR || out.Pix[7] != 128 {
		t.Errorf("像素1 应 R=%d A=128（预乘）, got R=%d A=%d", wantR, out.Pix[4], out.Pix[7])
	}
}
