package theme

import (
	"os"
	"path/filepath"
	"testing"
)

// v3-C：外链 _layouts/_palettes + Overrides 机制已删，改为 base 单链继承内联块。
// 本测试构造一个 base 主题（test-base）+ 派生主题，覆盖继承 / 求值 / 暗色。

// 共享 base 主题：colors（含 derive）+ views（候选项圆角，验证 views 继承）。
// V3-D：layout/density 几何块已删，继承覆盖改用 colors/views 验证。
const sampleBaseThemeYAML = `meta: {name: "test-base", version: "1.0"}
colors:
  primary: "#4285F4"
  derive: {enabled: true, algorithm: hsl-shift}
  bg:        { light: "#FFFFFF", dark: "#2D2D2D" }
  text:      { light: "#1E1E1E", dark: "#E0E0E0" }
  selection: "${primary}"
views:
  item:
    border: {radius: 7}
`

// 创建一个临时主题目录结构（写入 base 主题）。
func setupTestThemes(t *testing.T) (themesDir string, cleanup func()) {
	t.Helper()
	tmp, err := os.MkdirTemp("", "themetest")
	if err != nil {
		t.Fatal(err)
	}
	cleanup = func() { os.RemoveAll(tmp) }

	mustWrite := func(p, content string) {
		if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(p, []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	mustWrite(filepath.Join(tmp, "test-base", "theme.yaml"), sampleBaseThemeYAML)

	return tmp, cleanup
}

// 直接构造一个手工 Manager 指向临时 themesDir
func makeTestManager(themesDir string) *Manager {
	return &Manager{themeDirs: []string{themesDir}}
}

// loadMerged 加载并 base 合并一个主题，返回合并后的 raw Theme（供测试直接 ResolveV25）。
func loadMerged(t *testing.T, m *Manager, name string) *Theme {
	t.Helper()
	th, _, err := m.loadThemeFileWithDir(name)
	if err != nil {
		t.Fatalf("loadThemeFileWithDir %s: %v", name, err)
	}
	return th
}

func TestResolveV25_Inherited(t *testing.T) {
	tmp, cleanup := setupTestThemes(t)
	defer cleanup()
	m := makeTestManager(tmp)

	// 派生主题：base = test-base，仅自带 behavior（颜色/几何全继承）。
	derived := `meta: {name: "derived"}
base: test-base
`
	dir := filepath.Join(tmp, "derived")
	_ = os.MkdirAll(dir, 0o755)
	_ = os.WriteFile(filepath.Join(dir, "theme.yaml"), []byte(derived), 0o644)

	th := loadMerged(t, m, "derived")
	r, err := m.ResolveV25(th, false, dir)
	if err != nil {
		t.Fatalf("ResolveV25: %v", err)
	}
	// views 继承（V3-D：layout 几何块已删，改用 views 继承验证）：派生主题仅自带 behavior，
	// item 圆角应从 base 的 views.item.border.radius=7 继承。
	if r.Views == nil || r.Views.Item.Border.Radius == nil {
		t.Fatalf("继承 views.item.border.radius 缺失")
	}
	if got := r.Views.Item.Border.Radius.Value; got != 7 {
		t.Errorf("继承 views.item.border.radius want 7, got %d", got)
	}
	// colors 继承 + ${} 展开（selection = ${primary}）
	if ColorToHexRGB(r.Palette.Tokens["selection"]) != "#4285F4" {
		t.Errorf("selection should be primary, got %s", ColorToHexRGB(r.Palette.Tokens["selection"]))
	}
	if ColorToHexRGB(r.Palette.Bg) != "#FFFFFF" {
		t.Errorf("bg should be #FFFFFF, got %s", ColorToHexRGB(r.Palette.Bg))
	}
}

func TestResolveV25_DarkMode(t *testing.T) {
	tmp, cleanup := setupTestThemes(t)
	defer cleanup()
	m := makeTestManager(tmp)

	th := loadMerged(t, m, "test-base")
	r, err := m.ResolveV25(th, true, tmp)
	if err != nil {
		t.Fatal(err)
	}
	if ColorToHexRGB(r.Palette.Bg) != "#2D2D2D" {
		t.Errorf("dark bg want #2D2D2D, got %s", ColorToHexRGB(r.Palette.Bg))
	}
}

func TestResolveV25_UnknownBase(t *testing.T) {
	tmp, cleanup := setupTestThemes(t)
	defer cleanup()
	m := makeTestManager(tmp)

	bad := `meta: {name: "bad"}
base: nosuch-base
`
	dir := filepath.Join(tmp, "bad")
	_ = os.MkdirAll(dir, 0o755)
	_ = os.WriteFile(filepath.Join(dir, "theme.yaml"), []byte(bad), 0o644)

	if _, _, err := m.loadThemeFileWithDir("bad"); err == nil {
		t.Errorf("expected error for unknown base theme")
	}
}

// TestInheritResolveOrder 是 A 修正「先合并后求值」的回归守护：
// base 给 primary + derive.enabled；派生主题仅覆盖 primary——断言派生主题的语义色
// 基于**新 primary** 重新派生（而非沿用 base 旧派生值）。
func TestInheritResolveOrder(t *testing.T) {
	tmp, cleanup := setupTestThemes(t)
	defer cleanup()
	m := makeTestManager(tmp)

	// base：只给 primary + 开启 derive（不显式给任何语义色，全靠派生）。
	base := `meta: {name: "io-base"}
colors:
  primary: "#4285F4"
  derive: {enabled: true, algorithm: hsl-shift}
`
	bdir := filepath.Join(tmp, "io-base")
	_ = os.MkdirAll(bdir, 0o755)
	_ = os.WriteFile(filepath.Join(bdir, "theme.yaml"), []byte(base), 0o644)

	// 派生：仅换 primary（accent 应跟着变成新 primary）。
	derived := `meta: {name: "io-derived"}
base: io-base
colors:
  primary: "#FF0000"
`
	ddir := filepath.Join(tmp, "io-derived")
	_ = os.MkdirAll(ddir, 0o755)
	_ = os.WriteFile(filepath.Join(ddir, "theme.yaml"), []byte(derived), 0o644)

	// base 自身：accent = 旧 primary。
	bth := loadMerged(t, m, "io-base")
	br, err := m.ResolveV25(bth, false, bdir)
	if err != nil {
		t.Fatalf("ResolveV25 base: %v", err)
	}
	if got := ColorToHexRGB(br.Palette.Accent); got != "#4285F4" {
		t.Fatalf("base accent want #4285F4, got %s", got)
	}

	// 派生：accent 必须 = 新 primary #FF0000（证明 derive 在合并后基于新 primary 重跑）。
	dth := loadMerged(t, m, "io-derived")
	dr, err := m.ResolveV25(dth, false, ddir)
	if err != nil {
		t.Fatalf("ResolveV25 derived: %v", err)
	}
	if got := ColorToHexRGB(dr.Palette.Accent); got != "#FF0000" {
		t.Errorf("派生主题只换 primary，accent 应基于新 primary 重新派生为 #FF0000, got %s "+
			"（若为 #4285F4 说明 derive 误用 base 旧值——先求值后合并 bug）", got)
	}
	if got := ColorToHexRGB(dr.Palette.Primary); got != "#FF0000" {
		t.Errorf("派生 primary want #FF0000, got %s", got)
	}
}
