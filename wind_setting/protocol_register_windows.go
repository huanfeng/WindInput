//go:build windows

package main

import (
	"fmt"
	"os"
	"strings"

	"golang.org/x/sys/windows/registry"
)

// protocolManagedBySystem 表示协议注册是否由系统声明式托管（Windows=false，可运行时开关）。
const protocolManagedBySystem = false

const protocolScheme = "windinput"

// protocolKeyPath 是协议根键在 HKCU 下的路径（var 便于测试覆盖）。
var protocolKeyPath = `Software\Classes\` + protocolScheme

// protocolCommand 返回期望的 shell\open\command 值。
func protocolCommand(exePath string) string {
	return `"` + exePath + `" "%1"`
}

// RegisterProtocol 把 windinput:// 注册到当前用户 HKCU，command 指向当前可执行文件。
func RegisterProtocol() error {
	exe, err := os.Executable()
	if err != nil {
		return err
	}
	return registerProtocolAt(registry.CURRENT_USER, protocolKeyPath, exe)
}

func registerProtocolAt(root registry.Key, keyPath, exePath string) error {
	k, _, err := registry.CreateKey(root, keyPath, registry.WRITE)
	if err != nil {
		return fmt.Errorf("创建协议键失败: %w", err)
	}
	defer k.Close()
	if err := k.SetStringValue("", "URL:清风输入法协议"); err != nil {
		return err
	}
	if err := k.SetStringValue("URL Protocol", ""); err != nil {
		return err
	}
	cmdKey, _, err := registry.CreateKey(root, keyPath+`\shell\open\command`, registry.WRITE)
	if err != nil {
		return fmt.Errorf("创建 command 键失败: %w", err)
	}
	defer cmdKey.Close()
	return cmdKey.SetStringValue("", protocolCommand(exePath))
}

// UnregisterProtocol 删除当前用户的协议键。
func UnregisterProtocol() error {
	return unregisterProtocolAt(registry.CURRENT_USER, protocolKeyPath)
}

func unregisterProtocolAt(root registry.Key, keyPath string) error {
	// DeleteKey 要求目标无子键，自底向上逐层删除。
	_ = registry.DeleteKey(root, keyPath+`\shell\open\command`)
	_ = registry.DeleteKey(root, keyPath+`\shell\open`)
	_ = registry.DeleteKey(root, keyPath+`\shell`)
	return registry.DeleteKey(root, keyPath)
}

// ProtocolStatus 返回 (是否已注册, 当前 command)。
func ProtocolStatus() (bool, string) {
	return protocolStatusAt(registry.CURRENT_USER, protocolKeyPath)
}

func protocolStatusAt(root registry.Key, keyPath string) (bool, string) {
	k, err := registry.OpenKey(root, keyPath+`\shell\open\command`, registry.QUERY_VALUE)
	if err != nil {
		return false, ""
	}
	defer k.Close()
	cmd, _, err := k.GetStringValue("")
	if err != nil {
		return false, ""
	}
	return true, cmd
}

// SelfHealProtocol 在设置程序启动时对账：缺失或 command 与当前 exe 不符则重写。
// 覆盖便携版移动、版本升级换路径的场景。
func SelfHealProtocol() {
	exe, err := os.Executable()
	if err != nil {
		return
	}
	registered, cmd := ProtocolStatus()
	if !registered || !strings.EqualFold(cmd, protocolCommand(exe)) {
		_ = RegisterProtocol()
	}
}
