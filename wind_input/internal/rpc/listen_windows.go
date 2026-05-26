//go:build windows

package rpc

import (
	"net"

	"github.com/Microsoft/go-winio"
)

// listenWindows 与 listen_darwin.go 对称, 为 server.go / event.go 提供平台 listener。
//
// 通过 go-winio overlapped I/O 监听 Named Pipe, 与 PR-6 之前的行为完全一致。

// listenRPCEndpoint 启动 RPC 端点监听 (Windows: Named Pipe)。
// inputBuf / outputBuf 分别控制 input/output 缓冲区大小 (字节)。
func listenRPCEndpoint(name string, inputBuf, outputBuf int32) (net.Listener, error) {
	pipeConfig := &winio.PipeConfig{
		// SDDL: 允许 SYSTEM(SY)、管理员(BA)和所有已认证用户(AU)完全访问。
		// 解决提升权限进程创建的管道默认 DACL 阻止非提升进程连接的问题。
		SecurityDescriptor: "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AU)",
		InputBufferSize:    inputBuf,
		OutputBufferSize:   outputBuf,
	}
	return winio.ListenPipe(name, pipeConfig)
}
