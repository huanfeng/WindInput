package theme

import (
	"os"
	"path/filepath"
	"testing"
)

// TestManagerLoadV3Theme 端到端验证 Manager.LoadTheme 能正确处理 v3 base 继承主题：
// 写入真实 base 主题 + 派生主题，调用 LoadTheme，断言 resolved 来自 v3 路径（含继承）。
func TestManagerLoadV3Theme(t *testing.T) {
	tmp, cleanup := setupTestThemes(t)
	defer cleanup()

	// 派生主题：base = test-base（颜色/几何全继承），无 views 块。
	themeYAML := `meta:
  name: "v3-test"
  version: "1.0"
base: test-base
`
	themeDir := filepath.Join(tmp, "v3-test")
	if err := os.MkdirAll(themeDir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(themeDir, "theme.yaml"), []byte(themeYAML), 0o644); err != nil {
		t.Fatal(err)
	}

	m := &Manager{themeDirs: []string{tmp}}
	if err := m.LoadTheme("v3-test"); err != nil {
		t.Fatalf("LoadTheme v3-test: %v", err)
	}

	rv := m.GetResolvedV25()
	if rv == nil {
		t.Fatal("resolved nil")
	}
	// 该主题无 views 块 → 走 defaultViews 基线（无圆背景 + 默认数字标签）。
	vi := rv.Views.Index
	if vi.Background.Shape == "circle" {
		t.Errorf("IndexStyle want none (no circle bg)")
	}
	if got := BuildIndexLabelsFromSlots(vi.Labels); got != "1/2/3/4/5/6/7/8/9/0" {
		t.Errorf("IndexLabels want default digits, got %q", got)
	}
	// colors 继承 + 展开（selection = ${primary} = #4285F4）。
	if ColorToHexRGB(rv.Palette.Tokens["selection"]) != "#4285F4" {
		t.Errorf("selection want #4285F4, got %s", ColorToHexRGB(rv.Palette.Tokens["selection"]))
	}
}

// TestManagerV3DarkModeSwitch 验证暗色模式切换走 v3 路径
func TestManagerV3DarkModeSwitch(t *testing.T) {
	tmp, cleanup := setupTestThemes(t)
	defer cleanup()
	themeYAML := `meta: {name: "v3-dark"}
base: test-base
`
	dir := filepath.Join(tmp, "v3-dark")
	_ = os.MkdirAll(dir, 0o755)
	_ = os.WriteFile(filepath.Join(dir, "theme.yaml"), []byte(themeYAML), 0o644)

	m := &Manager{themeDirs: []string{tmp}}
	if err := m.LoadTheme("v3-dark"); err != nil {
		t.Fatal(err)
	}
	light := m.GetResolvedV25()
	lightBg := ColorToHexRGB(light.Palette.Bg)

	m.SetDarkMode(true)
	dark := m.GetResolvedV25()
	darkBg := ColorToHexRGB(dark.Palette.Bg)

	if lightBg == darkBg {
		t.Errorf("dark mode 切换后 bg 未变化: %s == %s", lightBg, darkBg)
	}
	if darkBg != "#2D2D2D" {
		t.Errorf("dark bg want #2D2D2D, got %s", darkBg)
	}
}
