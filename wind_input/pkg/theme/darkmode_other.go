//go:build !windows

package theme

import "log/slog"

// darkmode_other.go 提供非 Win 平台的 dark mode 检测 stub。
//
// macOS 上系统主题由 NSApplication.effectiveAppearance 提供 (KVO 可观察),
// 但当前 PR 范围只让代码可编译, 不实际监听 — macOS 的实现路径是:
//   - 通过 IMKit `.app` 内的 AppKit 监听 effectiveAppearance KVO
//   - 用 bridge 协议把主题变更通知发到 Go 服务
//   - Go 服务调 Manager.SetDarkMode(isDark) 触发主题重算
//
// 因此 Go 服务侧本身不需要主动检测系统外观, 这里都 stub 为 false / no-op。

// IsSystemDarkMode 非 Win 平台始终返回 false (待 IMKit 端推送实际值)。
func IsSystemDarkMode() bool { return false }

// DarkModeWatcher 非 Win 平台占位类型。
type DarkModeWatcher struct {
	logger   *slog.Logger
	callback func(isDark bool)
}

// NewDarkModeWatcher 返回 no-op watcher。
func NewDarkModeWatcher(logger *slog.Logger, callback func(isDark bool)) *DarkModeWatcher {
	return &DarkModeWatcher{logger: logger, callback: callback}
}

// Start no-op on non-Win.
func (w *DarkModeWatcher) Start() {}

// Stop no-op on non-Win.
func (w *DarkModeWatcher) Stop() {}
