//go:build darwin

package clipboard

import "errors"

// clipboard_darwin.go 提供 darwin 上的 stub 实现。
//
// 现状: PR-7 决定 clipboard 暂时留 stub, 待 macOS IMKit `.app` 工程落地后,
// 再决定走以下哪条路径:
//   - osascript "the clipboard as text" / "set the clipboard to" (无需权限, 进程级)
//   - CGO 调 NSPasteboard.generalPasteboard (需要 AppKit, 直接调用最高性能)
//   - 命令直通车的"复制候选"功能由 IMKit `.app` 自己完成 (用 NSPasteboard),
//     Go 服务侧根本不需要剪贴板访问
//
// 当前实现: 返回 ErrNotImplemented, 让"复制候选"等需要剪贴板的 cmdbar action
// 在 darwin 上明确失败 (而不是静默无效)。

// ErrNotImplemented 表示该平台上剪贴板功能未实现。
var ErrNotImplemented = errors.New("clipboard: not implemented on darwin (pending macOS IMKit integration)")

// SetText darwin 占位实现, 始终返回 ErrNotImplemented。
func SetText(text string) error { return ErrNotImplemented }

// GetText darwin 占位实现, 始终返回 ErrNotImplemented。
func GetText() (string, error) { return "", ErrNotImplemented }
