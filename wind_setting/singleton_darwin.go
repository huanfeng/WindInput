//go:build darwin

package main

import (
	"context"
	"os/exec"
)

// darwin 单实例策略: 依赖 macOS .app bundle 的天然单实例 —— 通过 `open` / NSWorkspace
// 启动 .app 时, LaunchServices 不会重复拉起同一 bundle, 而是激活已有窗口。
// 因此这里无需互斥锁/IPC, ensureSingleInstance 总是成功且 release 为 no-op。

// ensureSingleInstance darwin 上恒成功 (单实例由 LaunchServices 保证)。
// startPage / addWordParams 由命令行参数照常解析 (main 里), 这里不需要跨实例转发。
func ensureSingleInstance(startPage string, addWordParams AddWordParams, protocolURL string) (func(), bool) {
	return func() {}, true
}

// showNativeMessageBox 用 osascript 弹原生对话框 (不依赖 WebView)。
func showNativeMessageBox(title, message string) {
	script := "display dialog " + quoteAS(message) +
		" with title " + quoteAS(title) +
		" buttons {\"OK\"} default button \"OK\" with icon caution"
	_ = exec.Command("osascript", "-e", script).Run()
}

// quoteAS 把字符串转义为 AppleScript 字符串字面量。
func quoteAS(s string) string {
	out := make([]rune, 0, len(s)+2)
	out = append(out, '"')
	for _, r := range s {
		switch r {
		case '"', '\\':
			out = append(out, '\\', r)
		case '\n':
			out = append(out, '\\', 'n')
		default:
			out = append(out, r)
		}
	}
	out = append(out, '"')
	return string(out)
}

// startIPCListener darwin 上无跨实例 IPC 需求 (单实例由系统保证), no-op。
func startIPCListener(ctx context.Context, app *App) {}
