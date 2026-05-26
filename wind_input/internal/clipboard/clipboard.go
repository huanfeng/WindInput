//go:build windows

// Package clipboard provides Windows clipboard read/write operations.
package clipboard

import (
	"encoding/binary"
	"fmt"
	"image"
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
	cfDIBV5       = 17
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

// SetImage 将图像以 CF_DIBV5 格式写入 Windows 剪贴板，保留透明度，
// 便于粘贴到聊天软件等。输入按 image/draw 约定为预乘 alpha（与 savePNG 一致），
// 此处还原为 straight alpha 后写入，避免半透明像素偏暗。
func SetImage(img *image.RGBA) error {
	if img == nil {
		return fmt.Errorf("nil image")
	}
	b := img.Bounds()
	w, h := b.Dx(), b.Dy()
	if w <= 0 || h <= 0 {
		return fmt.Errorf("empty image")
	}

	const headerSize = 124 // BITMAPV5HEADER
	pixSize := w * h * 4
	buf := make([]byte, headerSize+pixSize)
	le := binary.LittleEndian

	le.PutUint32(buf[0:], headerSize)       // bV5Size
	le.PutUint32(buf[4:], uint32(int32(w))) // bV5Width
	le.PutUint32(buf[8:], uint32(int32(h))) // bV5Height (正值 => bottom-up)
	le.PutUint16(buf[12:], 1)               // bV5Planes
	le.PutUint16(buf[14:], 32)              // bV5BitCount
	le.PutUint32(buf[16:], 3)               // bV5Compression = BI_BITFIELDS
	le.PutUint32(buf[20:], uint32(pixSize)) // bV5SizeImage
	le.PutUint32(buf[40:], 0x00FF0000)      // bV5RedMask
	le.PutUint32(buf[44:], 0x0000FF00)      // bV5GreenMask
	le.PutUint32(buf[48:], 0x000000FF)      // bV5BlueMask
	le.PutUint32(buf[52:], 0xFF000000)      // bV5AlphaMask
	le.PutUint32(buf[56:], 0x73524742)      // bV5CSType = LCS_sRGB ('sRGB')
	le.PutUint32(buf[108:], 4)              // bV5Intent = LCS_GM_IMAGES

	off := headerSize
	for y := h - 1; y >= 0; y-- { // bottom-up：从图像底行开始
		sy := b.Min.Y + y
		for x := 0; x < w; x++ {
			c := img.RGBAAt(b.Min.X+x, sy) // 预乘 alpha
			r, g, bl := c.R, c.G, c.B
			if c.A != 0 && c.A != 255 {
				a := uint32(c.A)
				r = uint8(uint32(c.R) * 255 / a)
				g = uint8(uint32(c.G) * 255 / a)
				bl = uint8(uint32(c.B) * 255 / a)
			}
			buf[off+0] = bl
			buf[off+1] = g
			buf[off+2] = r
			buf[off+3] = c.A
			off += 4
		}
	}

	r, _, err := procOpenClipboard.Call(0)
	if r == 0 {
		return fmt.Errorf("OpenClipboard: %w", err)
	}
	defer procCloseClipboard.Call()

	procEmptyClipboard.Call()

	hMem, _, err := procGlobalAlloc.Call(gmemMoveable, uintptr(len(buf)))
	if hMem == 0 {
		return fmt.Errorf("GlobalAlloc: %w", err)
	}

	ptr, _, err := procGlobalLock.Call(hMem)
	if ptr == 0 {
		procGlobalFree.Call(hMem)
		return fmt.Errorf("GlobalLock: %w", err)
	}
	dst := (*byte)(*(*unsafe.Pointer)(unsafe.Pointer(&ptr)))
	copy(unsafe.Slice(dst, len(buf)), buf)
	procGlobalUnlock.Call(hMem)

	r, _, err = procSetClipboardData.Call(cfDIBV5, hMem)
	if r == 0 {
		procGlobalFree.Call(hMem)
		return fmt.Errorf("SetClipboardData: %w", err)
	}
	// SetClipboardData 成功后系统接管 hMem，不再释放。
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
