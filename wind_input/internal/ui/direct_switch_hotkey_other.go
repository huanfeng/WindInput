//go:build !windows

package ui

// SyncDirectSwitchHotkey 在非 Windows 平台是 no-op（DirectSwitchHotkeys 为 Windows 专有机制）。
func SyncDirectSwitchHotkey(hotkey string) error { return nil }
