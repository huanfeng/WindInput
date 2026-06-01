//go:build darwin

package proc

import (
	"errors"
	"testing"
)

// TestShellEx_TermUnsupported_Darwin 验证 macOS 上 term flag 返回
// unsupported 错误 (弹出可见终端窗口暂未实现), 而无 flag 时静默执行成功。
//
// 放在 darwin-tagged 文件而非共享的 proc_test.go: ErrUnsupportedPlatform 只在
// proc_darwin.go / proc_other.go 定义, Windows 无该符号, 若在无约束文件引用会让
// Windows 上 go vet / build 报 undefined。
func TestShellEx_TermUnsupported_Darwin(t *testing.T) {
	if err := ShellEx("exit 0", []string{"term"}); !errors.Is(err, ErrUnsupportedPlatform) {
		t.Errorf("ShellEx term on darwin: want ErrUnsupportedPlatform, got %v", err)
	}
	// 空白 flag 应被跳过, 等同无 flag, 静默执行成功。
	if err := ShellEx("exit 0", []string{"", "  "}); err != nil {
		t.Errorf("ShellEx blank flags on darwin should succeed, got %v", err)
	}
}
