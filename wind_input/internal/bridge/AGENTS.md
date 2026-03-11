<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# internal/bridge

## Purpose
Named Pipe IPC 服务端，负责与 C++ TSF（文本服务框架）桥接层进行双向通信。维护两条管道：

- `\\.\pipe\wind_input`（BridgePipeName）：双向请求/响应管道（MESSAGE 模式）
- `\\.\pipe\wind_input_push`（PushPipeName）：单向推送管道，用于主动向 TSF 推送状态变更

通过 `MessageHandler` 接口将消息分发给 `coordinator`。

## Key Files
| File | Description |
|------|-------------|
| `protocol.go` | 协议类型定义（ResponseType、KeyEventData、StatusUpdateData 等） |
| `server.go` | Named Pipe 服务端主体（连接管理、消息读写、pipeReader/pipeWriter） |
| `server_handler.go` | 消息分发：解码二进制消息并路由到 MessageHandler 各方法 |
| `server_push.go` | 推送管道管理（`PushStateToAllClients`、`PushCommitTextToActiveClient`） |

## For AI Agents

### Working In This Directory
- 管道使用 MESSAGE 模式（`PIPE_TYPE_MESSAGE|PIPE_READMODE_MESSAGE`），每次 ReadFile 返回完整消息
- 缓冲区大小 64KB（与 Weasel 一致）
- 安全描述符允许 Everyone/SYSTEM/Administrators 访问（SDDL: `D:P(A;;GA;;;WD)(A;;GA;;;SY)(A;;GA;;;BA)`）
- 推送管道按进程 ID（PID）跟踪客户端，`activeProcessID` 标识当前有焦点的进程，安全推送只发给活跃客户端
- 请求处理带 200ms 超时（`RequestProcessTimeout`）
- 异步请求（`IsAsyncRequest`）不发送响应

### Testing Requirements
- 需要在 Windows 环境测试（依赖 Named Pipe）
- 协议变更需同步修改 C++ TSF Bridge 侧代码

### Common Patterns
- `BridgePipeName` 常量被 `cmd/service/main.go` 用于检测已运行实例
- `MessageHandler` 接口由 `coordinator.Coordinator` 实现
- `BridgeServer` 接口由 `bridge.Server` 实现，供 coordinator 回调推送状态

## Dependencies
### Internal
- `internal/ipc` — BinaryCodec（二进制消息编解码）

### External
- `golang.org/x/sys/windows` — Named Pipe API

<!-- MANUAL: -->
