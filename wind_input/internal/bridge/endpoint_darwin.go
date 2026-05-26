//go:build darwin

package bridge

import (
	"os"
	"path/filepath"

	"github.com/huanfeng/wind_input/pkg/buildvariant"
)

// endpoint_darwin.go 定义 darwin 上 bridge IPC 的端点地址 (Unix Domain Socket)。
//
// 选址原则:
//   - 默认在 ~/Library/Application Support/WindInput<suffix>/ 下,
//     与 macOS 用户态应用数据约定一致, 不污染 /tmp
//   - macOS 上 UDS 路径长度有 104 字节限制 (sockaddr_un.sun_path),
//     Library 路径通常远短于此, 但避免在长用户名/含中文路径下溢出
//
// 端点名:
//   - bridge.sock:      主请求/响应通道, 与 BridgePipeName 语义对齐
//   - bridge_push.sock: 服务端 → 客户端推送通道
//
// Win 上的 \\.\pipe\... 与 darwin 上的 socket 路径形态不同,
// 但都通过同名变量暴露给 server.Start() 与 cmd/service/main.go 使用。

// BridgePipeName 主请求/响应通道的 UDS 路径。
// 实际值在 init() 中按 $HOME / buildvariant 拼出。
var BridgePipeName = "" //  bridge.sock 路径; init() 中赋值

// PushPipeName 服务端 → 客户端推送通道的 UDS 路径。
var PushPipeName = ""

func init() {
	dir := bridgeRuntimeDir()
	BridgePipeName = filepath.Join(dir, "bridge.sock")
	PushPipeName = filepath.Join(dir, "bridge_push.sock")
}

// bridgeRuntimeDir 计算 darwin 上 bridge socket 的存放目录。
// 优先级:
//  1. $WIND_INPUT_RUNTIME_DIR (若设置且非空, 测试与 portable 场景用)
//  2. $HOME/Library/Application Support/WindInput<suffix>
//  3. fallback /tmp/wind_input<suffix> (HOME 缺失的退化路径)
//
// 目录不存在时 server.Start() 会 MkdirAll。
func bridgeRuntimeDir() string {
	if d := os.Getenv("WIND_INPUT_RUNTIME_DIR"); d != "" {
		return d
	}
	suffix := buildvariant.Suffix()
	home, err := os.UserHomeDir()
	if err != nil || home == "" {
		return filepath.Join("/tmp", "wind_input"+suffix)
	}
	return filepath.Join(home, "Library", "Application Support", "WindInput"+suffix)
}
