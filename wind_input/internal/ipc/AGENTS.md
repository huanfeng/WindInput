<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-20 -->

# internal/ipc

## Purpose
底层 IPC 基础设施。定义二进制通信协议（命令码、消息头、编解码器）和基础 Named Pipe 服务端框架。`bridge` 包在此之上构建业务逻辑。

注意：`server.go` 中还保留了早期的 JSON 协议服务端（`\\.\pipe\tsf_ime_service`），当前主服务已迁移到 `bridge` 包的二进制协议，此文件为遗留代码。

## Key Files
| File | Description |
|------|-------------|
| `protocol.go` | JSON 协议类型（RequestType、Request、Response、Candidate）— 遗留 |
| `binary_protocol.go` | 二进制协议命令码常量（上行 `CmdKeyEvent`/`CmdFocusGained` 等，下行 `CmdCommitText`/`CmdStatusUpdate`/`CmdHostRenderSetup` 等）；消息头/载荷结构体（`IpcHeader`、`KeyPayload`、`CaretPayload` 等）；共享内存协议常量（`SharedRenderMagic`、`SharedRenderHeaderSize`、`MaxSharedRenderSize`、`SharedFlagVisible`/`SharedFlagContentReady`）；`SharedRenderHeader`、`HostRenderSetupPayload` 结构体；`StatusHostRenderAvail` 状态标志位 |
| `binary_codec.go` | `BinaryCodec`：消息的二进制编解码；新增 `EncodeStatusUpdateEx`（含 `hostRenderAvail` 参数）、`EncodeHostRenderSetup`（编码共享内存名称和事件名称）、`EncodeBatchResponse`/`DecodeBatchEvents`（批量消息）、`EncodeStatePush`；`CalcKeyHash`/`ParseKeyHash` 热键哈希函数 |
| `server.go` | JSON Named Pipe 服务端（`\\.\pipe\tsf_ime_service`）— 遗留，当前未使用 |

## For AI Agents

### Working In This Directory
- **当前实际使用**的是 `binary_codec.go` 和 `binary_protocol.go`，由 `bridge` 包调用
- 热键哈希函数为 `CalcKeyHash(modifiers, keyCode uint32) uint32`；`ParseKeyHash(hash uint32)` 为逆向解码
- `EncodeStatusUpdateEx` 与 `EncodeStatusUpdate` 的区别：前者多一个 `hostRenderAvail bool` 参数，会设置 `StatusHostRenderAvail` 标志位
- `CmdHostRenderSetup`（下行 0x0501）和 `CmdHostRenderRequest`（上行 0x0501，C++ DLL 请求）共用同一命令码值，但方向不同
- `SharedRenderHeader` 固定 64 字节：前 40 字节有效字段，后 24 字节保留；后跟 BGRA 像素数据
- `CmdBatchEvents` 是批量事件命令，`bridge` 对其有特殊处理路径
- `IsAsyncRequest(header)` 判断是否为不需要响应的异步请求（版本字段高位为 `AsyncFlag=0x8000`）
- 修改命令码时需同步修改 C++ TSF Bridge 侧的枚举定义

### Testing Requirements
- 编解码往返测试可作为单元测试添加
- 与 C++ 侧协议兼容性需集成测试

### Common Patterns
- 消息格式：`[Header 8B][Payload]`，Header 包含协议版本、命令码和 Payload 长度
- `bridge` 包直接使用 `ipc.NewBinaryCodec()` 实例，不需要直接与 `ipc.Server` 交互

## Dependencies
### Internal
- 无（被 `bridge`、`hotkey`、`coordinator` 引用）

### External
- `golang.org/x/sys/windows` — Named Pipe API（server.go 遗留）

<!-- MANUAL: -->
