//go:build darwin

package clipboard

import (
	"os/exec"
	"strings"
)

// clipboard_darwin.go 用 macOS 自带的 pbcopy/pbpaste 实现剪贴板读写。
// 经 stdin 管道传文本, 无 shell 注入风险; 无需 CGO/AppKit, 服务进程即可访问
// 当前用户的通用剪贴板 (NSGeneralPboard)。

// SetText 把文本写入系统剪贴板 (pbcopy)。
func SetText(text string) error {
	cmd := exec.Command("pbcopy")
	cmd.Stdin = strings.NewReader(text)
	return cmd.Run()
}

// GetText 读取系统剪贴板文本 (pbpaste)。
func GetText() (string, error) {
	out, err := exec.Command("pbpaste").Output()
	if err != nil {
		return "", err
	}
	return string(out), nil
}
