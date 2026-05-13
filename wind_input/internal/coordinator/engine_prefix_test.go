// engine_prefix_test.go — 验证 testhelper 的 withEngineMgr 选项能正确装配
// 一个最小化的 engine.Manager, 使其内部 HasPrefix 可被 Coordinator 调用.
//
// 这是 z 决策集成测试的奠基测试: 一旦 HasPrefix 在脚手架里通了,
// 后续的 z 键混合路径测试就能复用同样的 fixture, 不再需要触达 schema /
// EnsurePinyinLoaded 之类的重路径.
package coordinator

import "testing"

func TestEngineMgrFixture_HasPrefixViaStubLayer(t *testing.T) {
	h := newTestCoordinator(t,
		withEngineMgr(
			withCodetableEntry("za", "在"),
			withCodetableEntry("zhang", "张"),
		),
	)

	if h.engineMgr == nil {
		t.Fatal("withEngineMgr should attach a non-nil engineMgr")
	}

	cases := []struct {
		prefix string
		want   bool
	}{
		{"z", true},     // 任意 z 前缀均有匹配 (za / zhang)
		{"za", true},    // 精确前缀
		{"zh", true},    // 前缀 zhang
		{"zhang", true}, // 精确码
		{"q", false},    // 无 q 前缀条目
		{"zb", false},   // 无该前缀
		{"", false},     // 空前缀按 HasPrefix 契约返回 false
	}
	for _, tc := range cases {
		t.Run(tc.prefix, func(t *testing.T) {
			got := h.engineMgr.HasPrefix(tc.prefix)
			if got != tc.want {
				t.Errorf("HasPrefix(%q) = %v, want %v", tc.prefix, got, tc.want)
			}
		})
	}
}
