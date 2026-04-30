<!-- Generated: 2026-03-13 | Updated: 2026-05-01 -->

# wind_tsf - Windows TSF Input Method Bridge

## Purpose

C++17 DLL implementing the Windows Text Services Framework (TSF) interface for the 清风输入法 (WindInput) Chinese input method. This component:

- Registers as a system-level input method with Windows TSF
- Captures keyboard events and forwards them to the wind_input Go service via Named Pipe IPC
- Manages composition, caret position tracking, and candidate selection
- Provides language bar UI integration and hotkey management
- Implements display attributes (underline) for composition text
- Maintains state synchronization with the Go service via binary protocol
- Provides HostWindow 机制，在宿主进程（如开始菜单）内通过 CreateWindowInBand 创建带外层级窗口，解决 Win11 开始菜单候选框 z-order 问题

The DLL is built with CMake/MSVC and exports standard TSF COM interfaces (DllCanUnloadNow, DllGetClassObject, DllRegisterServer, DllUnregisterServer).

> **注意：wind_dwrite.dll 已移除。** DirectWrite 渲染改由 Go 侧通过 CGO 直接调用系统 dwrite.dll，C++ 侧不再构建 wind_dwrite 目标。CMakeLists.txt 现在只构建 wind_tsf.dll 一个目标。

## Key Files

| File | Description |
|------|-------------|
| `CMakeLists.txt` | CMake 构建配置（C++17, UTF-8；唯一目标 wind_tsf.dll 输出到 ../build/；从 res/version.rc.in 模板生成版本资源） |
| `wind_tsf.def` | Module definition file (exports COM entry points) |
| `README.md` | Project documentation |

## Subdirectories

| Directory | Purpose |
|-----------|---------|
| `include/` | Header files (see `include/AGENTS.md`) |
| `src/` | Implementation files (see `src/AGENTS.md`) |
| `res/` | Resource files: icon + version.rc.in 模板 (see `res/AGENTS.md`) |

## Build Instructions

```bash
cd wind_tsf
mkdir -p build
cd build
cmake .. -G "Visual Studio 17 2022" -A x64
cmake --build . --config Release
```

Output:
- `../build/wind_tsf.dll` — 主 TSF 输入法 DLL（唯一构建目标）

版本号可通过 `-D` 参数传入：
```bash
cmake .. -DAPP_VERSION_MAJOR=1 -DAPP_VERSION_MINOR=0 -DAPP_VERSION_PATCH=0 -DAPP_VERSION_STR="1.0.0"
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
- 日志文件：`%LOCALAPPDATA%\WindInput\logs\wind_tsf.log`
- 配置文件：`%LOCALAPPDATA%\WindInput\logs\tsf_log_config`（mode/level 两个键）
- 多进程安全：Named Mutex + append 模式文件 I/O
- 自动轮转：超过 5MB 时重命名为 `wind_tsf.old.log`
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
7. **HostWindow:** 只在 host_render_processes 配置的进程中激活（由 Go 服务通过 IPC 指令触发 Initialize）

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
2. View log at `%LOCALAPPDATA%\WindInput\logs\wind_tsf.log`
3. For real-time output: set `mode=debugstring` and use DebugView.exe
4. Monitor Named Pipes with NamedPipeMon.exe
5. Check Go service logs for parsing errors
6. Verify protocol version match (BinaryProtocol.h PROTOCOL_VERSION)

<!-- MANUAL: Any manually added notes below this line are preserved on regeneration -->
