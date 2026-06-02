package theme

import (
	"path/filepath"
	"runtime"
	"testing"
)

// builtinThemesDir 返回仓库内 build/data/themes 的绝对路径，用于端到端加载真实内置主题
func builtinThemesDir(t *testing.T) string {
	t.Helper()
	_, file, _, _ := runtime.Caller(0)
	// file = .../wind_input/pkg/theme/builtin_themes_test.go
	// build/data/themes 在 file 向上 4 层后的 build/data/themes
	root := filepath.Join(filepath.Dir(file), "..", "..", "..", "build", "data", "themes")
	abs, err := filepath.Abs(root)
	if err != nil {
		t.Fatal(err)
	}
	return abs
}

// TestBuiltinDefaultTheme 加载实际 build/data/themes/default 主题，
// 确认 v2.5 路径产出合理的 ResolvedV25（颜色非零、index style 模板生效）
func TestBuiltinDefaultTheme(t *testing.T) {
	dir := builtinThemesDir(t)
	m := &Manager{themeDirs: []string{dir}}

	if err := m.LoadTheme("default"); err != nil {
		t.Fatalf("LoadTheme default: %v (themesDir=%s)", err, dir)
	}
	r := m.GetResolvedV25()
	if r == nil {
		t.Fatal("resolved nil")
	}
	// 默认主题用 circle index style
	if !r.Layout.CandidateWindow.CandidateList.Index.Circle {
		t.Errorf("default index want circle")
	}
	// primary #4285F4 应反映在 IndexBg
	if ColorToHexRGB(r.Palette.CandidateWindow.IndexBg) != "#4285F4" {
		t.Errorf("default IndexBg want #4285F4, got %s", ColorToHexRGB(r.Palette.CandidateWindow.IndexBg))
	}
}

// TestBuiltinMsimeTheme 加载实际 msime 主题，确认数字 index 模板 + accent 蓝
func TestBuiltinMsimeTheme(t *testing.T) {
	dir := builtinThemesDir(t)
	m := &Manager{themeDirs: []string{dir}}

	if err := m.LoadTheme("msime"); err != nil {
		t.Fatalf("LoadTheme msime: %v", err)
	}
	r := m.GetResolvedV25()
	idx := r.Layout.CandidateWindow.CandidateList.Index
	if idx.Circle {
		t.Errorf("msime index want text (circle off)")
	}
	if got := BuildIndexLabelsFromSlots(idx.Labels); got != "1/2/3/4/5/6/7/8/9/0" {
		t.Errorf("msime IndexLabels want digit template, got %q", got)
	}
	if ColorToHexRGB(r.Palette.CandidateWindow.IndexBg) != "#0078D4" {
		t.Errorf("msime IndexBg want #0078D4, got %s", ColorToHexRGB(r.Palette.CandidateWindow.IndexBg))
	}
	// msime 应启用 accent bar（选中项左侧蓝色条）
	if !r.Layout.CandidateWindow.CandidateList.AccentBar.Enabled {
		t.Errorf("msime AccentBar.Enabled want true")
	}
	if ColorToHexRGB(r.Palette.CandidateWindow.AccentBar) != "#0078D4" {
		t.Errorf("msime AccentBar want #0078D4, got %s", ColorToHexRGB(r.Palette.CandidateWindow.AccentBar))
	}
}

// TestListAvailableThemes 验证下划线前缀目录（_layouts / _palettes）不被列为主题
func TestListAvailableThemes_SkipsUnderscoreDirs(t *testing.T) {
	dir := builtinThemesDir(t)
	m := &Manager{themeDirs: []string{dir}}
	themes := m.ListAvailableThemes()
	for _, name := range themes {
		if name == "_layouts" || name == "_palettes" {
			t.Errorf("零件目录 %q 不应出现在主题列表", name)
		}
	}
	// 至少应列出 default 和 msime
	has := func(s string) bool {
		for _, x := range themes {
			if x == s {
				return true
			}
		}
		return false
	}
	if !has("default") || !has("msime") {
		t.Errorf("主题列表缺失 default/msime: %v", themes)
	}
}

// TestBuiltinDarkMode 验证 dark mode 切换走 v2.5 path 并产出 dark 变体颜色
func TestBuiltinDarkMode(t *testing.T) {
	dir := builtinThemesDir(t)
	m := &Manager{themeDirs: []string{dir}}
	m.SetDarkMode(true)

	if err := m.LoadTheme("default"); err != nil {
		t.Fatalf("LoadTheme default: %v", err)
	}
	r := m.GetResolvedV25()
	if ColorToHexRGB(r.Palette.CandidateWindow.Background) != "#2D2D2D" {
		t.Errorf("default dark bg want #2D2D2D, got %s", ColorToHexRGB(r.Palette.CandidateWindow.Background))
	}
}
