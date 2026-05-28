<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-08 | Updated: 2026-05-26 -->

# internal/bridge

## Purpose
跨平台 IPC 服务端，与 IME 客户端进行双向通信。两个端点：

- **主请求/响应通道** (`BridgePipeName`)
  - Win: `\\.\pipe\wind_input<suffix>` Named Pipe (MESSAGE 模式)
  - darwin: `~/Library/Application Support/WindInput<suffix>/bridge.sock` UDS
- **推送通道** (`PushPipeName`)
  - Win: `\\.\pipe\wind_input<suffix>_push` Named Pipe
  - darwin: `~/Library/Application Support/WindInput<suffix>/bridge_push.sock` UDS

平台特定能力:
- **Win**: Host Render（命名共享内存 + 命名 Event 把候选词位图传给白名单宿主进程，绕过 Win11 开始菜单 Band 层级）
- **darwin**: Host Render（POSIX SHM `shm_open`+`mmap` 单段 `/WindInput_SHM`, Go 服务用 `gg` 渲染 `*image.RGBA` 写入, 经 push 通道 `CmdHostRenderFrame` 通知唯一消费者 IMKit `.app` mmap 同段贴 NSPanel）。Go 端是渲染源, IMKit `.app` 只 blit。

二进制协议 (`internal/ipc.BinaryCodec`) 跨平台一致, IMKit `.app` 端写一份解码器同时服务 Win+macOS.

## Key Files

### 平台无关
| File | Description |
|------|-------------|
| `protocol.go` | 协议类型定义（ResponseType、KeyEventData、StatusUpdateData 等） |
| `server.go` | Named Pipe 服务端主体（基于 go-winio overlapped I/O；bridge pipe 走请求-响应 RPC，push pipe 走单向广播；net.Conn 接口统一读写）；handleClient 对 `CmdIMEActivated` / `CmdFocusGained` 走「先 Ack 后处理」两段式：第一段 processRequest 立即返回 Ack 释放 C++ 端同步等待，第二段在同 goroutine 内调用 `runActivationHandlerAndPush` 执行 handler 并通过 push pipe 推送状态 |
| `server_handler.go` | 消息分发：解码二进制消息并路由到 MessageHandler 各方法；`runActivationHandlerAndPush`、`applyFocusGainedCaret` 实现 activation 异步化的第二段；`PushActivationStatusToActiveClient` 把完整状态以 `CmdActivationStatusPush` 推回 C++ |
| `server_push.go` | 推送管道管理（per-client outbound channel + 单 writer goroutine + phase-2 死链监听；所有 push 仅触达 active client，`pushToActiveClient` 是统一入口）；`PushActivationStatusToActiveClient` 用于 IMEActivated/FocusGained 异步化的状态回包（含 hotkeys + hostRenderAvail） |
| `host_render.go` | `HostRenderManager`：管理白名单进程的宿主渲染状态；`HostRenderState` 持有每个进程的共享内存引用；通过 `OpenProcess`/`QueryFullProcessImageNameW` 识别进程名称 |
| `shared_memory.go` | `SharedMemory`：命名共享内存 + 命名事件对；`WriteFrame` 将 RGBA→BGRA 转换后写入位图并信令通知；`WriteHide` 发送隐藏命令；安全描述符包含 AppContainer 低完整性标记（`S:(ML;;NW;;;LW)`）以支持 UWP 进程访问 |
| `protocol.go` | 协议类型定义 (ResponseType、KeyEventData、StatusUpdateData 等) + `MessageHandler` 接口 |
| `deferred_handler.go` | `DeferredHandler`: coordinator 还未就绪时返回安全默认值的代理 |
| `keycode_name.go` | `keyCodeToKeyName(keyCode)`: VK 码 → 引擎 key 名字符串 (a-z/0-9/标点/功能键); Win+darwin server 共用 (原仅在 server_handler.go) |

### Windows-only (`//go:build windows`)
| File | Description |
|------|-------------|
| `endpoint_windows.go` | `BridgePipeName` / `PushPipeName` Named Pipe 路径常量 |
| `server.go` | Named Pipe 服务端 (go-winio overlapped I/O; net.Conn 接口统一读写); Server struct 含 windows.Handle / push handle map / focus token 等 Win 特有字段 |
| `server_handler.go` | 消息分发: 解码二进制消息并路由到 MessageHandler 各方法 (Win 端 Server method) |
| `server_push.go` | Push 管道管理 (per-client outbound channel + 单 writer goroutine + 死链监听; 所有 push 仅触达 active client) |
| `host_render.go` | `HostRenderManager`: 白名单进程的宿主渲染状态; 通过 `OpenProcess`/`QueryFullProcessImageNameW` 识别进程名称 |
| `shared_memory.go` | `SharedMemory`: 命名共享内存 + 命名事件; `WriteFrame` RGBA→BGRA 转换写入; AppContainer 低完整性标记 (`S:(ML;;NW;;;LW)`) 支持 UWP |

### darwin-only (`//go:build darwin`)
| File | Description |
|------|-------------|
| `endpoint_darwin.go` | UDS 路径常量 (`bridge.sock` / `bridge_push.sock`); `WIND_INPUT_RUNTIME_DIR` 环境变量覆盖支持 |
| `server_darwin.go` | UDS server: 双 socket 监听, accept loop 每连接 goroutine, 帧 dispatch 覆盖 KeyEvent/Caret/Focus/IME/ToggleMode; `writeKeyResult` 完整编码 commit/composition 响应 (InsertText/UpdateComposition/InsertTextWithCursor/MoveCursor/DeletePair); `KeyEventData.Key` 用 `keyCodeToKeyName` 填充 (否则 engine "Unhandled key"); `BroadcastFrame` exported 供 forwarder 推帧; client ID 用 `connID` 替代 Win PID |
| `host_render_darwin.go` | darwin POSIX SHM 实装: `shmOpen`/`shmUnlink` (raw `SYS_SHM_OPEN`=266 syscall, x/sys/unix 未导出) + `mmap`; `SharedMemory.WriteFrame(img,x,y) (seq,err)` RGBA→BGRA 写单段 `/WindInput_SHM`; `WriteHide`; `HostRenderManager` 单消费者模型 (无 PID 分桶, processNames 白名单忽略, `IsProcessWhitelisted` 恒 true) |
| `server_darwin_test.go` | 7 个端到端测试 (KeyEvent roundtrip / FocusGained / 多 client / 断线 / Push fanout / stale socket 清理 / endpoint 路径派生) |

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

bridge handler goroutine 处理仍走同步响应的命令（`CmdHostRenderRequest`、`CmdToggleMode`、`CmdSystemModeSwitch`、`CmdMenuCommand`、`CmdKeyEvent`、`CmdCommitRequest` 等）时，**调用方（C++ TSF DLL）正阻塞在宿主进程的 UI 线程上等响应**（`READ_TIMEOUT_MS = 1500ms`，见 `wind_tsf/include/IPCClient.h`）。在这条路径上 Go 端**严禁**做以下调用：

注：`CmdIMEActivated` / `CmdFocusGained` 已异步化（先 Ack 后处理，见 `server.go` 中的 `isActivation` 分支与 `runActivationHandlerAndPush`），handler 内部允许跨进程调用——但 **handler 仍在 handleClient goroutine 内执行**，会延迟本 client 的后续命令读取。重 IO/慢调用仍应单独 spawn goroutine 处理。

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
- 已有正例：activation 命令的两段式异步化（`server.go::handleClient` 的 `isActivation` 分支 + `runActivationHandlerAndPush` + `CmdActivationStatusPush`），handler 在 Ack 之后才执行，且状态通过 push pipe 回送。

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

### darwin 端注意事项
- UDS 不支持 SDDL; 改用 `MkdirAll(dir, 0o700)` 把端点目录权限限定到当前用户
- 启动时清理 stale socket 文件 (上次进程未优雅退出残留)
- `IsActivelyFocusedPID` 始终返回 false: PID 概念在 darwin 不适用, macOS 端通过 IMKit 自报 bundleID 替代 (待 PR-A 接入)
- KeyEvent 同步响应已完整: `writeKeyResult` 把 commit/composition 编回响应帧 (IMKit `InputController.handle` 同步读取后 insertText/setMarkedText); **不要回退到 default→Ack**, 否则选词文本被吞 → "输了字不上屏"
- host render: forwarder (`cmd/service/forwarder_darwin.go`) 订阅 `ui.Manager` cmdCh, 收 CandidatesShow → gg 渲染 → `SharedMemory.WriteFrame` → `BroadcastFrame(EncodeHostRenderFrame)`; SHM 在 `SetupHostRender(0)` 懒分配, `CleanupAll` 时 munmap+unlink
- POSIX SHM 名 `/WindInput_SHM` ≤30 字符 (macOS PSHMNAMLEN=31); 进程异常退出残留段在 `NewSharedMemory` 起手 `shmUnlink` 清掉
- 多客户端用 `connID` (accept 自增) 替代 Win 的 PID 索引; macOS 单 IMKit `.app` 进程多 IMKInputController 实例各自独立 socket 连接, 见 [`docs/design/macos-port.md`](../../../docs/design/macos-port.md)

## Dependencies
### Internal
- `internal/ipc` — BinaryCodec (二进制消息编解码)、`HostRenderSetupPayload`、`MaxSharedRenderSize`、共享内存协议常量
- `pkg/buildvariant` — 端点路径 suffix

### External
- Win: `golang.org/x/sys/windows` (Named Pipe API)、`github.com/Microsoft/go-winio` (overlapped I/O)
- darwin: `net.Listen("unix", ...)` + `os`/`path/filepath`/`syscall` + `golang.org/x/sys/unix` (mmap/ftruncate; shm_open 走 raw SYS_SHM_OPEN syscall)

## 全局约束
- 协议跨语言镜像约束: 修改帧布局必须同步 `wind_tsf/include/BinaryProtocol.h` (C++) + 未来 macOS IMKit `.app` 的 Swift/Obj-C 解码器
- 日志隐私: bridge 收到的用户文本 (commit text / preedit) 不进 INFO

<!-- MANUAL: -->
