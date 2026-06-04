package theme

import (
	"os"
	"path/filepath"
	"runtime"
	"slices"
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
	// build/ 是构建产物（被 .gitignore 的 /build/ 忽略），未构建或 CI checkout 后不存在。
	// 与 dict 包测试一致：数据目录缺失时跳过端到端主题加载，而非 t.Fatalf 硬失败。
	if _, statErr := os.Stat(abs); os.IsNotExist(statErr) {
		t.Skipf("跳过测试：内置主题目录不存在（需先构建 build/data/themes）：%s", abs)
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
	// 默认主题用 circle 序号背景（P7-5：归口 views.index.background.shape）
	if r.Views.Index.Background.Shape != "circle" {
		t.Errorf("default index want circle bg, got %q", r.Views.Index.Background.Shape)
	}
	// primary #4285F4 应反映在 accent token（序号底色源）
	if ColorToHexRGB(r.Palette.Accent) != "#4285F4" {
		t.Errorf("default accent want #4285F4, got %s", ColorToHexRGB(r.Palette.Accent))
	}
	// behavior 块显式声明：单页不显翻页区、多页显页码
	if r.Behavior.AlwaysShowPager {
		t.Errorf("default always_show_pager want false (单页不显翻页区)")
	}
	if !r.Behavior.ShowPageNumber {
		t.Errorf("default show_page_number want true")
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
	// P7-5：序号样式/标签/强调条开关归口 views。
	vi := r.Views.Index
	if vi.Background.Shape == "circle" {
		t.Errorf("msime index want none (circle off)")
	}
	if got := BuildIndexLabelsFromSlots(vi.Labels); got != "1/2/3/4/5/6/7/8/9/0" {
		t.Errorf("msime IndexLabels want digit template, got %q", got)
	}
	if ColorToHexRGB(r.Palette.Accent) != "#0078D4" {
		t.Errorf("msime accent want #0078D4, got %s", ColorToHexRGB(r.Palette.Accent))
	}
	// msime 应启用 accent bar（选中项左侧蓝色条）。V3-D：归位到 accent_bar 节点。
	if ab := r.Views.AccentBar; ab.Enabled == nil || !*ab.Enabled {
		t.Errorf("msime accent_bar.enabled want true")
	}
	if ColorToHexRGB(r.Palette.Accent) != "#0078D4" {
		t.Errorf("msime accent_bar source want #0078D4, got %s", ColorToHexRGB(r.Palette.Accent))
	}
	// behavior 块显式声明：单页不显翻页区、多页显页码
	if r.Behavior.AlwaysShowPager {
		t.Errorf("msime always_show_pager want false (单页不显翻页区)")
	}
	if !r.Behavior.ShowPageNumber {
		t.Errorf("msime show_page_number want true")
	}
}

// TestListAvailableThemes 验证下划线前缀的隐藏基础主题 _base 不被列为主题，
// 且旧 base 主题 windy-blue/msime-base 已合并删除、不再出现。
func TestListAvailableThemes_SkipsUnderscoreDirs(t *testing.T) {
	dir := builtinThemesDir(t)
	m := &Manager{themeDirs: []string{dir}}
	themes := m.ListAvailableThemes()
	// _ 前缀隐藏基础主题不应出现在主题列表
	if slices.Contains(themes, "_base") {
		t.Errorf("隐藏基础主题 _base 不应出现在主题列表: %v", themes)
	}
	// 旧 base 主题已合并进 _base 并删除
	if slices.Contains(themes, "windy-blue") || slices.Contains(themes, "msime-base") {
		t.Errorf("旧 base 主题 windy-blue/msime-base 应已删除: %v", themes)
	}
	// 至少应列出 default 和 msime
	if !slices.Contains(themes, "default") || !slices.Contains(themes, "msime") {
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
	if ColorToHexRGB(r.Palette.Bg) != "#2D2D2D" {
		t.Errorf("default dark bg want #2D2D2D, got %s", ColorToHexRGB(r.Palette.Bg))
	}
}
