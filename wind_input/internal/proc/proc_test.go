package proc

import (
	"runtime"
	"testing"
)

func TestIsURL(t *testing.T) {
	cases := []struct {
		in   string
		want bool
	}{
		{"https://example.com", true},
		{"http://example.com", true},
		{"HTTPS://EXAMPLE.COM", true},
		{"C:/Users/me/file.txt", false},
		{"notepad.exe", false},
		{"", false},
	}
	for _, c := range cases {
		t.Run(c.in, func(t *testing.T) {
			if got := IsURL(c.in); got != c.want {
				t.Errorf("IsURL(%q) = %v want %v", c.in, got, c.want)
			}
		})
	}
}

func TestRun_EmptyCommand(t *testing.T) {
	if err := Run(""); err == nil {
		t.Error("Run(\"\") want error")
	}
}

func TestOpen_EmptyTarget(t *testing.T) {
	if err := Open(""); err == nil {
		t.Error("Open(\"\") want error")
	}
}

func TestShell_EmptyCmdline(t *testing.T) {
	if err := Shell(""); err == nil {
		t.Error("Shell(\"\") want error")
	}
}

func TestShell_HappyPath(t *testing.T) {
	switch runtime.GOOS {
	case "windows":
		if err := Shell("exit 0"); err != nil {
			t.Errorf("Shell happy path: %v", err)
		}
	case "darwin":
		// macOS: Shell 走 `/bin/sh -c <cmdline>`。
		if err := Shell("exit 0"); err != nil {
			t.Errorf("Shell happy path (darwin): %v", err)
		}
	default:
		t.Skip("Shell only implemented on Windows / macOS")
	}
}

func TestRun_HappyPath(t *testing.T) {
	switch runtime.GOOS {
	case "windows":
		if err := Run("cmd", "/c", "exit 0"); err != nil {
			t.Errorf("Run happy path: %v", err)
		}
	case "darwin":
		// macOS: Run 直接 exec.Command, /bin/sh 必存在。
		if err := Run("/bin/sh", "-c", "exit 0"); err != nil {
			t.Errorf("Run happy path (darwin): %v", err)
		}
	default:
		t.Skip("Run happy path needs a platform shell")
	}
}

// 注: macOS 上 term flag 返回 ErrUnsupportedPlatform 的验证见 proc_darwin_test.go
// (该符号 Windows 不存在, 不能放在无 build 约束的共享测试文件)。

// TestShellEx_FlagValidation 验证 flag 白名单解析: 已知 flag 通过, 未知报错。
// 不真正启动 powershell, 只验证 ShellEx 入口的 flag 校验路径。
func TestShellEx_FlagValidation(t *testing.T) {
	if err := ShellEx("", nil); err == nil {
		t.Error("ShellEx empty cmdline must error")
	}
	// 未知 flag → error, 非平台错误
	err := ShellEx("exit 0", []string{"weird"})
	if err == nil || !contains(err.Error(), "unknown flag") {
		t.Errorf("ShellEx unknown flag: want error containing 'unknown flag', got %v", err)
	}
	// 空字符串 flag 应被跳过 (相当于无 flag)
	if runtime.GOOS == "windows" {
		if err := ShellEx("exit 0", []string{"", "  "}); err != nil {
			t.Errorf("ShellEx blank flags should be ignored, got %v", err)
		}
		// term flag 合法 (cmd /k exit 0, 但 exit 0 让窗口立即关掉, 不会留 console)
		if err := ShellEx("exit 0", []string{"term"}); err != nil {
			t.Errorf("ShellEx term flag: %v", err)
		}
	}
}

func contains(s, sub string) bool {
	for i := 0; i+len(sub) <= len(s); i++ {
		if s[i:i+len(sub)] == sub {
			return true
		}
	}
	return false
}

// TestApplyRunAttr_Windows 验证 applyRunAttr 只设 NEW_PROCESS_GROUP,
// **不**设 HideWindow / CREATE_NO_WINDOW / DETACHED_PROCESS —— 这些
// 都会把 GUI 应用 (notepad/calc) 的窗口隐藏掉。
func TestApplyRunAttr_Windows(t *testing.T) {
	if runtime.GOOS != "windows" {
		t.Skip("applyRunAttr only meaningful on Windows")
	}
	cmd := newCmdForTest()
	applyRunAttr(cmd)
	if cmd.SysProcAttr == nil {
		t.Fatalf("applyRunAttr did not set SysProcAttr")
	}
	got := readCreationFlags(cmd.SysProcAttr)
	const want = uint32(0x00000200) // NEW_PROCESS_GROUP
	if got != want {
		t.Errorf("applyRunAttr flags=0x%x want exactly 0x%x", got, want)
	}
}

// TestApplyShellAttr_Windows 验证 Shell 静默/可见两种模式的 flag 设置。
func TestApplyShellAttr_Windows(t *testing.T) {
	if runtime.GOOS != "windows" {
		t.Skip("applyShellAttr only meaningful on Windows")
	}
	// 静默模式: DETACHED + NO_WINDOW + NEW_PROCESS_GROUP
	checkShellFlags(t, false, 0x00000008|0x00000200|0x08000000)
	// term 模式: NEW_CONSOLE + NEW_PROCESS_GROUP, 不能含 DETACHED
	checkShellFlags(t, true, 0x00000010|0x00000200)
}

func checkShellFlags(t *testing.T, term bool, wantAllSet uint32) {
	t.Helper()
	cmd := newCmdForTest()
	applyShellAttr(cmd, term)
	if cmd.SysProcAttr == nil {
		t.Fatalf("applyShellAttr(term=%v) did not set SysProcAttr", term)
	}
	got := readCreationFlags(cmd.SysProcAttr)
	if got&wantAllSet != wantAllSet {
		t.Errorf("applyShellAttr(term=%v) flags=0x%x missing required bits 0x%x",
			term, got, wantAllSet)
	}
	// DETACHED_PROCESS 与 CREATE_NEW_CONSOLE 互斥
	if term && got&0x00000008 != 0 {
		t.Errorf("term mode must not set DETACHED_PROCESS, flags=0x%x", got)
	}
	if !term && got&0x00000010 != 0 {
		t.Errorf("non-term mode must not set CREATE_NEW_CONSOLE, flags=0x%x", got)
	}
}
