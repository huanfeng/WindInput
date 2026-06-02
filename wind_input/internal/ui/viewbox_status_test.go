package ui

import (
	"image/color"
	"testing"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// TestResolveTokenColor 验证通用 token 解析：hex 直解 / ${name} 走 resolver / 未知与空回退 nil。
func TestResolveTokenColor(t *testing.T) {
	res := func(name string) color.Color {
		if name == "background" {
			return color.RGBA{1, 2, 3, 255}
		}
		return nil
	}
	if c := resolveTokenColor("${background}", res); c != (color.RGBA{1, 2, 3, 255}) {
		t.Errorf("token 应解析为 resolver 值, got %v", c)
	}
	if c := resolveTokenColor("${unknown}", res); c != nil {
		t.Errorf("未知 token 应回退 nil, got %v", c)
	}
	if c := resolveTokenColor("", res); c != nil {
		t.Errorf("空串应回退 nil, got %v", c)
	}
	if c := resolveTokenColor("#FF0000", res); c == nil {
		t.Error("hex 应解析为非 nil")
	}
}

// TestResolveStatusColors 验证状态泡颜色优先级：自定义 cfg > views token(Palette.Status) > 默认。
func TestResolveStatusColors(t *testing.T) {
	mi := theme.ResolvedStatusPalette{
		Background: color.RGBA{10, 20, 30, 255},
		Text:       color.RGBA{200, 200, 200, 255},
	}
	rv := &theme.ResolvedV25{
		Palette: theme.ResolvedPalette{Status: mi},
		Views: &theme.Views{Status: &theme.ViewNode{
			Background: theme.ViewFill{Color: "${background}"},
			Color:      "${text}",
		}},
	}
	r := &StatusRenderer{resolvedV25: rv}

	// views token → Palette.Status
	rsv := r.resolveStatusColors(StatusWindowConfig{})
	if rsv.BgColor != mi.Background {
		t.Errorf("bg 应来自 Palette.Status, got %v", rsv.BgColor)
	}
	if rsv.TextColor != mi.Text {
		t.Errorf("text 应来自 Palette.Status, got %v", rsv.TextColor)
	}

	// 自定义 cfg 优先
	rsv2 := r.resolveStatusColors(StatusWindowConfig{BackgroundColor: "#FF0000", TextColor: "#00FF00"})
	if rsv2.BgColor == mi.Background {
		t.Error("自定义 bg 应覆盖 views token")
	}
}

// TestBuildStatusTree_Fingerprint 验证状态泡 View 树几何 + 颜色指纹（零回归基准）。
// 单节点文本 View：FixedW（minWidth 钳制）+ Padding + 居中文本 + bg 圆角。
func TestBuildStatusTree_Fingerprint(t *testing.T) {
	rsv := theme.ResolvedStatusViews{
		BgColor:   color.RGBA{60, 60, 60, 240},
		TextColor: color.RGBA{255, 255, 255, 255},
	}
	// 桩：固定字宽，scale=1，避免依赖真实字体后端
	m := fixedMeasurer{charW: 10}
	root := buildStatusTree("中", rsv, 18.0, 6.0, 8.0, 1.0, m)
	Layout(root, 0, 0, m)

	r := root.Rect()
	// 文本 "中" 宽 10，padding 6*2=12 → 22，minWidth 32 钳制 → FixedW 32
	if r.Dx() != 32 {
		t.Errorf("width 应被 minWidth 钳制为 32, got %d", r.Dx())
	}
	// 高 = fontSize 18 + padding 12 = 30
	if r.Dy() != 30 {
		t.Errorf("height 应为 30, got %d", r.Dy())
	}
	if root.Background.Color != (color.RGBA{60, 60, 60, 240}) {
		t.Errorf("bg 颜色指纹不符, got %v", root.Background.Color)
	}
	if root.TextStyle.Color != (color.RGBA{255, 255, 255, 255}) {
		t.Errorf("text 颜色指纹不符, got %v", root.TextStyle.Color)
	}
	if root.TextStyle.Align != AlignCenter {
		t.Error("状态泡文本应水平居中")
	}
}

// TestSharedDrawContext_ShapesVisible 锁住共享缓冲坑：dc 绘制必须实时反映到 img
// （gogpu/gg 的 NewContext().Image() 是快照，会导致背景/文字丢失——P4-A 曾踩此坑）。
func TestSharedDrawContext_ShapesVisible(t *testing.T) {
	dc, img := newSharedDrawContext(10, 10)
	dc.SetColor(color.RGBA{255, 0, 0, 255})
	dc.DrawRectangle(0, 0, 10, 10)
	dc.Fill()
	if _, _, _, a := img.At(5, 5).RGBA(); a == 0 {
		t.Fatal("dc 绘制未反映到 img（dc/img 未共享缓冲）")
	}
}

type fixedMeasurer struct{ charW float64 }

func (f fixedMeasurer) MeasureString(s string, fontSize float64) float64 {
	return float64(len([]rune(s))) * f.charW
}
