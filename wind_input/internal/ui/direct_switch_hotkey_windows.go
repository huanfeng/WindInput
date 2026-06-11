//go:build windows

package ui

import (
	"fmt"
	"strconv"
	"strings"

	"github.com/huanfeng/wind_input/pkg/buildvariant"
	"golang.org/x/sys/windows/registry"
)

// directSwitchKeyPath 是 Windows TSF「直接切换热键」表的注册表路径。
// 写入此处的条目让系统在「为每个应用窗口使用不同输入法」模式下，把指定 TIP
// 关联到一个热键；ctfmon 监听本键变更并即时生效，实现 per-app 切换到本输入法
// （只影响当前前台应用，不波及整个会话）。这是 Windows 原生机制，WindInput 进程
// 本身不参与该热键的按键处理。
//
// 值格式（全部 REG_SZ 字符串，与系统 UI 写入一致；DWORD 不被识别）：
//
//	CLSID      = "{...}"      本输入法 TIP CLSID（随构建变体）
//	Profile    = "{...}"      本输入法 guidProfile（随构建变体）
//	LangId     = "00000804"   简体中文
//	Modifiers  = "0000c0XX"   0xC000(固定高位) | 修饰键位(Ctrl=0x02/Shift=0x04/Alt=0x01)
//	VirtualKey = "000000XX"   Windows 虚拟键码
const directSwitchKeyPath = `Software\Microsoft\CTF\DirectSwitchHotkeys`

// directSwitchSlotBase 是子键名起始值（与系统约定一致，0x1000 起递增）。
const directSwitchSlotBase = 0x1000

// directSwitchModBase 是 Modifiers 高位固定标志（样本中恒为 0xC000）。
const directSwitchModBase = 0xC000

// windInputGUIDStrings 返回当前构建变体的 (CLSID, guidProfile) 字符串（大写带花括号）。
func windInputGUIDStrings() (clsid, profile string) {
	if buildvariant.IsDebug() {
		return "{99C2DEB0-5C57-45A2-9C63-FB54B34FD90A}", "{99C2DEB1-5C57-45A2-9C63-FB54B34FD90A}"
	}
	return "{99C2EE30-5C57-45A2-9C63-FB54B34FD90A}", "{99C2EE31-5C57-45A2-9C63-FB54B34FD90A}"
}

// SyncDirectSwitchHotkey 把 activate_ime 热键同步到 Windows DirectSwitchHotkeys 表。
//   - 先删除所有属于本输入法（CLSID 匹配）的旧条目（幂等，避免重复/陈旧残留）
//   - hotkey 为空 / "none" / 解析失败 → 仅清理，不再创建
//   - 否则在一个未占用的 slot 创建新条目
//
// 可从任意 goroutine 调用。ctfmon 监听本键变更即时生效，无需重载。
func SyncDirectSwitchHotkey(hotkey string) error {
	clsid, profile := windInputGUIDStrings()

	root, _, err := registry.CreateKey(registry.CURRENT_USER, directSwitchKeyPath, registry.ALL_ACCESS)
	if err != nil {
		return fmt.Errorf("open DirectSwitchHotkeys: %w", err)
	}
	defer root.Close()

	// 枚举现有子键：删除本输入法旧条目，记录最大 slot 编号（供新条目避位）
	names, _ := root.ReadSubKeyNames(-1)
	maxSlot := directSwitchSlotBase - 1
	for _, name := range names {
		sub, err := registry.OpenKey(root, name, registry.QUERY_VALUE)
		if err != nil {
			continue
		}
		c, _, _ := sub.GetStringValue("CLSID")
		sub.Close()
		if strings.EqualFold(c, clsid) {
			_ = registry.DeleteKey(root, name)
			continue
		}
		if n, err := strconv.ParseInt(name, 16, 64); err == nil && int(n) > maxSlot {
			maxSlot = int(n)
		}
	}

	mods, vk, ok := parseDirectSwitchHotkey(hotkey)
	if !ok {
		return nil // 仅清理（禁用本功能）
	}

	slotName := fmt.Sprintf("%08X", maxSlot+1)
	slot, _, err := registry.CreateKey(root, slotName, registry.ALL_ACCESS)
	if err != nil {
		return fmt.Errorf("create slot %s: %w", slotName, err)
	}
	defer slot.Close()

	if err := slot.SetStringValue("CLSID", clsid); err != nil {
		return fmt.Errorf("write CLSID: %w", err)
	}
	if err := slot.SetStringValue("Profile", profile); err != nil {
		return err
	}
	if err := slot.SetStringValue("LangId", "00000804"); err != nil {
		return err
	}
	if err := slot.SetStringValue("Modifiers", fmt.Sprintf("%08x", directSwitchModBase|mods)); err != nil {
		return err
	}
	if err := slot.SetStringValue("VirtualKey", fmt.Sprintf("%08x", vk)); err != nil {
		return err
	}
	return nil
}

// parseDirectSwitchHotkey 解析 "ctrl+shift+[" → (modifiers, vk)。
// 复用 ParseHotkeyString：其 Modifiers 位（Ctrl=0x02/Shift=0x04/Alt=0x01）与
// DirectSwitch 低位编码一致；VK 为 Windows 虚拟键码。
func parseDirectSwitchHotkey(s string) (mods, vk uint32, ok bool) {
	s = strings.TrimSpace(s)
	if s == "" || s == "none" {
		return 0, 0, false
	}
	entry, ok := ParseHotkeyString(s, 1, "activate_ime")
	if !ok {
		return 0, 0, false
	}
	return entry.Modifiers, entry.VK, true
}
