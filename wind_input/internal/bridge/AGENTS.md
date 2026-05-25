<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-08 | Updated: 2026-04-20 -->

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
| `server.go` | Named Pipe 服务端主体（基于 go-winio overlapped I/O；bridge pipe 走请求-响应 RPC，push pipe 走单向广播；net.Conn 接口统一读写） |
| `server_handler.go` | 消息分发：解码二进制消息并路由到 MessageHandler 各方法 |
| `server_push.go` | 推送管道管理（per-client outbound channel + 单 writer goroutine + phase-2 死链监听；所有 push 仅触达 active client，`pushToActiveClient` 是统一入口） |
| `host_render.go` | `HostRenderManager`：管理白名单进程的宿主渲染状态；`HostRenderState` 持有每个进程的共享内存引用；通过 `OpenProcess`/`QueryFullProcessImageNameW` 识别进程名称 |
| `shared_memory.go` | `SharedMemory`：命名共享内存 + 命名事件对；`WriteFrame` 将 RGBA→BGRA 转换后写入位图并信令通知；`WriteHide` 发送隐藏命令；安全描述符包含 AppContainer 低完整性标记（`S:(ML;;NW;;;LW)`）以支持 UWP 进程访问 |

## For AI Agents

### Working In This Directory
- 管道用 `github.com/Microsoft/go-winio` 起 listener (`winio.ListenPipe`)，配置 `MessageMode: true` 保证消息边界
- 缓冲区大小 64KB（与 Weasel 一致）
- 安全描述符允许 Everyone/SYSTEM/Administrators 访问（SDDL: `D:P(A;;GA;;;WD)(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AC)`），含 `S:(ML;;NW;;;LW)` 支持 UWP/AppContainer
- **关键：不要回到自写的同步 `windows.ReadFile` + `WriteFile`**——同一 handle 上 sync read park + sync write 会被内核串行化，writer 会被永久卡住。go-winio 内部用 overlapped I/O + IOCP 避免这个问题
- 客户端 PID 通过 `conn.(fdGetter).Fd()` 拿到底层 HANDLE 后调 `GetNamedPipeClientProcessId`
- Push pipe `pushClient` 用 `Disconnect()` + `Close()` 主动断开 client；只用 `Close()` 不会通知 client 端
- 推送管道按进程 ID（PID）跟踪客户端，`activeProcessID` 标识当前有焦点的进程，安全推送只发给活跃客户端
- 请求处理带 1000ms 超时（`RequestProcessTimeout`），覆盖高负载下的调度抖动
- 异步请求（`IsAsyncRequest`）不发送响应
- **Host Render 流程**：C++ DLL 看到 `StatusHostRenderAvail` 标志后发送 `CmdHostRenderRequest`；Go 侧 `HostRenderManager.SetupHostRender` 为该进程创建共享内存并返回 `CmdHostRenderSetup` 响应，随后每次候选词更新通过 `SHM.WriteFrame` 推送位图
- 共享内存命名规则：`Local\WindInput_SHM_<PID>`，事件命名：`Local\WindInput_EVT_<PID>`
- `HostRenderManager.UpdateWhitelist` 在配置重载时调用

### 红线：bridge handler 同步路径禁止「跨进程 Win32 / Shell 调用」

bridge handler goroutine 处理 `CmdIMEActivated` / `CmdFocusGained` / `CmdCaretUpdate` / `CmdHostRenderRequest` 等同步命令时，**调用方（C++ TSF DLL）正阻塞在宿主进程的 UI 线程上等响应**（`READ_TIMEOUT_MS = 1500ms`，见 `wind_tsf/include/IPCClient.h`）。在这条路径上 Go 端**严禁**做以下调用：

- `SHQueryUserNotificationState`、`SHGetKnownFolderPath` 等 shell32 跨进程 API
- 对 `GetForegroundWindow()` 返回的 hwnd 再做 `SendMessage` / `SendMessageTimeout`
- `BroadcastSystemMessage`、`AttachThreadInput`
- 任何 `OpenProcess` + 同步等待结果（除非命中本地缓存）
- 任何持锁等待 UI 线程的同步原语（包括 `Manager.cmdCh` 阻塞发送，必须用 `default` 分支降级）

**原因**：这些调用会反向 RPC 到 explorer / dwm / 其它 shell 服务，而那些服务此刻可能正被本 IME DLL 阻塞 → 形成环形等待，直到 C++ 端 1500ms 超时切断管道才解开，外在表现为「点任务栏 / 任务管理器 / 托盘小箭头都卡顿 ~1.5s」。已有事故：`coordinator/toolbar_visibility.go` 早期版本在 IME activate 同步路径里调用 `foreground.IsForegroundFullscreen()` → `SHQueryUserNotificationState`，全量复现该模式。

**正确做法**：
- 事件驱动缓存：用 ShellHook (`HSHELL_WINDOWENTERFULLSCREEN/EXIT`) 或 WinEventHook 在 UI 线程被动收事件，同步路径只读 cache。
- 把工作丢到独立 goroutine：`go func() { ... }()` 异步执行，立即返回 ACK。
- 已有正例：`HandleIMEActivated` 中 push pipe 写入用 `go bridgeServer.PushEnglishPairConfigToActiveClient(...)`，注释见 `coordinator/handle_lifecycle.go:786-792`。

**自动检测**：`processRequestWithTimeout` 内置 `slowRequestThreshold = 50ms` 慢请求 WARN。新增同步路径调用后看到 `Slow bridge request` 日志 = 命中此红线，立刻回查。

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
