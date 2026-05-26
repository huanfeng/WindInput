//go:build windows

package main

import (
	"os"

	"golang.org/x/sys/windows"
)

// redirectStderrFD Windows 上同时调用 SetStdHandle (让 Go runtime fatal 写到此 fd)
// 与 os.Stderr 更新 (logging.go 调用方完成)。
func redirectStderrFD(f *os.File) {
	windows.SetStdHandle(windows.STD_ERROR_HANDLE, windows.Handle(f.Fd()))
}
