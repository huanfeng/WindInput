package ui

// viewbox_menu.go — 弹出菜单 View 树构建与颜色解析（P4-D）。
// root LayoutColumn + 每项 LayoutRow（check/text/arrow），勾选✓/箭头▸/文本走 View 文本叶子；
// 分隔线后处理（矢量，定位用分隔项 Rect()）。复用 resolveTokenColor + newSharedDrawContext。

import (
	"image/color"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// resolveMenuColors 解析菜单 7 色：默认从 Palette.PopupMenu（P5），views.menu token 覆盖。
func (m *PopupMenu) resolveMenuColors() theme.ResolvedMenuViews {
	// 内置默认色（无主题时回退）
	rmv := theme.ResolvedMenuViews{
		BgColor:        color.RGBA{255, 255, 255, 255},
		BorderColor:    color.RGBA{199, 199, 199, 255},
		TextColor:      color.RGBA{0, 0, 0, 255},
		DisabledColor:  color.RGBA{161, 161, 161, 255},
		HoverBgColor:   color.RGBA{0, 120, 212, 255},
		HoverTextColor: color.RGBA{255, 255, 255, 255},
		SeparatorColor: color.RGBA{219, 219, 219, 255},
	}
	rv := m.resolvedV25
	if rv == nil {
		return rmv
	}
	pm := rv.Palette.PopupMenu
	rmv = theme.ResolvedMenuViews{
		BgColor: pm.Background, BorderColor: pm.Border, TextColor: pm.Text,
		DisabledColor: pm.Disabled, HoverBgColor: pm.HoverBg,
		HoverTextColor: pm.HoverText, SeparatorColor: pm.Separator,
	}
	if rv.Views == nil || rv.Views.Menu == nil {
		return rmv
	}
	mv := rv.Views.Menu
	res := func(name string) color.Color {
		switch name {
		case "background":
			return pm.Background
		case "border":
			return pm.Border
		case "text":
			return pm.Text
		case "disabled":
			return pm.Disabled
		case "hover_bg":
			return pm.HoverBg
		case "hover_text":
			return pm.HoverText
		case "separator":
			return pm.Separator
		}
		return nil
	}
	set := func(dst *color.Color, s string) {
		if c := resolveTokenColor(s, res); c != nil {
			*dst = c
		}
	}
	set(&rmv.BgColor, mv.Background.Color)
	set(&rmv.BorderColor, mv.Border.Color)
	set(&rmv.TextColor, mv.Color)
	set(&rmv.SeparatorColor, mv.Separator.Color)
	set(&rmv.DisabledColor, mv.Disabled)
	set(&rmv.HoverBgColor, mv.Hover.Background.Color)
	set(&rmv.HoverTextColor, mv.Hover.Color)
	return rmv
}

// menuTree 持有 root + 分隔项 View 引用（分隔线后处理定位用其 Rect()）。
type menuTree struct {
	root       *View
	separators []*View
}

// buildMenuTree 构建菜单 View 树（root LayoutColumn + 每项 LayoutRow）。
// width/height 用预算值（与命中测试一致）。hoverIdx/submenuIdx 决定 hover 态。
// 勾选✓/箭头▸/文本走 View 文本叶子；分隔项收集到 separators 供后处理画线。
func buildMenuTree(items []MenuItem, hoverIdx, submenuIdx int, hasChecked, hasChildren bool, rmv theme.ResolvedMenuViews, width, height int, baseFontSize float64, itemHeightLogical int, scale float64) *menuTree {
	fontSize := baseFontSize * scale
	itemH := int(float64(itemHeightLogical) * scale)
	sepH := int(float64(menuSeparatorHeight) * scale)
	padY := int(float64(menuPaddingY) * scale)
	padXHalf := int(float64(menuPaddingX) * scale / 2)
	checkW := 0
	if hasChecked {
		checkW = int(float64(menuCheckMarkWidth) * scale)
	}
	arrowW := 0
	if hasChildren {
		arrowW = int(float64(menuArrowWidth) * scale)
	}
	radius := int(float64(menuCornerRadius) * scale)

	root := &View{
		FixedW:     width,
		FixedH:     height,
		Layout:     LayoutColumn,
		Padding:    Edges{Top: padY, Bottom: padY},
		Background: Fill{Color: rmv.BgColor},
		Border:     Border{Radius: radius, Color: rmv.BorderColor, Width: 1},
	}
	mt := &menuTree{root: root}

	for i := range items {
		item := items[i]
		if item.Separator {
			sep := &View{FixedH: sepH, Stretch: true}
			root.Children = append(root.Children, sep)
			mt.separators = append(mt.separators, sep)
			continue
		}
		isHovered := (i == hoverIdx && !item.Disabled) || (i == submenuIdx)
		textColor := rmv.TextColor
		switch {
		case item.Disabled:
			textColor = rmv.DisabledColor
		case isHovered:
			textColor = rmv.HoverTextColor
		}

		row := &View{
			FixedH:     itemH,
			Layout:     LayoutRow,
			Stretch:    true,        // 撑满 root 宽（hover 满宽）
			CrossAlign: AlignCenter, // check/text/arrow 在 itemH 内垂直居中
			Padding:    Edges{Left: padXHalf, Right: padXHalf},
		}
		if isHovered {
			row.Background = Fill{Color: rmv.HoverBgColor}
		}

		if hasChecked {
			check := &View{FixedW: checkW}
			if item.Checked {
				check.Text = "✓"
				check.TextStyle = TextStyle{FontSize: fontSize, Color: textColor, Align: AlignCenter}
			}
			row.Children = append(row.Children, check)
		}
		text := &View{
			Text:      item.Text,
			TextStyle: TextStyle{FontSize: fontSize, Color: textColor},
			Margin:    Edges{Left: padXHalf},
			Grow:      true,
		}
		row.Children = append(row.Children, text)
		if hasChildren {
			arrow := &View{FixedW: arrowW}
			if len(item.Children) > 0 {
				arrow.Text = "▸"
				arrow.TextStyle = TextStyle{FontSize: fontSize, Color: textColor, Align: AlignCenter}
			}
			row.Children = append(row.Children, arrow)
		}
		root.Children = append(root.Children, row)
	}
	return mt
}
