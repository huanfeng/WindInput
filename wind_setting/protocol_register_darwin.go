//go:build darwin

package main

// macOS 上 windinput:// 由 Info.plist 的 CFBundleURLTypes 声明式注册，
// 随 .app 打包、LaunchServices 自动登记，无运行时写入需求。

// protocolManagedBySystem=true：前端据此把注册区块显示为只读。
const protocolManagedBySystem = true

func RegisterProtocol() error   { return nil }
func UnregisterProtocol() error { return nil }

func ProtocolStatus() (bool, string) {
	return true, "由系统管理（随应用注册）"
}

func SelfHealProtocol() {}
