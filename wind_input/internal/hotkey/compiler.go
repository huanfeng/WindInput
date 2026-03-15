// Package hotkey provides hotkey compilation and management
package hotkey

import (
	"strings"

	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/pkg/config"
)

// Compiler compiles hotkey configuration into KeyHash lists for C++ side
type Compiler struct {
	config *config.Config
}

// NewCompiler creates a new hotkey compiler
func NewCompiler(cfg *config.Config) *Compiler {
	return &Compiler{config: cfg}
}

// UpdateConfig updates the configuration reference
func (c *Compiler) UpdateConfig(cfg *config.Config) {
	c.config = cfg
}

// Compile compiles all hotkeys into KeyDown and KeyUp hash lists
// keyDownList: hotkeys triggered on key down
// keyUpList: hotkeys triggered on key up (toggle mode keys like Shift, Ctrl, CapsLock)
func (c *Compiler) Compile() (keyDownList, keyUpList []uint32) {
	if c.config == nil {
		return nil, nil
	}

	// =========================================================================
	// KeyDown triggered hotkeys
	// =========================================================================

	// 1. Function hotkeys (Ctrl+`, Shift+Space, Ctrl+., etc.)
	if hash, ok := c.parseHotkeyString(c.config.Hotkeys.SwitchEngine); ok {
		keyDownList = append(keyDownList, hash)
	}
	if hash, ok := c.parseHotkeyString(c.config.Hotkeys.ToggleFullWidth); ok {
		keyDownList = append(keyDownList, hash)
	}
	if hash, ok := c.parseHotkeyString(c.config.Hotkeys.TogglePunct); ok {
		keyDownList = append(keyDownList, hash)
	}

	// 2. Select key groups (semicolon_quote, comma_period, lrshift, lrctrl)
	// Note: These are only active when there are candidates, but we still
	// add them to the whitelist. Go side will handle the context check.
	for _, group := range c.config.Input.SelectKeyGroups {
		hashes := c.compileSelectKeyGroup(group)
		keyDownList = append(keyDownList, hashes...)
	}

	// 3. Page keys (pageupdown, minus_equal, brackets, shift_tab)
	for _, pk := range c.config.Input.PageKeys {
		hashes := c.compilePageKeyGroup(pk)
		keyDownList = append(keyDownList, hashes...)
	}

	// 4. Highlight keys (arrows are CursorKeys handled by C++, tab needs explicit registration)
	for _, hk := range c.config.Input.HighlightKeys {
		hashes := c.compileHighlightKeyGroup(hk)
		keyDownList = append(keyDownList, hashes...)
	}

	// =========================================================================
	// KeyUp triggered hotkeys (toggle mode keys)
	// =========================================================================
	for _, key := range c.config.Hotkeys.ToggleModeKeys {
		if hash, ok := c.compileToggleModeKey(key); ok {
			keyUpList = append(keyUpList, hash)
		}
	}

	return keyDownList, keyUpList
}

// parseHotkeyString parses a hotkey string like "ctrl+`", "shift+space" into KeyHash
func (c *Compiler) parseHotkeyString(hotkeyStr string) (uint32, bool) {
	if hotkeyStr == "" || hotkeyStr == "none" {
		return 0, false
	}

	hotkeyStr = strings.ToLower(hotkeyStr)
	parts := strings.Split(hotkeyStr, "+")
	if len(parts) == 0 {
		return 0, false
	}

	var mods uint32
	var keyCode uint32
	var hasKey bool

	for _, part := range parts {
		part = strings.TrimSpace(part)
		switch part {
		case "ctrl":
			mods |= ipc.ModCtrl
		case "shift":
			mods |= ipc.ModShift
		case "alt":
			mods |= ipc.ModAlt
		case "win":
			mods |= ipc.ModWin
		default:
			// This is the key part
			if code, ok := getVirtualKeyCode(part); ok {
				keyCode = code
				hasKey = true
			}
		}
	}

	if !hasKey {
		return 0, false
	}

	return ipc.CalcKeyHash(mods, keyCode), true
}

// compileToggleModeKey compiles a toggle mode key name to KeyHash
// Note: When a modifier key is pressed, C++ GetCurrentModifiers() returns BOTH
// the generic modifier (ModShift/ModCtrl) AND the specific one (ModLShift/ModRShift).
// So we need to include both in the hash for proper matching.
func (c *Compiler) compileToggleModeKey(key string) (uint32, bool) {
	switch strings.ToLower(key) {
	case "lshift":
		// Left Shift: includes both generic Shift and specific LShift
		return ipc.CalcKeyHash(ipc.ModShift|ipc.ModLShift, ipc.VK_LSHIFT), true
	case "rshift":
		// Right Shift: includes both generic Shift and specific RShift
		return ipc.CalcKeyHash(ipc.ModShift|ipc.ModRShift, ipc.VK_RSHIFT), true
	case "lctrl":
		// Left Ctrl: includes both generic Ctrl and specific LCtrl
		return ipc.CalcKeyHash(ipc.ModCtrl|ipc.ModLCtrl, ipc.VK_LCONTROL), true
	case "rctrl":
		// Right Ctrl: includes both generic Ctrl and specific RCtrl
		return ipc.CalcKeyHash(ipc.ModCtrl|ipc.ModRCtrl, ipc.VK_RCONTROL), true
	case "capslock":
		// CapsLock uses special marker
		return ipc.CalcKeyHash(ipc.ModCapsLock, ipc.VK_CAPITAL), true
	default:
		return 0, false
	}
}

// compileSelectKeyGroup compiles a select key group to KeyHash list
func (c *Compiler) compileSelectKeyGroup(group string) []uint32 {
	var hashes []uint32

	switch group {
	case "semicolon_quote":
		// ; and '
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_OEM_1)) // ;
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_OEM_7)) // '
	case "comma_period":
		// , and .
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_OEM_COMMA))  // ,
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_OEM_PERIOD)) // .
	case "lrshift":
		// Left/Right Shift as select keys (include both generic and specific modifiers)
		hashes = append(hashes, ipc.CalcKeyHash(ipc.ModShift|ipc.ModLShift, ipc.VK_LSHIFT))
		hashes = append(hashes, ipc.CalcKeyHash(ipc.ModShift|ipc.ModRShift, ipc.VK_RSHIFT))
	case "lrctrl":
		// Left/Right Ctrl as select keys (include both generic and specific modifiers)
		hashes = append(hashes, ipc.CalcKeyHash(ipc.ModCtrl|ipc.ModLCtrl, ipc.VK_LCONTROL))
		hashes = append(hashes, ipc.CalcKeyHash(ipc.ModCtrl|ipc.ModRCtrl, ipc.VK_RCONTROL))
	}

	return hashes
}

// compilePageKeyGroup compiles a page key group to KeyHash list
func (c *Compiler) compilePageKeyGroup(group string) []uint32 {
	var hashes []uint32

	switch group {
	case "pageupdown":
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_PRIOR)) // PageUp
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_NEXT))  // PageDown
	case "minus_equal":
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_OEM_MINUS)) // -
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_OEM_PLUS))  // =
	case "brackets":
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_OEM_4)) // [
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_OEM_6)) // ]
	case "shift_tab":
		// Shift+Tab for page up, Tab alone for page down
		hashes = append(hashes, ipc.CalcKeyHash(ipc.ModShift, ipc.VK_TAB)) // Shift+Tab
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_TAB))            // Tab
	}

	return hashes
}

// compileHighlightKeyGroup compiles a highlight key group to KeyHash list
func (c *Compiler) compileHighlightKeyGroup(group string) []uint32 {
	var hashes []uint32

	switch group {
	case "tab":
		// Tab for highlight down, Shift+Tab for highlight up
		hashes = append(hashes, ipc.CalcKeyHash(ipc.ModShift, ipc.VK_TAB)) // Shift+Tab
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_TAB))            // Tab
		// "arrows" doesn't need compilation - VK_UP/VK_DOWN are CursorKeys handled by C++
	}

	return hashes
}

// getVirtualKeyCode maps a key name to Windows virtual key code
func getVirtualKeyCode(keyName string) (uint32, bool) {
	switch strings.ToLower(keyName) {
	// Special keys
	case "`", "~", "grave":
		return ipc.VK_OEM_3, true
	case "space":
		return ipc.VK_SPACE, true
	case ".", "period":
		return ipc.VK_OEM_PERIOD, true
	case ",", "comma":
		return ipc.VK_OEM_COMMA, true
	case ";", "semicolon":
		return ipc.VK_OEM_1, true
	case "'", "quote":
		return ipc.VK_OEM_7, true
	case "-", "minus":
		return ipc.VK_OEM_MINUS, true
	case "=", "equal", "plus":
		return ipc.VK_OEM_PLUS, true
	case "[", "lbracket":
		return ipc.VK_OEM_4, true
	case "]", "rbracket":
		return ipc.VK_OEM_6, true
	case "\\", "backslash":
		return ipc.VK_OEM_5, true
	case "/", "slash":
		return ipc.VK_OEM_2, true
	case "tab":
		return ipc.VK_TAB, true
	case "enter", "return":
		return ipc.VK_RETURN, true
	case "backspace", "back":
		return ipc.VK_BACK, true
	case "escape", "esc":
		return ipc.VK_ESCAPE, true
	case "pageup", "prior":
		return ipc.VK_PRIOR, true
	case "pagedown", "next":
		return ipc.VK_NEXT, true

	// Letters A-Z
	case "a":
		return 0x41, true
	case "b":
		return 0x42, true
	case "c":
		return 0x43, true
	case "d":
		return 0x44, true
	case "e":
		return 0x45, true
	case "f":
		return 0x46, true
	case "g":
		return 0x47, true
	case "h":
		return 0x48, true
	case "i":
		return 0x49, true
	case "j":
		return 0x4A, true
	case "k":
		return 0x4B, true
	case "l":
		return 0x4C, true
	case "m":
		return 0x4D, true
	case "n":
		return 0x4E, true
	case "o":
		return 0x4F, true
	case "p":
		return 0x50, true
	case "q":
		return 0x51, true
	case "r":
		return 0x52, true
	case "s":
		return 0x53, true
	case "t":
		return 0x54, true
	case "u":
		return 0x55, true
	case "v":
		return 0x56, true
	case "w":
		return 0x57, true
	case "x":
		return 0x58, true
	case "y":
		return 0x59, true
	case "z":
		return 0x5A, true

	// Numbers 0-9
	case "0":
		return 0x30, true
	case "1":
		return 0x31, true
	case "2":
		return 0x32, true
	case "3":
		return 0x33, true
	case "4":
		return 0x34, true
	case "5":
		return 0x35, true
	case "6":
		return 0x36, true
	case "7":
		return 0x37, true
	case "8":
		return 0x38, true
	case "9":
		return 0x39, true

	// Function keys
	case "f1":
		return 0x70, true
	case "f2":
		return 0x71, true
	case "f3":
		return 0x72, true
	case "f4":
		return 0x73, true
	case "f5":
		return 0x74, true
	case "f6":
		return 0x75, true
	case "f7":
		return 0x76, true
	case "f8":
		return 0x77, true
	case "f9":
		return 0x78, true
	case "f10":
		return 0x79, true
	case "f11":
		return 0x7A, true
	case "f12":
		return 0x7B, true

	default:
		return 0, false
	}
}

// GetHotkeyDisplayName returns a human-readable name for a key hash
func GetHotkeyDisplayName(hash uint32) string {
	mods, keyCode := ipc.ParseKeyHash(hash)

	var parts []string

	if mods&ipc.ModCtrl != 0 {
		parts = append(parts, "Ctrl")
	}
	if mods&ipc.ModShift != 0 {
		parts = append(parts, "Shift")
	}
	if mods&ipc.ModAlt != 0 {
		parts = append(parts, "Alt")
	}
	if mods&ipc.ModWin != 0 {
		parts = append(parts, "Win")
	}

	keyName := getKeyName(keyCode)
	parts = append(parts, keyName)

	return strings.Join(parts, "+")
}

// getKeyName returns a human-readable name for a virtual key code
func getKeyName(keyCode uint32) string {
	switch keyCode {
	case ipc.VK_SPACE:
		return "Space"
	case ipc.VK_TAB:
		return "Tab"
	case ipc.VK_RETURN:
		return "Enter"
	case ipc.VK_BACK:
		return "Backspace"
	case ipc.VK_ESCAPE:
		return "Esc"
	case ipc.VK_PRIOR:
		return "PageUp"
	case ipc.VK_NEXT:
		return "PageDown"
	case ipc.VK_CAPITAL:
		return "CapsLock"
	case ipc.VK_LSHIFT:
		return "LShift"
	case ipc.VK_RSHIFT:
		return "RShift"
	case ipc.VK_LCONTROL:
		return "LCtrl"
	case ipc.VK_RCONTROL:
		return "RCtrl"
	case ipc.VK_OEM_1:
		return ";"
	case ipc.VK_OEM_2:
		return "/"
	case ipc.VK_OEM_3:
		return "`"
	case ipc.VK_OEM_4:
		return "["
	case ipc.VK_OEM_5:
		return "\\"
	case ipc.VK_OEM_6:
		return "]"
	case ipc.VK_OEM_7:
		return "'"
	case ipc.VK_OEM_COMMA:
		return ","
	case ipc.VK_OEM_PERIOD:
		return "."
	case ipc.VK_OEM_MINUS:
		return "-"
	case ipc.VK_OEM_PLUS:
		return "="
	default:
		// Letters and numbers
		if keyCode >= 0x41 && keyCode <= 0x5A {
			return string(rune('A' + keyCode - 0x41))
		}
		if keyCode >= 0x30 && keyCode <= 0x39 {
			return string(rune('0' + keyCode - 0x30))
		}
		if keyCode >= 0x70 && keyCode <= 0x7B {
			return "F" + string(rune('1'+keyCode-0x70))
		}
		return "?"
	}
}
