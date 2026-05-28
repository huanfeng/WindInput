<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-05-01 -->

# internal/ipc

## Purpose
底层 IPC 基础设施。定义二进制通信协议（命令码、消息头、编解码器）和基础 Named Pipe 服务端框架。`bridge` 包在此之上构建业务逻辑。

注意：`server.go` 中还保留了早期的 JSON 协议服务端（`\\.\pipe\tsf_ime_service`），当前主服务已迁移到 `bridge` 包的二进制协议，此文件为遗留代码。

### 跨语言协议同步（必读）
本目录的二进制协议与 C++ 侧 [`wind_tsf/include/BinaryProtocol.h`](../../../wind_tsf/include/BinaryProtocol.h) 互为镜像，由 [`wind_tsf/src/IPCClient.cpp`](../../../wind_tsf/src/IPCClient.cpp) 在 C++ 端实现编解码。**修改 `binary_protocol.go` / `binary_codec.go` 中的命令码、Header 字段、Payload 结构、状态标志位时，必须同步修改 `BinaryProtocol.h` 与 `IPCClient.cpp`，否则会破坏 IPC 兼容性。** C++ 侧的概览见 [`/wind_tsf/AGENTS.md`](../../../wind_tsf/AGENTS.md)。

## Key Files
| File | Description |
|------|-------------|
| `protocol.go` | JSON 协议类型（RequestType、Request、Response、Candidate）— 遗留 |
| `binary_protocol.go` | 二进制协议命令码常量（上行 `CmdKeyEvent`/`CmdFocusGained` 等，下行 `CmdCommitText`/`CmdStatusUpdate`/`CmdActivationStatusPush`/`CmdHostRenderSetup` 等）；消息头/载荷结构体（`IpcHeader`、`KeyPayload`、`CaretPayload` 等）；共享内存协议常量（`SharedRenderMagic`、`SharedRenderHeaderSize`、`MaxSharedRenderSize`、`SharedFlagVisible`/`SharedFlagContentReady`）；`SharedRenderHeader`、`HostRenderSetupPayload` 结构体；`StatusHostRenderAvail` 状态标志位 |
| `binary_codec.go` | `BinaryCodec`：消息的二进制编解码；`EncodeStatusUpdateEx`（含 `hostRenderAvail` 参数）、`EncodeActivationStatusPush`（IMEActivated/FocusGained 异步化后的状态回包，含 hotkeys + hostRenderAvail + iconLabel）、`EncodeHostRenderSetup`（编码共享内存名称和事件名称）、`EncodeBatchResponse`/`DecodeBatchEvents`（批量消息）、`EncodeStatePush`（hotkey-less 广播）；`CalcKeyHash`/`ParseKeyHash` 热键哈希函数 |
| `binary_protocol.go` | 二进制协议命令码常量（上行 `CmdKeyEvent`/`CmdFocusGained` 等，下行 `CmdCommitText`/`CmdStatusUpdate`/`CmdHostRenderSetup`/`CmdHostRenderFrame` 等）；消息头/载荷结构体（`IpcHeader`、`KeyPayload`、`CaretPayload`、`HostRenderFramePayload` 等）；共享内存协议常量（`SharedRenderMagic`、`SharedRenderHeaderSize`、`MaxSharedRenderSize`、`SharedFlagVisible`/`SharedFlagContentReady`）；`SharedRenderHeader`、`HostRenderSetupPayload` 结构体；`StatusHostRenderAvail` 状态标志位 |
| `binary_protocol.go` | 二进制协议命令码常量（上行 `CmdKeyEvent`/`CmdFocusGained`/`CmdCandidateSelect` 等，下行 `CmdCommitText`/`CmdStatusUpdate`/`CmdHostRenderSetup`/`CmdHostRenderFrame`/`CmdCandidateRects` 等）；消息头/载荷结构体（`IpcHeader`、`KeyPayload`、`CaretPayload`、`HostRenderFramePayload`、`CandidateHitRect` 等）；共享内存协议常量（`SharedRenderMagic`、`SharedRenderHeaderSize`、`MaxSharedRenderSize`、`SharedFlagVisible`/`SharedFlagContentReady`）；`SharedRenderHeader`、`HostRenderSetupPayload` 结构体；`StatusHostRenderAvail` 状态标志位 |
| `binary_codec.go` | `BinaryCodec`：消息的二进制编解码；`EncodeStatusUpdateEx`（含 `hostRenderAvail`）、`EncodeHostRenderSetup`、`EncodeHostRenderFrame`（darwin SHM 新帧就绪通知, 24B: seq+x+y+w+h+flags）、`EncodeBatchResponse`/`DecodeBatchEvents`、`EncodeStatePush`；`CalcKeyHash`/`ParseKeyHash` 热键哈希 |
| `server.go` | JSON Named Pipe 服务端（`\\.\pipe\tsf_ime_service`）— 遗留，当前未使用 |

## For AI Agents

### Working In This Directory
- **当前实际使用**的是 `binary_codec.go` 和 `binary_protocol.go`，由 `bridge` 包调用
- 热键哈希函数为 `CalcKeyHash(modifiers, keyCode uint32) uint32`；`ParseKeyHash(hash uint32)` 为逆向解码
- `EncodeStatusUpdateEx` 与 `EncodeStatusUpdate` 的区别：前者多一个 `hostRenderAvail bool` 参数，会设置 `StatusHostRenderAvail` 标志位
- `EncodeActivationStatusPush`（命令码 `CmdActivationStatusPush=0x020C`）与 `EncodeStatusUpdateEx` 的载荷格式相同（含 hotkeys + hostRenderAvail + iconLabel），区别仅在 command 字段；用于 IMEActivated/FocusGained 异步化后通过 push pipe 推送状态回包，C++ 端 AsyncReader 解析后 Post 到 TSF 线程做 `_SyncStateFromResponse` + `_EnsureHostRenderSetup`
- 与 `EncodeStatePush`（`CmdStatePush=0x0206`）的区别：StatePush 是状态变更广播（hotkey 不变所以不带），ActivationStatusPush 是 activation 握手回包（必须带完整 hotkeys + hostRenderAvail）
- `CmdHostRenderSetup`（下行 0x0501）和 `CmdHostRenderRequest`（上行 0x0501，C++ DLL 请求）共用同一命令码值，但方向不同
- `CmdHostRenderFrame`（下行 0x0502, push）darwin 专用：Win 用命名 Event 通知 host render 新帧, darwin 无等价 API 改走 push 通道发 `HostRenderFramePayload`(seq+几何), 客户端据 seq 从 SHM 取帧 blit
- `CmdCandidateRects`（下行 0x0503, push）darwin 专用：候选命中矩形 `[]CandidateHitRect` (panel-local), `EncodeCandidateRects`; 供 IMKit `.app` NSPanel 鼠标 hit-test
- `CmdCandidateSelect`（上行 0x020D）darwin 专用：NSPanel 鼠标点中候选, payload=pageLocalIndex u32; Go 选词结果走 push 通道 (`PushCommitTextToActiveClient`) 异步交付
- `CmdCandidateHover`（上行 0x020E）darwin 专用：NSPanel 鼠标悬停候选, payload=pageLocalIndex i32 (-1=无); forwarder 缓存候选按 hoverIndex 重渲染高亮
- `CmdCandidateRects` 中 index<0 为翻页按钮 (-1=上页 -2=下页), 客户端点中合成 PgUp/PgDn 键
- `CmdModeStatus`（下行 0x0504, push）darwin 专用：输入模式状态指示器, `EncodeModeStatus(flags, effectiveMode, label)`; payload=flags(u32)+effectiveMode(u32)+labelLen(u32)+label(UTF-8); flags 复用 `StatusChineseMode/StatusFullWidth/StatusChinesePunct/StatusCapsLock/StatusToolbarVisible` 位; forwarder 收 `CmdToolbarShow/Update/Hide` 转译为此帧, .app 据此更新菜单栏 NSStatusItem
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
