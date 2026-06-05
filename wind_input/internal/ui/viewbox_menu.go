//go:build windows

package ui

// viewbox_menu.go — 弹出菜单 View 树构建与颜色解析（P4-D）。
// 仅 Win：依赖 Windows 专属 PopupMenu 渲染器（darwin 菜单走原生 Swift）。
// root LayoutColumn + 每项 LayoutRow（check/text/arrow），勾选✓/箭头▸/文本走 View 文本叶子；
// 分隔线后处理（矢量，定位用分隔项 Rect()）。复用 resolveTokenColor + newSharedDrawContext。

import (
	"image/color"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// resolveMenuViews 解析菜单盒模型 ResolvedMenuViews（P8 切片3）：默认从 Palette token，
// views.menu（Root/Item/Separator）覆盖几何+颜色，item 的 hover/disabled 走递归 RVNode patch（V3-D）。
func (m *PopupMenu) resolveMenuViews() theme.ResolvedMenuViews {
	if rv := m.resolvedV3; rv != nil {
		var mv *theme.MenuViews
		if rv.Views != nil {
			mv = rv.Views.Menu
		}
		return theme.ResolveMenuViews(mv, rv.Palette)
	}
	// 内置默认（无主题兜底，等价旧 7 色硬编码）
	return theme.ResolvedMenuViews{
		Root: theme.RVNode{BgColor: color.RGBA{255, 255, 255, 255}, BorderColor: color.RGBA{199, 199, 199, 255}},
		Item: theme.RVNode{
			TextColor: color.RGBA{0, 0, 0, 255},
			Hover:     &theme.RVNode{BgColor: color.RGBA{0, 120, 212, 255}, TextColor: color.RGBA{255, 255, 255, 255}},
			Disabled:  &theme.RVNode{TextColor: color.RGBA{161, 161, 161, 255}},
		},
		Separator: theme.RVNode{BgColor: color.RGBA{219, 219, 219, 255}},
	}
}

// menuTree 持有 root + 分隔项 View 引用（分隔线后处理定位用其 Rect()）。
type menuTree struct {
	root       *View
	separators []*View
}

// buildMenuTree 构建菜单 View 树（root LayoutColumn + 每项 LayoutRow）。
// width/height 用预算值（与命中测试一致）。hoverIdx/submenuIdx 决定 hover 态。
// 勾选✓/箭头▸/文本走 View 文本叶子；分隔项收集到 separators 供后处理画线。
func buildMenuTree(items []MenuItem, hoverIdx, submenuIdx int, hasChecked, hasChildren bool, rmv theme.ResolvedMenuViews, width, height int, baseFontSize float64, itemHeightLogical int, scale float64, ir *imageResolver, resources map[string]string) *menuTree {
	fontSize := (baseFontSize + rmv.Item.FontSize) * scale
	itemH := int(float64(itemHeightLogical) * scale)
	sepH := int(float64(menuSeparatorHeight) * scale)
	itemWeight := rmv.Item.FontWeight
	itemFamily := rmv.Item.FontFamily

	// root 上下 padding（views.menu.root 未配则兜底 menuPaddingY）
	padTop := rmv.Root.PadTop.Scaled(scale)
	padBottom := rmv.Root.PadBottom.Scaled(scale)
	if padTop == 0 && padBottom == 0 {
		p := int(float64(menuPaddingY) * scale)
		padTop, padBottom = p, p
	}
	// item 左右 padding（views.menu.item 未配则兜底 menuPaddingX/2）
	padL := rmv.Item.PadLeft.Scaled(scale)
	padR := rmv.Item.PadRight.Scaled(scale)
	if padL == 0 && padR == 0 {
		p := int(float64(menuPaddingX) * scale / 2)
		padL, padR = p, p
	}
	// root 左右 padding（views.menu.root；未配=0，零回归）
	rootPadL := rmv.Root.PadLeft.Scaled(scale)
	rootPadR := rmv.Root.PadRight.Scaled(scale)
	// item 上下 padding（views.menu.item；未配=0 → 行高=itemH，零回归）
	itemPadT := rmv.Item.PadTop.Scaled(scale)
	itemPadB := rmv.Item.PadBottom.Scaled(scale)
	checkW := 0
	if hasChecked {
		checkW = int(float64(menuCheckMarkWidth) * scale)
	}
	arrowW := 0
	if hasChildren {
		arrowW = int(float64(menuArrowWidth) * scale)
	}
	// root 圆角半径（兜底 menuCornerRadius）。边框不在此画——由 render 配合「内圆角 clip」后处理绘制，
	// 使 hover 满宽高亮裁到边框内侧、既不溢出圆角也不覆盖圆角边框（见 popup_menu_render.go）。
	radius := rmv.Root.BorderRadius.Scaled(scale)
	if radius == 0 {
		radius = int(float64(menuCornerRadius) * scale)
	}

	root := &View{
		FixedW:     width,
		FixedH:     height,
		Layout:     LayoutColumn,
		Padding:    Edges{Top: padTop, Bottom: padBottom, Left: rootPadL, Right: rootPadR},
		Background: ir.fillFor(rmv.Root.BgColor, rmv.Root.BgImage, rmv.Root.BgGradient, resources), // P8 切片6：菜单背景可带图/渐变（PaintTree 在内圆角 clip 内绘制）
		Border:     Border{Radius: radius},
	}
	ir.appendLayers(root, rmv.Root.Layers, resources, func(v float64) int { return int(v * scale) }) // P8 切片6：菜单装饰层
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
		textColor := rmv.Item.TextColor
		switch {
		case item.Disabled:
			if rmv.Item.Disabled != nil && rmv.Item.Disabled.TextColor != nil {
				textColor = rmv.Item.Disabled.TextColor
			}
		case isHovered:
			if rmv.Item.Hover != nil && rmv.Item.Hover.TextColor != nil {
				textColor = rmv.Item.Hover.TextColor
			}
		}

		// item 上下 padding 独立生效（仿候选项）：Padding{Top,Bottom} 缩减内容区，
		// FixedH=itemH+padT+padB 使内容区恰为 itemH、children 在其中居中。
		// 对称 padding 与旧版（FixedH=itemH、无上下 padding）逐像素一致；非对称不再被均摊。
		row := &View{
			FixedH:     itemH + itemPadT + itemPadB,
			Layout:     LayoutRow,
			Stretch:    true,        // 撑满 root 宽（hover 满宽）
			CrossAlign: AlignCenter, // check/text/arrow 在内容区垂直居中
			Padding:    Edges{Top: itemPadT, Bottom: itemPadB, Left: padL, Right: padR},
		}
		// 菜单项边框（color/width/radius）：基态读 views.menu.item.border，hover 态逐字段覆盖。
		// color 未配=不描边（零回归，与候选项/tooltip 统一）；radius 始终生效以裁 hover 满宽高亮圆角。
		itemBorderColor := rmv.Item.BorderColor
		itemBorderWidth := rmv.Item.BorderWidth
		itemBorderRadius := rmv.Item.BorderRadius
		if isHovered && rmv.Item.Hover != nil {
			h := rmv.Item.Hover
			if h.BorderColor != nil {
				itemBorderColor = h.BorderColor
			}
			if h.BorderWidth != (theme.Dimension{}) {
				itemBorderWidth = h.BorderWidth
			}
			if h.BorderRadius != (theme.Dimension{}) {
				itemBorderRadius = h.BorderRadius
			}
		}
		if isHovered && rmv.Item.Hover != nil && rmv.Item.Hover.BgColor != nil {
			row.Background = Fill{Color: rmv.Item.Hover.BgColor}
		}
		if itemBorderColor != nil || itemBorderRadius != (theme.Dimension{}) {
			row.Border = Border{Color: itemBorderColor, Width: itemBorderWidth.Scaled(scale), Radius: itemBorderRadius.Scaled(scale)}
		}

		if hasChecked {
			check := &View{FixedW: checkW}
			if item.Checked {
				check.Text = "✓"
				check.TextStyle = TextStyle{FontSize: fontSize, Color: textColor, Align: AlignCenter, Weight: itemWeight, Family: itemFamily}
			}
			row.Children = append(row.Children, check)
		}
		text := &View{
			Text:      item.Text,
			TextStyle: TextStyle{FontSize: fontSize, Color: textColor, Weight: itemWeight, Family: itemFamily},
			Margin:    Edges{Left: padL},
			Grow:      true,
		}
		row.Children = append(row.Children, text)
		if hasChildren {
			arrow := &View{FixedW: arrowW}
			if len(item.Children) > 0 {
				arrow.Text = "▸"
				arrow.TextStyle = TextStyle{FontSize: fontSize, Color: textColor, Align: AlignCenter, Weight: itemWeight, Family: itemFamily}
			}
			row.Children = append(row.Children, arrow)
		}
		root.Children = append(root.Children, row)
	}
	return mt
}
