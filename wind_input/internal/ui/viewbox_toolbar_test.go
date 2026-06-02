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
	r := &ToolbarRenderer{resolvedV25: &theme.ResolvedV25{Palette: theme.ResolvedPalette{Toolbar: tb}}}
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
	tt := buildToolbarTree(ToolbarState{ChineseMode: true, ModeLabel: "拼"}, rtv, 1.0)
	Layout(tt.root, 0, 0, m)
	// 整条宽 = 116, 高 = 30（scale=1）
	if tt.root.Rect().Dx() != 116 || tt.root.Rect().Dy() != 30 {
		t.Errorf("整条尺寸应 116x30, got %dx%d", tt.root.Rect().Dx(), tt.root.Rect().Dy())
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
	tt2 := buildToolbarTree(ToolbarState{ChineseMode: false}, rtv, 1.0)
	if tt2.mode.Background.Color != (color.RGBA{115, 127, 148, 255}) {
		t.Errorf("英文模式 mode 背景应=ModeEnglishBg, got %v", tt2.mode.Background.Color)
	}
	// 按钮左缘定位与现状一致：settings 框 Min.X = 90
	Layout(tt2.root, 0, 0, m)
	if tt2.settings.Rect().Min.X != 90 {
		t.Errorf("settings 框左缘应 90, got %d", tt2.settings.Rect().Min.X)
	}
}
