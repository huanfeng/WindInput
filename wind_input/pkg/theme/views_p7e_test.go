package theme

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"gopkg.in/yaml.v3"
)

// TestResourceRef_YAMLUnion 验证 P7-E：resources 条目支持标量与 {light,dark} 两种写法 + PathFor 选变体。
func TestResourceRef_YAMLUnion(t *testing.T) {
	var scalar ResourceRef
	if err := yaml.Unmarshal([]byte(`"panel.png"`), &scalar); err != nil {
		t.Fatal(err)
	}
	if scalar.Light != "panel.png" || scalar.Dark != "panel.png" {
		t.Errorf("标量写法应明暗共用: %+v", scalar)
	}

	var dual ResourceRef
	if err := yaml.Unmarshal([]byte("{light: a.png, dark: b.png}"), &dual); err != nil {
		t.Fatal(err)
	}
	if dual.PathFor(false) != "a.png" || dual.PathFor(true) != "b.png" {
		t.Errorf("PathFor 选变体错: light=%s dark=%s", dual.PathFor(false), dual.PathFor(true))
	}

	// 单侧给定 → 另一侧回退（保证单变体素材仍可用）
	var onlyDark ResourceRef
	if err := yaml.Unmarshal([]byte("{dark: d.png}"), &onlyDark); err != nil {
		t.Fatal(err)
	}
	if onlyDark.PathFor(false) != "d.png" {
		t.Errorf("缺 light 应回退 dark, got %s", onlyDark.PathFor(false))
	}
}

// TestViewShadowSpec_Resolve 验证 P7-E：结构化 shadow 的 offset_x/y + color 解析进 ResolvedViews
// （blur/spread 为预留字段，解析不报错、不消费）。
func TestViewShadowSpec_Resolve(t *testing.T) {
	pal := testPalette()
	v := defaultViews()
	// V3-D：shadow 归位到 window 节点。
	v.Window.Shadow = &ViewShadowSpec{OffsetX: dimp(3), OffsetY: dimp(5), Blur: dimp(8), Color: NewLightDark("#FF0000")}
	rv := ResolveCandidateViews(v, pal)
	if rv.ShadowOffsetX.Value != 3 || rv.ShadowOffsetY.Value != 5 {
		t.Errorf("structured shadow offset x/y 应解析: %v/%v", rv.ShadowOffsetX, rv.ShadowOffsetY)
	}
	if rv.ShadowColor == nil {
		t.Error("structured shadow color 应覆盖 palette.Shadow")
	}
}

// TestResources_DarkVariantSwitch 端到端验证 P7-E：resources 的 {light,dark} 变体经 ResolveV3
// 按 isDark 选路径——切暗色后 ResolvedV3.Resources 指向 dark 文件。
func TestResources_DarkVariantSwitch(t *testing.T) {
	tmp, cleanup := setupTestThemes(t)
	defer cleanup()
	themeYAML := `meta: {name: "v3-res"}
base: test-base
resources:
  panel: {light: "light-panel.png", dark: "dark-panel.png"}
  mark: "mark.png"
`
	dir := filepath.Join(tmp, "v3-res")
	_ = os.MkdirAll(dir, 0o755)
	_ = os.WriteFile(filepath.Join(dir, "theme.yaml"), []byte(themeYAML), 0o644)

	m := &Manager{themeDirs: []string{tmp}}
	if err := m.LoadTheme("v3-res"); err != nil {
		t.Fatal(err)
	}
	light := m.GetResolvedV3()
	if !strings.HasSuffix(light.Resources["panel"], "light-panel.png") {
		t.Errorf("亮色应选 light 变体, got %s", light.Resources["panel"])
	}
	if !strings.HasSuffix(light.Resources["mark"], "mark.png") {
		t.Errorf("单图写法应正常解析, got %s", light.Resources["mark"])
	}

	m.SetDarkMode(true)
	dark := m.GetResolvedV3()
	if !strings.HasSuffix(dark.Resources["panel"], "dark-panel.png") {
		t.Errorf("暗色应选 dark 变体, got %s", dark.Resources["panel"])
	}
	if !strings.HasSuffix(dark.Resources["mark"], "mark.png") {
		t.Errorf("单图写法暗色仍同一文件, got %s", dark.Resources["mark"])
	}
}

// TestViewGradient_MergePreserved 验证 P7-E：渐变字段位经 mergeViews 保留（schema 冻结，渲染 later）。
func TestViewGradient_MergePreserved(t *testing.T) {
	base := defaultViews()
	ov := Views{Window: ViewNode{Background: ViewFill{Gradient: &ViewGradient{
		Type: "linear", Angle: 90,
		Stops: []ViewGradientStop{{Color: NewLightDark("#000000")}, {Color: NewLightDark("#FFFFFF"), Pos: 1}},
	}}}}
	merged := mergeViews(base, ov)
	g := merged.Window.Background.Gradient
	if g == nil || g.Angle != 90 || len(g.Stops) != 2 {
		t.Errorf("gradient 字段应被 merge 保留: %+v", g)
	}
}
