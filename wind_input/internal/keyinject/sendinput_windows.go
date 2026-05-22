//go:build windows

package keyinject

import (
	"fmt"
	"strconv"
	"strings"
	"syscall"
	"unsafe"

	"golang.org/x/sys/windows"
)

var (
	user32        = windows.NewLazySystemDLL("user32.dll")
	procSendInput = user32.NewProc("SendInput")
	procVkKeyScan = user32.NewProc("VkKeyScanW")
)

// Win32 INPUT structure constants.
const (
	inputKeyboard = 1

	keyeventfKeyUp   = 0x0002
	keyeventfUnicode = 0x0004
)

// keyboardInput mirrors the KEYBDINPUT layout. The outer INPUT union is
// expressed by packing the keyboard variant followed by enough padding
// to match the C union size on amd64 (KEYBDINPUT + 8 byte tail; full
// INPUT is 32 bytes on amd64, 28 on 386). We always allocate the amd64
// size; on 386 SendInput accepts size=28, so we pass per-arch size.
type keyboardInput struct {
	Type      uint32
	_         uint32 // align Ki to 8-byte boundary on amd64
	Vk        uint16
	Scan      uint16
	Flags     uint32
	Time      uint32
	ExtraInfo uintptr
	_         [8]byte // tail padding so sizeof matches amd64 INPUT
}

// Virtual key codes for keys we synthesize. Letters / digits use
// VkKeyScanW for layout-aware lookup.
var vkTable = map[string]uint16{
	"enter":       0x0D,
	"tab":         0x09,
	"escape":      0x1B,
	"space":       0x20,
	"backspace":   0x08,
	"delete":      0x2E,
	"insert":      0x2D,
	"home":        0x24,
	"end":         0x23,
	"pageup":      0x21,
	"pagedown":    0x22,
	"up":          0x26,
	"down":        0x28,
	"left":        0x25,
	"right":       0x27,
	"capslock":    0x14,
	"printscreen": 0x2C,
	"scrolllock":  0x91,
	"pause":       0x13,

	// punctuation by US layout — fallback only; letters/digits use VkKeyScan
	"semicolon": 0xBA,
	"equal":     0xBB,
	"comma":     0xBC,
	"minus":     0xBD,
	"period":    0xBE,
	"slash":     0xBF,
	"grave":     0xC0,
	"lbracket":  0xDB,
	"backslash": 0xDC,
	"rbracket":  0xDD,
	"quote":     0xDE,
}

func init() {
	for i := 1; i <= 24; i++ {
		vkTable[fmt.Sprintf("f%d", i)] = uint16(0x70 + i - 1)
	}
}

func vkFor(key string) (uint16, bool) {
	// vk:<hex|dec> — raw VK code passthrough, e.g. "vk:5d" or "vk:93"
	if strings.HasPrefix(key, "vk:") {
		n, err := strconv.ParseUint(key[3:], 16, 16)
		if err == nil && n > 0 && n <= 0xFF {
			return uint16(n), true
		}
		return 0, false
	}
	if v, ok := vkTable[key]; ok {
		return v, true
	}
	if len(key) == 1 {
		c := key[0]
		// Single ASCII char — query layout.
		r, _, _ := procVkKeyScan.Call(uintptr(uint16(c)))
		// Low byte = VK, high byte = shift state. We ignore shift state
		// because our Modifiers field carries explicit Shift if needed.
		vk := uint16(r & 0xFF)
		if vk == 0xFFFF || vk == 0 {
			return 0, false
		}
		return vk, true
	}
	return 0, false
}

func modVK(m string) uint16 {
	switch m {
	case "ctrl":
		return 0x11 // VK_CONTROL
	case "shift":
		return 0x10 // VK_SHIFT
	case "alt":
		return 0x12 // VK_MENU
	case "win":
		return 0x5B // VK_LWIN
	}
	return 0
}

func sendKeys(inputs []keyboardInput) error {
	if len(inputs) == 0 {
		return nil
	}
	for i := range inputs {
		inputs[i].Type = inputKeyboard
	}
	size := unsafe.Sizeof(inputs[0])
	r, _, err := procSendInput.Call(
		uintptr(len(inputs)),
		uintptr(unsafe.Pointer(&inputs[0])),
		size,
	)
	if int(r) != len(inputs) {
		if err != nil && err != syscall.Errno(0) {
			return fmt.Errorf("SendInput: sent=%d want=%d: %w", r, len(inputs), err)
		}
		return fmt.Errorf("SendInput: sent=%d want=%d", r, len(inputs))
	}
	return nil
}

func keydown(vk uint16) keyboardInput { return keyboardInput{Vk: vk} }
func keyup(vk uint16) keyboardInput   { return keyboardInput{Vk: vk, Flags: keyeventfKeyUp} }

// Tap synthesizes a single combo press: modifiers down, key down, key
// up, modifiers up (reverse order).
func Tap(c Combo) error {
	vk, ok := vkFor(c.Key)
	if !ok {
		return fmt.Errorf("keyinject: no VK for %q", c.Key)
	}
	var modVKs []uint16
	for _, m := range c.Modifiers {
		mv := modVK(m)
		if mv == 0 {
			return fmt.Errorf("keyinject: bad modifier %q", m)
		}
		modVKs = append(modVKs, mv)
	}
	events := make([]keyboardInput, 0, 2+2*len(modVKs))
	for _, mv := range modVKs {
		events = append(events, keydown(mv))
	}
	events = append(events, keydown(vk), keyup(vk))
	for i := len(modVKs) - 1; i >= 0; i-- {
		events = append(events, keyup(modVKs[i]))
	}
	return sendKeys(events)
}

// Sequence taps each combo in order.
func Sequence(combos ...Combo) error {
	for _, c := range combos {
		if err := Tap(c); err != nil {
			return err
		}
	}
	return nil
}

// Hold presses modifiers then the primary key down without releasing.
// Caller must call Release with the same Combo to avoid stuck keys.
func Hold(c Combo) error {
	vk, ok := vkFor(c.Key)
	if !ok {
		return fmt.Errorf("keyinject: no VK for %q", c.Key)
	}
	var events []keyboardInput
	for _, m := range c.Modifiers {
		mv := modVK(m)
		if mv == 0 {
			return fmt.Errorf("keyinject: bad modifier %q", m)
		}
		events = append(events, keydown(mv))
	}
	events = append(events, keydown(vk))
	return sendKeys(events)
}

// Release lifts the primary key then modifiers in reverse order.
func Release(c Combo) error {
	vk, ok := vkFor(c.Key)
	if !ok {
		return fmt.Errorf("keyinject: no VK for %q", c.Key)
	}
	var modVKs []uint16
	for _, m := range c.Modifiers {
		mv := modVK(m)
		if mv == 0 {
			return fmt.Errorf("keyinject: bad modifier %q", m)
		}
		modVKs = append(modVKs, mv)
	}
	events := []keyboardInput{keyup(vk)}
	for i := len(modVKs) - 1; i >= 0; i-- {
		events = append(events, keyup(modVKs[i]))
	}
	return sendKeys(events)
}

// TypeText sends each Unicode character in s as a synthetic key event,
// bypassing the keyboard layout. Suitable for inserting arbitrary text
// into applications that accept WM_KEYDOWN with KEYEVENTF_UNICODE.
// Surrogate pairs (codepoints > U+FFFF) are sent as two UTF-16 units.
func TypeText(s string) error {
	var events []keyboardInput
	for _, r := range s {
		if r <= 0xFFFF {
			scan := uint16(r)
			events = append(events,
				keyboardInput{Scan: scan, Flags: keyeventfUnicode},
				keyboardInput{Scan: scan, Flags: keyeventfUnicode | keyeventfKeyUp},
			)
		} else {
			// Encode as UTF-16 surrogate pair.
			r -= 0x10000
			high := uint16(0xD800 + (r>>10)&0x3FF)
			low := uint16(0xDC00 + r&0x3FF)
			events = append(events,
				keyboardInput{Scan: high, Flags: keyeventfUnicode},
				keyboardInput{Scan: low, Flags: keyeventfUnicode},
				keyboardInput{Scan: low, Flags: keyeventfUnicode | keyeventfKeyUp},
				keyboardInput{Scan: high, Flags: keyeventfUnicode | keyeventfKeyUp},
			)
		}
	}
	return sendKeys(events)
}
