//go:build darwin

package clipboard

import (
	"os"
	"os/exec"
	"strings"
)

// clipboard_darwin.go 用 macOS 自带的 pbcopy/pbpaste 实现剪贴板读写。
// 经 stdin 管道传文本, 无 shell 注入风险; 无需 CGO/AppKit, 服务进程即可访问
// 当前用户的通用剪贴板 (NSGeneralPboard)。
//
// 编码: pbcopy/pbpaste 的文本编码取决于进程 locale。LaunchAgent 启动的服务进程
// 通常无 LANG/LC_*, pbpaste 会按系统默认编码 (中文环境下为 GBK) 输出, 导致非
// ASCII 文本不是合法 UTF-8 (上屏时 Swift 严格 UTF-8 解码失败)。故强制 UTF-8 locale。

// utf8Env 在当前环境基础上强制 UTF-8 locale, 保证 pbcopy/pbpaste 走 UTF-8。
func utf8Env() []string {
	return append(os.Environ(), "LC_ALL=en_US.UTF-8", "LANG=en_US.UTF-8")
}

// SetText 把文本写入系统剪贴板 (pbcopy)。
func SetText(text string) error {
	cmd := exec.Command("pbcopy")
	cmd.Env = utf8Env()
	cmd.Stdin = strings.NewReader(text)
	return cmd.Run()
}

// GetText 读取系统剪贴板文本 (pbpaste)。
func GetText() (string, error) {
	cmd := exec.Command("pbpaste")
	cmd.Env = utf8Env()
	out, err := cmd.Output()
	if err != nil {
		return "", err
	}
	return string(out), nil
}
