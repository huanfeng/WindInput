package theme

import "testing"

// TestFallbackToDefaultWhenNotV3 守护回归（2026-06-04）：用户选中旧 v2.5/v2.6 主题
// （无 colors 块，如 D:\UserData 下的旧 jidian-classic）时，LoadTheme 应**自动整体回退内置
// default**（不兼容旧主题，用户决策）——否则 resolvedV25=nil 会让候选窗拿到 0 尺寸几何，
// gg.NewPixmapFromBuffer panic「width and height must be > 0」，候选窗彻底消失。
func TestFallbackToDefaultWhenNotV3(t *testing.T) {
	m := &Manager{themeDirs: bitmapTestThemeDirs(t)}
	if err := m.LoadTheme("legacy-v26"); err != nil {
		t.Fatalf("LoadTheme(legacy-v26): %v", err)
	}
	rv := m.GetResolvedV25()
	if rv == nil {
		t.Fatal("不合法主题应整体回退 default，resolvedV25 不应为 nil（候选窗会 0 尺寸 panic）")
	}
	if rv.Palette.Bg == nil {
		t.Error("回退后 palette 应有效（Bg 非 nil）")
	}
	// 整体回退：currentThemeID 应切到 default（设置界面/SetDarkMode 一致）。
	if m.currentThemeID != "default" {
		t.Errorf("应整体回退为 default 主题, currentThemeID=%q", m.currentThemeID)
	}
}

// TestLegacyFixtureIsNotV3 确认 fixture 确为旧格式（前置条件，防 fixture 被误迁 v3）。
func TestLegacyFixtureIsNotV3(t *testing.T) {
	m := &Manager{themeDirs: bitmapTestThemeDirs(t)}
	tm, _, err := m.loadThemeFileWithDir("legacy-v26")
	if err != nil {
		t.Fatalf("加载 fixture: %v", err)
	}
	if tm.HasV3Schema() {
		t.Fatal("legacy-v26 fixture 不应有 v3 colors 块（否则测不到回退）")
	}
}
