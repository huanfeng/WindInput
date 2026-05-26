//go:build !windows

package keyinject

import "errors"

// ErrUnsupportedPlatform is returned by Tap/Sequence on non-Windows
// builds. keyinject is Windows-specific because the underlying syscall
// (user32.SendInput) does not exist on other platforms; non-Windows
// builds keep Parse usable for unit tests.
var ErrUnsupportedPlatform = errors.New("keyinject: not supported on this platform")

// Tap is a no-op stub on non-Windows platforms.
func Tap(c Combo) error { return ErrUnsupportedPlatform }

// Sequence is a no-op stub on non-Windows platforms.
func Sequence(combos ...Combo) error { return ErrUnsupportedPlatform }

// Hold is a no-op stub on non-Windows platforms.
// macOS 实现路径未来通过 CGEventTap / CGEventCreateKeyboardEvent, 由 IMKit `.app` 完成。
func Hold(c Combo) error { return ErrUnsupportedPlatform }

// Release is a no-op stub on non-Windows platforms.
func Release(c Combo) error { return ErrUnsupportedPlatform }

// TypeText is a no-op stub on non-Windows platforms.
// macOS 上"命令直通车"的文本上屏由 IMKit `.app` 走 NSPasteboard + Cmd+V 模拟, 或
// 通过 CGEventCreateKeyboardEvent 走系统事件; Go 服务侧不直接合成键。
func TypeText(s string) error { return ErrUnsupportedPlatform }
