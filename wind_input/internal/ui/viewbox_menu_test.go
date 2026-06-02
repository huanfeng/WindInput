package ui

import (
	"image/color"
	"testing"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// TestResolveMenuColors 7 色映射 Palette.PopupMenu。
func TestResolveMenuColors(t *testing.T) {
	pm := theme.ResolvedPopupMenuPalette{
		Background: color.RGBA{255, 255, 255, 255},
		Border:     color.RGBA{199, 199, 199, 255},
		Text:       color.RGBA{0, 0, 0, 255},
		Disabled:   color.RGBA{161, 161, 161, 255},
		HoverBg:    color.RGBA{0, 120, 212, 255},
		HoverText:  color.RGBA{255, 255, 255, 255},
		Separator:  color.RGBA{219, 219, 219, 255},
	}
	m := &PopupMenu{resolvedV25: &theme.ResolvedV25{Palette: theme.ResolvedPalette{PopupMenu: pm}}}
	rmv := m.resolveMenuColors()
	if rmv.BgColor != color.Color(pm.Background) || rmv.HoverBgColor != color.Color(pm.HoverBg) ||
		rmv.DisabledColor != color.Color(pm.Disabled) || rmv.SeparatorColor != color.Color(pm.Separator) {
		t.Errorf("menu 7 色映射错误: %+v", rmv)
	}
}

// TestBuildMenuTree_Geometry 验证菜单项布局 + hover/disabled 状态色 + 勾选/箭头 + 分隔项收集。
func TestBuildMenuTree_Geometry(t *testing.T) {
	rmv := theme.ResolvedMenuViews{
		BgColor: color.RGBA{255, 255, 255, 255}, BorderColor: color.RGBA{1, 2, 3, 255},
		TextColor: color.RGBA{0, 0, 0, 255}, DisabledColor: color.RGBA{161, 161, 161, 255},
		HoverBgColor: color.RGBA{0, 120, 212, 255}, HoverTextColor: color.RGBA{255, 255, 255, 255},
		SeparatorColor: color.RGBA{219, 219, 219, 255},
	}
	items := []MenuItem{
		{Text: "项目一", Checked: true},
		{Separator: true},
		{Text: "子菜单", Children: []MenuItem{{Text: "子项"}}},
		{Text: "禁用项", Disabled: true},
	}
	m := fixedMeasurer{charW: 14}
	// hoverIdx=0（项目一 hover），hasChecked=true，hasChildren=true
	mt := buildMenuTree(items, 0, -1, true, true, rmv, 200, 80, 14.0, 24, 1.0)
	Layout(mt.root, 0, 0, m)
	if mt.root.Background.Color != (color.RGBA{255, 255, 255, 255}) {
		t.Error("root bg 应=BgColor")
	}
	if len(mt.root.Children) != 4 {
		t.Fatalf("应 4 项（含分隔）, got %d", len(mt.root.Children))
	}
	// 项0 hover：背景 HoverBg
	if mt.root.Children[0].Background.Color != (color.RGBA{0, 120, 212, 255}) {
		t.Errorf("hover 项背景应=HoverBg, got %v", mt.root.Children[0].Background.Color)
	}
	// 分隔项收集到 separators
	if len(mt.separators) != 1 {
		t.Errorf("应 1 个分隔项, got %d", len(mt.separators))
	}
	// 禁用项（索引3）文本色 = DisabledColor：行的最后一个子节点是 arrow（hasChildren），text 是第 2 个（check/text/arrow）
	disabledRow := mt.root.Children[3]
	textCell := disabledRow.Children[1] // check, text, arrow
	if textCell.TextStyle.Color != (color.RGBA{161, 161, 161, 255}) {
		t.Errorf("禁用项文本色应=DisabledColor, got %v", textCell.TextStyle.Color)
	}
	// 文本在 itemH(24) 内垂直居中：text 高=14，应居中偏移 (24-14)/2=5
	row0 := mt.root.Children[0]
	text0 := row0.Children[1]
	if got := text0.Rect().Min.Y - row0.Rect().Min.Y; got != 5 {
		t.Errorf("菜单项文本应垂直居中(偏移 5), got %d", got)
	}
}
