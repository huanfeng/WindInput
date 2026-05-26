//go:build windows

package bridge

import "github.com/huanfeng/wind_input/pkg/buildvariant"

// endpoint_windows.go 定义 Windows 上 bridge IPC 的端点地址。
//
// Windows 走 Named Pipe (\\.\pipe\wind_input + 可选 buildvariant 后缀);
// darwin 端走 Unix Domain Socket, 参见 endpoint_darwin.go。

// BridgePipeName 主请求/响应通道的命名管道路径。
// 由 wind_tsf 端 CreateFile 打开此名称连接到 Go 服务。
var BridgePipeName = `\\.\pipe\wind_input` + buildvariant.Suffix()

// PushPipeName 服务端 → 客户端推送通道的命名管道路径。
var PushPipeName = `\\.\pipe\wind_input` + buildvariant.Suffix() + `_push`
