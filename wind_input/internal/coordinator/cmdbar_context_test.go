package coordinator

import (
	"os"
	"testing"

	"github.com/huanfeng/wind_input/internal/cmdbar"
)

// TestCmdbarEvalContextLast 验证 Last(n) 透传到 InputHistory。
// last() 与 z 键重复上屏共用 inputHistory, 不再单独维护 cmdbar.History。
func TestCmdbarEvalContextLast(t *testing.T) {
	hist := NewInputHistory(64)
	hist.Record("a", "", "", 0)
	hist.Record("b", "", "", 0)
	hist.Record("c", "", "", 0)
	ctx := &cmdbarEvalContext{
		input:   "ocbd",
		history: hist,
	}

	if got := ctx.Input(); got != "ocbd" {
		t.Fatalf("Input() = %q, want %q", got, "ocbd")
	}
	if got := ctx.Last(1); got != "c" {
		t.Fatalf("Last(1) = %q, want %q", got, "c")
	}
	if got := ctx.Last(2); got != "b" {
		t.Fatalf("Last(2) = %q, want %q", got, "b")
	}
	if got := ctx.Last(99); got != "" {
		t.Fatalf("Last(99) = %q, want empty", got)
	}
}

// TestCmdbarEvalContextLastNilHistory 验证 history==nil 时 Last 不 panic。
func TestCmdbarEvalContextLastNilHistory(t *testing.T) {
	ctx := &cmdbarEvalContext{}
	if got := ctx.Last(1); got != "" {
		t.Fatalf("Last on nil history should return empty, got %q", got)
	}
}

// TestCmdbarEvalContextClipFallback 验证 Clip(n>1) 暂返回空 (剪贴板栈 P5 才接)。
func TestCmdbarEvalContextClipFallback(t *testing.T) {
	ctx := &cmdbarEvalContext{}
	if got := ctx.Clip(2); got != "" {
		t.Fatalf("Clip(2) should return empty pending P5, got %q", got)
	}
	if got := ctx.Clip(5); got != "" {
		t.Fatalf("Clip(5) should return empty, got %q", got)
	}
}

// TestCmdbarEvalContextEnv 验证 Env 读 os 环境变量。
func TestCmdbarEvalContextEnv(t *testing.T) {
	const key = "WINDINPUT_CMDBAR_CTX_TEST"
	t.Setenv(key, "hello")
	ctx := &cmdbarEvalContext{}
	if got := ctx.Env(key); got != "hello" {
		t.Fatalf("Env(%s) = %q, want %q", key, got, "hello")
	}
	// 空名应返回空。
	if got := ctx.Env(""); got != "" {
		t.Fatalf("Env(\"\") = %q, want empty", got)
	}
	// 未设置的变量应返回 os.Getenv 默认 "" (确保确实走了 os.Getenv 而非 panic)。
	_ = os.Getenv
	if got := ctx.Env("WINDINPUT_CMDBAR_UNSET_NEVER_DEFINED"); got != "" {
		t.Fatalf("Env on unset var should return empty, got %q", got)
	}
}

// TestCmdbarEvalContextSelPlaceholder 验证 P5 阶段 Sel 仍为占位空串
// (兼容性原因暂不接入)。App/Title 在 P5 已走 foreground 包真值, 不在此处断言
// 内容 (依赖前台窗口环境), 仅在 TestCmdbarEvalContextAppTitleCallable 验证可调用。
func TestCmdbarEvalContextSelPlaceholder(t *testing.T) {
	ctx := &cmdbarEvalContext{}
	if ctx.Sel() != "" {
		t.Fatal("Sel() must be empty placeholder in P5")
	}
}

// TestCmdbarEvalContextAppTitleCallable 验证 App/Title 不再 panic, 返回 string。
// CI 环境可能没有前台窗口, 不断言具体内容。
func TestCmdbarEvalContextAppTitleCallable(t *testing.T) {
	ctx := &cmdbarEvalContext{}
	_ = ctx.App()
	_ = ctx.Title()
}

// TestCmdbarEvalContextServicesPassthrough 验证 Services() 透传注入的 bundle。
func TestCmdbarEvalContextServicesPassthrough(t *testing.T) {
	svcs := &cmdbar.Services{}
	ctx := &cmdbarEvalContext{services: svcs}
	if ctx.Services() != svcs {
		t.Fatal("Services() did not return the injected bundle")
	}
}
