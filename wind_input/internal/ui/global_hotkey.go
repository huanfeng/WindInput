//go:build windows

package ui

import (
	"log/slog"
	"strings"

	"github.com/huanfeng/wind_input/pkg/keys"
)

var (
	procRegisterHotKey   = user32.NewProc("RegisterHotKey")
	procUnregisterHotKey = user32.NewProc("UnregisterHotKey")
)

// Windows RegisterHotKey modifier constants
const (
	hotkeyModAlt      = 0x0001
	hotkeyModControl  = 0x0002
	hotkeyModShift    = 0x0004
	hotkeyModNoRepeat = 0x4000

	wmHotkey = 0x0312
)

// GlobalHotkeyEntry 已迁至 types_neutral.go (平台无关), 让 darwin stub 共享同一定义。

// ParseHotkeyString parses a hotkey config string (e.g., "ctrl+`") into a GlobalHotkeyEntry.
// Returns ok=false if the string is empty, "none", or unrecognized.
func ParseHotkeyString(s string, id int, command string) (GlobalHotkeyEntry, bool) {
	if s == "" || s == "none" {
		return GlobalHotkeyEntry{}, false
	}

	var mods uint32
	var vk uint32

	switch s {
	case "ctrl+`":
		mods = hotkeyModControl
		vk = 0xC0 // VK_OEM_3
	case "shift+space":
		mods = hotkeyModShift
		vk = 0x20 // VK_SPACE
	case "ctrl+.":
		mods = hotkeyModControl
		vk = 0xBE // VK_OEM_PERIOD
	case "ctrl+,":
		mods = hotkeyModControl
		vk = 0xBC // VK_OEM_COMMA
	case "ctrl+shift+e":
		mods = hotkeyModControl | hotkeyModShift
		vk = 0x45 // 'E'
	case "ctrl+shift+space":
		mods = hotkeyModControl | hotkeyModShift
		vk = 0x20 // VK_SPACE
	default:
		// Generic parser: split by "+" and resolve modifiers + key
		parts := strings.Split(strings.ToLower(s), "+")
		for i, part := range parts {
			switch keys.Modifier(part) {
			case keys.ModCtrl:
				mods |= hotkeyModControl
			case keys.ModShift:
				mods |= hotkeyModShift
			case keys.ModAlt:
				mods |= hotkeyModAlt
			default:
				if i == len(parts)-1 {
					vk = resolveVK(part)
				}
			}
		}
		if vk == 0 {
			return GlobalHotkeyEntry{}, false
		}
	}

	return GlobalHotkeyEntry{ID: id, Modifiers: mods, VK: vk, Command: command}, true
}

// vkByKey 把规范化 keys.Key 映射到 Windows 虚拟键码（仅本文件 RegisterHotKey 用到的子集）。
var vkByKey = map[keys.Key]uint32{
	keys.KeyGrave:     0xC0, // VK_OEM_3
	keys.KeySpace:     0x20, // VK_SPACE
	keys.KeyPeriod:    0xBE, // VK_OEM_PERIOD
	keys.KeyComma:     0xBC, // VK_OEM_COMMA
	keys.KeySemicolon: 0xBA, // VK_OEM_1
	keys.KeyQuote:     0xDE, // VK_OEM_7
	keys.KeySlash:     0xBF, // VK_OEM_2
	keys.KeyBackslash: 0xDC, // VK_OEM_5
	keys.KeyLBracket:  0xDB, // VK_OEM_4
	keys.KeyRBracket:  0xDD, // VK_OEM_6
	keys.KeyMinus:     0xBD, // VK_OEM_MINUS
	keys.KeyEqual:     0xBB, // VK_OEM_PLUS
	keys.KeyTab:       0x09,
	keys.KeyEscape:    0x1B,
}

func init() {
	// 字母 a-z -> 0x41-0x5A
	for c := byte('a'); c <= 'z'; c++ {
		vkByKey[keys.Key(string(c))] = uint32(c-'a') + 0x41
	}
	// 数字 0-9 -> 0x30-0x39
	for c := byte('0'); c <= '9'; c++ {
		vkByKey[keys.Key(string(c))] = uint32(c-'0') + 0x30
	}
	// F1-F12 -> 0x70-0x7B
	fNames := []keys.Key{
		keys.KeyF1, keys.KeyF2, keys.KeyF3, keys.KeyF4, keys.KeyF5, keys.KeyF6,
		keys.KeyF7, keys.KeyF8, keys.KeyF9, keys.KeyF10, keys.KeyF11, keys.KeyF12,
	}
	for i, k := range fNames {
		vkByKey[k] = 0x70 + uint32(i)
	}
}

// resolveVK converts a key name string (any alias / case) to a Windows virtual key code.
// Returns 0 if the name is not recognized.
func resolveVK(name string) uint32 {
	k, ok := keys.ParseKey(name)
	if !ok {
		return 0
	}
	return vkByKey[k]
}

// globalHotkeyState tracks registered hotkeys on the UI thread
type globalHotkeyState struct {
	entries  []GlobalHotkeyEntry
	callback func(command string)
	logger   *slog.Logger
}

func (s *globalHotkeyState) register(entries []GlobalHotkeyEntry) {
	// Unregister any previously registered hotkeys first
	s.unregister()

	for _, e := range entries {
		ret, _, err := procRegisterHotKey.Call(
			0, // NULL hwnd = thread-level hotkey
			uintptr(e.ID),
			uintptr(e.Modifiers|hotkeyModNoRepeat),
			uintptr(e.VK),
		)
		if ret == 0 {
			if s.logger != nil {
				s.logger.Warn("Failed to register global hotkey",
					"command", e.Command, "id", e.ID, "vk", e.VK, "mods", e.Modifiers, "error", err)
			}
		} else {
			if s.logger != nil {
				s.logger.Debug("Registered global hotkey",
					"command", e.Command, "id", e.ID, "vk", e.VK, "mods", e.Modifiers)
			}
		}
	}
	s.entries = entries
}

func (s *globalHotkeyState) unregister() {
	for _, e := range s.entries {
		procUnregisterHotKey.Call(0, uintptr(e.ID))
	}
	if len(s.entries) > 0 && s.logger != nil {
		s.logger.Debug("Unregistered all global hotkeys", "count", len(s.entries))
	}
	s.entries = nil
}

func (s *globalHotkeyState) handleWMHotkey(id int) {
	if s.callback == nil {
		return
	}
	for _, e := range s.entries {
		if e.ID == id {
			go s.callback(e.Command)
			return
		}
	}
}
