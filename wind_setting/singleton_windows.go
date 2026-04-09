package main

import (
	"context"
	"log"
	"os"
	"path/filepath"
	"strings"
	"unsafe"

	"golang.org/x/sys/windows"

	"github.com/huanfeng/wind_input/pkg/buildvariant"
	wailsRuntime "github.com/wailsapp/wails/v2/pkg/runtime"
)

var (
	mutexName   = "Global\\WindInput_Setting_SingleInstance" + buildvariant.Suffix()
	eventName   = "Global\\WindInput_Setting_NavigateEvent" + buildvariant.Suffix()
	windowTitle = buildvariant.DisplayName() + " 设置"
)

var (
	moduser32                    = windows.NewLazySystemDLL("user32.dll")
	modkernel32                  = windows.NewLazySystemDLL("kernel32.dll")
	procFindWindowW              = moduser32.NewProc("FindWindowW")
	procSetForegroundWindow      = moduser32.NewProc("SetForegroundWindow")
	procBringWindowToTop         = moduser32.NewProc("BringWindowToTop")
	procShowWindow               = moduser32.NewProc("ShowWindow")
	procIsIconic                 = moduser32.NewProc("IsIconic")
	procGetWindowThreadProcessId = moduser32.NewProc("GetWindowThreadProcessId")
	procAttachThreadInput        = moduser32.NewProc("AttachThreadInput")
	procGetCurrentThreadId       = modkernel32.NewProc("GetCurrentThreadId")
)

const swRestore = 9

// navigateFilePath returns the path used to pass page name between instances.
func navigateFilePath() string {
	return filepath.Join(os.TempDir(), "WindInput_Setting_Navigate"+buildvariant.Suffix()+".txt")
}

// ensureSingleInstance checks if another instance is already running.
// If another instance exists, sends the startPage via event+file, activates
// the existing window, and returns false.
func ensureSingleInstance(startPage string, addWordParams AddWordParams) (windows.Handle, bool) {
	name, _ := windows.UTF16PtrFromString(mutexName)
	handle, err := windows.CreateMutex(nil, false, name)
	if err == windows.ERROR_ALREADY_EXISTS {
		if handle != 0 {
			windows.CloseHandle(handle)
		}
		if startPage != "" {
			// 加词模式：发送 "add-word|text|code|schema" 格式
			if startPage == "add-word" {
				msg := "add-word|" + addWordParams.Text + "|" + addWordParams.Code + "|" + addWordParams.SchemaID
				sendPageToExisting(msg)
			} else {
				sendPageToExisting(startPage)
			}
		}
		activateExistingWindow()
		return 0, false
	}
	return handle, true
}

// sendPageToExisting writes the target page to a temp file and signals the
// named event so the existing instance picks it up.
func sendPageToExisting(page string) {
	// Write page to temp file
	tmpFile := navigateFilePath()
	if err := os.WriteFile(tmpFile, []byte(page), 0644); err != nil {
		log.Printf("[singleton] 写入导航文件失败: %v", err)
		return
	}
	log.Printf("[singleton] 已写入导航页面: %s -> %s", page, tmpFile)

	// Open/create the named event and signal it.
	// CreateEvent returns a valid handle even when the event already exists
	// (err == ERROR_ALREADY_EXISTS), so we only fail on handle == 0.
	evtName, _ := windows.UTF16PtrFromString(eventName)
	evtHandle, _ := windows.CreateEvent(nil, 0, 0, evtName)
	if evtHandle == 0 {
		log.Printf("[singleton] 打开导航事件失败")
		return
	}
	defer windows.CloseHandle(evtHandle)

	if err := windows.SetEvent(evtHandle); err != nil {
		log.Printf("[singleton] 触发导航事件失败: %v", err)
		return
	}
	log.Printf("[singleton] 已触发导航事件")
}

// startIPCListener creates a named event and waits for signals from new
// instances. When signaled, reads the page name from the temp file and
// emits a Wails "navigate" event to the frontend.
func startIPCListener(ctx context.Context) {
	evtName, _ := windows.UTF16PtrFromString(eventName)
	// auto-reset event (manualReset=0), initial state not signaled (initialState=0)
	evtHandle, _ := windows.CreateEvent(nil, 0, 0, evtName)
	if evtHandle == 0 {
		log.Printf("[singleton] 创建导航事件失败")
		return
	}
	log.Printf("[singleton] IPC 监听已启动")

	go func() {
		defer windows.CloseHandle(evtHandle)
		for {
			ret, _ := windows.WaitForSingleObject(evtHandle, 500)

			select {
			case <-ctx.Done():
				log.Printf("[singleton] IPC 监听已停止")
				return
			default:
			}

			if ret == windows.WAIT_OBJECT_0 {
				tmpFile := navigateFilePath()
				data, err := os.ReadFile(tmpFile)
				if err != nil {
					log.Printf("[singleton] 读取导航文件失败: %v", err)
					continue
				}
				os.Remove(tmpFile)

				raw := strings.TrimSpace(string(data))
				log.Printf("[singleton] 收到导航请求: %q", raw)

				// 支持加词参数格式: "add-word|text|code|schema"
				if strings.HasPrefix(raw, "add-word|") {
					parts := strings.SplitN(raw, "|", 4)
					params := map[string]string{"page": "add-word"}
					if len(parts) > 1 {
						params["text"] = parts[1]
					}
					if len(parts) > 2 {
						params["code"] = parts[2]
					}
					if len(parts) > 3 {
						params["schema_id"] = parts[3]
					}
					wailsRuntime.EventsEmit(ctx, "navigate-addword", params)
					log.Printf("[singleton] 已发送加词导航事件到前端")
				} else if raw != "" && validPages[raw] {
					wailsRuntime.EventsEmit(ctx, "navigate", raw)
					log.Printf("[singleton] 已发送导航事件到前端: %s", raw)
				}
			}
		}
	}()
}

func activateExistingWindow() {
	titlePtr, _ := windows.UTF16PtrFromString(windowTitle)
	hwnd, _, _ := procFindWindowW.Call(0, uintptr(unsafe.Pointer(titlePtr)))
	if hwnd == 0 {
		return
	}

	// If the window is minimized, restore it
	iconic, _, _ := procIsIconic.Call(hwnd)
	if iconic != 0 {
		procShowWindow.Call(hwnd, swRestore)
	}

	// AttachThreadInput trick: temporarily attach our input queue to the
	// target window's thread, bypassing Windows' foreground lock restriction.
	targetThread, _, _ := procGetWindowThreadProcessId.Call(hwnd, 0)
	currentThread, _, _ := procGetCurrentThreadId.Call()

	if targetThread != 0 && currentThread != 0 && targetThread != currentThread {
		procAttachThreadInput.Call(currentThread, targetThread, 1)
		procSetForegroundWindow.Call(hwnd)
		procBringWindowToTop.Call(hwnd)
		procAttachThreadInput.Call(currentThread, targetThread, 0)
	} else {
		procSetForegroundWindow.Call(hwnd)
	}
}
