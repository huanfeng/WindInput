//go:build windows

package ui

import "testing"

// TestIsPureSymbolText 锁住符号字体切换的门槛：只有"纯符号串"才整串切到 Segoe UI Symbol，
// 混合 CJK 的状态文本必须保留主字体（修复全半角符号把"中/英"拖成宋体的回归）。
func TestIsPureSymbolText(t *testing.T) {
	cases := []struct {
		name string
		text string
		want bool
	}{
		{"empty", "", false},
		{"pure-cjk", "中英", false},
		{"pure-latin", "EN", false},
		{"single-geometric", "◐", true},
		{"single-check", "✓", true},
		{"symbols-with-spaces", "▶ ●", true},
		{"status-mixed-cjk-and-fullwidth-glyph", "中 英 ◐", false}, // 回归用例
		{"cjk-with-check", "✓ 中文模式", false},
		{"emoji-not-symbol", "❤", false}, // 不在白名单，交给 emoji 回退链
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := isPureSymbolText(c.text); got != c.want {
				t.Errorf("isPureSymbolText(%q) = %v, want %v", c.text, got, c.want)
			}
		})
	}
}
