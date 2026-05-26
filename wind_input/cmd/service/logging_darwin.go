//go:build darwin

package main

import (
	"os"
	"syscall"
)

// redirectStderrFD darwin 上用 dup2 把 fd 2 (stderr) 重定向到 f, 这样 Go runtime
// fatal (panic / OOM) 写到 stderr 的内容也会进 crash.log。
// 与 Win 端 SetStdHandle 行为对齐。
func redirectStderrFD(f *os.File) {
	_ = syscall.Dup2(int(f.Fd()), 2)
}
