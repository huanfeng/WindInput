//go:build windows

package ui

import (
	"image/color"
	"testing"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// TestResolveToolbarViews_BaseAndMode base 映射 + mode 色映射 + settings。
func TestResolveToolbarViews_BaseAndMode(t *testing.T) {
	tb := theme.ResolvedToolbarPalette{
		Background:       color.RGBA{255, 255, 255, 255},
		Border:           color.RGBA{199, 209, 224, 255},
		Grip:             color.RGBA{153, 173, 199, 179},
		ModeChineseBg:    color.RGBA{51, 154, 245, 255},
		ModeEnglishBg:    color.RGBA{115, 127, 148, 255},
		ModeText:         color.RGBA{255, 255, 255, 255},
		FullWidthOffBg:   color.RGBA{230, 234, 239, 255},
		FullWidthOffText: color.RGBA{89, 102, 122, 255},
		SettingsBg:       color.RGBA{230, 234, 239, 255},
		SettingsIcon:     color.RGBA{122, 102, 184, 255},
		SettingsHole:     color.RGBA{230, 234, 239, 255},
	}
	r := &ToolbarRenderer{resolvedV3: &theme.ResolvedV3{Palette: theme.ResolvedPalette{Toolbar: tb}}}
	rtv := r.resolveToolbarViews()
	if rtv.ButtonBg != color.Color(tb.FullWidthOffBg) {
		t.Error("button base bg 应=FullWidthOffBg")
	}
	if rtv.ButtonText != color.Color(tb.FullWidthOffText) {
		t.Error("button base text 应=FullWidthOffText")
	}
	if rtv.ModeChineseBg != color.Color(tb.ModeChineseBg) || rtv.ModeText != color.Color(tb.ModeText) {
		t.Error("mode 色映射错误")
	}
	if rtv.SettingsIcon != color.Color(tb.SettingsIcon) {
		t.Error("settings icon 映射错误")
	}
}

// TestBuildToolbarTree_Geometry 验证整条宽高 + mode 按钮背景按 state 选色。
func TestBuildToolbarTree_Geometry(t *testing.T) {
	rtv := theme.ResolvedToolbarViews{
		BarBg: color.RGBA{255, 255, 255, 255}, BarBorder: color.RGBA{1, 2, 3, 255},
		ButtonBg: color.RGBA{230, 234, 239, 255}, ButtonText: color.RGBA{89, 102, 122, 255},
		ModeChineseBg: color.RGBA{51, 154, 245, 255}, ModeEnglishBg: color.RGBA{115, 127, 148, 255},
		ModeText:   color.RGBA{255, 255, 255, 255},
		SettingsBg: color.RGBA{230, 234, 239, 255}, SettingsIcon: color.RGBA{1, 1, 1, 255}, SettingsHole: color.RGBA{2, 2, 2, 255},
	}
	m := fixedMeasurer{charW: 10}
	geom := resolveToolbarGeom(nil, 1.0)
	tt := buildToolbarTree(ToolbarState{ChineseMode: true, ModeLabel: "拼"}, rtv, geom)
	Layout(tt.root, 0, 0, m)
	// 整条宽 = 114（grip 10 + 4×26 槽位；L2 去除旧 116 尾部死区）, 高 = 30（scale=1）
	if tt.root.Rect().Dx() != 114 || tt.root.Rect().Dy() != 30 {
		t.Errorf("整条尺寸应 114x30, got %dx%d", tt.root.Rect().Dx(), tt.root.Rect().Dy())
	}
	// 按钮框 Stretch 撑高 = 30 - pad*2 = 26
	if tt.mode.Rect().Dy() != 26 {
		t.Errorf("按钮框高应 26（Stretch）, got %d", tt.mode.Rect().Dy())
	}
	// 中文模式：mode 按钮背景 = ModeChineseBg
	if tt.mode.Background.Color != (color.RGBA{51, 154, 245, 255}) {
		t.Errorf("中文模式 mode 背景应=ModeChineseBg, got %v", tt.mode.Background.Color)
	}
	// 英文模式
	tt2 := buildToolbarTree(ToolbarState{ChineseMode: false}, rtv, geom)
	if tt2.mode.Background.Color != (color.RGBA{115, 127, 148, 255}) {
		t.Errorf("英文模式 mode 背景应=ModeEnglishBg, got %v", tt2.mode.Background.Color)
	}
	// 按钮左缘定位与现状一致：settings 框 Min.X = 90
	Layout(tt2.root, 0, 0, m)
	if tt2.settings.Rect().Min.X != 90 {
		t.Errorf("settings 框左缘应 90, got %d", tt2.settings.Rect().Min.X)
	}
}

// TestToolbarGeometry_SingleSource 守护 L1：命中/边界/尺寸均从一次 buildToolbarTree+Layout 派生，
// 且与旧线性公式逐项等价（scale=1：grip=10, button=26, pad=2, H=30）。
func TestToolbarGeometry_SingleSource(t *testing.T) {
	m := fixedMeasurer{charW: 10}
	tt := buildToolbarTree(ToolbarState{ChineseMode: true}, theme.ResolvedToolbarViews{}, resolveToolbarGeom(nil, 1.0))
	Layout(tt.root, 0, 0, m)

	// content 矩形（GetButtonBounds 语义）= 旧 GetButtonBounds 公式
	type rectWant struct {
		name             string
		v                *View
		minX, minY, w, h int
	}
	for _, c := range []rectWant{
		{"grip", tt.grip, 0, 0, 10, 30},
		{"mode", tt.mode, 12, 2, 22, 26},
		{"width", tt.width, 38, 2, 22, 26},
		{"punct", tt.punct, 64, 2, 22, 26},
		{"settings", tt.settings, 90, 2, 22, 26},
	} {
		r := c.v.Rect()
		if r.Min.X != c.minX || r.Min.Y != c.minY || r.Dx() != c.w || r.Dy() != c.h {
			t.Errorf("%s content=(%d,%d,%d,%d), 期望(%d,%d,%d,%d)",
				c.name, r.Min.X, r.Min.Y, r.Dx(), r.Dy(), c.minX, c.minY, c.w, c.h)
		}
	}

	// 命中带（HitTest 语义）= margin 盒：与旧 x-分段一致、满高 [0,30)、首尾相接平铺
	type bandWant struct {
		name       string
		v          *View
		minX, maxX int
	}
	prevMax := 0
	for _, b := range []bandWant{
		{"grip", tt.grip, 0, 10},
		{"mode", tt.mode, 10, 36},
		{"width", tt.width, 36, 62},
		{"punct", tt.punct, 62, 88},
		{"settings", tt.settings, 88, 114},
	} {
		hb := viewOuterRect(b.v)
		if hb.Min.X != b.minX || hb.Max.X != b.maxX {
			t.Errorf("%s 命中带 x=[%d,%d), 期望[%d,%d)", b.name, hb.Min.X, hb.Max.X, b.minX, b.maxX)
		}
		if hb.Min.Y != 0 || hb.Max.Y != 30 {
			t.Errorf("%s 命中带应满高[0,30), got[%d,%d)", b.name, hb.Min.Y, hb.Max.Y)
		}
		if hb.Min.X != prevMax {
			t.Errorf("%s 命中带应与前带相接：期望 Min.X=%d, got %d", b.name, prevMax, hb.Min.X)
		}
		prevMax = hb.Max.X
	}

	// 整条尺寸（GetToolbarSize 语义）= grip 10 + 4×26 槽位 = 114（去尾部死区）
	if sz := tt.root.Rect().Size(); sz.X != 114 || sz.Y != 30 {
		t.Errorf("整条尺寸应 114x30, got %dx%d", sz.X, sz.Y)
	}
}

// tbDim 构造 *theme.Dimension（dp）供测试覆盖几何。
func tbDim(v int) *theme.Dimension { d := theme.Dp(v); return &d }

// TestResolveToolbarGeom_SchemaOverride 守护 L2：views.toolbar 几何字段覆盖默认、总宽随之变化；
// 未覆盖项保持默认（零回归）。
func TestResolveToolbarGeom_SchemaOverride(t *testing.T) {
	rv := &theme.ResolvedV3{Views: &theme.Views{Toolbar: &theme.ToolbarViews{
		GripWidth:   tbDim(12),
		ButtonWidth: tbDim(30),
	}}}
	g := resolveToolbarGeom(rv, 1.0)
	if g.gripWidth != 12 || g.buttonWidth != 30 {
		t.Errorf("schema 覆盖未生效: gripW=%d btnW=%d", g.gripWidth, g.buttonWidth)
	}
	if g.height != 30 || g.buttonPadding != 2 || g.buttonRadius != 4 {
		t.Errorf("未覆盖项应保持默认: h=%d pad=%d r=%d", g.height, g.buttonPadding, g.buttonRadius)
	}
	// 总宽随之变化 = grip + 4×button 槽位
	tt := buildToolbarTree(ToolbarState{}, theme.ResolvedToolbarViews{}, g)
	Layout(tt.root, 0, 0, zeroMeasurer{})
	if w, want := tt.root.Rect().Dx(), 12+4*30; w != want {
		t.Errorf("覆盖后总宽应=%d, got %d", want, w)
	}
}

// TestResolveToolbarGeom_NilDefaults 守护：rv 为 nil 时回退内置默认（grip 10 + 4×26 = 114）。
func TestResolveToolbarGeom_NilDefaults(t *testing.T) {
	g := resolveToolbarGeom(nil, 1.0)
	if g.gripWidth != 10 || g.buttonWidth != 26 || g.height != 30 {
		t.Errorf("默认几何错误: gripW=%d btnW=%d h=%d", g.gripWidth, g.buttonWidth, g.height)
	}
}
