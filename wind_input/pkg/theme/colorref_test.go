package theme

import (
	"testing"

	"gopkg.in/yaml.v3"
)

// TestColorRef_InlineParse 守护 view 颜色字段支持内联 {light,dark}（与标量向后兼容）。
func TestColorRef_InlineParse(t *testing.T) {
	var n ViewNode
	src := "color: {light: \"#111111\", dark: \"#222222\"}\n" +
		"background:\n  color: \"${accent}\"\n"
	if err := yaml.Unmarshal([]byte(src), &n); err != nil {
		t.Fatalf("解析失败: %v", err)
	}
	if n.Color.Select(false) != "#111111" || n.Color.Select(true) != "#222222" {
		t.Errorf("内联 {light,dark} 解析错误: light=%q dark=%q", n.Color.Select(false), n.Color.Select(true))
	}
	// 标量向后兼容：明暗共用。
	if n.Background.Color.Select(false) != "${accent}" || n.Background.Color.Select(true) != "${accent}" {
		t.Errorf("标量颜色应明暗共用, got light=%q dark=%q", n.Background.Color.Select(false), n.Background.Color.Select(true))
	}
}

// TestColorRef_ResolveByIsDark 守护内联 {light,dark} 在 ResolveCandidateViews 中按 isDark 求出不同颜色。
func TestColorRef_ResolveByIsDark(t *testing.T) {
	views := Views{Text: ViewNode{Color: ColorRef{Light: "#111111", Dark: "#222222"}}}
	light := ResolveCandidateViews(views, ResolvedPalette{IsDark: false})
	dark := ResolveCandidateViews(views, ResolvedPalette{IsDark: true})
	if light.Text.TextColor == nil || dark.Text.TextColor == nil {
		t.Fatal("文本色应解析出非 nil")
	}
	if light.Text.TextColor == dark.Text.TextColor {
		t.Error("内联 {light,dark} 应在亮/暗下解析出不同颜色")
	}
}
