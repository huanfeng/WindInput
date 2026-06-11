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
- **Win**: Host Render（**单块全局命名共享内存** + **per-PID 命名 Event** 把候选词位图传给白名单宿主进程，绕过 Win11 开始菜单 Band 层级；SHM 共享省内存，event 按进程隔离防唤醒串扰）
- **darwin**: Host Render（POSIX SHM `shm_open`+`mmap` 单段 `/WindInput_SHM`, Go 服务用 `gg` 渲染 `*image.RGBA` 写入, 经 push 通道 `CmdHostRenderFrame` 通知唯一消费者 IMKit `.app` mmap 同段贴 NSPanel）。Go 端是渲染源, IMKit `.app` 只 blit。

二进制协议 (`internal/ipc.BinaryCodec`) 跨平台一致, IMKit `.app` 端写一份解码器同时服务 Win+macOS.

## Key Files

### 平台无关
| File | Description |
|------|-------------|
| `protocol.go` | 协议类型定义 (ResponseType、KeyEventData、StatusUpdateData 等) + `MessageHandler` 接口 |
| `deferred_handler.go` | `DeferredHandler`: coordinator 还未就绪时返回安全默认值的代理 |
| `keycode_name.go` | `keyCodeToKeyName(keyCode)`: VK 码 → 引擎 key 名字符串 (a-z/0-9/标点/功能键); Win+darwin server 共用 (原仅在 server_handler.go) |
| `protocol.go` 中 `candidateSelector` | 可选扩展接口 (HandleCandidateSelect), 不并入 MessageHandler; darwin 收 CmdCandidateSelect 时类型断言调用, Coordinator 实现 + DeferredHandler 转发 |
| `protocol.go` 中 `candidateContextMenuHandler` | 可选扩展接口 (HandleCandidateContextMenu(index int, action string)); darwin 收 CmdCandidateContextMenu 时类型断言调用 |
| `unified_menu.go` | 统一菜单数据 + 编码: `MenuItem` 树结构 (ID/Label/Separator/Checked/Disabled/Children); `unifiedMenuHandler` 接口 (UnifiedMenuItems() []MenuItem; HandleUnifiedMenuAction(id int)); `encodeUnifiedMenuPayload(items)` 递归编码供 CmdMenuShow 回包; Coordinator 实现 + DeferredHandler 转发 |

### Windows-only (`//go:build windows`)
| File | Description |
|------|-------------|
| `endpoint_windows.go` | `BridgePipeName` / `PushPipeName` Named Pipe 路径常量 |
| `server.go` | Named Pipe 服务端 (go-winio overlapped I/O; net.Conn 接口统一读写); Server struct 含 windows.Handle / push handle map / focus token 等 Win 特有字段; handleClient 对 `CmdIMEActivated`/`CmdFocusGained` 走「先 Ack 后处理」两段式 (processRequest 立即返回 Ack 释放 C++ 同步等待, 第二段同 goroutine 调 `runActivationHandlerAndPush` 经 push pipe 推状态) |
| `server_handler.go` | 消息分发: 解码二进制消息并路由到 MessageHandler 各方法 (Win 端 Server method); `runActivationHandlerAndPush`(收 payload, FocusGained 时解出 InputScope bitmask 传给 `HandleFocusGained`)/`applyFocusGainedCaret` 实现 activation 异步化第二段; `PushActivationStatusToActiveClient` 把完整状态以 `CmdActivationStatusPush` 推回 C++ |
| `server_push.go` | Push 管道管理 (per-client outbound channel + 单 writer goroutine + phase-2 死链监听; 所有 push 仅触达 active client); `PushActivationStatusToActiveClient` 用于 activation 异步化的状态回包 (含 hotkeys + hostRenderAvail)。**单写者不变式**: 所有出站消息 (含 CommitText/ClearComposition/UpdateComposition) 必须经 `enqueueBroadcast` 入 outbound 队列由 `pushWriterLoop` 单点写出, 禁止直写 conn (保序 + 不阻塞调用方); active client 定位统一走 `resolveActivePushClient` (token→PID→token 三段); `pushClient.Write` 带 `pushWriteTimeout` 30s 写超时覆盖半开连接 |
| `host_render.go` | `HostRenderManager`: 白名单进程的宿主渲染状态; 通过 `OpenProcess`/`QueryFullProcessImageNameW` 识别进程名称。**单块全局 SHM + per-PID event 模型**: 全局一份 `SharedMemory`（懒建常驻，`winSHMName`，物理页跨进程共享→内存恒一份），每个 PID 一个私有 `NamedEvent`（`Local\WindInput_EVT_<PID>`）存于 `clients` map。白名单支持 `filepath.Match` 通配符（`*` 短路匹配全部→全局模式）。`HostRenderState.WriteFrame/WriteHide` 写共享 SHM 后只 signal **本进程**的 event（焦点进程才被 Go signal，背景进程渲染线程休眠→无串扰）。`SetupSeq` 防竞态保留；`CleanupClient` 只关该 PID event（不动全局 SHM），`CleanupAll` 关所有 event + 全局 SHM |
| `shared_memory.go` | `SharedMemory`: 纯命名共享内存（**不含 event**，`WriteFrame`/`WriteHide` 不 signal，由调用方 signal）; `NamedEvent`: per-PID auto-reset 唤醒 event; `hostRenderSecurityAttributes` 共用 AppContainer 低完整性 SDDL (`S:(ML;;NW;;;LW)`) 给 SHM + event; `WriteFrame` RGBA→BGRA 转换写入 |

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
- **Host Render 流程（多窗口）**：C++ DLL 看到 `StatusHostRenderAvail` 标志后发送 `CmdHostRenderRequest`；Go 侧 `HostRenderManager.SetupHostRender` 对 `hostRenderKinds`（候选 + tooltip + 状态窗）**每类懒建一块全局 SHM + 为该 PID 每类建私有 event**，返回 `CmdHostRenderSetup` 多条目响应（`[]ipc.HostRenderSetupEntry`，每条 {kind, maxBufSize, shmName, evtName}），DLL 每条目建一个 band 窗口。焦点进程的各窗口更新经 `HostRenderState.WriteFrame(kind, ...)`（写该 kind 的 SHM + signal 该 kind 的 event）推送位图；`server.GetActiveHostRenderFor(kind)` 返回绑定该 kind 的 (writeFrame, hideFunc) 闭包，候选用 `GetActiveHostRender()`（= kind 候选）
- **为何每类独立通道**：候选/tooltip/状态窗**可能同时可见**，不能共用单 bitmap 通道（会互相覆盖）。共享内存命名：`Local\WindInput_SHM`（候选，向后兼容）/ `_TIP`(tooltip) / `_STS`(状态)（+变体后缀，每类全局唯一，物理页跨进程共享→每类内存恒一份）；事件命名：`Local\WindInput_EVT_<PID>` + 同后缀（**按 (PID,kind) 隔离**）。**关键教训**：SHM 可共享但 event 绝不能共享——多个渲染线程争抢同一 auto-reset event 时 `SetEvent` 只唤醒其中一个（不确定），焦点进程拿不到帧→不显示（曾因合并 event 出现"切换后再也不显示"回归）
- **失焦不销毁 HostWindow**：HostWindow 在失焦时常驻（靠 Go 的 `WriteHide`+本进程 event 隐藏）。**不可**在失焦时销毁——SearchHost/任务管理器用 XamlIsland locked/transient DocMgr，`OnSetFocus` 对其跳过 `focus_gained`，而 HostWindow 重建依赖 `focus_gained`，销毁后再也不会重建 → 候选永久不显示（已踩坑）。`_DestroyHostWindow` 只在 Deactivate / `_EnsureHostRenderSetup` 刷新时调用
- **Host Render 鼠标交互（Win）**：候选命中矩形**内嵌进 SHM**——`SharedMemory.WriteFrame(img, x, y, rects []ipc.CandidateHitRect, renderedHover int)` 把矩形表写在像素数据之后（头部 `RectCount`/`RectsOffset`），与位图共享同一 sequence（无跨通道错位）；翻页按钮编码为 `Index=-1`(上页)/`-2`(下页)。`renderedHover`（hover 编码 >=0 候选/-1 无/-2 上翻页/-3 下翻页）写入头部 `RenderedHoverIndex`，DLL 每帧据此同步去重基线 `_lastHoverIndex`——修复"打字清高亮后鼠标停同一候选不重新高亮"（基线对齐屏幕真实高亮，而非 DLL 上次发出的 hover）。DLL 的 `CHostWindow::_WndProc` 命中测试后经 `SendAsync` 上报：左键→`CmdCandidateSelect`(index i32，负值=翻页按钮 -1/-2)、悬停→`CmdCandidateHover`(index + 屏幕锚点 anchorX/belowY/aboveY i32)、滚轮→`CmdCandidateScroll`(delta i32，**不**在 DLL 翻页)。server 端 `handleHostCandidateSelect`/`handleHostCandidateHover`/`handleHostCandidateScroll`（均在同步快速路径，因 DLL 走 async 不等响应）类型断言 `candidateSelector`/`hostCandidateHoverHandler`/`candidateScrollHandler` 派发到 Coordinator，复用与本地候选窗相同的 `handleCandidateSelect`/`handleCandidateHoverChange`/`handlePageUp/Down`（滚轮默认 no-op，标准版无滚轮翻页）
- **host render 悬停高亮**：本地 `window` 在 host 模式始终隐藏，故 ① `Manager.RefreshCandidates` 不能再用 `window.IsVisible()` gate（改为 `hostRenderFunc != nil` 也放行）；② `doShowCandidates` 的 `hoverIndex` 改取 `Manager.hostHoverIndex`（由 `Coordinator.HandleCandidateHoverAt`→`SetHostHoverIndex` 同步，本地 window 的 hoverIndex 恒 -1 无法承载）；隐藏时 `doHide` 重置 hostHoverIndex
- **tooltip host render（Phase 1）**：高 Band 宿主下 tooltip 也走 host render（否则被候选 band 窗口遮挡）。`Coordinator.updateHostRenderState` 除候选外，再取 `GetActiveHostRenderFor(HostWindowTooltip)` 包成 `func(img,x,y)`（rects=nil/renderedHover=-1）经 `Manager.SetTooltipHostRenderFunc` 注入 `TooltipWindow`；后者 `Show/Hide/ForceHide` 内部分支：host 模式渲染 bitmap 写 tooltip SHM、本地窗口隐藏（`hostVisible` 镜像）。dispatch（`CmdTooltipShow`→ForceHide+Show / `CmdTooltipHide`→ForceHide）无需改动。状态窗同理：`GetActiveHostRenderFor(HostWindowStatus)`→`Manager.SetStatusHostRenderFunc`→`StatusWindow.SetHostRenderFunc`，host 逻辑集中在 `StatusWindow.Show/Hide` 内分流（`doShowStatus`/`doHideStatus`/`scheduleHide` 统一走 `Show`/`Hide`）。tooltip/状态均**纯显示**（无鼠标交互，host 模式下失去拖动/右键菜单）
- **host render 通用化对 composition 终止的影响**：`Coordinator.HandleCompositionTerminated` 不再对 host 模式一刀切忽略（那是 SearchHost 受限宿主"每次设 composition 即被终止"的特例）；改为按"距上次按键时间窗"判定（host 模式放宽到 500ms），紧跟按键的伪终止保留输入、用户点击移光标的真终止隐藏候选——否则普通应用（记事本）host render 时点击移光标候选框不消失
- `hostCandidateHoverHandler`（protocol.go）是 Win 专用可选接口 `HandleCandidateHoverAt(index, tooltipX, belowY, aboveY)`，区别于 darwin 的 index-only `candidateHoverHandler`（Win tooltip 由 Go 端窗口渲染需屏幕锚点）；DeferredHandler 转发
- `HostRenderManager.UpdateWhitelist` 在配置重载时调用（注：当前 compat 配置变更仍要求重启服务生效，见 `coordinator/reload_handler.go`）

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
- host render: forwarder (`cmd/service/forwarder_darwin.go`) 订阅 `ui.Manager` cmdCh, 收 CandidatesShow → gg 渲染 → `SharedMemory.WriteFrame` → `BroadcastFrame(EncodeHostRenderFrame)` + `BroadcastFrame(EncodeCandidateRects)`; SHM 在 `SetupHostRender(0)` 懒分配, `CleanupAll` 时 munmap+unlink
- 鼠标选词: `.app` NSPanel 点中候选 → 发 `CmdCandidateSelect`(pageLocalIndex) → server_darwin dispatch 类型断言 `candidateSelector` → `Coordinator.HandleCandidateSelect` → doSelectCandidate → `PushCommitTextToActiveClient` (commit 走 push 通道, `.app` 路由到 active InputController)
- 鼠标悬停: 发 `CmdCandidateHover` → server_darwin 双派发: (1) `SetCandidateHoverHandler` 注入的 forwarder 回调按 hoverIndex 重渲染高亮; (2) 类型断言 `candidateHoverHandler` → `Coordinator.HandleCandidateHover` 触发 tooltip 异步查询 (结果经 push `CmdTooltipShow` 下发, .app 据悬停候选矩形定位)
- 候选右键: 发 `CmdCandidateContextMenu`(index+action) → 类型断言 `candidateContextMenuHandler` → `Coordinator.HandleCandidateContextMenu`
- 统一菜单 (候选框空白处右键): .app 发上行 `CmdShowContextMenu` → server_darwin 调 `unifiedMenuHandler.UnifiedMenuItems()` 建树 → `encodeUnifiedMenuPayload` 经 conn.Write 回 `CmdMenuShow` (请求-响应, 非 Ack/非 push); .app 据树建 NSMenu, 点中发上行 `CmdMenuAction`(id) → `Coordinator.HandleUnifiedMenuAction`
- POSIX SHM 名 `/WindInput_SHM` + `buildvariant.Suffix()` (release 无后缀, debug `/WindInput_SHM_debug`), ≤30 字符 (macOS PSHMNAMLEN=31)。变体后缀**必须**隔离: 否则开机后两变体服务都自启时 `NewSharedMemory` 起手的 `shmUnlink` 会互相清掉对方的段 → 候选框渲染坏掉 (输入走已隔离 socket 仍正常)。Swift `CandidatePanelHost` 按 `BridgeEndpoints.variantSuffix` 对齐同名。进程异常退出残留段在 `NewSharedMemory` 起手 `shmUnlink` 清掉
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
