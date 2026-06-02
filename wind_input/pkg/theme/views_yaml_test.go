package theme

import (
	"os"
	"testing"

	"gopkg.in/yaml.v3"
)

// TestDefaultThemeViewsParse 验证真实 default/theme.yaml 的 views 块能解析为 Theme.Views，
// 且关键颜色 token 就位（兜底运行时加载，避免 YAML 语法/结构错导致主题加载失败）。
func TestDefaultThemeViewsParse(t *testing.T) {
	data, err := os.ReadFile("../../themes/default/theme.yaml")
	if err != nil {
		t.Skip("default theme.yaml 不可读: " + err.Error())
	}
	var th Theme
	if err := yaml.Unmarshal(data, &th); err != nil {
		t.Fatalf("default theme.yaml 解析失败: %v", err)
	}
	if th.Views == nil {
		t.Fatal("default theme.yaml 应含 views 块")
	}
	if th.Views.Window.Background.Color != "${background}" {
		t.Errorf("window background token, got %q", th.Views.Window.Background.Color)
	}
	if th.Views.Item.Selected == nil || th.Views.Item.Selected.Background.Color != "${selected_bg}" {
		t.Error("item selected bg token 缺失")
	}
	if th.Views.AccentBar.Background.Color != "${accent}" {
		t.Errorf("accent_bar token, got %q", th.Views.AccentBar.Background.Color)
	}
	if th.Views.Index.Color != "${index_text}" {
		t.Errorf("index text token, got %q", th.Views.Index.Color)
	}
}

// TestDefaultThemeStatusParse 验证 default theme.yaml 的 views.status 解析（P4-A）。
func TestDefaultThemeStatusParse(t *testing.T) {
	data, err := os.ReadFile("../../themes/default/theme.yaml")
	if err != nil {
		t.Skip("default theme.yaml 不可读: " + err.Error())
	}
	var th Theme
	if err := yaml.Unmarshal(data, &th); err != nil {
		t.Fatalf("解析失败: %v", err)
	}
	if th.Views == nil || th.Views.Status == nil {
		t.Fatal("default theme.yaml 应含 views.status")
	}
	if th.Views.Status.Background.Color != "${background}" {
		t.Errorf("status background token, got %q", th.Views.Status.Background.Color)
	}
	if th.Views.Status.Color != "${text}" {
		t.Errorf("status text token, got %q", th.Views.Status.Color)
	}
}

// TestDefaultThemeTooltipParse 验证 default theme.yaml 的 views.tooltip（P4-B）。
func TestDefaultThemeTooltipParse(t *testing.T) {
	data, err := os.ReadFile("../../themes/default/theme.yaml")
	if err != nil {
		t.Skip("default theme.yaml 不可读: " + err.Error())
	}
	var th Theme
	if err := yaml.Unmarshal(data, &th); err != nil {
		t.Fatalf("解析失败: %v", err)
	}
	if th.Views == nil || th.Views.Tooltip == nil || th.Views.Tooltip.Background.Color != "${background}" {
		t.Fatal("default theme.yaml 应含 views.tooltip 且 background token")
	}
}

// TestDefaultThemeToolbarParse 验证 default theme.yaml 的 views.toolbar（P4-C）。
func TestDefaultThemeToolbarParse(t *testing.T) {
	data, err := os.ReadFile("../../themes/default/theme.yaml")
	if err != nil {
		t.Skip("default theme.yaml 不可读: " + err.Error())
	}
	var th Theme
	if err := yaml.Unmarshal(data, &th); err != nil {
		t.Fatalf("解析失败: %v", err)
	}
	if th.Views == nil || th.Views.Toolbar == nil || th.Views.Toolbar.Button.Mode == nil {
		t.Fatal("default theme.yaml 应含 views.toolbar.button.mode")
	}
	if th.Views.Toolbar.Button.Mode.Chinese.Background.Color != "${mode_cn_bg}" {
		t.Errorf("mode_cn_bg token, got %q", th.Views.Toolbar.Button.Mode.Chinese.Background.Color)
	}
}

// TestDefaultThemeMenuParse 验证 default theme.yaml 的 views.menu（P4-D）。
func TestDefaultThemeMenuParse(t *testing.T) {
	data, err := os.ReadFile("../../themes/default/theme.yaml")
	if err != nil {
		t.Skip("default theme.yaml 不可读: " + err.Error())
	}
	var th Theme
	if err := yaml.Unmarshal(data, &th); err != nil {
		t.Fatalf("解析失败: %v", err)
	}
	if th.Views == nil || th.Views.Menu == nil || th.Views.Menu.Hover.Background.Color != "${hover_bg}" {
		t.Fatal("default theme.yaml 应含 views.menu.hover.background")
	}
}

// TestMsimeThemeViewsParse 验证 msime/theme.yaml 的 views 块解析 + 关键字段（item radius 2 / 颜色 token）。
func TestMsimeThemeViewsParse(t *testing.T) {
	data, err := os.ReadFile("../../themes/msime/theme.yaml")
	if err != nil {
		t.Skip("msime theme.yaml 不可读: " + err.Error())
	}
	var th Theme
	if err := yaml.Unmarshal(data, &th); err != nil {
		t.Fatalf("msime theme.yaml 解析失败: %v", err)
	}
	if th.Views == nil {
		t.Fatal("msime theme.yaml 应含 views 块")
	}
	if th.Views.Item.Border.Radius == nil || th.Views.Item.Border.Radius.Value != 4 {
		t.Errorf("msime item radius 应为 4, got %v", th.Views.Item.Border.Radius)
	}
	// 窗口边框=1px 发丝线（设备像素，不随 DPI 加粗）——匹配旧渲染器的固定 1px 边框。
	if w := th.Views.Window.Border.Width; w == nil || w.Value != 1 || !w.Px {
		t.Errorf("msime window border width 应为 1px(设备像素), got %v", w)
	}
	if th.Views.Window.Background.Color != "${background}" {
		t.Errorf("window background token, got %q", th.Views.Window.Background.Color)
	}
	if th.Views.Index.Color != "${index_text}" {
		t.Errorf("index text token, got %q", th.Views.Index.Color)
	}
}

// TestViews_StatusParse 验证 views.status 块解析到 Views.Status（ViewNode）。
func TestViews_StatusParse(t *testing.T) {
	data := `
status:
  background: {color: "${background}"}
  color: "${text}"
`
	var v Views
	if err := yaml.Unmarshal([]byte(data), &v); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if v.Status == nil {
		t.Fatal("views.status 应解析为非 nil")
	}
	if v.Status.Background.Color != "${background}" {
		t.Errorf("status background token, got %q", v.Status.Background.Color)
	}
	if v.Status.Color != "${text}" {
		t.Errorf("status text token, got %q", v.Status.Color)
	}
}

// TestViews_TooltipParse 验证 views.tooltip 解析到 Views.Tooltip。
func TestViews_TooltipParse(t *testing.T) {
	var v Views
	if err := yaml.Unmarshal([]byte("tooltip:\n  background: {color: \"${background}\"}\n  color: \"${text}\"\n"), &v); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if v.Tooltip == nil || v.Tooltip.Background.Color != "${background}" || v.Tooltip.Color != "${text}" {
		t.Fatalf("tooltip token 解析错误: %+v", v.Tooltip)
	}
}

// TestViews_ToolbarParse 验证 views.toolbar 的 button base + mode 状态覆盖解析。
func TestViews_ToolbarParse(t *testing.T) {
	data := `
toolbar:
  background: {color: "${background}"}
  grip: {color: "${grip}"}
  button:
    background: {color: "${button_bg}"}
    color: "${button_text}"
    mode:
      chinese: {background: {color: "${mode_cn_bg}"}}
      english: {background: {color: "${mode_en_bg}"}}
  settings:
    icon: {color: "${settings_icon}"}
    hole: {color: "${settings_hole}"}
`
	var v Views
	if err := yaml.Unmarshal([]byte(data), &v); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if v.Toolbar == nil {
		t.Fatal("views.toolbar 应非 nil")
	}
	if v.Toolbar.Button.Background.Color != "${button_bg}" {
		t.Errorf("button base bg token, got %q", v.Toolbar.Button.Background.Color)
	}
	if v.Toolbar.Button.Mode == nil || v.Toolbar.Button.Mode.Chinese.Background.Color != "${mode_cn_bg}" {
		t.Error("mode.chinese bg 覆盖缺失")
	}
	if v.Toolbar.Settings.Icon.Color != "${settings_icon}" {
		t.Errorf("settings icon token, got %q", v.Toolbar.Settings.Icon.Color)
	}
}

// TestViews_MenuParse 验证 views.menu 解析（含 hover 状态）。
func TestViews_MenuParse(t *testing.T) {
	data := `
menu:
  background: {color: "${background}"}
  color: "${text}"
  separator: {color: "${separator}"}
  disabled: "${disabled}"
  hover:
    background: {color: "${hover_bg}"}
    color: "${hover_text}"
`
	var v Views
	if err := yaml.Unmarshal([]byte(data), &v); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if v.Menu == nil {
		t.Fatal("views.menu 应非 nil")
	}
	if v.Menu.Color != "${text}" || v.Menu.Disabled != "${disabled}" {
		t.Errorf("menu text/disabled token 错误: %+v", v.Menu)
	}
	if v.Menu.Hover.Background.Color != "${hover_bg}" || v.Menu.Hover.Color != "${hover_text}" {
		t.Error("menu hover 覆盖缺失")
	}
}

// TestViews_YAMLUnmarshal 验证 views 块 YAML 解析为 *int 显式语义（未写=nil）。
func TestViews_YAMLUnmarshal(t *testing.T) {
	data := `
window:
  padding: {top: 6, right: 8, bottom: 6, left: 8}
  border: {radius: 8}
item:
  padding: {left: 8, right: 10}
  border: {radius: 4}
text:
  margin: {left: 4}
comment:
  margin: {left: 6}
`
	var v Views
	if err := yaml.Unmarshal([]byte(data), &v); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if v.Window.Padding.Top == nil || v.Window.Padding.Top.Value != 6 {
		t.Errorf("window padding top: %v", v.Window.Padding.Top)
	}
	if v.Window.Border.Radius == nil || v.Window.Border.Radius.Value != 8 {
		t.Errorf("window border radius: %v", v.Window.Border.Radius)
	}
	if v.Item.Padding.Right == nil || v.Item.Padding.Right.Value != 10 {
		t.Errorf("item padding right: %v", v.Item.Padding.Right)
	}
	if v.Text.Margin.Left == nil || v.Text.Margin.Left.Value != 4 {
		t.Errorf("text margin left: %v", v.Text.Margin.Left)
	}
	// 未写字段应为 nil（显式语义）
	if v.Item.Margin.Left != nil {
		t.Errorf("item margin left 未写应为 nil, got %v", *v.Item.Margin.Left)
	}
	if v.Window.Padding.Right == nil || v.Window.Padding.Right.Value != 8 {
		t.Errorf("window padding right: %v", v.Window.Padding.Right)
	}
}
