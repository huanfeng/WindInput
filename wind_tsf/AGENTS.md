<!-- Generated: 2026-03-13 | Updated: 2026-06-20 -->

# wind_tsf - Windows TSF Input Method Bridge

> 已并入 **WindInput** 仓库（`wind_tsf/`）。对端服务现为 **Rust 版** `wind_input`（原 Go 服务继任者）；
> IPC 协议兼容（`include/BinaryProtocol.h` ↔ Rust `crates/wind-ipc`）。默认经 **MinGW 在 Linux 交叉编译**
> （`Makefile` / `dev.sh tsf`），亦保留 MSVC/CMake 原生构建。迁移说明见 `../docs/redesign/tsf-migration.md`。
> 下文历史性提到的 "Go service" 即指现在的 Rust 服务（协议未变）。

## Purpose

C++17 DLL implementing the Windows Text Services Framework (TSF) interface for the 清风输入法 (WindInput) Chinese input method. This component:

- Registers as a system-level input method with Windows TSF
- Captures keyboard events and forwards them to the wind_input 服务（Rust）via Named Pipe IPC
- Manages composition, caret position tracking, and candidate selection
- Provides language bar UI integration and hotkey management
- Implements display attributes (underline) for composition text
- Maintains state synchronization with the 服务 via binary protocol
- Provides HostWindow 机制，在宿主进程（如开始菜单）内通过 CreateWindowInBand 创建带外层级窗口，解决 Win11 开始菜单候选框 z-order 问题

The DLL exports standard TSF COM interfaces (DllCanUnloadNow, DllGetClassObject, DllRegisterServer, DllUnregisterServer).
两条构建路径编同一份源码：**MinGW（`Makefile`，本仓库默认）** 与 **MSVC（`CMakeLists.txt`）**。

> **注意：wind_dwrite.dll 已移除。** DirectWrite 渲染改由对端服务直接调用系统 dwrite.dll，C++ 侧不再构建 wind_dwrite 目标。
> **MinGW 兼容垫片**：mingw-w64 自带 TSF 头不完整，缺失的接口/GUID/常量由 `include/mingw_tsf_compat.h` +
> `src/mingw_tsf_compat.cpp` 补齐（整体 `#ifdef __MINGW32__`，MSVC 构建为空）。详见迁移文档。

## Key Files

| File | Description |
|------|-------------|
| `Makefile` | MinGW 交叉编译（Linux→Windows，本仓库默认路径）；`DEV_VARIANT=1` 出dev 变体 |
| `CMakeLists.txt` | MSVC/Windows 原生构建（C++17, UTF-8；唯一目标 wind_tsf.dll；从 res/version.rc.in 生成版本资源） |
| `include/mingw_tsf_compat.h` / `src/mingw_tsf_compat.cpp` | MinGW TSF 兼容垫片（仅 `__MINGW32__`；补缺失接口/GUID/常量） |
| `wind_tsf.def` | Module definition file (exports COM entry points) |
| `README.md` | Project documentation |

## Subdirectories

| Directory | Purpose |
|-----------|---------|
| `include/` | Header files (see `include/AGENTS.md`) |
| `src/` | Implementation files (see `src/AGENTS.md`) |
| `res/` | Resource files: icon + version.rc.in 模板 (see `res/AGENTS.md`) |

## Build Instructions

### MinGW 交叉编译（默认，Linux→Windows）

```bash
cd wind_tsf
make                       # → build/wind_tsf.dll（PE32+ x64，静态链接）
make DEV_VARIANT=1       # → build_dev/wind_tsf_dev.dll
make VERSION=1.0.0         # 指定版本号写入资源
# 或经仓库脚本：../scripts/dev.sh tsf [debug]
```

### MSVC/Windows 原生（可选）

```bash
cd wind_tsf && mkdir -p build && cd build
cmake .. -G "Visual Studio 17 2022" -A x64
cmake --build . --config Release      # → build/Release/wind_tsf.dll
# 版本号：cmake .. -DAPP_VERSION_STR="1.0.0" -DAPP_VERSION_MAJOR=1 ...
```

## IPC Communication

**Named Pipes:**
- `\\.\pipe\wind_input` - Main command pipe (C++ -> Go, bidirectional)
- `\\.\pipe\wind_input_push` - Async push pipe (Go -> C++, proactive state updates)

**Binary Protocol:**
- Header: 8 bytes (version, command, payload length)
- Payload: variable length, UTF-8 encoded text
- Key types: KeyEvent, CommitRequest, CaretUpdate, FocusGained/Lost, IMEActivated/Deactivated, ToggleMode, MenuCommand, etc.

**跨语言协议同步（必读）：**
本目录的 [`include/BinaryProtocol.h`](include/BinaryProtocol.h) 与 [`src/IPCClient.cpp`](src/IPCClient.cpp) 是 Go 端 [`wind_input/internal/ipc/binary_protocol.go`](../wind_input/internal/ipc/binary_protocol.go) 与 [`binary_codec.go`](../wind_input/internal/ipc/binary_codec.go) 的 C++ 镜像。**修改命令码、Header 字段、Payload 结构、状态标志位时，必须双边同步**，否则会破坏 IPC 兼容性。Go 侧实现概览见 [`/wind_input/internal/ipc/AGENTS.md`](../wind_input/internal/ipc/AGENTS.md)。

**Circuit Breaker:**
- Handles service unavailability gracefully
- Max 3 consecutive failures before opening circuit
- 3-second reset interval before retry

## Component Architecture

### Core TSF Integration (TextService)
- `CTextService` - Main TSF text input processor (ITfTextInputProcessor, ITfThreadMgrEventSink, ITfCompositionSink, ITfDisplayAttributeProvider)
- `CClassFactory` - COM class factory for instantiation
- `CDisplayAttributeInfo` - Composition text styling (underline effect)
- `CCaretEditSession` - TSF edit session for caret position retrieval（已修复 edit session 调用时序问题）
- Full state sync mechanism (`_DoFullStateSync()`) after reconnection

### Input Processing (KeyEventSink)
- `CKeyEventSink` - Keyboard event capture (ITfKeyEventSink)
- Modifier key state machine (tracks Shift/Ctrl/Alt/Win state, replaces GetAsyncKeyState)
- Barrier mechanism for commit requests (Space/Enter/number key coordination with Go service)
- Barrier timeout handling (500ms default)
- Toggle key tap detection (500ms threshold)
- Composition state tracking and reset on focus loss
- Read-only context detection (browser support)

### IPC Communication (IPCClient)
- `CIPCClient` - Named pipe client with circuit breaker, async reader thread
- Binary protocol serialization/deserialization (v1.1)
- Async reader for receiving state pushes from Go service
- Batch event support for performance optimization
- Timeout handling and error recovery (100ms connect, 50-100ms read/write)
- Circuit breaker state management (3 failure threshold, 3-second reset interval)
- Separate read pipe for async push notifications

### UI Integration (LangBarItemButton)
- `CLangBarItemButton` - Language bar button (ITfLangBarItemButton, ITfSource)
- Mode/width/punctuation/toolbar toggle menu
- Context menu for settings/dictionary/about/exit
- Thread-safe updates via message window (for async callbacks)
- Caps Lock state indicator
- Screen-aware context menu positioning

### Hotkey Management (HotkeyManager)
- `CHotkeyManager` - Hotkey whitelist from Go service
- O(1) lookup using unordered_set
- Classification: toggle mode, letter, number, punctuation, backspace, enter, escape, space, tab, page key, cursor key, select key
- Key normalization (left/right modifier handling)

### HostWindow（开始菜单宿主窗口代理）
- `CHostWindow` - 在宿主进程（如 SearchHost.exe）内通过 `CreateWindowInBand`（user32.dll 非公开 API）创建与宿主同级 Band 的分层窗口
- Go 服务通过共享内存传递渲染帧（像素数据），HostWindow 的渲染线程读取 SharedRenderHeader 并 BitBlt 到分层窗口
- `_ResolveAPIs()` - 动态解析 CreateWindowInBand 和 GetWindowBand 函数指针
- `_GetHostBand()` - 获取宿主进程前台窗口的 Band 等级
- `_CreateBandWindow()` - 在宿主进程的 Band 等级创建无边框分层窗口
- `_RenderThread()` / `_RenderLoop()` - 渲染线程，等待事件信号后读取共享内存渲染一帧
- 支持跳过过期帧（lastSequence 机制）

### File Logging (FileLogger)
- `CFileLogger` - 运行时可配置的文件日志单例（`FileLogger.h` / `FileLogger.cpp`）
- 四种输出模式：`none`（默认，零开销）/ `file` / `debugstring` / `all`
- 日志文件：`%LOCALAPPDATA%\WindInput\logs\tsf_log\wind_tsf.<宿主名>.<pid>.log`——
  **每进程一个**，且单独放在 `tsf_log\` 子目录里（文件数是「用过的宿主 × pid」量级，
  与 core 的 `wind_input.log` 平铺会把主日志淹掉）
- 配置文件：`%LOCALAPPDATA%\WindInput\logs\tsf_log_config`（mode / level / dump_hotkey 三个键）——
  **留在 `logs\` 这一层不跟着进子目录**：它是用户手工创建的日志总开关，搬走会让存量失效
- `dump_hotkey=1` 才启用**环形缓冲 + Ctrl+Shift+F12 导出**，**出厂关**。这个热键的拦截排在
  `OnTestKeyDown` / `OnKeyDown` 的所有闸门之前（早于 `IsKeyboardDisabled`、密码框抑制、
  只读上下文），开着就意味着「只要本输入法激活，Ctrl+Shift+F12 永远到不了宿主」——
  而它在 VS / JetBrains 里是有主的快捷键，我们的处理还会往焦点处插一行提示文本。
  判据收在 `CKeyEventSink::_IsLogDumpHotkey`（吃与导出两处共用，不许各写一份）
- ⚠ 改完 `tsf_log_config` 要**重启宿主进程**才生效：唯一的重读入口 `ReloadConfig` 就挂在
  上面那个热键上，而它默认关着。建文件时把 mode / level / dump_hotkey 一次写齐
- dev 变体只换目录（`WindInputDev\logs\`），**文件名与配置名同正式版**——目录已隔离，
  文件名不再重复带 `_dev`
- 多进程安全：**每进程独占自己那个文件，无需任何跨进程同步**。此前是一把
  `Local\WindInput*TSFLogMutex` 串行化所有宿主，而抢锁发生在 TSF 输入线程上
- 常开追加句柄（`FILE_APPEND_DATA`），一行日志只剩一次 `WriteFile`
- 自动轮转：超过 5MB 时重命名为 `wind_tsf.<宿主名>.<pid>.old.log`
- **外部改动日志文件是受支持的**（`_ResyncFile`，每秒自检一次）：
  - 删掉文件或整个 `tsf_log\` 目录 → 一秒内自动重建，**不必重启宿主**
  - 清空文件（截断到 0）→ 追加句柄天然从头续写，不留 NUL 空洞，排查时可随时截断
  - 不做自检的话，删除后写入会**静默进入一个已摘名的幽灵文件**：`WriteFile` 照常返回
    成功、目录里却什么都没有，很容易误判成「这功能压根没跑」
- 过期文件的回收由 **core 服务启动时**做（`log_rotate::prune_stale_tsf_logs`，默认 7 天）：
  DLL 的 `Init()` 跑在 loader lock 下，不能做目录遍历
- 在 `dllmain.cpp` 的 DLL_PROCESS_ATTACH / DLL_PROCESS_DETACH 中 Init/Shutdown

## CLSID / GUID 正式化

CLSID 已替换为正式 UUID 并集中管理（Globals.h / Globals.cpp）：
- `c_clsidTextService` = `{99C2EE30-5C57-45A2-9C63-FB54B34FD90A}`
- `c_guidProfile`      = `{99C2EE31-5C57-45A2-9C63-FB54B34FD90A}`

安装脚本中的 InstallLayoutOrTip 调用使用同一套 GUID。

## Dependencies

### Internal
- `BinaryProtocol.h` - Shared binary protocol definitions with Go service
- `Globals.h` - Logging macros, COM utilities, global state

### External
- **Windows SDK:** msctf.h, ctfutb.h (TSF interfaces)
- **Windows System Libraries:** kernel32, ole32, user32, winuser.h (COM, window management)
- **C++ Standard Library:** string, vector, unordered_set

## For AI Agents

### Working In This Directory

When implementing features or fixes in wind_tsf:

1. **Read the binary protocol** (BinaryProtocol.h) before modifying IPC communication
2. **Understand TSF lifecycle:** Activate (thread manager registration) -> Initialize components -> Deactivate
3. **Use logging macros** from Globals.h (WIND_LOG_ERROR_FMT, WIND_LOG_DEBUG, etc.) instead of printf
4. **COM reference counting:** Use SafeRelease() template for interface cleanup
5. **Named pipes:** Connection is lazy (on-demand), with circuit breaker fallback
6. **Edit sessions:** For TSF API calls (composition, caret position), must be called within RequestEditSession
7. **HostWindow:** 只在 compat.toml 中 `host_render = true` 的进程中激活（由 Rust 服务通过 IPC 指令触发 Initialize）
8. **UI-less（宿主自绘候选）:** `ITfCandidateListUIElement` 在宿主 `pbShow=FALSE` 时答真实快照（`CMD_UIELEMENT_QUERY` 拉取），否则答占位；`_uiElementShown` 记的是宿主意愿，EndUIElement 不得清它。见 `docs/design/game-compat-tsf-uielement.md`

### Common Patterns

**Key Event Handling:**
```cpp
// CKeyEventSink::OnKeyDown() flow:
1. Update modifier state machine (_UpdateModsOnKeyDown)
2. Check hotkey whitelist (CHotkeyManager::IsKeyDownHotkey)
3. For special keys (Space/Enter), create commit request with barrier
4. For normal input, send key event to Go service via CIPCClient
5. Check service response (key consumed vs passed through)
```

**Composition Updates:**
```cpp
// CTextService::UpdateComposition() flow:
1. RequestEditSession with TF_ES_SYNC
2. Inside CUpdateCompositionEditSession:
   - Get composition range from context
   - Replace text with new composition
   - Set caret position
   - Apply display attribute
```

**State Synchronization:**
```cpp
// Full state sync (after reconnection):
1. Call _DoFullStateSync() which sends IMEActivated
2. Go service responds with StatusUpdate (mode, width, punct, caps lock state)
3. CTextService::_SyncStateFromResponse() applies status
4. Update language bar and internal state flags
```

**Async Reader Thread:**
```cpp
// Async push from Go service (e.g., user clicked candidate):
1. CIPCClient::StartAsyncReader() spawns async read thread
2. Thread listens on separate push pipe
3. Calls registered callbacks:
   - StatePushCallback for status updates
   - CommitTextCallback for candidate selection
   - ClearCompositionCallback for mode toggle via menu
4. Main thread posts message to CLangBarItemButton::_hMsgWnd for UI updates
```

### Testing Requirements

**Build Verification:**
- `cmake --build . --config Release` must succeed with no C++ compiler errors
- 产出唯一目标：`wind_tsf.dll`（不再产出 wind_dwrite.dll）
- wind_tsf.dll must export 4 functions: DllCanUnloadNow, DllGetClassObject, DllRegisterServer, DllUnregisterServer

**Registration:**
- Must call `DllRegisterServer()` to register with Windows TSF
- Creates HKEY_CURRENT_USER\Software\Microsoft\Windows NT\CurrentVersion\IMEUI... registry entries
- Register Profile (GUID) with TSF manager

**Manual Testing:**
- Register DLL: `regsvr32 wind_tsf.dll`
- Switch input method in Windows Settings and select 清风输入法
- Type in Chinese: keyboard input should trigger Go service
- Language bar should show mode indicator
- Right-click language bar menu should work

**Protocol Verification:**
- Use Named Pipe Monitor to sniff binary messages between DLL and Go service
- Verify payload structure matches BinaryProtocol.h definitions

## Common Tasks

### Adding a New TSF Event
1. Add event handler to CTextService (implements ITf*Interface)
2. Route to appropriate component (KeyEventSink, IPCClient, etc.)
3. Log via WIND_LOG_* macro
4. Test with Windows Input Method Tester (imm32tst.exe)

### Adding a New IPC Command
1. Define command ID in BinaryProtocol.h (CMD_* constant)
2. Add payload struct if needed (must be packed)
3. Implement send method in CIPCClient
4. Implement parsing in CIPCClient::_ParseResponse()
5. Update Go service to handle the command
6. Test binary protocol compatibility

### Debugging IPC Issues
1. Enable file logging: create `%LOCALAPPDATA%\WindInput\logs\tsf_log_config` with `mode=file` and `level=debug`
   （要用 Ctrl+Shift+F12 导出环形缓冲的话再加一行 `dump_hotkey=1`，见 File Logging 一节；
   这些键只在**宿主进程启动时**读一次）
2. View log at `%LOCALAPPDATA%\WindInput\logs\tsf_log\wind_tsf.<host>.<pid>.log`（每个宿主进程一个文件；
   按时间戳合并多个宿主：`sort -m -k1,2 tsf_log/wind_tsf.*.log`，格式与 `wind_input.log` 逐字对齐）
3. For real-time output: set `mode=debugstring` and use DebugView.exe
4. Monitor Named Pipes with NamedPipeMon.exe
5. Check Go service logs for parsing errors
6. Verify protocol version match (BinaryProtocol.h PROTOCOL_VERSION)

<!-- MANUAL: Any manually added notes below this line are preserved on regeneration -->
