// z_trigger_state_test.go — 验证 withZHybridSchema 选项装配正确, 让
// isZKeyHybridMode / isTempPinyinZTrigger / engineMgr.IsTempPinyinEnabled /
// engineMgr.IsZKeyRepeatEnabled 在测试里可控可断言.
//
// 这一组用例只覆盖纯查询路径, 不触发 HandleKeyEvent, 为后续 z 决策的端到端
// 集成测试铺路.
package coordinator

import "testing"

func TestZHybridSchema_FlagWiring(t *testing.T) {
	cases := []struct {
		name    string
		zRepeat bool

		wantZTrigger     bool // c.isTempPinyinZTrigger()
		wantHybrid       bool // c.isZKeyHybridMode()
		wantTempEnabled  bool // engineMgr.IsTempPinyinEnabled()
		wantRepeatEnable bool // engineMgr.IsZKeyRepeatEnabled()
	}{
		{
			name:             "temp pinyin only (no z repeat)",
			zRepeat:          false,
			wantZTrigger:     true,
			wantHybrid:       false,
			wantTempEnabled:  true,
			wantRepeatEnable: false,
		},
		{
			name:             "hybrid mode (z repeat + temp pinyin)",
			zRepeat:          true,
			wantZTrigger:     true,
			wantHybrid:       true,
			wantTempEnabled:  true,
			wantRepeatEnable: true,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			h := newTestCoordinator(t, withZHybridSchema(tc.zRepeat))

			if got := h.isTempPinyinZTrigger(); got != tc.wantZTrigger {
				t.Errorf("isTempPinyinZTrigger() = %v, want %v", got, tc.wantZTrigger)
			}
			if got := h.isZKeyHybridMode(); got != tc.wantHybrid {
				t.Errorf("isZKeyHybridMode() = %v, want %v", got, tc.wantHybrid)
			}
			if got := h.engineMgr.IsTempPinyinEnabled(); got != tc.wantTempEnabled {
				t.Errorf("engineMgr.IsTempPinyinEnabled() = %v, want %v", got, tc.wantTempEnabled)
			}
			if got := h.engineMgr.IsZKeyRepeatEnabled(); got != tc.wantRepeatEnable {
				t.Errorf("engineMgr.IsZKeyRepeatEnabled() = %v, want %v", got, tc.wantRepeatEnable)
			}
		})
	}
}

// 默认 (无 withZHybridSchema) 时, 所有 z 相关查询都应返回 false,
// 防止后续测试无意中泄漏配置导致假阳性.
func TestZHybridSchema_DefaultsAreFalse(t *testing.T) {
	h := newTestCoordinator(t, withEngineMgr())

	if h.isTempPinyinZTrigger() {
		t.Error("isTempPinyinZTrigger() should default to false")
	}
	if h.isZKeyHybridMode() {
		t.Error("isZKeyHybridMode() should default to false")
	}
	if h.engineMgr.IsTempPinyinEnabled() {
		t.Error("engineMgr.IsTempPinyinEnabled() should default to false")
	}
	if h.engineMgr.IsZKeyRepeatEnabled() {
		t.Error("engineMgr.IsZKeyRepeatEnabled() should default to false")
	}
}
