package ui

import (
	"sync/atomic"
	"unsafe"

	"golang.org/x/sys/windows"
)

// DPI constants
const (
	DefaultDPI = 96
	LOGPIXELSX = 88
	LOGPIXELSY = 90

	// MDT_EFFECTIVE_DPI for GetDpiForMonitor
	MDT_EFFECTIVE_DPI = 0
)

var (
	shcore               = windows.NewLazySystemDLL("shcore.dll")
	procGetDpiForMonitor = shcore.NewProc("GetDpiForMonitor")
)

// effectiveDPI stores the current effective DPI, updated when WM_DPICHANGED is received.
// Zero means not yet set (will fall back to GetSystemDPI).
var effectiveDPI atomic.Int32

// procGetDpiForWindow is declared in window.go alongside other user32 procs.

// SetEffectiveDPI updates the global effective DPI value.
// Called from WM_DPICHANGED handlers when the DPI changes (e.g., moving to another monitor).
func SetEffectiveDPI(dpi int) {
	effectiveDPI.Store(int32(dpi))
}

// GetEffectiveDPI returns the current effective DPI.
// If a per-monitor DPI has been set via WM_DPICHANGED, that value is used.
// Otherwise, falls back to the system DPI.
func GetEffectiveDPI() int {
	if dpi := effectiveDPI.Load(); dpi > 0 {
		return int(dpi)
	}
	return GetSystemDPI()
}

// GetDpiForWindow returns the DPI for the monitor that the given window is on.
// Requires Windows 10 1607+ (GetDpiForWindow API). Returns 0 on failure.
func GetDpiForWindow(hwnd windows.HWND) int {
	if procGetDpiForWindow.Find() == nil {
		ret, _, _ := procGetDpiForWindow.Call(uintptr(hwnd))
		if ret != 0 {
			return int(ret)
		}
	}
	return 0
}

// UpdateEffectiveDPIFromPoint determines the DPI for the monitor containing the
// given screen point and updates the global effective DPI accordingly.
// This should be called before rendering to ensure correct DPI when the caret
// moves to a different monitor (before WM_DPICHANGED is received).
func UpdateEffectiveDPIFromPoint(x, y int) {
	if procGetDpiForMonitor.Find() != nil {
		return // API not available
	}

	// MonitorFromPoint: pack POINT into a single register (x64 ABI)
	pt := uintptr(uint32(x)) | (uintptr(uint32(y)) << 32)
	hMonitor, _, _ := procMonitorFromPoint.Call(pt, MONITOR_DEFAULTTONEAREST)
	if hMonitor == 0 {
		return
	}

	var dpiX, dpiY uint32
	ret, _, _ := procGetDpiForMonitor.Call(
		hMonitor,
		MDT_EFFECTIVE_DPI,
		uintptr(unsafe.Pointer(&dpiX)),
		uintptr(unsafe.Pointer(&dpiY)),
	)
	// S_OK == 0
	if ret == 0 && dpiX > 0 {
		SetEffectiveDPI(int(dpiX))
	}
}

// GetSystemDPI returns the system DPI
func GetSystemDPI() int {
	// Try Windows 10 1607+ API first
	if procGetDpiForSystem.Find() == nil {
		ret, _, _ := procGetDpiForSystem.Call()
		if ret != 0 {
			return int(ret)
		}
	}

	// Fallback: Use GetDeviceCaps with screen DC
	hdcScreen, _, _ := procGetDC.Call(0)
	if hdcScreen != 0 {
		defer procReleaseDC.Call(0, hdcScreen)
		dpi, _, _ := procGetDeviceCaps.Call(hdcScreen, LOGPIXELSX)
		if dpi != 0 {
			return int(dpi)
		}
	}

	return DefaultDPI
}

// GetDPIScale returns the DPI scale factor (1.0 = 100%, 1.5 = 150%, etc.)
// Uses the effective DPI (updated by WM_DPICHANGED) for per-monitor awareness.
func GetDPIScale() float64 {
	dpi := GetEffectiveDPI()
	return float64(dpi) / float64(DefaultDPI)
}

// ScaleForDPI scales a value according to the current DPI
func ScaleForDPI(value float64) float64 {
	return value * GetDPIScale()
}

// ScaleIntForDPI scales an integer value according to the current DPI
func ScaleIntForDPI(value int) int {
	return int(float64(value) * GetDPIScale())
}
