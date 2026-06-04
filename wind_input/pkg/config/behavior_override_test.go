package config

import (
	"os"
	"path/filepath"
	"testing"
)

// behavior_override_test.go — V3-D 哲学Y：config.UI 为主题 behavior 三字段补全用户覆盖通道
// （always_show_pager / show_page_number / vertical_max_width），各带 *FollowTheme 开关。
// 仿 fontsize_follow_test.go：默认值 + 缺失继承默认 + 存读 round-trip 闭环。

// TestBehaviorOverride_Defaults 验证三对覆盖字段的默认值（新装跟随主题）。
func TestBehaviorOverride_Defaults(t *testing.T) {
	d := DefaultConfig().UI
	if !d.AlwaysShowPagerFollowTheme {
		t.Error("DefaultConfig 应默认 AlwaysShowPagerFollowTheme=true（新装跟随主题）")
	}
	if !d.ShowPageNumberFollowTheme {
		t.Error("DefaultConfig 应默认 ShowPageNumberFollowTheme=true")
	}
	if !d.VerticalMaxWidthFollowTheme {
		t.Error("DefaultConfig 应默认 VerticalMaxWidthFollowTheme=true")
	}
	// 值字段兜底初值
	if d.ShowPageNumber != true {
		t.Errorf("ShowPageNumber 默认应为 true, got %v", d.ShowPageNumber)
	}
	if d.VerticalMaxWidth != 600 {
		t.Errorf("VerticalMaxWidth 默认应为 600, got %d", d.VerticalMaxWidth)
	}
}

// TestBehaviorOverride_AbsentInheritsDefault 验证配置存在但未写覆盖字段时继承默认（merge-on-default）。
func TestBehaviorOverride_AbsentInheritsDefault(t *testing.T) {
	dir := t.TempDir()
	p := filepath.Join(dir, "absent.yaml")
	// 只写 font_size，三对覆盖字段缺失 → 继承默认（FollowTheme=true）。
	if err := os.WriteFile(p, []byte("ui:\n  font_size: 20\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	cfg, err := LoadFrom(p)
	if err != nil {
		t.Fatalf("LoadFrom: %v", err)
	}
	if !cfg.UI.AlwaysShowPagerFollowTheme || !cfg.UI.ShowPageNumberFollowTheme || !cfg.UI.VerticalMaxWidthFollowTheme {
		t.Errorf("缺失字段应继承默认 FollowTheme=true, got pager=%v pageNum=%v vmax=%v",
			cfg.UI.AlwaysShowPagerFollowTheme, cfg.UI.ShowPageNumberFollowTheme, cfg.UI.VerticalMaxWidthFollowTheme)
	}
}

// TestBehaviorOverride_RoundTrip 守护三对字段（值 + FollowTheme）的持久化闭环：
// SaveTo 走 diff-save、LoadFrom 走 merge-on-default；FollowTheme 的 true/false 都须 round-trip 一致
// （默认 true 的 bool 若误加 omitempty 会导致 false 存不下来——回归守护）。
func TestBehaviorOverride_RoundTrip(t *testing.T) {
	dir := t.TempDir()

	for _, follow := range []bool{true, false} {
		cfg := DefaultConfig()
		cfg.UI.AlwaysShowPagerFollowTheme = follow
		cfg.UI.ShowPageNumberFollowTheme = follow
		cfg.UI.VerticalMaxWidthFollowTheme = follow
		// 自定义模式下的用户值
		cfg.UI.AlwaysShowPager = true
		cfg.UI.ShowPageNumber = false
		cfg.UI.VerticalMaxWidth = 480

		path := filepath.Join(dir, "rt.yaml")
		if err := SaveTo(cfg, path); err != nil {
			t.Fatalf("SaveTo(follow=%v): %v", follow, err)
		}
		got, err := LoadFrom(path)
		if err != nil {
			t.Fatalf("LoadFrom(follow=%v): %v", follow, err)
		}
		if got.UI.AlwaysShowPagerFollowTheme != follow {
			t.Errorf("round-trip AlwaysShowPagerFollowTheme: 存 %v 回读 %v", follow, got.UI.AlwaysShowPagerFollowTheme)
		}
		if got.UI.ShowPageNumberFollowTheme != follow {
			t.Errorf("round-trip ShowPageNumberFollowTheme: 存 %v 回读 %v", follow, got.UI.ShowPageNumberFollowTheme)
		}
		if got.UI.VerticalMaxWidthFollowTheme != follow {
			t.Errorf("round-trip VerticalMaxWidthFollowTheme: 存 %v 回读 %v", follow, got.UI.VerticalMaxWidthFollowTheme)
		}
		if got.UI.AlwaysShowPager != true {
			t.Errorf("round-trip AlwaysShowPager(follow=%v): 存 true 回读 %v", follow, got.UI.AlwaysShowPager)
		}
		if got.UI.ShowPageNumber != false {
			t.Errorf("round-trip ShowPageNumber(follow=%v): 存 false 回读 %v", follow, got.UI.ShowPageNumber)
		}
		if got.UI.VerticalMaxWidth != 480 {
			t.Errorf("round-trip VerticalMaxWidth(follow=%v): 存 480 回读 %d", follow, got.UI.VerticalMaxWidth)
		}
	}
}
