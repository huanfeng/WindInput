<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-08 | Updated: 2026-04-08 -->

# internal/bridge

## Purpose
Named Pipe IPC 服务端，负责与 C++ TSF（文本服务框架）桥接层进行双向通信。维护两条管道：

- `\\.\pipe\wind_input`（BridgePipeName）：双向请求/响应管道（MESSAGE 模式）
- `\\.\pipe\wind_input_push`（PushPipeName）：单向推送管道，用于主动向 TSF 推送状态变更

新增宿主进程代理渲染功能（Host Render），通过共享内存将候选词位图传递给白名单进程（如 Windows 开始菜单宿主进程 SearchHost.exe）的 DLL 注入代码渲染。

## Key Files
| File | Description |
|------|-------------|
| `protocol.go` | 协议类型定义（ResponseType、KeyEventData、StatusUpdateData 等） |
| `server.go` | Named Pipe 服务端主体（连接管理、消息读写、pipeReader/pipeWriter） |
| `server_handler.go` | 消息分发：解码二进制消息并路由到 MessageHandler 各方法 |
| `server_push.go` | 推送管道管理（`PushStateToAllClients`、`PushCommitTextToActiveClient`） |
| `host_render.go` | `HostRenderManager`：管理白名单进程的宿主渲染状态；`HostRenderState` 持有每个进程的共享内存引用；通过 `OpenProcess`/`QueryFullProcessImageNameW` 识别进程名称 |
| `shared_memory.go` | `SharedMemory`：命名共享内存 + 命名事件对；`WriteFrame` 将 RGBA→BGRA 转换后写入位图并信令通知；`WriteHide` 发送隐藏命令；安全描述符包含 AppContainer 低完整性标记（`S:(ML;;NW;;;LW)`）以支持 UWP 进程访问 |

## For AI Agents

### Working In This Directory
- 管道使用 MESSAGE 模式（`PIPE_TYPE_MESSAGE|PIPE_READMODE_MESSAGE`），每次 ReadFile 返回完整消息
- 缓冲区大小 64KB（与 Weasel 一致）
- 安全描述符允许 Everyone/SYSTEM/Administrators 访问（SDDL: `D:P(A;;GA;;;WD)(A;;GA;;;SY)(A;;GA;;;BA)`）
- 推送管道按进程 ID（PID）跟踪客户端，`activeProcessID` 标识当前有焦点的进程，安全推送只发给活跃客户端
- 请求处理带 200ms 超时（`RequestProcessTimeout`）
- 异步请求（`IsAsyncRequest`）不发送响应
- **Host Render 流程**：C++ DLL 看到 `StatusHostRenderAvail` 标志后发送 `CmdHostRenderRequest`；Go 侧 `HostRenderManager.SetupHostRender` 为该进程创建共享内存并返回 `CmdHostRenderSetup` 响应，随后每次候选词更新通过 `SHM.WriteFrame` 推送位图
- 共享内存命名规则：`Local\WindInput_SHM_<PID>`，事件命名：`Local\WindInput_EVT_<PID>`
- `HostRenderManager.UpdateWhitelist` 在配置重载时调用

### Testing Requirements
- 需要在 Windows 环境测试（依赖 Named Pipe）
- 协议变更需同步修改 C++ TSF Bridge 侧代码
- 共享内存位图格式变更需同步修改 DLL 侧读取代码

### Common Patterns
- `BridgePipeName` 常量被 `cmd/service/main.go` 用于检测已运行实例
- `MessageHandler` 接口由 `coordinator.Coordinator` 实现
- `BridgeServer` 接口由 `bridge.Server` 实现，供 coordinator 回调推送状态
- `SharedMemory.WriteFrame` 执行 RGBA→BGRA 内联转换（像素格式：B/G/R/A 顺序写入）

## Dependencies
### Internal
- `internal/ipc` — BinaryCodec（二进制消息编解码）、`HostRenderSetupPayload`、`MaxSharedRenderSize`、共享内存协议常量

### External
- `golang.org/x/sys/windows` — Named Pipe API、`CreateFileMappingW`、`MapViewOfFile`、`CreateEventW`

<!-- MANUAL: -->
