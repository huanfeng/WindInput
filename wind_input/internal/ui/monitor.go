package ui

import (
	"unsafe"

	"golang.org/x/sys/windows"
)

// MONITORINFO structure for GetMonitorInfo
type MONITORINFO struct {
	CbSize    uint32
	RcMonitor RECT
	RcWork    RECT
	DwFlags   uint32
}

// RECT structure
type RECT struct {
	Left   int32
	Top    int32
	Right  int32
	Bottom int32
}

// Monitor flags
const (
	MONITOR_DEFAULTTONEAREST = 0x00000002
	VK_CAPITAL               = 0x14 // CapsLock key
)

// GetMonitorWorkAreaFromPoint returns the work area (excluding taskbar) of the monitor
// containing the specified point. Returns (left, top, right, bottom).
func GetMonitorWorkAreaFromPoint(x, y int) (left, top, right, bottom int) {
	// MonitorFromPoint expects POINT struct packed into a single 64-bit value on x64 Windows ABI
	// POINT struct: { LONG x, LONG y } = 8 bytes total
	// In x64 calling convention, 8-byte structs are passed in a single register
	// Low 32 bits = x, High 32 bits = y
	pt := uintptr(uint32(x)) | (uintptr(uint32(y)) << 32)

	hMonitor, _, _ := procMonitorFromPoint.Call(
		pt,
		MONITOR_DEFAULTTONEAREST,
	)

	if hMonitor == 0 {
		// Fallback to primary monitor work area
		return 0, 0, 1920, 1080
	}

	// Get monitor info
	var mi MONITORINFO
	mi.CbSize = uint32(unsafe.Sizeof(mi))
	ret, _, _ := procGetMonitorInfoW.Call(hMonitor, uintptr(unsafe.Pointer(&mi)))

	if ret == 0 {
		// Fallback
		return 0, 0, 1920, 1080
	}

	return int(mi.RcWork.Left), int(mi.RcWork.Top), int(mi.RcWork.Right), int(mi.RcWork.Bottom)
}

// GetCurrentMonitorWorkArea returns the work area (excluding taskbar) of the monitor
// containing the mouse cursor. Returns (left, top, right, bottom).
func GetCurrentMonitorWorkArea() (left, top, right, bottom int) {
	// Get cursor position
	var pt POINT
	procGetCursorPos.Call(uintptr(unsafe.Pointer(&pt)))

	return GetMonitorWorkAreaFromPoint(int(pt.X), int(pt.Y))
}

// GetDefaultToolbarPosition returns the default position for the toolbar
// (bottom-right corner of the current monitor's work area)
func GetDefaultToolbarPosition(toolbarWidth, toolbarHeight int) (x, y int) {
	left, top, right, bottom := GetCurrentMonitorWorkArea()

	// Position at bottom-right corner with some margin (DPI scaled)
	margin := ScaleIntForDPI(10)
	x = right - toolbarWidth - margin
	y = bottom - toolbarHeight - margin

	// Ensure position is within work area
	if x < left {
		x = left + margin
	}
	if y < top {
		y = top + margin
	}

	return x, y
}

// GetToolbarPositionForCaret returns the toolbar position for the monitor containing the caret
// (bottom-right corner of that monitor's work area)
func GetToolbarPositionForCaret(caretX, caretY, toolbarWidth, toolbarHeight int) (x, y int) {
	left, top, right, bottom := GetMonitorWorkAreaFromPoint(caretX, caretY)

	// Position at bottom-right corner with some margin (DPI scaled)
	margin := ScaleIntForDPI(10)
	x = right - toolbarWidth - margin
	y = bottom - toolbarHeight - margin

	// Ensure position is within work area
	if x < left {
		x = left + margin
	}
	if y < top {
		y = top + margin
	}

	return x, y
}

// MonitorKey returns a stable uint64 identifier for the monitor whose work area
// ends at (workRight, workBottom). Used to key per-monitor toolbar positions.
func MonitorKey(workRight, workBottom int) uint64 {
	return uint64(uint32(workRight))<<32 | uint64(uint32(workBottom))
}

// GetCapsLockState returns the current state of CapsLock key
// Returns true if CapsLock is ON, false otherwise
func GetCapsLockState() bool {
	state, _, _ := procGetKeyState.Call(uintptr(VK_CAPITAL))
	// The low-order bit indicates toggle state (0 = off, 1 = on)
	return (state & 0x0001) != 0
}

// CandidateLayout represents the layout direction of the candidate window
type CandidateLayout int

const (
	LayoutVertical   CandidateLayout = iota // Candidates displayed vertically (current default)
	LayoutHorizontal                        // Candidates displayed horizontally (future)
)

// PositionPreference indicates where the candidate window should be displayed
type PositionPreference int

const (
	PositionAuto  PositionPreference = iota // Auto-detect based on screen bounds
	PositionAbove                           // Force display above caret
	PositionBelow                           // Force display below caret
)

// AdjustCandidatePosition adjusts the candidate window position to ensure it stays within screen bounds.
// Parameters:
//   - caretX, caretY: the caret position (caretY is the BOTTOM of the caret)
//   - caretHeight: height of the caret/cursor
//   - windowWidth, windowHeight: size of the candidate window
//   - layout: the layout direction of the candidate window
//   - preference: position preference (auto, above, or below)
//
// Returns:
//   - x, y: adjusted position for the candidate window
//   - showAbove: true if window is displayed above caret (for sticky state tracking)
func AdjustCandidatePosition(caretX, caretY, caretHeight, windowWidth, windowHeight int, layout CandidateLayout, preference PositionPreference) (x, y int, showAbove bool) {
	// Get the work area of the monitor containing the caret
	workLeft, workTop, workRight, workBottom := GetMonitorWorkAreaFromPoint(caretX, caretY)

	// Small gap between caret and candidate window
	const gap = 2

	switch layout {
	case LayoutHorizontal:
		// Horizontal layout: show below caret (same as vertical, just candidates arranged horizontally)
		// Note: caretY is the BOTTOM of the caret, so:
		//   - Caret top = caretY - caretHeight
		//   - Caret bottom = caretY
		x = caretX

		// Determine if we should show above or below
		shouldShowAbove := false

		if preference == PositionAbove {
			// Forced to show above (sticky state)
			shouldShowAbove = true
		} else if preference == PositionBelow {
			// Forced to show below
			shouldShowAbove = false
		} else {
			// Auto-detect: check if there's enough space below
			yBelow := caretY + gap
			if yBelow+windowHeight > workBottom {
				shouldShowAbove = true
			}
		}

		if shouldShowAbove {
			// Show above the caret
			y = caretY - caretHeight - gap - windowHeight
			showAbove = true
		} else {
			// Show below the caret
			y = caretY + gap
			showAbove = false
		}

		// Ensure y is within boundaries
		if y < workTop {
			y = workTop
		}
		if y+windowHeight > workBottom {
			y = workBottom - windowHeight
		}

		// Check right boundary for horizontal overflow
		if x+windowWidth > workRight {
			x = workRight - windowWidth
		}

		// Ensure x is within left boundary
		if x < workLeft {
			x = workLeft
		}

	case LayoutVertical:
		fallthrough
	default:
		// Vertical layout (default): prefer to show below caret
		// Note: caretY is the BOTTOM of the caret, so:
		//   - Caret top = caretY - caretHeight
		//   - Caret bottom = caretY
		x = caretX

		// Determine if we should show above or below
		shouldShowAbove := false

		if preference == PositionAbove {
			// Forced to show above (sticky state)
			shouldShowAbove = true
		} else if preference == PositionBelow {
			// Forced to show below
			shouldShowAbove = false
		} else {
			// Auto-detect: check if there's enough space below
			yBelow := caretY + gap
			if yBelow+windowHeight > workBottom {
				shouldShowAbove = true
			}
		}

		if shouldShowAbove {
			// Show above the caret
			// Window bottom should be at (caret top - gap)
			// Caret top = caretY - caretHeight
			// Window bottom = caretY - caretHeight - gap
			// Window top (y) = window bottom - windowHeight
			y = caretY - caretHeight - gap - windowHeight
			showAbove = true
		} else {
			// Show below the caret
			// Window top should be at (caret bottom + gap)
			// Caret bottom = caretY
			y = caretY + gap
			showAbove = false
		}

		// Ensure y is within boundaries
		if y < workTop {
			y = workTop
		}
		if y+windowHeight > workBottom {
			y = workBottom - windowHeight
		}

		// Check right boundary for horizontal overflow
		if x+windowWidth > workRight {
			x = workRight - windowWidth
		}

		// Ensure x is within left boundary
		if x < workLeft {
			x = workLeft
		}
	}

	return x, y, showAbove
}

// CreateEvent creates a Windows event object
func CreateEvent() (windows.Handle, error) {
	ret, _, err := procCreateEventW.Call(0, 1, 0, 0) // Manual reset, initial state = not signaled
	if ret == 0 {
		return 0, err
	}
	return windows.Handle(ret), nil
}

// SetEvent sets the event to signaled state
func SetEvent(event windows.Handle) {
	procSetEvent.Call(uintptr(event))
}

// ResetEvent resets the event to non-signaled state
func ResetEvent(event windows.Handle) {
	procResetEvent.Call(uintptr(event))
}

// CloseEvent closes the event handle
func CloseEvent(event windows.Handle) {
	procCloseHandle.Call(uintptr(event))
}

// MsgWaitForMultipleObjects waits for messages or events
// Returns: 0 = event signaled, 1 = message available, WAIT_TIMEOUT = timeout
func MsgWaitForMultipleObjects(event windows.Handle, timeoutMs uint32) uint32 {
	handles := [1]uintptr{uintptr(event)}
	ret, _, _ := procMsgWaitForMultipleObjects.Call(
		1,                                    // nCount
		uintptr(unsafe.Pointer(&handles[0])), // pHandles
		0,                                    // bWaitAll = FALSE
		uintptr(timeoutMs),                   // dwMilliseconds
		QS_ALLINPUT,                          // dwWakeMask
	)
	return uint32(ret)
}

// PeekMessage checks for a message without blocking
func PeekMessage(msg *MSG) bool {
	ret, _, _ := procPeekMessageW.Call(
		uintptr(unsafe.Pointer(msg)),
		0, 0, 0,
		PM_REMOVE,
	)
	return ret != 0
}

// ProcessMessage translates and dispatches a message
func ProcessMessage(msg *MSG) {
	procTranslateMessage.Call(uintptr(unsafe.Pointer(msg)))
	procDispatchMessageW.Call(uintptr(unsafe.Pointer(msg)))
}
