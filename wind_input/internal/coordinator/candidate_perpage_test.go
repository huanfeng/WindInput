// candidate_perpage_test.go — 候选数分档（基础档/扩展档）物化与判定逻辑测试
package coordinator

import (
	"testing"

	"github.com/huanfeng/wind_input/internal/ui"
)

// TestRefreshEffectivePerPage 覆盖 refreshEffectivePerPage 在各场景下把哪一档
// 物化到 c.candidatesPerPage：扩展档禁用/启用、各触发原因、base 兜底。
func TestRefreshEffectivePerPage(t *testing.T) {
	tests := []struct {
		name     string
		base     int
		extended int
		setup    func(*Coordinator)
		want     int
	}{
		{
			name: "扩展档禁用_即使临时拼音也用基础档",
			base: 5, extended: 0,
			setup: func(c *Coordinator) { c.tempPinyinMode = true },
			want:  5,
		},
		{
			name: "临时拼音_切扩展档",
			base: 5, extended: 9,
			setup: func(c *Coordinator) { c.tempPinyinMode = true },
			want:  9,
		},
		{
			name: "快捷输入_切扩展档",
			base: 5, extended: 9,
			setup: func(c *Coordinator) { c.quickInputMode = true },
			want:  9,
		},
		{
			name: "短语候选_切扩展档",
			base: 5, extended: 9,
			setup: func(c *Coordinator) {
				c.candidates = []ui.Candidate{{Text: "白", PhraseTemplate: "$X"}}
			},
			want: 9,
		},
		{
			name: "普通候选无原因_用基础档",
			base: 5, extended: 9,
			setup: func(c *Coordinator) {
				c.candidates = []ui.Candidate{{Text: "好"}}
			},
			want: 5,
		},
		{
			name: "无任何原因_用基础档",
			base: 5, extended: 9,
			setup: nil,
			want:  5,
		},
		{
			name: "base配置为0_兜底回退7",
			base: 0, extended: 9,
			setup: nil,
			want:  7,
		},
		{
			name: "base为0但有原因_仍用扩展档",
			base: 0, extended: 9,
			setup: func(c *Coordinator) { c.quickInputMode = true },
			want:  9,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			c := newTestCoordinator(t).Coordinator
			c.candidatesPerPageBase = tt.base
			c.candidatesPerPageExtended = tt.extended
			if tt.setup != nil {
				tt.setup(c)
			}
			c.refreshEffectivePerPage()
			if c.candidatesPerPage != tt.want {
				t.Errorf("refreshEffectivePerPage() => candidatesPerPage=%d, want %d", c.candidatesPerPage, tt.want)
			}
		})
	}
}

// TestRefreshEffectivePerPage_RecoversExtendedValue 验证派生场景消失后能收回扩展值：
// 先因短语候选物化为扩展档，再换成普通候选重算，应回落到基础档（修复「删回普通字
// 扩展档不收回」的隐患）。
func TestRefreshEffectivePerPage_RecoversExtendedValue(t *testing.T) {
	c := newTestCoordinator(t).Coordinator
	c.candidatesPerPageBase = 5
	c.candidatesPerPageExtended = 9

	// 含短语候选 → 扩展档
	c.candidates = []ui.Candidate{{Text: "白", PhraseTemplate: "$X"}}
	c.refreshEffectivePerPage()
	if c.candidatesPerPage != 9 {
		t.Fatalf("含短语候选应为扩展档 9，实际 %d", c.candidatesPerPage)
	}

	// 退格回普通候选 → 应收回到基础档
	c.candidates = []ui.Candidate{{Text: "好"}}
	c.refreshEffectivePerPage()
	if c.candidatesPerPage != 5 {
		t.Fatalf("退回普通候选应收回基础档 5，实际 %d", c.candidatesPerPage)
	}
}
