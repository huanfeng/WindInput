package theme

import (
	"os"
	"path/filepath"
	"testing"
)

// TestManagerLoadV25Theme 端到端验证 Manager.LoadTheme 能正确处理 v2.5 主题：
// 写入真实 theme.yaml + _layouts + _palettes，调用 LoadTheme，断言 resolved 来自 v2.5 路径
func TestManagerLoadV25Theme(t *testing.T) {
	tmp, cleanup := setupTestThemes(t)
	defer cleanup()

	// 写入一个 v2.5 主题入口
	themeYAML := `meta:
  name: "v25-test"
  version: "1.0"
layout: test-layout
palette: test-palette
`
	themeDir := filepath.Join(tmp, "v25-test")
	if err := os.MkdirAll(themeDir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(themeDir, "theme.yaml"), []byte(themeYAML), 0o644); err != nil {
		t.Fatal(err)
	}

	m := &Manager{themeDirs: []string{tmp}}
	if err := m.LoadTheme("v25-test"); err != nil {
		t.Fatalf("LoadTheme v25-test: %v", err)
	}

	rv := m.GetResolvedV25()
	if rv == nil {
		t.Fatal("resolved nil")
	}
	idx := rv.Layout.CandidateWindow.CandidateList.Index
	// index 模板生效（来自 layout test-layout 的 index.style="1."）
	if idx.Circle {
		t.Errorf("IndexStyle want text (circle off)")
	}
	if got := BuildIndexLabelsFromSlots(idx.Labels); got != "1./2./3./4./5./6./7./8./9./0." {
		t.Errorf("IndexLabels want slash-template, got %q", got)
	}
	// palette 解析（来自 test-palette 的 #4285F4 primary）
	if ColorToHexRGB(rv.Palette.CandidateWindow.SelectedBg) != "#4285F4" {
		t.Errorf("SelectedBg want #4285F4, got %s", ColorToHexRGB(rv.Palette.CandidateWindow.SelectedBg))
	}
}

// TestManagerV25DarkModeSwitch 验证暗色模式切换走 v2.5 路径
func TestManagerV25DarkModeSwitch(t *testing.T) {
	tmp, cleanup := setupTestThemes(t)
	defer cleanup()
	themeYAML := `meta: {name: "v25-dark"}
layout: test-layout
palette: test-palette
`
	dir := filepath.Join(tmp, "v25-dark")
	_ = os.MkdirAll(dir, 0o755)
	_ = os.WriteFile(filepath.Join(dir, "theme.yaml"), []byte(themeYAML), 0o644)

	m := &Manager{themeDirs: []string{tmp}}
	if err := m.LoadTheme("v25-dark"); err != nil {
		t.Fatal(err)
	}
	light := m.GetResolvedV25()
	lightBg := ColorToHexRGB(light.Palette.CandidateWindow.Background)

	m.SetDarkMode(true)
	dark := m.GetResolvedV25()
	darkBg := ColorToHexRGB(dark.Palette.CandidateWindow.Background)

	if lightBg == darkBg {
		t.Errorf("dark mode 切换后 bg 未变化: %s == %s", lightBg, darkBg)
	}
	if darkBg != "#2D2D2D" {
		t.Errorf("dark bg want #2D2D2D, got %s", darkBg)
	}
}
