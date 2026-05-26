//go:build darwin

package rpcapi

import (
	"net"
	"os"
	"path/filepath"
	"time"

	"github.com/huanfeng/wind_input/pkg/buildvariant"
)

// RPCPipeName 控制 RPC 端点路径 (Wails 设置端 ↔ wind_input 服务).
// 在 darwin 上是 Unix Domain Socket 路径; init() 中按 $HOME / buildvariant 拼出。
var RPCPipeName = ""

// RPCEventPipeName 事件推送端点路径 (服务端 → Wails)。
var RPCEventPipeName = ""

func init() {
	dir := rpcRuntimeDir()
	RPCPipeName = filepath.Join(dir, "rpc.sock")
	RPCEventPipeName = filepath.Join(dir, "rpc_events.sock")
}

// rpcRuntimeDir 计算 darwin 上 RPC socket 的存放目录。
// 与 internal/bridge 端点保持一致约定: ~/Library/Application Support/WindInput<suffix>。
// 可由 WIND_INPUT_RUNTIME_DIR 覆盖, 方便测试与 portable 模式。
func rpcRuntimeDir() string {
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

// dialEndpoint darwin 上走 net.Dial("unix", path) 连 UDS。timeout 通过 Dialer 实现。
func dialEndpoint(name string, timeout time.Duration) (net.Conn, error) {
	d := net.Dialer{Timeout: timeout}
	return d.Dial("unix", name)
}
