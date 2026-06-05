//go:build windows

package ui

import (
	"image/color"
	"testing"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// TestResolveMenuColors 菜单 7 色映射 menu_* 功能 token（v3）。
func TestResolveMenuColors(t *testing.T) {
	bg := color.RGBA{255, 255, 255, 255}
	border := color.RGBA{199, 199, 199, 255}
	text := color.RGBA{0, 0, 0, 255}
	disabled := color.RGBA{161, 161, 161, 255}
	hoverBg := color.RGBA{0, 120, 212, 255}
	hoverText := color.RGBA{255, 255, 255, 255}
	separator := color.RGBA{219, 219, 219, 255}
	tokens := map[string]color.Color{
		"menu_bg":         bg,
		"menu_border":     border,
		"menu_text":       text,
		"menu_disabled":   disabled,
		"menu_hover_bg":   hoverBg,
		"menu_hover_text": hoverText,
		"menu_separator":  separator,
	}
	m := &PopupMenu{resolvedV3: &theme.ResolvedV3{Palette: theme.ResolvedPalette{Tokens: tokens}}}
	rmv := m.resolveMenuViews()
	if rmv.Root.BgColor != color.Color(bg) || rmv.Root.BorderColor != color.Color(border) ||
		rmv.Item.TextColor != color.Color(text) || rmv.Separator.BgColor != color.Color(separator) {
		t.Errorf("menu 颜色映射错误: %+v", rmv)
	}
	if rmv.Item.Hover == nil || rmv.Item.Hover.BgColor != color.Color(hoverBg) ||
		rmv.Item.Hover.TextColor != color.Color(hoverText) {
		t.Errorf("menu hover 态映射错误: %+v", rmv.Item.Hover)
	}
	if rmv.Item.Disabled == nil || rmv.Item.Disabled.TextColor != color.Color(disabled) {
		t.Errorf("menu disabled 态映射错误: %+v", rmv.Item.Disabled)
	}
}

// TestBuildMenuTree_Geometry 验证菜单项布局 + hover/disabled 状态色 + 勾选/箭头 + 分隔项收集。
func TestBuildMenuTree_Geometry(t *testing.T) {
	rmv := theme.ResolvedMenuViews{
		Root: theme.RVNode{BgColor: color.RGBA{255, 255, 255, 255}, BorderColor: color.RGBA{1, 2, 3, 255}},
		Item: theme.RVNode{
			TextColor: color.RGBA{0, 0, 0, 255},
			Hover:     &theme.RVNode{BgColor: color.RGBA{0, 120, 212, 255}, TextColor: color.RGBA{255, 255, 255, 255}},
			Disabled:  &theme.RVNode{TextColor: color.RGBA{161, 161, 161, 255}},
		},
		Separator: theme.RVNode{BgColor: color.RGBA{219, 219, 219, 255}},
	}
	items := []MenuItem{
		{Text: "项目一", Checked: true},
		{Separator: true},
		{Text: "子菜单", Children: []MenuItem{{Text: "子项"}}},
		{Text: "禁用项", Disabled: true},
	}
	m := fixedMeasurer{charW: 14}
	// hoverIdx=0（项目一 hover），hasChecked=true，hasChildren=true
	mt := buildMenuTree(items, 0, -1, true, true, rmv, 200, 80, 14.0, 24, 1.0, &imageResolver{}, nil)
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

// TestBuildMenuTree_ItemBorder 守护 menu.item 边框 color/width 真实生效（基态 + hover 逐字段覆盖）。
func TestBuildMenuTree_ItemBorder(t *testing.T) {
	baseBorder := color.RGBA{10, 20, 30, 255}
	hoverBorder := color.RGBA{40, 50, 60, 255}
	rmv := theme.ResolvedMenuViews{
		Root: theme.RVNode{BgColor: color.RGBA{255, 255, 255, 255}},
		Item: theme.RVNode{
			TextColor:   color.RGBA{0, 0, 0, 255},
			BorderColor: baseBorder,
			BorderWidth: theme.Dimension{Value: 2},
			Hover:       &theme.RVNode{BgColor: color.RGBA{0, 120, 212, 255}, BorderColor: hoverBorder},
		},
	}
	items := []MenuItem{{Text: "项目一"}, {Text: "项目二"}}
	m := fixedMeasurer{charW: 14}
	// hoverIdx=0：项0 hover、项1 基态；scale=1.0。
	mt := buildMenuTree(items, 0, -1, false, false, rmv, 200, 80, 14.0, 24, 1.0, &imageResolver{}, nil)
	Layout(mt.root, 0, 0, m)

	// 基态项（索引1）：边框 color=base、width=2（修复前只读 radius，color/width 被丢弃）。
	base := mt.root.Children[1]
	if base.Border.Color != color.Color(baseBorder) {
		t.Errorf("基态 menu.item 边框色应=base, got %v", base.Border.Color)
	}
	if base.Border.Width != 2 {
		t.Errorf("基态 menu.item 边框宽应=2, got %d", base.Border.Width)
	}
	// hover 项（索引0）：color 被 hover 覆盖、width 继承 base（hover 未配 width）。
	hov := mt.root.Children[0]
	if hov.Border.Color != color.Color(hoverBorder) {
		t.Errorf("hover menu.item 边框色应=hover, got %v", hov.Border.Color)
	}
	if hov.Border.Width != 2 {
		t.Errorf("hover menu.item 边框宽应继承 base=2, got %d", hov.Border.Width)
	}
}

// TestBuildMenuTree_Padding 守护 root 左右 padding 生效 + item 上下 padding 独立化（仿候选项）。
func TestBuildMenuTree_Padding(t *testing.T) {
	rmv := theme.ResolvedMenuViews{
		Root: theme.RVNode{
			BgColor:  color.RGBA{255, 255, 255, 255},
			PadLeft:  theme.Dimension{Value: 7},
			PadRight: theme.Dimension{Value: 7},
		},
		Item: theme.RVNode{
			TextColor: color.RGBA{0, 0, 0, 255},
			PadTop:    theme.Dimension{Value: 4},
			PadBottom: theme.Dimension{Value: 6},
		},
	}
	items := []MenuItem{{Text: "项目一"}}
	m := fixedMeasurer{charW: 14}
	// 无 check/arrow（hasChecked=false, hasChildren=false）；itemH=24, scale=1.0。
	mt := buildMenuTree(items, -1, -1, false, false, rmv, 200, 80, 14.0, 24, 1.0, &imageResolver{}, nil)
	Layout(mt.root, 0, 0, m)

	row := mt.root.Children[0]
	// root 左 padding 生效：行左缘 = root 左缘 + rootPadL(7)。
	if got := row.Rect().Min.X - mt.root.Rect().Min.X; got != 7 {
		t.Errorf("root 左 padding 应=7, got %d", got)
	}
	// item 上下 padding 独立：行高 = itemH(24) + padT(4) + padB(6) = 34。
	if got := row.Rect().Dy(); got != 34 {
		t.Errorf("item 行高应=itemH+padT+padB=34, got %d", got)
	}
	// 内容区高=itemH(24)，text(高14)在内容区居中：偏移 = padT(4) + (24-14)/2 = 9。
	text := row.Children[0]
	if got := text.Rect().Min.Y - row.Rect().Min.Y; got != 9 {
		t.Errorf("text 偏移应=padT+居中=9, got %d", got)
	}
}
