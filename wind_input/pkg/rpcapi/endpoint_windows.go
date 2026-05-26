//go:build windows

package rpcapi

import (
	"net"
	"time"

	"github.com/Microsoft/go-winio"
	"github.com/huanfeng/wind_input/pkg/buildvariant"
)

// RPCPipeName 控制 RPC 管道名 (Wails 设置端 ↔ wind_input 服务)。
var RPCPipeName = `\\.\pipe\wind_input` + buildvariant.Suffix() + `_rpc`

// RPCEventPipeName 事件推送管道名 (服务端 → Wails 主动通知)。
var RPCEventPipeName = `\\.\pipe\wind_input` + buildvariant.Suffix() + `_events`

// dialEndpoint Windows 上走 winio.DialPipe (overlapped I/O Named Pipe)。
func dialEndpoint(name string, timeout time.Duration) (net.Conn, error) {
	return winio.DialPipe(name, &timeout)
}
