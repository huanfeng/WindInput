//go:build darwin

package main

import (
	"fmt"
	"net"
	"os"
	"path/filepath"
	"syscall"
	"time"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/pkg/buildvariant"
)

// main_darwin.go 提供 cmd/service 入口在 darwin 上的平台 helper。
// 与 main_windows.go 接口对称, 但实现走 POSIX:
//   - showErrorMessageBox: stderr 打印 (无 GUI MessageBox; 后续 macOS IMKit
//     `.app` 工程会用 NSAlert 提供图形提示, 此处 stub 仅用于服务自身致命错误)
//   - setDPIAwareness: no-op (macOS 系统自动处理 retina 缩放)
//   - checkSingleton: 用 PID 文件 + flock 实现单例 (跨进程 advisory lock)
//   - waitForPreviousExit / isPipeAlreadyExists: 用 bridge UDS 路径探测

// showErrorMessageBox darwin 上仅写 stderr。
// 真正的图形错误提示由 macOS IMKit `.app` 接入后用 NSAlert 提供。
func showErrorMessageBox(message string) {
	fmt.Fprintf(os.Stderr, "[%s] FATAL: %s\n", buildvariant.DisplayName(), message)
}

// setDPIAwareness darwin 上 no-op (系统自动 retina 缩放)。
func setDPIAwareness() {}

// pidFilePath 返回单例 PID 文件路径, 与 bridge socket 同目录方便清理。
func pidFilePath() string {
	dir := filepath.Dir(bridge.BridgePipeName)
	return filepath.Join(dir, "wind_input.pid")
}

// checkSingleton 用 flock 在 PID 文件上加非阻塞排他锁实现单例。
// 进程退出时锁会被内核自动释放, 比 Win mutex 更可靠 (不会因 panic 残留)。
// 返回 (release, ok): release 关闭文件并清理 PID 文件; ok=false 表示已有实例运行。
func checkSingleton() (release func(), ok bool) {
	path := pidFilePath()
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return func() {}, false
	}
	f, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return func() {}, false
	}
	// 非阻塞排他锁
	if err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		_ = f.Close()
		return func() {}, false
	}
	// 写入 PID 方便排查 (失败不影响锁)
	_ = f.Truncate(0)
	_, _ = fmt.Fprintf(f, "%d\n", os.Getpid())

	release = func() {
		_ = syscall.Flock(int(f.Fd()), syscall.LOCK_UN)
		_ = f.Close()
		_ = os.Remove(path)
	}
	return release, true
}

// waitForPreviousExit 在 restart 路径上等前一实例释放 bridge socket。
func waitForPreviousExit() {
	const maxWait = 10 * time.Second
	const pollInterval = 100 * time.Millisecond
	deadline := time.Now().Add(maxWait)
	for time.Now().Before(deadline) {
		if !isPipeAlreadyExists() {
			time.Sleep(pollInterval)
			return
		}
		time.Sleep(pollInterval)
	}
}

// isPipeAlreadyExists 通过 dial bridge UDS 判定是否有另一实例。
func isPipeAlreadyExists() bool {
	c, err := net.DialTimeout("unix", bridge.BridgePipeName, 200*time.Millisecond)
	if err != nil {
		return false
	}
	_ = c.Close()
	return true
}
