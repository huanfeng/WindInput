<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# internal/ipc

## Purpose
底层 IPC 基础设施。定义二进制通信协议（命令码、消息头、编解码器）和基础 Named Pipe 服务端框架。`bridge` 包在此之上构建业务逻辑。

注意：`server.go` 中还保留了早期的 JSON 协议服务端（`\\.\pipe\tsf_ime_service`），当前主服务已迁移到 `bridge` 包的二进制协议，此文件为遗留代码。

## Key Files
| File | Description |
|------|-------------|
| `protocol.go` | JSON 协议类型（RequestType、Request、Response、Candidate）— 遗留 |
| `binary_protocol.go` | 二进制协议命令码常量（`CmdKeyEvent`、`CmdFocusLost` 等）和消息头结构 |
| `binary_codec.go` | `BinaryCodec`：消息的二进制编解码（`ReadHeader`、`ReadPayload`、`WriteMessage`）、`KeyHash` 函数 |
| `server.go` | JSON Named Pipe 服务端（`\\.\pipe\tsf_ime_service`）— 遗留，当前未使用 |

## For AI Agents

### Working In This Directory
- **当前实际使用**的是 `binary_codec.go` 和 `binary_protocol.go`，由 `bridge` 包调用
- `KeyHash(vkCode, modifiers uint32) uint32` 编码热键，与 C++ 侧算法必须一致
- `CmdBatchEvents` 是批量事件命令，`bridge` 对其有特殊处理路径
- `IsAsyncRequest(header)` 判断是否为不需要响应的异步请求
- 修改命令码时需同步修改 C++ TSF Bridge 侧的枚举定义

### Testing Requirements
- 编解码往返测试可作为单元测试添加
- 与 C++ 侧协议兼容性需集成测试

### Common Patterns
- 消息格式：`[Header][Payload]`，Header 包含命令码和 Payload 长度
- `bridge` 包直接使用 `ipc.NewBinaryCodec()` 实例，不需要直接与 `ipc.Server` 交互

## Dependencies
### Internal
- 无（被 `bridge` 和 `hotkey` 引用）

### External
- `golang.org/x/sys/windows` — Named Pipe API（server.go 遗留）

<!-- MANUAL: -->
