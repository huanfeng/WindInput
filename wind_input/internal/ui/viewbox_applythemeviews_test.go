package ui

import (
	"image/color"
	"testing"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// TestApplyThemeViews 验证主题 views 仅用显式字段覆盖合成桥 base（几何 + 颜色），未写字段保留 base。
func TestApplyThemeViews(t *testing.T) {
	base := NewRenderer(parityConfig()).buildResolvedViews() // Item.PadLeft=8, PadRight=8
	right10 := 10
	selBg := color.RGBA{9, 9, 9, 255}
	cand := theme.ResolvedCandidateWindowPalette{SelectedBg: selBg, Text: color.RGBA{1, 2, 3, 255}}
	tv := &theme.Views{
		Item: theme.ViewNode{
			Padding:  theme.ViewEdges{Right: &right10},
			Selected: &theme.ViewNode{Background: theme.ViewFill{Color: "${selected_bg}"}},
		},
		Text: theme.ViewNode{Color: "${text}"},
	}
	applyThemeViews(&base, tv, cand, nil)

	if base.Item.PadRight != 10 {
		t.Errorf("item pad right 应被主题覆盖为 10, got %d", base.Item.PadRight)
	}
	if base.Item.PadLeft != 8 {
		t.Errorf("item pad left 未写应保留 base 8, got %d", base.Item.PadLeft)
	}
	if base.ItemHeight != 32 {
		t.Errorf("杂项 ItemHeight 应保留 base（YAML 不配）, got %v", base.ItemHeight)
	}
	if base.Item.SelectedBg != selBg {
		t.Error("item selected bg 应被 ${selected_bg} token 覆盖")
	}
	if base.Text.TextColor != cand.Text {
		t.Error("text color 应被 ${text} token 覆盖")
	}
}

// TestResolveViewColor 验证颜色字段解析：${name} token / hex / 未知 / 空。
func TestResolveViewColor(t *testing.T) {
	cand := theme.ResolvedCandidateWindowPalette{Text: color.RGBA{1, 2, 3, 255}}
	if resolveViewColor("${text}", cand, nil) != cand.Text {
		t.Error("${text} 应映射 TextColor")
	}
	if resolveViewColor("#FF0000", cand, nil) == nil {
		t.Error("hex 应解析")
	}
	if resolveViewColor("${unknown}", cand, nil) != nil {
		t.Error("未知 token 应返回 nil（不覆盖）")
	}
	if resolveViewColor("", cand, nil) != nil {
		t.Error("空应返回 nil")
	}
}
