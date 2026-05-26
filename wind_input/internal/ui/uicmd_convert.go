package ui

import "github.com/huanfeng/wind_input/internal/uicmd"

// uicmd_convert.go 提供 ui 包内部类型与 internal/uicmd wire 镜像之间的双向映射。
//
// 设计要点:
//   - ui 端历史枚举 (ToastLevel/ToastPosition) 是 int (iota), 而 uicmd 端用 string 字面值
//     以保证跨语言可读 (macOS IMKit 端解析 "info"/"success" 比解析数字直观)。
//   - 故必须做显式映射, 不能直接 cast。
//   - 反向映射 (uicmd → ui) 对未识别值返回默认值, 保证 wire 兼容性扩展。

// ---- Toolbar ----

func toUIToolbarState(s ToolbarState) uicmd.ToolbarState {
	return uicmd.ToolbarState{
		ChineseMode:   s.ChineseMode,
		CapsLock:      s.CapsLock,
		FullWidth:     s.FullWidth,
		ChinesePunct:  s.ChinesePunct,
		EffectiveMode: int32(s.EffectiveMode),
		ModeLabel:     s.ModeLabel,
	}
}

func fromUIToolbarState(s uicmd.ToolbarState) ToolbarState {
	return ToolbarState{
		ChineseMode:   s.ChineseMode,
		CapsLock:      s.CapsLock,
		FullWidth:     s.FullWidth,
		ChinesePunct:  s.ChinesePunct,
		EffectiveMode: int(s.EffectiveMode),
		ModeLabel:     s.ModeLabel,
	}
}

// ---- Status ----

func toUIStatusState(s StatusState) uicmd.StatusState {
	return uicmd.StatusState{
		ModeLabel:  s.ModeLabel,
		PunctLabel: s.PunctLabel,
		WidthLabel: s.WidthLabel,
	}
}

// ---- Hotkeys ----

func toUIHotkeyEntries(in []GlobalHotkeyEntry) []uicmd.HotkeyEntry {
	if len(in) == 0 {
		return nil
	}
	out := make([]uicmd.HotkeyEntry, len(in))
	for i, e := range in {
		out[i] = uicmd.HotkeyEntry{
			ID:      int32(e.ID),
			Mods:    e.Modifiers,
			KeyCode: e.VK,
			Command: e.Command,
		}
	}
	return out
}

func fromUIHotkeyEntries(in []uicmd.HotkeyEntry) []GlobalHotkeyEntry {
	if len(in) == 0 {
		return nil
	}
	out := make([]GlobalHotkeyEntry, len(in))
	for i, e := range in {
		out[i] = GlobalHotkeyEntry{
			ID:        int(e.ID),
			Modifiers: e.Mods,
			VK:        e.KeyCode,
			Command:   e.Command,
		}
	}
	return out
}

// ---- Toast ----

func toUIToastLevel(l ToastLevel) uicmd.ToastLevel {
	switch l {
	case ToastSuccess:
		return uicmd.ToastSuccess
	case ToastWarn:
		return uicmd.ToastWarn
	case ToastError:
		return uicmd.ToastError
	default:
		return uicmd.ToastInfo
	}
}

func fromUIToastLevel(l uicmd.ToastLevel) ToastLevel {
	switch l {
	case uicmd.ToastSuccess:
		return ToastSuccess
	case uicmd.ToastWarn:
		return ToastWarn
	case uicmd.ToastError:
		return ToastError
	default:
		return ToastInfo
	}
}

func toUIToastPosition(p ToastPosition) uicmd.ToastPosition {
	switch p {
	case ToastBottomRight:
		return uicmd.ToastBottomRight
	default:
		// ToastCenter / ToastTopRight / ToastTop 在 wire 镜像未独立列出时,
		// 统一回退到 center (与 IMKit 端最常用位置一致)。
		return uicmd.ToastCenter
	}
}

func fromUIToastPosition(p uicmd.ToastPosition) ToastPosition {
	switch p {
	case uicmd.ToastBottomRight:
		return ToastBottomRight
	default:
		return ToastCenter
	}
}
