//go:build windows

package ui

// viewbox_toolbar.go — 工具栏 View 树构建与颜色解析（P4-C）。
// 仅 Win：依赖 Windows 专属 ToolbarRenderer（darwin 无浮动工具栏）。
// View 承载整条背景/边框 + 4 按钮背景框布局 + mode 文字；grip 点阵 / 全半角符号(●/月牙) /
// 标点双符号 / 齿轮 这些矢量符号保留为后处理（定位用 Layout 后各按钮 Rect()）。
// 复用 P4-A resolveTokenColor + newSharedDrawContext。

import (
	"image/color"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// resolveToolbarViews 解析工具栏颜色：默认从 Palette.Toolbar 映射（P5），views.toolbar token 覆盖。
// button base 默认 = FullWidthOff*（非激活按钮底色/前景，零回归）；mode 中/英覆盖 background。
func (r *ToolbarRenderer) resolveToolbarViews() theme.ResolvedToolbarViews {
	// 内置默认色（无主题时回退）
	rtv := theme.ResolvedToolbarViews{
		BarBg:         color.RGBA{255, 255, 255, 255},
		BarBorder:     color.RGBA{199, 209, 224, 255},
		Grip:          color.RGBA{153, 173, 199, 179},
		ButtonBg:      color.RGBA{230, 234, 239, 255},
		ButtonText:    color.RGBA{89, 102, 122, 255},
		ModeChineseBg: color.RGBA{51, 154, 245, 255},
		ModeEnglishBg: color.RGBA{115, 127, 148, 255},
		ModeText:      color.RGBA{255, 255, 255, 255},
		SettingsBg:    color.RGBA{230, 234, 239, 255},
		SettingsIcon:  color.RGBA{122, 102, 184, 255},
		SettingsHole:  color.RGBA{230, 234, 239, 255},
	}
	rv := r.resolvedV25
	if rv == nil {
		return rtv
	}
	tb := rv.Palette.Toolbar
	rtv = theme.ResolvedToolbarViews{
		BarBg:         tb.Background,
		BarBorder:     tb.Border,
		Grip:          tb.Grip,
		ButtonBg:      tb.FullWidthOffBg,
		ButtonText:    tb.FullWidthOffText,
		ModeChineseBg: tb.ModeChineseBg,
		ModeEnglishBg: tb.ModeEnglishBg,
		ModeText:      tb.ModeText,
		SettingsBg:    tb.SettingsBg,
		SettingsIcon:  tb.SettingsIcon,
		SettingsHole:  tb.SettingsHole,
	}
	if rv.Views == nil || rv.Views.Toolbar == nil {
		return rtv
	}
	t := rv.Views.Toolbar
	// v3：toolbar views token 名与 colors 的 toolbar_* token 对齐。
	res := func(name string) color.Color {
		switch name {
		case "toolbar_background":
			return tb.Background
		case "toolbar_border":
			return tb.Border
		case "toolbar_grip":
			return tb.Grip
		case "toolbar_full_width_off_bg":
			return tb.FullWidthOffBg
		case "toolbar_full_width_off_text":
			return tb.FullWidthOffText
		case "toolbar_mode_chinese_bg":
			return tb.ModeChineseBg
		case "toolbar_mode_english_bg":
			return tb.ModeEnglishBg
		case "toolbar_mode_text":
			return tb.ModeText
		case "toolbar_settings_icon":
			return tb.SettingsIcon
		case "toolbar_settings_hole":
			return tb.SettingsHole
		}
		return nil
	}
	set := func(dst *color.Color, s string) {
		if c := resolveTokenColor(s, res); c != nil {
			*dst = c
		}
	}
	set(&rtv.BarBg, t.Background.Color)
	set(&rtv.BarBorder, t.Border.Color)
	set(&rtv.Grip, t.Grip.Color)
	set(&rtv.ButtonBg, t.Button.Background.Color)
	set(&rtv.ButtonText, t.Button.Color)
	if t.Button.Mode != nil {
		set(&rtv.ModeChineseBg, t.Button.Mode.Chinese.Background.Color)
		set(&rtv.ModeEnglishBg, t.Button.Mode.English.Background.Color)
	}
	set(&rtv.SettingsBg, t.Settings.Background.Color)
	set(&rtv.SettingsIcon, t.Settings.Icon.Color)
	set(&rtv.SettingsHole, t.Settings.Hole.Color)
	return rtv
}

// toolbarTree 持有 root + 各按钮 View 引用（后处理矢量符号定位用其 Rect()）。
type toolbarTree struct {
	root, grip, mode, width, punct, settings *View
}

// modeButtonText 复刻现状 mode 按钮文字选择逻辑。
func modeButtonText(state ToolbarState) string {
	if state.ChineseMode {
		if state.ModeLabel != "" {
			return state.ModeLabel
		}
		return "中"
	}
	if state.CapsLock {
		return "A"
	}
	return "英"
}

// buildToolbarTree 构建工具栏 View 树（整条 LayoutRow：grip + 4 按钮）。
// 按钮框走 View（Background+Border，Stretch 撑满整条高 - margin）；mode 是带背景的文本叶子；
// width/punct/settings 是无 Text 的框（符号后处理）。grip 是占位框。
// 几何 hardcode × scale，与现状 Render 的按钮布局逐像素一致。
func buildToolbarTree(state ToolbarState, rtv theme.ResolvedToolbarViews, scale float64) *toolbarTree {
	gripW := int(float64(gripWidth) * scale)
	btnW := int(float64(buttonWidth) * scale)
	pad := int(float64(buttonPadding) * scale)
	btnRadius := int(4.0 * scale)
	fontSize := 14.0 * scale

	// grip 占位框（Stretch 撑高，后处理画点阵）
	grip := &View{FixedW: gripW, Stretch: true}

	// 按钮框（无 Text，符号后处理）：FixedW = btnW-pad*2，margin pad，Stretch 撑高
	mkFrame := func(bg color.Color) *View {
		return &View{
			FixedW:     btnW - pad*2,
			Margin:     Edges{Top: pad, Right: pad, Bottom: pad, Left: pad},
			Background: Fill{Color: bg},
			Border:     Border{Radius: btnRadius},
			Stretch:    true,
		}
	}

	modeBg := rtv.ModeEnglishBg
	if state.ChineseMode {
		modeBg = rtv.ModeChineseBg
	}
	// mode 是带背景的文本叶子（文字居中由 paintText AlignCenter + 基线居中）
	mode := &View{
		Text:       modeButtonText(state),
		TextStyle:  TextStyle{FontSize: fontSize, Color: rtv.ModeText, Align: AlignCenter},
		FixedW:     btnW - pad*2,
		Margin:     Edges{Top: pad, Right: pad, Bottom: pad, Left: pad},
		Background: Fill{Color: modeBg},
		Border:     Border{Radius: btnRadius},
		Stretch:    true,
	}

	width := mkFrame(rtv.ButtonBg)
	punct := mkFrame(rtv.ButtonBg)
	settings := mkFrame(rtv.SettingsBg)

	root := &View{
		FixedW:     int(float64(toolbarBaseWidth) * scale),
		FixedH:     int(float64(toolbarBaseHeight) * scale),
		Layout:     LayoutRow,
		Background: Fill{Color: rtv.BarBg},
		Border:     Border{Radius: int(6.0 * scale), Color: rtv.BarBorder, Width: 1},
		Children:   []*View{grip, mode, width, punct, settings},
	}
	return &toolbarTree{root: root, grip: grip, mode: mode, width: width, punct: punct, settings: settings}
}
