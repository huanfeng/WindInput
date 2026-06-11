//go:build windows

package ui

import (
	"fmt"
	"runtime"
	"syscall"
	"time"
	"unsafe"

	"golang.org/x/sys/windows"
)

var (
	ole32                = windows.NewLazySystemDLL("ole32.dll")
	procCoInitializeEx   = ole32.NewProc("CoInitializeEx")
	procCoUninitialize   = ole32.NewProc("CoUninitialize")
	procCoCreateInstance = ole32.NewProc("CoCreateInstance")
)

// TSF COM 常量
const (
	coinitApartmentThreaded     uintptr = 0x2
	clsctxInprocServer          uintptr = 0x1
	tfProfileTypeInputProcessor uintptr = 1
	tfIPPMFDontCareCurrentLang  uintptr = 0x00020000
	langidSimplifiedChinese     uintptr = 0x0804
	rpcEChangedMode             uintptr = 0x80010106
)

// CLSID_TF_InputProcessorProfiles = {33C53A50-F456-4884-B049-85FD643ECFED}
var clsidTFInputProcessorProfiles = windows.GUID{
	Data1: 0x33C53A50,
	Data2: 0xF456,
	Data3: 0x4884,
	Data4: [8]byte{0xB0, 0x49, 0x85, 0xFD, 0x64, 0x3E, 0xCF, 0xED},
}

// IID_ITfInputProcessorProfileMgr = {71C6E74D-0F28-11D8-A82A-00065B84435C}
var iidITfInputProcessorProfileMgr = windows.GUID{
	Data1: 0x71C6E74D,
	Data2: 0x0F28,
	Data3: 0x11D8,
	Data4: [8]byte{0xA8, 0x2A, 0x00, 0x06, 0x5B, 0x84, 0x43, 0x5C},
}

// WindInput TSF CLSID = {99C2EE30-5C57-45A2-9C63-FB54B34FD90A}
var windInputCLSID = windows.GUID{
	Data1: 0x99C2EE30,
	Data2: 0x5C57,
	Data3: 0x45A2,
	Data4: [8]byte{0x9C, 0x63, 0xFB, 0x54, 0xB3, 0x4F, 0xD9, 0x0A},
}

// WindInput guidProfile = {99C2EE31-5C57-45A2-9C63-FB54B34FD90A}
var windInputGUIDProfile = windows.GUID{
	Data1: 0x99C2EE31,
	Data2: 0x5C57,
	Data3: 0x45A2,
	Data4: [8]byte{0x9C, 0x63, 0xFB, 0x54, 0xB3, 0x4F, 0xD9, 0x0A},
}

const activateIMETimeout = 3 * time.Second

// ActivateIME 通过 TSF COM API 将系统输入法切换到 WindInput。
// 内部在专用 OS 线程上完成 COM 调用，可从任意 goroutine 调用。
// 若 COM 调用 3 秒内未返回，返回超时错误，避免 goroutine 与 OS 线程泄漏。
func ActivateIME() error {
	errCh := make(chan error, 1)
	go func() {
		runtime.LockOSThread()
		defer runtime.UnlockOSThread()
		errCh <- activateIMEOnCurrentThread()
	}()
	select {
	case err := <-errCh:
		return err
	case <-time.After(activateIMETimeout):
		return fmt.Errorf("ActivateIME: timeout after %s", activateIMETimeout)
	}
}

func activateIMEOnCurrentThread() error {
	hr, _, _ := procCoInitializeEx.Call(0, coinitApartmentThreaded)
	switch uintptr(hr) {
	case 0: // S_OK
		defer procCoUninitialize.Call()
	case 1: // S_FALSE: already initialized in same mode, no need to uninitialize
	case rpcEChangedMode:
		// 线程已用其他 apartment 模式初始化（如 MTA），继续尝试
	default:
		return fmt.Errorf("CoInitializeEx: 0x%08X", uint32(hr))
	}

	var pObj uintptr
	hr, _, _ = procCoCreateInstance.Call(
		uintptr(unsafe.Pointer(&clsidTFInputProcessorProfiles)),
		0,
		clsctxInprocServer,
		uintptr(unsafe.Pointer(&iidITfInputProcessorProfileMgr)),
		uintptr(unsafe.Pointer(&pObj)),
	)
	if hr != 0 {
		return fmt.Errorf("CoCreateInstance ITfInputProcessorProfileMgr: 0x%08X", uint32(hr))
	}
	defer comRelease(pObj)

	// vtable[3] = ActivateProfile（IUnknown 占 0-2）
	vtblPtr := *(*uintptr)(unsafe.Pointer(pObj))
	vtbl := (*[10]uintptr)(unsafe.Pointer(vtblPtr))
	hr, _, _ = syscall.SyscallN(vtbl[3],
		pObj,
		tfProfileTypeInputProcessor,
		langidSimplifiedChinese,
		uintptr(unsafe.Pointer(&windInputCLSID)),
		uintptr(unsafe.Pointer(&windInputGUIDProfile)),
		0, // hkl = NULL（TIP 不使用）
		tfIPPMFDontCareCurrentLang,
	)
	if hr != 0 {
		return fmt.Errorf("ITfInputProcessorProfileMgr::ActivateProfile: 0x%08X", uint32(hr))
	}
	return nil
}

// ActivateIME 将系统输入法切换到本输入法。可从任意 goroutine 调用。
func (m *Manager) ActivateIME() {
	if err := ActivateIME(); err != nil {
		m.logger.Warn("ActivateIME failed", "error", err)
	}
}

func comRelease(p uintptr) {
	if p == 0 {
		return
	}
	vtblPtr := *(*uintptr)(unsafe.Pointer(p))
	vtbl := (*[3]uintptr)(unsafe.Pointer(vtblPtr))
	syscall.SyscallN(vtbl[2], p) //nolint:errcheck
}
