<!-- Updated: 2026-05-26 -->

# macOS 移植设计

> 本文档面向"未来要让 WindInput 在 macOS 上跑起来"的开发者. 描述整体架构、协议、目录约定与待办清单. 当前进度: Go 服务端已完成 darwin 全包编译, 可产出 Mach-O 二进制; macOS IMKit `.app` 工程尚未启动.

## 双进程模型

```
┌──────────────────────────── macOS ────────────────────────────┐
│                                                                │
│  wind_input (Go 服务) — 纯逻辑          WindInput.app (IMKit)  │
│  ┌───────────────────┐                ┌──────────────────┐    │
│  │ 逻辑层 (跨平台)   │                │ IMKInputController│    │
│  │  - coordinator    │                │  - 键事件捕获     │    │
│  │  - engine/dict    │                │  - composition 管理│   │
│  │  - schema         │                ├──────────────────┤    │
│  └───────────────────┘                │ 渲染层 (macOS)   │    │
│         ▲                             │  - NSPanel       │    │
│         │  纯数据 (候选词文本/坐标)    │  - CoreText      │    │
│         │  无 bitmap, 无共享内存       │  - NSVisualEffect│    │
│         │                             └────────▲─────────┘    │
│         │       Unix Domain Socket             │              │
│         └─────────────────────────────────────┘              │
│                                                                │
└────────────────────────────────────────────────────────────────┘
```

**核心约定**: Go 服务**只产生数据**(候选词、菜单结构、状态), 不画任何像素;
IMKit `.app` **自绘 NSPanel**, 自己处理 retina/dark mode/光标定位.

与 Win 端 `wind_tsf` (in-proc C++ DLL + bitmap 推送) 的本质不同:
- macOS IMKit 是独立 GUI 进程, 天然能画浮窗 (NSPanel level = `kCGPopUpMenuWindowLevel`)
- 无需 host render / 共享内存 / Band 层级穿透机制
- Go 服务退化为"算法 + 协议"两件事, 极大降低 darwin 端复杂度

## 进程身份与多客户端

| 维度 | Windows TSF | macOS IMKit |
|------|-------------|-------------|
| Go 服务进程 | 1 个 | 1 个 |
| 客户端进程 | N 个 (Chrome/WPS/...) 各加载 wind_tsf.dll | **1 个 IMKit `.app`** |
| "客户端" 概念 | 宿主进程 (PID) | IMKInputController 实例 (每个聚焦的文本框) |
| 协议层 client ID | 进程 PID | `connID` (UDS accept 序号) |
| 应用识别 | `GetNamedPipeClientProcessId` + 进程名 | IMKit 自报 `bundleIdentifier` (待 PR-A 实装) |

**接入方案**: 每个 `IMKInputController` 实例打开**独立**的 UDS 连接, 让 Go 服务"多客户端"模型几乎零修改 (见 `internal/bridge/server_darwin.go` 中 `connID` 替代 `processID`).

## Socket 端点

所有 UDS 路径默认在 `~/Library/Application Support/WindInput<suffix>/` 下:

| 端点 | 路径 | 用途 |
|------|------|------|
| `bridge.sock` | bridge 主 | 请求-响应 (IMKInputController → Go) |
| `bridge_push.sock` | bridge push | 服务端 → 客户端推送 (status/commit/composition) |
| `rpc.sock` | RPC 主 | Wails 设置端 ↔ Go (词库/Shadow/系统管理) |
| `rpc_events.sock` | RPC 事件 | Go → Wails 推送 (config 变更等) |
| `wind_input.pid` | 单例文件 | `flock` advisory lock + PID 内容 |

`<suffix>` 由 `pkg/buildvariant.Suffix()` 给出 (debug/release 区分).
可由环境变量 `WIND_INPUT_RUNTIME_DIR` 覆盖, 方便测试与 portable 部署.

## 协议层 (跨平台共享)

bridge IPC 使用 `internal/ipc` 的二进制协议, **与 Win 完全一致**:
- LittleEndian, 8 字节 header (`uint16 version + uint16 cmd + uint32 length`)
- 各 cmd payload 见 `internal/bridge/protocol.go` 与 `internal/ipc/binary_protocol.go`
- IMKit `.app` 端写一个**Swift/Obj-C 二进制解码器**即可同时服务 Win 与 macOS 协议(代码可复用)

UI 命令/事件模型在 `internal/uicmd`:
- Command (Go → 渲染端): 25+ 类型, 涵盖候选框/工具栏/Toast/菜单/快捷键/主题
- Event (渲染端 → Go): 9 种, 涵盖鼠标点选/翻页/拖动/菜单确认/快捷键触发
- 见 `internal/uicmd/AGENTS.md` 完整字段说明

## 当前已完成

- ✅ Go 服务端全包 `GOOS=darwin go build ./...` 通过, 产出 11.5MB arm64 / 12.2MB amd64 Mach-O PIE 二进制
- ✅ `internal/bridge` UDS server: 双 socket (主+push) 监听, 每连接 goroutine, 帧 dispatch 覆盖 KeyEvent/Caret/Focus/IME/ToggleMode/Push*
- ✅ `internal/uicmd` 平台无关命令/事件模型 + 二进制 codec
- ✅ `internal/ui` Manager darwin stub: 60+ method 通过 cmdCh 投递命令, 事件 Events() 通道暴露; Win 端 callback 包装同时推 Event 实现双流并行
- ✅ `pkg/rpcapi` 端点 + dial 平台分支 (winio.DialPipe ↔ net.Dial unix)
- ✅ `internal/rpc` 服务端 listen 平台分支 (winio.ListenPipe ↔ net.Listen unix)
- ✅ 单例: Win mutex (`Global\\WindInput*IMEService`) ↔ darwin `flock` PID 文件
- ✅ stderr 重定向: Win SetStdHandle ↔ darwin `syscall.Dup2(fd, 2)`
- ✅ 词库 mmap: Win `CreateFileMappingW` ↔ darwin `syscall.Mmap(PROT_READ, MAP_SHARED)`

## 当前 stub 未实装 (等 IMKit `.app` 接入)

- ⏳ `internal/clipboard` darwin: 返回 `ErrNotImplemented`
  - 建议路径: cmdbar "复制候选" 由 IMKit `.app` 直接调 `NSPasteboard.generalPasteboard`, Go 服务不接触剪贴板
- ⏳ `internal/keyinject` darwin: Tap/Sequence/Hold/Release/TypeText 都返回 `ErrUnsupportedPlatform`
  - 建议路径: 命令直通车文本注入由 IMKit `.app` 走 `CGEventCreateKeyboardEvent` 或 NSPasteboard+Cmd+V
- ⏳ `pkg/theme` `DarkModeWatcher` darwin: no-op
  - 建议路径: IMKit `.app` 监听 `NSApplication.effectiveAppearance` KVO, 通过 bridge 协议推变更到 Go 服务
- ⏳ `internal/ui` Manager darwin: 仅 stub
  - 真正的 UI 由 IMKit `.app` 自绘 NSPanel
- ⏳ `internal/bridge.IsActivelyFocusedPID` darwin: 始终 false
  - PID 概念不适用; IMKit `.app` 端通过 `bundleIdentifier` 自报后, 在 bridge 协议加 capability 字段携带, Go 服务端用 bundleID 替代

## PR-A: macOS IMKit `.app` 工程 (未启动)

未来要做的工作:
1. 在仓库根新建 `wind_macos/` (与 `wind_tsf/` 对位的 macOS 端工程)
2. Xcode 工程或 SwiftPM 包, 主类 `WindInputInputController: IMKInputController`
3. `Info.plist` 声明:
   ```
   InputMethodConnectionName = WindInput_1_Connection
   tsInputMethodCharacterRepertoireKey = (zh_Hans, en)
   ComponentInputModeDict
   ```
4. 启动时 `Dial` `~/Library/Application Support/WindInput*/bridge.sock`
5. 实现:
   - 键事件 → 编 ipc binary frame → 写 socket
   - 收 `CmdCommitText` / `CmdUpdateComposition` / `CmdClearComposition` → 调 `client().insertText` / `client().setMarkedText`
   - 收 `CmdCandidatesShow` (uicmd.Command) → 自绘 NSPanel 显示候选框
   - 收 `CmdToastShow` / `CmdStatusShow` / etc → 对应 NSPanel / NSStatusItem
6. 安装到 `/Library/Input Methods/WindInput.app`
7. `productbuild` 生成 `.pkg` + 代码签名 + Notarization

参考实现: 鼠须管 (Squirrel) 是最相近的开源 macOS IME, 同样走"独立 .app + 自绘 NSPanel"路径.
仓库: <https://github.com/rime/squirrel>

## 与 Win 端 wind_tsf 的对应关系

| Windows | macOS | 备注 |
|---------|-------|------|
| `wind_tsf.dll` (in-proc) | `WindInput.app` (out-of-proc) | 进程模型本质不同 |
| Named Pipe `\\.\pipe\wind_input*` | UDS `~/Library/.../bridge.sock` | 协议字节布局完全一致 |
| `IPCClient.cpp` 二进制编解码 | (待 Swift/Obj-C 实现) | 复用 `internal/ipc` 协议 |
| `HostWindow` + LayeredWindow + 共享内存 | NSPanel 自绘 | macOS 无需 bitmap 跨进程 |
| `RegisterHotKey` | `CGEventTap` / `NSEvent.addGlobalMonitorForEvents` | 需 Accessibility 权限 |
| `OpenClipboard` | `NSPasteboard.generalPasteboard` | 由 IMKit `.app` 直调 |

## 后续路线图

| 阶段 | 内容 | 状态 |
|------|------|------|
| PR-1 ~ PR-6 | Go 服务端平台抽象 + darwin stub | ✅ 完成 |
| PR-D | 文档 (本文档 + macos-build.md + AGENTS.md 更新) | ⏳ 进行中 |
| PR-A | macOS IMKit `.app` 工程 | ⏳ 未启动 |
| PR-B | 真主题/剪贴板等 stub 替换为 IMKit 端实装 | ⏳ 跟随 PR-A |
| PR-C | 安装包 (.pkg / 代码签名 / Notarization) | ⏳ 收尾 |

## 参考资料

- macOS Input Method Kit 官方文档: <https://developer.apple.com/documentation/inputmethodkit>
- 鼠须管 Squirrel 源码 (对位参考): <https://github.com/rime/squirrel>
- 本仓库 Win 端 IPC 实现 (协议字节布局): `wind_tsf/src/IPCClient.cpp`
- 本仓库 Go 端 IPC 协议: `wind_input/internal/ipc/binary_protocol.go`
