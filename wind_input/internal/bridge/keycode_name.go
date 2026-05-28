package bridge

import (
	"fmt"

	"github.com/huanfeng/wind_input/internal/ipc"
)

// keyCodeToKeyName converts a virtual key code to a key name string,
// matching the convention used by the Coordinator/Engine input layer.
// 原在 server_handler.go (windows-only), 抽出来让 darwin 路径 (server_darwin.go)
// 也能填 KeyEventData.Key 字段 — 否则 engine 永远 "Unhandled key".
func keyCodeToKeyName(keyCode uint32) string {
	switch keyCode {
	case ipc.VK_BACK:
		return "backspace"
	case ipc.VK_TAB:
		return "tab"
	case ipc.VK_RETURN:
		return "enter"
	case ipc.VK_ESCAPE:
		return "escape"
	case ipc.VK_SPACE:
		return "space"
	case ipc.VK_PRIOR:
		return "page_up"
	case ipc.VK_NEXT:
		return "page_down"
	case ipc.VK_CAPITAL:
		return "capslock"
	case ipc.VK_LSHIFT:
		return "lshift"
	case ipc.VK_RSHIFT:
		return "rshift"
	case ipc.VK_LCONTROL:
		return "lctrl"
	case ipc.VK_RCONTROL:
		return "rctrl"
	case ipc.VK_OEM_1:
		return ";"
	case ipc.VK_OEM_PLUS:
		return "="
	case ipc.VK_OEM_COMMA:
		return ","
	case ipc.VK_OEM_MINUS:
		return "-"
	case ipc.VK_OEM_PERIOD:
		return "."
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
	default:
		if keyCode >= 0x41 && keyCode <= 0x5A {
			return string(rune('a' + keyCode - 0x41))
		}
		if keyCode >= 0x30 && keyCode <= 0x39 {
			return string(rune('0' + keyCode - 0x30))
		}
		if keyCode >= 0x60 && keyCode <= 0x69 {
			return string(rune('0' + keyCode - 0x60))
		}
		switch keyCode {
		case 0x6A:
			return "*"
		case 0x6B:
			return "+"
		case 0x6D:
			return "-"
		case 0x6E:
			return "."
		case 0x6F:
			return "/"
		}
		return fmt.Sprintf("vk_%d", keyCode)
	}
}
