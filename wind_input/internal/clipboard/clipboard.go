// Package clipboard provides Windows clipboard read/write operations.
package clipboard

import (
	"fmt"
	"syscall"
	"unsafe"

	"golang.org/x/sys/windows"
)

var (
	user32 = windows.NewLazySystemDLL("user32.dll")

	procOpenClipboard    = user32.NewProc("OpenClipboard")
	procCloseClipboard   = user32.NewProc("CloseClipboard")
	procEmptyClipboard   = user32.NewProc("EmptyClipboard")
	procSetClipboardData = user32.NewProc("SetClipboardData")
	procGetClipboardData = user32.NewProc("GetClipboardData")

	kernel32 = windows.NewLazySystemDLL("kernel32.dll")

	procGlobalAlloc  = kernel32.NewProc("GlobalAlloc")
	procGlobalFree   = kernel32.NewProc("GlobalFree")
	procGlobalLock   = kernel32.NewProc("GlobalLock")
	procGlobalUnlock = kernel32.NewProc("GlobalUnlock")
)

const (
	cfUnicodeText = 13
	gmemMoveable  = 0x0002
)

// SetText copies the given string to the Windows clipboard.
func SetText(text string) error {
	r, _, err := procOpenClipboard.Call(0)
	if r == 0 {
		return fmt.Errorf("OpenClipboard: %w", err)
	}
	defer procCloseClipboard.Call()

	procEmptyClipboard.Call()

	// Convert to UTF-16 with null terminator
	utf16, err := syscall.UTF16FromString(text)
	if err != nil {
		return fmt.Errorf("UTF16FromString: %w", err)
	}

	size := len(utf16) * 2 // each uint16 = 2 bytes
	hMem, _, err := procGlobalAlloc.Call(gmemMoveable, uintptr(size))
	if hMem == 0 {
		return fmt.Errorf("GlobalAlloc: %w", err)
	}

	ptr, _, err := procGlobalLock.Call(hMem)
	if ptr == 0 {
		procGlobalFree.Call(hMem)
		return fmt.Errorf("GlobalLock: %w", err)
	}

	// Copy UTF-16 data — use uintptr in Slice call to satisfy go vet;
	// ptr is a valid locked global memory address from GlobalLock.
	dst := (*byte)(*(*unsafe.Pointer)(unsafe.Pointer(&ptr)))
	copy(unsafe.Slice(dst, size), unsafe.Slice((*byte)(unsafe.Pointer(&utf16[0])), size))

	procGlobalUnlock.Call(hMem)

	r, _, err = procSetClipboardData.Call(cfUnicodeText, hMem)
	if r == 0 {
		procGlobalFree.Call(hMem)
		return fmt.Errorf("SetClipboardData: %w", err)
	}
	// After SetClipboardData succeeds, the system owns hMem — do not free it.

	return nil
}

// GetText reads the current text content from the Windows clipboard.
// Returns empty string if clipboard is empty or does not contain text.
func GetText() (string, error) {
	r, _, err := procOpenClipboard.Call(0)
	if r == 0 {
		return "", fmt.Errorf("OpenClipboard: %w", err)
	}
	defer procCloseClipboard.Call()

	hData, _, _ := procGetClipboardData.Call(cfUnicodeText)
	if hData == 0 {
		return "", nil // No text data available
	}

	ptr, _, err := procGlobalLock.Call(hData)
	if ptr == 0 {
		return "", fmt.Errorf("GlobalLock: %w", err)
	}
	defer procGlobalUnlock.Call(hData)

	// Read UTF-16 null-terminated string
	// ptr comes from GlobalLock syscall; reinterpret via &ptr to satisfy go vet.
	text := syscall.UTF16ToString((*[1 << 20]uint16)(*(*unsafe.Pointer)(unsafe.Pointer(&ptr)))[:])
	return text, nil
}
