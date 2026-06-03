//go:build windows

package ui

import (
	"image"
	"image/color"
	"testing"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// solidRGBA 返回 w×h 的不透明纯色图（预乘 alpha 下纯色 = 直接值，A=255）。
func solidRGBA(w, h int, c color.RGBA) *image.RGBA {
	img := image.NewRGBA(image.Rect(0, 0, w, h))
	for i := 0; i+3 < len(img.Pix); i += 4 {
		img.Pix[i], img.Pix[i+1], img.Pix[i+2], img.Pix[i+3] = c.R, c.G, c.B, c.A
	}
	return img
}

// cornerAlphaStats 统计左上角 n×n 的 alpha 分布：semi=半透明(16≤a<250)，opaque=不透明(a≥250)。
func cornerAlphaStats(img *image.RGBA, n int) (semi, opaque int) {
	for y := 0; y < n; y++ {
		for x := 0; x < n; x++ {
			a := img.Pix[y*img.Stride+x*4+3]
			switch {
			case a < 16:
			case a < 250:
				semi++
			default:
				opaque++
			}
		}
	}
	return
}

// darkFringeCount 统计左上角 n×n 内「非透明但去预乘后偏暗(R<110)」的像素数——
// 即深色底色在圆角抗锯齿边缘透出形成的「深色毛边」。背景图为浅色时，此值应为 0。
func darkFringeCount(img *image.RGBA, n int) int {
	cnt := 0
	for y := 0; y < n; y++ {
		for x := 0; x < n; x++ {
			off := y*img.Stride + x*4
			a := img.Pix[off+3]
			if a < 16 {
				continue
			}
			if int(img.Pix[off])*255/int(a) < 110 { // 去预乘还原 R
				cnt++
			}
		}
	}
	return cnt
}

// renderMenuCorner 渲染带背景图 + 圆角边框的菜单到位图。clipFull 决定 clip 半径：
//
//	true  = 裁到完整圆角 r=radius（prod 修复后逻辑：边框 AA 有不透明底垫）
//	false = 裁到 innerR=radius-borderWidth（旧逻辑：边框圆角留透明月牙，回归对照）
func renderMenuCorner(t *testing.T, clipFull bool, scale float64) *image.RGBA {
	t.Helper()
	td := &recordingDrawer{}
	rmv := theme.ResolvedMenuViews{
		Root: theme.RVNode{
			BgColor:      color.RGBA{255, 255, 255, 255},
			BgImage:      &theme.RVImage{Ref: "bg", Mode: "stretch"},
			BorderColor:  color.RGBA{0, 200, 0, 255},
			BorderRadius: theme.Dp(6),
			BorderWidth:  theme.Dp(1),
		},
		Item: theme.RVNode{TextColor: color.RGBA{0, 0, 0, 255}},
	}
	ir := &imageResolver{cache: map[string]*image.RGBA{"bg": solidRGBA(64, 64, color.RGBA{255, 0, 0, 255})}}
	mw, mh := int(120*scale), int(60*scale)
	mt := buildMenuTree([]MenuItem{{Text: "A"}, {Text: "B"}}, -1, -1, false, false, rmv, mw, mh, 12.0, 24, scale, ir, nil)
	Layout(mt.root, 0, 0, td)
	dc, img := newSharedDrawContext(mw, mh)
	radius := rmv.Root.BorderRadius.Scaled(scale)
	bw := rmv.Root.BorderWidth.Scaled(scale)
	if bw == 0 {
		bw = 1
	}
	if clipFull {
		dc.DrawRoundedRectangle(0, 0, float64(mw), float64(mh), float64(radius))
	} else {
		innerR := radius - bw
		dc.DrawRoundedRectangle(float64(bw), float64(bw), float64(mw-2*bw), float64(mh-2*bw), float64(innerR))
	}
	dc.Clip()
	PaintTree(mt.root, dc, img, td)
	dc.ResetClip()
	half := float64(bw) / 2
	dc.SetColor(rmv.Root.BorderColor)
	dc.SetLineWidth(float64(bw))
	dc.DrawRoundedRectangle(half, half, float64(mw)-2*half, float64(mh)-2*half, float64(radius))
	dc.Stroke()
	return img
}

// TestMenuCorner_OpaqueBorderBacking 回归（P8 切片6 菜单圆角透明修复）：菜单圆角边框必须有不透明底垫。
// 旧 innerR=radius-bw 裁剪让边框圆角抗锯齿像素背后透明 → layered 窗口透出下方；
// 改裁到完整 radius 后底色填到圆角边缘 → 边框圆角不透明（仅最外缘细 AA）。
func TestMenuCorner_OpaqueBorderBacking(t *testing.T) {
	for _, scale := range []float64{1.0, 1.25, 1.5} {
		const n = 12
		fullSemi, fullOpaque := cornerAlphaStats(renderMenuCorner(t, true, scale), n)
		innerSemi, innerOpaque := cornerAlphaStats(renderMenuCorner(t, false, scale), n)
		if fullSemi >= innerSemi {
			t.Errorf("scale=%.2f: 裁完整圆角半透明像素(%d)应少于旧 innerR(%d)", scale, fullSemi, innerSemi)
		}
		if fullOpaque <= innerOpaque {
			t.Errorf("scale=%.2f: 裁完整圆角不透明像素(%d)应多于旧 innerR(%d)", scale, fullOpaque, innerOpaque)
		}
	}
}

// renderStatusCorner 渲染带浅色背景图的状态泡到位图，bg 为底色（fallback/AA 边缘色）。
func renderStatusCorner(t *testing.T, bg color.RGBA) *image.RGBA {
	t.Helper()
	td := &recordingDrawer{}
	light := solidRGBA(64, 64, color.RGBA{210, 225, 255, 255})
	ir := &imageResolver{cache: map[string]*image.RGBA{"bg": light}}
	node := theme.RVNode{BgColor: bg, TextColor: color.RGBA{0, 0, 0, 255}, BgImage: &theme.RVImage{Ref: "bg", Mode: "stretch"}}
	root := buildStatusTree("WWWW", node, 18, 6, 8, 2.0, td, ir, nil)
	Layout(root, 0, 0, td)
	dc, img := newSharedDrawContext(root.Rect().Dx(), root.Rect().Dy())
	PaintTree(root, dc, img, td)
	return img
}

// TestStatusCorner_NoDarkFringe 回归（P8 切片6 status/tooltip/toast 圆角深色毛边修复）：
// 深色半透明底 + 浅色背景图 → 圆角抗锯齿边缘透出深色底色（毛边）；改用「不透明 + 同调浅色」底色后消除。
// 主题层修复（jidian status/tooltip/toast 底色改不透明浅蓝），此处用引擎行为佐证并守护原则。
func TestStatusCorner_NoDarkFringe(t *testing.T) {
	const n = 12
	darkFringe := darkFringeCount(renderStatusCorner(t, color.RGBA{0x3C, 0x3C, 0x3C, 0xF0}), n)  // 深色半透明底：现状有毛边
	lightFringe := darkFringeCount(renderStatusCorner(t, color.RGBA{0xEA, 0xF1, 0xFC, 0xFF}), n) // 不透明浅底：修复
	if darkFringe == 0 {
		t.Error("预期深色半透明底 + 浅图在圆角产生深色毛边（作为对照）；若为 0 说明前提变了")
	}
	if lightFringe != 0 {
		t.Errorf("不透明浅底 + 浅图圆角不应有深色毛边，got %d 个像素", lightFringe)
	}
}
