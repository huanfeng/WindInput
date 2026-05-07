// Package hotkey provides hotkey compilation and management
package hotkey

import (
	"strings"

	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/pkg/buildvariant"
	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/keys"
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
	if hash, ok := c.parseHotkeyString(c.config.Hotkeys.AddWord); ok {
		keyDownList = append(keyDownList, hash)
	}
	if hash, ok := c.parseHotkeyString(c.config.Hotkeys.ToggleToolbar); ok {
		keyDownList = append(keyDownList, hash)
	}
	if hash, ok := c.parseHotkeyString(c.config.Hotkeys.OpenSettings); ok {
		keyDownList = append(keyDownList, hash)
	}
	if hash, ok := c.parseHotkeyString(c.config.Hotkeys.ToggleS2T); ok {
		keyDownList = append(keyDownList, hash)
	}

	// Debug: Ctrl+Shift+R for clipboard paste code (hardcoded, debug only)
	if buildvariant.IsDebug() {
		keyDownList = append(keyDownList, ipc.CalcKeyHash(ipc.ModCtrl|ipc.ModShift, 0x52))
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
		switch keys.Modifier(part) {
		case keys.ModCtrl:
			mods |= ipc.ModCtrl
		case keys.ModShift:
			mods |= ipc.ModShift
		case keys.ModAlt:
			mods |= ipc.ModAlt
		case keys.ModWin:
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
	k, ok := keys.ParseKey(key)
	if !ok {
		return 0, false
	}
	switch k {
	case keys.KeyLShift:
		// Left Shift: includes both generic Shift and specific LShift
		return ipc.CalcKeyHash(ipc.ModShift|ipc.ModLShift, ipc.VK_LSHIFT), true
	case keys.KeyRShift:
		// Right Shift: includes both generic Shift and specific RShift
		return ipc.CalcKeyHash(ipc.ModShift|ipc.ModRShift, ipc.VK_RSHIFT), true
	case keys.KeyLCtrl:
		// Left Ctrl: includes both generic Ctrl and specific LCtrl
		return ipc.CalcKeyHash(ipc.ModCtrl|ipc.ModLCtrl, ipc.VK_LCONTROL), true
	case keys.KeyRCtrl:
		// Right Ctrl: includes both generic Ctrl and specific RCtrl
		return ipc.CalcKeyHash(ipc.ModCtrl|ipc.ModRCtrl, ipc.VK_RCONTROL), true
	case keys.KeyCapsLock:
		// CapsLock uses special marker
		return ipc.CalcKeyHash(ipc.ModCapsLock, ipc.VK_CAPITAL), true
	default:
		return 0, false
	}
}

// compileSelectKeyGroup compiles a select key group to KeyHash list
func (c *Compiler) compileSelectKeyGroup(group keys.PairGroup) []uint32 {
	var hashes []uint32

	switch group {
	case keys.PairSemicolonQuote:
		// ; and '
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_OEM_1)) // ;
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_OEM_7)) // '
	case keys.PairCommaPeriod:
		// , and .
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_OEM_COMMA))  // ,
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_OEM_PERIOD)) // .
	case keys.PairLRShift:
		// Left/Right Shift as select keys (include both generic and specific modifiers)
		hashes = append(hashes, ipc.CalcKeyHash(ipc.ModShift|ipc.ModLShift, ipc.VK_LSHIFT))
		hashes = append(hashes, ipc.CalcKeyHash(ipc.ModShift|ipc.ModRShift, ipc.VK_RSHIFT))
	case keys.PairLRCtrl:
		// Left/Right Ctrl as select keys (include both generic and specific modifiers)
		hashes = append(hashes, ipc.CalcKeyHash(ipc.ModCtrl|ipc.ModLCtrl, ipc.VK_LCONTROL))
		hashes = append(hashes, ipc.CalcKeyHash(ipc.ModCtrl|ipc.ModRCtrl, ipc.VK_RCONTROL))
	}

	return hashes
}

// compilePageKeyGroup compiles a page key group to KeyHash list
func (c *Compiler) compilePageKeyGroup(group keys.PairGroup) []uint32 {
	var hashes []uint32

	switch group {
	case keys.PairPageUpDown:
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_PRIOR)) // PageUp
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_NEXT))  // PageDown
	case keys.PairMinusEqual:
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_OEM_MINUS)) // -
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_OEM_PLUS))  // =
	case keys.PairBrackets:
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_OEM_4)) // [
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_OEM_6)) // ]
	case keys.PairShiftTab:
		// Shift+Tab for page up, Tab alone for page down
		hashes = append(hashes, ipc.CalcKeyHash(ipc.ModShift, ipc.VK_TAB)) // Shift+Tab
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_TAB))            // Tab
	}

	return hashes
}

// compileHighlightKeyGroup compiles a highlight key group to KeyHash list
func (c *Compiler) compileHighlightKeyGroup(group keys.PairGroup) []uint32 {
	var hashes []uint32

	switch group {
	case keys.PairTab:
		// Tab for highlight down, Shift+Tab for highlight up
		hashes = append(hashes, ipc.CalcKeyHash(ipc.ModShift, ipc.VK_TAB)) // Shift+Tab
		hashes = append(hashes, ipc.CalcKeyHash(0, ipc.VK_TAB))            // Tab
		// PairArrows doesn't need compilation - VK_UP/VK_DOWN are CursorKeys handled by C++
	}

	return hashes
}

// keyToVK 把规范化的 keys.Key 映射到 Windows 虚拟键码。
// 字母 a-z、数字 0-9、F1-F12 通过 init() 批量注册，其余在 var 中显式声明。
var keyToVK = map[keys.Key]uint32{
	keys.KeyGrave:     ipc.VK_OEM_3,
	keys.KeySpace:     ipc.VK_SPACE,
	keys.KeyPeriod:    ipc.VK_OEM_PERIOD,
	keys.KeyComma:     ipc.VK_OEM_COMMA,
	keys.KeySemicolon: ipc.VK_OEM_1,
	keys.KeyQuote:     ipc.VK_OEM_7,
	keys.KeyMinus:     ipc.VK_OEM_MINUS,
	keys.KeyEqual:     ipc.VK_OEM_PLUS,
	keys.KeyLBracket:  ipc.VK_OEM_4,
	keys.KeyRBracket:  ipc.VK_OEM_6,
	keys.KeyBackslash: ipc.VK_OEM_5,
	keys.KeySlash:     ipc.VK_OEM_2,
	keys.KeyTab:       ipc.VK_TAB,
	keys.KeyEnter:     ipc.VK_RETURN,
	keys.KeyBackspace: ipc.VK_BACK,
	keys.KeyEscape:    ipc.VK_ESCAPE,
	keys.KeyPageUp:    ipc.VK_PRIOR,
	keys.KeyPageDown:  ipc.VK_NEXT,
}

func init() {
	// 字母 a-z -> 0x41-0x5A
	for c := byte('a'); c <= 'z'; c++ {
		keyToVK[keys.Key(string(c))] = uint32(c-'a') + 0x41
	}
	// 数字 0-9 -> 0x30-0x39
	for c := byte('0'); c <= '9'; c++ {
		keyToVK[keys.Key(string(c))] = uint32(c-'0') + 0x30
	}
	// F1-F12 -> 0x70-0x7B
	for i := 1; i <= 12; i++ {
		keyToVK[keys.Key("f"+itoa(i))] = 0x70 + uint32(i-1)
	}
}

// itoa 返回 1..12 的十进制字符串（避免引入 strconv 仅为此用途）。
func itoa(i int) string {
	if i < 10 {
		return string(rune('0' + i))
	}
	return "1" + string(rune('0'+i-10))
}

// getVirtualKeyCode 把任意按键名（含别名/大小写）映射到 Windows 虚拟键码。
// 入口先经 keys.ParseKey 规范化，再查 keyToVK 表。
func getVirtualKeyCode(keyName string) (uint32, bool) {
	k, ok := keys.ParseKey(keyName)
	if !ok {
		return 0, false
	}
	vk, ok := keyToVK[k]
	return vk, ok
}

// GetHotkeyDisplayName returns a human-readable name for a key hash
// TODO: 供设置界面显示热键名称时使用
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
