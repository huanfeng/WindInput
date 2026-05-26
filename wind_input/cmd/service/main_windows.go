//go:build windows

package main

import (
	"os"
	"syscall"
	"time"
	"unsafe"

	"golang.org/x/sys/windows"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/pkg/buildvariant"
)

// main_windows.go 集中 cmd/service 入口的 Windows 专属系统调用:
// MessageBox / DPI awareness / 单例 mutex / Named Pipe 探测。
//
// darwin 端对应 stub 见 main_darwin.go, 接口签名对齐但行为平台特化。

// mutexName Win 单例 mutex 名 (Global namespace 让所有桌面都能共享同一实例)。
var mutexName = "Global\\WindInput" + buildvariant.Suffix() + "IMEService"

// showErrorMessageBox 用 Win32 MessageBox 弹窗显示致命错误。
// 当服务由 TSF DLL 启动时, stderr 通常被丢弃, 必须用图形对话框告知用户。
func showErrorMessageBox(message string) {
	user32 := windows.NewLazySystemDLL("user32.dll")
	messageBox := user32.NewProc("MessageBoxW")
	title, _ := windows.UTF16PtrFromString(buildvariant.DisplayName())
	msg, _ := windows.UTF16PtrFromString(message)
	messageBox.Call(0, uintptr(unsafe.Pointer(msg)), uintptr(unsafe.Pointer(title)), 0x10) // MB_ICONERROR
}

// DPI awareness constants
const (
	procDPIUnaware         = 0
	procDPISystemAware     = 1
	procDPIPerMonitorAware = 2
)

// setDPIAwareness 把进程 DPI 感知设为 Per-Monitor V2, 避免高分屏 UI 模糊。
// 优先用 Win 8.1+ shcore.dll, 回退到 Win Vista+ user32.dll。
func setDPIAwareness() {
	shcore := syscall.NewLazyDLL("shcore.dll")
	setProcessDpiAwareness := shcore.NewProc("SetProcessDpiAwareness")
	if setProcessDpiAwareness.Find() == nil {
		setProcessDpiAwareness.Call(uintptr(procDPIPerMonitorAware))
		return
	}
	user32 := syscall.NewLazyDLL("user32.dll")
	setProcessDPIAware := user32.NewProc("SetProcessDPIAware")
	if setProcessDPIAware.Find() == nil {
		setProcessDPIAware.Call()
	}
}

// checkSingleton 通过 named mutex 检测/创建进程单例。
// 返回 (release, ok): release 关闭 mutex (defer 调用); ok=false 表示另一实例运行中。
func checkSingleton() (release func(), ok bool) {
	name, _ := windows.UTF16PtrFromString(mutexName)
	handle, err := windows.CreateMutex(nil, false, name)
	if err != nil {
		if err == windows.ERROR_ALREADY_EXISTS {
			if handle != 0 {
				windows.CloseHandle(handle)
			}
			return func() {}, false
		}
	}
	if handle != 0 {
		event, _ := windows.WaitForSingleObject(handle, 0)
		if event == uint32(windows.WAIT_OBJECT_0) || event == uint32(windows.WAIT_ABANDONED) {
			return func() { windows.CloseHandle(handle) }, true
		}
		windows.CloseHandle(handle)
		return func() {}, false
	}
	return func() {}, false
}

// waitForPreviousExit 在 restart 路径上等前一实例完全释放管道与 mutex。
// 通过轮询 bridge 管道是否还存在来判定。
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

// isPipeAlreadyExists 探测 bridge 命名管道是否存在(另一服务实例正在跑)。
func isPipeAlreadyExists() bool {
	pipePath, _ := windows.UTF16PtrFromString(bridge.BridgePipeName)
	handle, err := windows.CreateFile(
		pipePath,
		windows.GENERIC_READ|windows.GENERIC_WRITE,
		0, nil,
		windows.OPEN_EXISTING,
		0, 0,
	)
	if err == nil {
		windows.CloseHandle(handle)
		return true
	}
	if err == windows.ERROR_PIPE_BUSY {
		return true
	}
	_ = os.Stat // keep os reference if pruned by future edits
	return false
}
