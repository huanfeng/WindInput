package hotkey

import (
	"testing"

	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/pkg/config"
)

// classifyHash 把 keyDown 列表里的哈希按 policy 位分到 3 组（raw 哈希返回，便于断言）。
func classifyHash(t *testing.T, hashes []uint32) (both, chineseOnly, session []uint32) {
	t.Helper()
	for _, h := range hashes {
		raw := h &^ ipc.HotkeyPolicyMask
		switch {
		case h&ipc.HotkeyPolicySession != 0:
			session = append(session, raw)
		case h&ipc.HotkeyPolicyChineseOnly != 0:
			chineseOnly = append(chineseOnly, raw)
		default:
			both = append(both, raw)
		}
	}
	return
}

func TestCompile_NumberHotkeysExpandAsSession(t *testing.T) {
	cfg := &config.Config{}
	cfg.Hotkeys.PinCandidate = "ctrl+number"
	cfg.Hotkeys.DeleteCandidate = "ctrl+shift+number"

	compiler := NewCompiler(cfg)
	keyDown, _ := compiler.Compile()
	_, _, session := classifyHash(t, keyDown)

	if len(session) != 20 {
		t.Fatalf("期望 20 个 session 哈希 (Pin 10 + Delete 10), 实得 %d", len(session))
	}
	for d := uint32(0); d <= 9; d++ {
		wantPin := ipc.CalcKeyHash(ipc.ModCtrl, 0x30+d)
		wantDel := ipc.CalcKeyHash(ipc.ModCtrl|ipc.ModShift, 0x30+d)
		if !contains(session, wantPin) {
			t.Errorf("session 缺 Ctrl+%d (hash=0x%08X)", d, wantPin)
		}
		if !contains(session, wantDel) {
			t.Errorf("session 缺 Ctrl+Shift+%d (hash=0x%08X)", d, wantDel)
		}
	}
}

func TestCompile_NumberHotkeyNone(t *testing.T) {
	cfg := &config.Config{}
	cfg.Hotkeys.PinCandidate = "none"
	cfg.Hotkeys.DeleteCandidate = ""

	compiler := NewCompiler(cfg)
	keyDown, _ := compiler.Compile()
	_, _, session := classifyHash(t, keyDown)

	if len(session) != 0 {
		t.Fatalf("none/空配置不应产出 session 热键, 实得 %d", len(session))
	}
}

func TestCompile_FunctionHotkeyPolicies(t *testing.T) {
	cfg := &config.Config{}
	// 两模式都吃
	cfg.Hotkeys.SwitchEngine = "ctrl+`"
	cfg.Hotkeys.ToggleFullWidth = "shift+space"
	// 仅中文模式吃
	cfg.Hotkeys.AddWord = "ctrl+="
	cfg.Hotkeys.TogglePunct = "ctrl+."

	compiler := NewCompiler(cfg)
	keyDown, _ := compiler.Compile()
	both, chineseOnly, _ := classifyHash(t, keyDown)

	if len(both) < 2 {
		t.Errorf("期望至少 2 个 'both' 热键 (SwitchEngine + ToggleFullWidth), 实得 %d", len(both))
	}
	if len(chineseOnly) < 2 {
		t.Errorf("期望至少 2 个 'chineseOnly' 热键 (AddWord + TogglePunct), 实得 %d", len(chineseOnly))
	}
}

func contains(s []uint32, target uint32) bool {
	for _, v := range s {
		if v == target {
			return true
		}
	}
	return false
}
