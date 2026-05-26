//go:build darwin

package rpc

import (
	"fmt"
	"net"
	"os"
	"path/filepath"
)

// listen_darwin.go 提供 RPC 端点在 darwin 上的 Unix Domain Socket listener,
// 与 listen_windows.go 接口对称。pkg/rpcapi/endpoint_darwin.go 已经把
// RPCPipeName / RPCEventPipeName 设为 UDS 路径。

// listenRPCEndpoint 启动 RPC 端点监听 (darwin: Unix Domain Socket)。
// inputBuf / outputBuf 在 darwin 上忽略 (UDS 缓冲由内核管理)。
// 自动 MkdirAll 端点所在目录 + 清理 stale socket 文件。
func listenRPCEndpoint(name string, inputBuf, outputBuf int32) (net.Listener, error) {
	dir := filepath.Dir(name)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return nil, fmt.Errorf("rpc: mkdir runtime dir %s: %w", dir, err)
	}
	// 清理上次未优雅退出残留的 socket 文件
	_ = os.Remove(name)
	return net.Listen("unix", name)
}
