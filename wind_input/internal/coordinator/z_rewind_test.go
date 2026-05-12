// z_rewind_test.go — 验证 z 键混合切入后的"原子回退"行为.
//
// 场景: 用户配置 z 临时拼音 + 有 zzhb 等长前缀短语. 输入 zzh 时还匹配
// 命令前缀, 输入 zzha 时 zHybridFallback 触发切到临时拼音 buffer="zha".
// 用户此刻按 backspace, 期望回到 zzh 的正常输入态而不是从临时拼音 buffer
// 删字符. 一旦用户在临时拼音里敲了任何新字符, 回退路径作废.
package coordinator

import (
	"testing"

	"github.com/huanfeng/wind_input/internal/ipc"
)

// 切入瞬间的 backspace 应当原子回退: tempPinyinMode 清零, inputBuffer 恢复.
func TestZRewind_BackspaceRestoresPreSwitchBuffer(t *testing.T) {
	h := newTestCoordinator(t,
		withEngineMgr(withCodetableEntry("zzhb", "$")),
		withZHybridSchema(true),
	)
	// 模拟刚切入临时拼音瞬间的状态:
	//   - inputBuffer 在 enterTempPinyinFromZBuffer 里被 clearState 清掉
	//   - tempPinyinBuffer = "zh" + "a" = "zha"
	//   - rewindBuffer = 切入前的 inputBuffer "zzh", rewindKey = "a"
	h.tempPinyinMode = true
	h.tempPinyinBuffer = "zha"
	h.tempPinyinCursorPos = len("zha")
	h.tempPinyinTriggerKey = "z"
	h.tempPinyinRewindBuffer = "zzh"
	h.tempPinyinRewindKey = "a"

	h.pressKeyCode(int(ipc.VK_BACK))

	if h.tempPinyinMode {
		t.Errorf("tempPinyinMode should be false after rewind backspace")
	}
	if h.tempPinyinBuffer != "" {
		t.Errorf("tempPinyinBuffer = %q, want empty", h.tempPinyinBuffer)
	}
	if h.inputBuffer != "zzh" {
		t.Errorf("inputBuffer = %q, want %q", h.inputBuffer, "zzh")
	}
	if h.tempPinyinRewindBuffer != "" || h.tempPinyinRewindKey != "" {
		t.Errorf("rewind state not cleared: buf=%q key=%q",
			h.tempPinyinRewindBuffer, h.tempPinyinRewindKey)
	}
}

// 用户切入后又敲了新字符, 回退缓存作废; 此后 backspace 走标准临时拼音删字符路径,
// 不应当再回退到 inputBuffer.
func TestZRewind_NewCharInvalidatesRewind(t *testing.T) {
	h := newTestCoordinator(t,
		withEngineMgr(withCodetableEntry("zzhb", "$")),
		withZHybridSchema(true),
	)
	h.tempPinyinMode = true
	h.tempPinyinBuffer = "zha"
	h.tempPinyinCursorPos = len("zha")
	h.tempPinyinTriggerKey = "z"
	h.tempPinyinRewindBuffer = "zzh"
	h.tempPinyinRewindKey = "a"

	// 在临时拼音里敲 'b' → buffer 变 "zhab", rewind 缓存被清掉
	h.pressKey("b")
	if h.tempPinyinBuffer != "zhab" {
		t.Fatalf("after typing 'b': tempPinyinBuffer = %q, want %q",
			h.tempPinyinBuffer, "zhab")
	}
	if h.tempPinyinRewindBuffer != "" || h.tempPinyinRewindKey != "" {
		t.Fatalf("rewind state should be cleared after typing: buf=%q key=%q",
			h.tempPinyinRewindBuffer, h.tempPinyinRewindKey)
	}

	// 再 backspace → 走临时拼音标准删字符, 不再回退到 inputBuffer
	h.pressKeyCode(int(ipc.VK_BACK))

	if !h.tempPinyinMode {
		t.Errorf("tempPinyinMode should remain true (rewind invalidated)")
	}
	if h.tempPinyinBuffer != "zha" {
		t.Errorf("tempPinyinBuffer = %q, want %q (after deleting 'b')",
			h.tempPinyinBuffer, "zha")
	}
	if h.inputBuffer != "" {
		t.Errorf("inputBuffer should remain empty, got %q", h.inputBuffer)
	}
}
