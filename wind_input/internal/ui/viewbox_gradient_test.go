//go:build windows

package ui

import (
	"image/color"
	"testing"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// TestPaintGradientBackground 验证 Fill.Gradient 经 paintShapes 栅格化绘制。
// 底色垫底（白）——模拟真实 window/独立窗口 root 有默认底色的场景，避免 blendOver
// 的 dst-alpha 门控（全透明画布会整体跳过渐变）。
func TestPaintGradientBackground(t *testing.T) {
	red := color.RGBA{255, 0, 0, 255}
	blue := color.RGBA{0, 0, 255, 255}
	v := &View{
		FixedW: 20, FixedH: 4,
		Background: Fill{
			Color: color.RGBA{255, 255, 255, 255}, // 底色垫底
			Gradient: &theme.RVGradient{Type: "linear", Angle: 0, Stops: []theme.RVGradientStop{
				{Color: red, Pos: 0}, {Color: blue, Pos: 1},
			}},
		},
	}
	Layout(v, 0, 0, fixedMeasurer{charW: 1})
	dc, img := newSharedDrawContext(20, 4)
	v.paintShapes(dc, img)

	left := img.RGBAAt(1, 2)
	right := img.RGBAAt(18, 2)
	if left.R < 150 || left.B > 100 {
		t.Errorf("渐变左侧应偏红，got %+v", left)
	}
	if right.B < 150 || right.R > 100 {
		t.Errorf("渐变右侧应偏蓝，got %+v", right)
	}

	// 无渐变 + 纯底色：像素=底色（白），证明渐变是叠加能力、不影响零回归。
	v2 := &View{FixedW: 4, FixedH: 4, Background: Fill{Color: color.RGBA{255, 255, 255, 255}}}
	Layout(v2, 0, 0, fixedMeasurer{charW: 1})
	dc2, img2 := newSharedDrawContext(4, 4)
	v2.paintShapes(dc2, img2)
	if px := img2.RGBAAt(2, 2); px.R < 250 || px.B < 250 {
		t.Errorf("无渐变应为纯白底色，got %+v", px)
	}
}
