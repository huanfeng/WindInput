<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-20 -->

# src/ - Implementation Files

## Purpose

C++ implementation files for the TSF DLL。所有文件编译链接进唯一目标 `wind_tsf.dll`。文件按组件（text service, IPC, hotkey, UI, host window, logging）和入口点（dllmain）组织。

> **注意：`WindDWriteShim.cpp` 已移除。** DirectWrite 渲染改由 Go 侧通过 CGO 直接调用系统 dwrite.dll，不再在 C++ 侧构建。

## Key Files

| File | Description |
|------|-------------|
| `dllmain.cpp` | DLL entry point (DllMain, DllCanUnloadNow, DllGetClassObject, DllRegisterServer, DllUnregisterServer) |
| `Globals.cpp` | Global state initialization (HINSTANCE, ref count, GUID definitions) |
| `TextService.cpp` | CTextService implementation (TSF integration, composition, caret tracking, state sync) |
| `KeyEventSink.cpp` | CKeyEventSink implementation (key capture, modifier state machine, barrier mechanism) |
| `IPCClient.cpp` | CIPCClient implementation (named pipe, binary protocol, circuit breaker, async reader) |
| `ClassFactory.cpp` | CClassFactory implementation (COM class factory for TextService) |
| `HotkeyManager.cpp` | CHotkeyManager implementation (hotkey lookup, key classification) |
| `LangBarItemButton.cpp` | CLangBarItemButton implementation (language bar UI, menu, async updates) |
| `CaretEditSession.cpp` | CCaretEditSession implementation (TSF edit session for caret position) |
| `DisplayAttributeInfo.cpp` | Display attribute classes (styling for composition text) |
| `Register.cpp` | Registry integration (DllRegisterServer, DllUnregisterServer, profile/category registration) |
| `FileLogger.cpp` | CFileLogger implementation (Init/Shutdown, config file reading, file write with Named Mutex, auto-rotation at 5MB) |
| `HostWindow.cpp` | CHostWindow implementation（创建 Band 级分层窗口、共享内存渲染帧读取、渲染线程、动态解析 CreateWindowInBand/GetWindowBand API） |

## Component Responsibilities

### dllmain.cpp
- `DllMain()` - Process attach/detach, thread initialization
- `DllCanUnloadNow()` - Return S_FALSE if server locked, else S_OK
- `DllGetClassObject()` - Create and return CClassFactory
- `DllRegisterServer()` - Register with Windows (delegates to Register.cpp)
- `DllUnregisterServer()` - Unregister from Windows (delegates to Register.cpp)

### TextService.cpp
- `CTextService::Activate()` - Register thread manager event sink, initialize components
- `CTextService::Deactivate()` - Unregister sinks, cleanup
- `CTextService::OnSetFocus()` - Handle window focus changes
- `CTextService::UpdateComposition()` - Update inline composition text via edit session
- `CTextService::InsertText()` - Commit text without composition
- `CTextService::EndComposition()` - Terminate active composition
- `CTextService::InsertTextAndStartComposition()` - Commit + start new composition (for top-code)
- `CTextService::GetCaretPosition()` - Get caret position from context
- `CTextService::SendCaretPositionUpdate()` - Send caret update to Go service
- `CTextService::ToggleInputMode()` - Toggle Chinese/English mode
- `CTextService::_DoFullStateSync()` - Sync state with Go service after reconnection
- Edit session helper classes: CUpdateCompositionEditSession, CEndCompositionEditSession, etc.

### KeyEventSink.cpp
- `CKeyEventSink::OnKeyDown()` - Capture key down events
- `CKeyEventSink::OnKeyUp()` - Capture key up events
- `CKeyEventSink::OnTestKey()` - Peek at key without consuming
- `CKeyEventSink::OnPreservedKey()` - Handle hotkey (Ctrl+Space, etc.)
- `_UpdateModsOnKeyDown()` - Update modifier state machine on key down
- `_UpdateModsOnKeyUp()` - Update modifier state machine on key up
- `_GetModsSnapshot()` - Get current modifier state for event
- `_GetTogglesSnapshot()` - Get CapsLock/NumLock/ScrollLock state
- `_SendKeyToService()` - Serialize and send key event to Go service
- `_HandleServiceResponse()` - Parse response and apply (consume/pass through)
- `_SendCommitRequest()` - Send commit request with barrier for Space/Enter/number
- `_HandleCommitResult()` - Process commit result from Go service
- `_CheckBarrierTimeout()` - Check if barrier mechanism times out (500ms)
- `_IsMatchingKeyUp()` - Match KeyUp with pending toggle KeyDown
- `_IsContextReadOnly()` - Detect read-only input fields (browser support)
- `OnCompositionUnexpectedlyTerminated()` - Handle composition termination by application
- Toggle key tap detection (500ms threshold for mode toggle vs long press)

### IPCClient.cpp
- `CIPCClient::Connect()` - Connect to named pipe (with timeout)
- `CIPCClient::Disconnect()` - Close pipe handle
- `CIPCClient::IsServiceAvailable()` - Check circuit breaker and pipe state
- `SendKeyEvent()` - Send key event with binary protocol
- `SendCommitRequest()` - Send commit request with barrier
- `SendCaretUpdate()` - Send caret position to Go service
- `SendFocusGained()` / `SendFocusLost()` - Focus notifications
- `SendIMEActivated()` / `SendIMEDeactivated()` - IME state notifications
- `SendModeNotify()` - Notify mode change (TSF local toggle, async)
- `SendToggleMode()` - Toggle mode request from UI (sync)
- `SendCompositionTerminated()` - Notify composition unexpectedly terminated
- `SendAsync()` - Send async message (fire-and-forget)
- `SendSync()` - Send sync message (wait for response)
- `ReceiveResponse()` - Parse binary response from pipe
- `_SendBinaryMessage()` - Low-level pipe write
- `_ReceiveBinaryMessage()` - Low-level pipe read
- `_ParseResponse()` - Deserialize binary response to ServiceResponse struct
- `_WriteWithTimeout()` / `_ReadWithTimeout()` - Overlapped I/O with timeout
- `_RecordSuccess()` / `_RecordFailure()` - Circuit breaker tracking
- `_ShouldAttemptOperation()` - Circuit breaker decision logic
- `_Utf8ToWide()` / `_WideToUtf8()` - Encoding helpers
- `_Log()` / `_LogError()` / `_LogDebug()` - Logging helpers
- Async reader thread: `_AsyncReaderThread()`, `_AsyncReaderLoop()`, `StartAsyncReader()`, `StopAsyncReader()`
- Batch support: `BeginBatch()`, `AddBatchEvent()`, `SendBatch()`, `ReceiveBatchResponse()`

### HotkeyManager.cpp
- `CHotkeyManager::UpdateHotkeys()` - Update whitelist from Go service
- `CHotkeyManager::IsKeyDownHotkey()` / `IsKeyUpHotkey()` - O(1) lookup
- `CHotkeyManager::IsToggleModeKeyByVK()` - Check if key toggles mode
- `CHotkeyManager::ClassifyInputKey()` - Classify key type (letter, number, punct, etc.)
- `CHotkeyManager::CalcKeyHash()` - Calculate key hash for lookup
- `CHotkeyManager::NormalizeModifiers()` - Strip left/right specific modifiers

### LangBarItemButton.cpp
- `CLangBarItemButton::GetInfo()` - Return language bar item info
- `CLangBarItemButton::GetStatus()` - Return item visibility status
- `CLangBarItemButton::OnClick()` - Handle left-click on language bar
- `CLangBarItemButton::InitMenu()` - Build right-click context menu
- `CLangBarItemButton::OnMenuSelect()` - Handle menu item selection
- `CLangBarItemButton::GetIcon()` - Return language bar icon
- `CLangBarItemButton::GetText()` - Return tooltip text
- `CLangBarItemButton::UpdateLangBarButton()` - Update icon/text when mode changes
- `CLangBarItemButton::UpdateCapsLockState()` - Update indicator when CapsLock toggled
- `_MsgWndProc()` - Message window for cross-thread updates
- `PostUpdateFullStatus()` - Thread-safe status update via WM_UPDATE_STATUS
- `PostCommitText()` - Thread-safe commit via WM_COMMIT_TEXT
- `PostClearComposition()` - Thread-safe clear composition via WM_CLEAR_COMPOSITION

### CaretEditSession.cpp
- `CCaretEditSession::DoEditSession()` - TSF edit session callback
- `CCaretEditSession::GetCaretRect()` - Static method to retrieve caret position

### DisplayAttributeInfo.cpp
- `CDisplayAttributeInfoInput::GetAttributeInfo()` - Return underline styling for composition
- `CDisplayAttributeProvider::EnumDisplayAttributeInfo()` - Enumerate available attributes
- `CDisplayAttributeProvider::GetDisplayAttributeInfo()` - Get specific attribute by GUID
- `CEnumDisplayAttributeInfo` - Enumerator for display attributes

### Register.cpp
- `RegisterServer()` - Register CLSID in HKEY_CLASSES_ROOT
- `UnregisterServer()` - Unregister CLSID
- `RegisterProfile()` - Register input method profile with TSF manager
- `UnregisterProfile()` - Unregister profile
- `RegisterCategories()` - Register text service categories (TIP, INPUTPROCESSOR, etc.)
- `UnregisterCategories()` - Unregister categories

### FileLogger.cpp
- `CFileLogger::Instance()` - 获取单例
- `CFileLogger::Init()` - 读取配置文件，构建日志路径，初始化 Named Mutex
- `CFileLogger::Shutdown()` - 关闭 Mutex 句柄，清理资源
- `CFileLogger::Write()` - 线程安全写入（持有 Named Mutex，append 模式）
- `CFileLogger::IsEnabled()` - 内联快速路径检查（mode=none 时零开销）
- `_ReadConfig()` - 解析 `%LOCALAPPDATA%\WindInput\logs\tsf_log_config`（mode/level 两个键）
- `_BuildPaths()` - 构建 logDir/logPath/configPath
- `_RotateIfNeeded()` - 超过 5MB 时将 wind_tsf.log 重命名为 wind_tsf.old.log
- `_WriteToFile()` - UTF-8 写入文件
- `_WriteToDebugString()` - 调用 `OutputDebugStringW`
- `_FormatTimestamp()` - 生成 `[HH:MM:SS.mmm]` 格式时间戳

**Config file** (`%LOCALAPPDATA%\WindInput\logs\tsf_log_config`):
```
mode=none    # none | file | debugstring | all
level=debug  # off | error | warn | info | debug | trace
```

### HostWindow.cpp
- `CHostWindow::Initialize()` - 接收共享内存/事件名，调用 `_ResolveAPIs()` 和 `_CreateBandWindow()`，启动渲染线程
- `CHostWindow::Uninitialize()` - 停止渲染线程，销毁窗口，解除共享内存映射
- `CHostWindow::_ResolveAPIs()` - 动态从 user32.dll 获取 `CreateWindowInBand` 和 `GetWindowBand` 函数指针
- `CHostWindow::_GetHostBand()` - 获取宿主进程前台窗口的 DWM Band 等级
- `CHostWindow::_CreateBandWindow()` - 在指定 Band 等级创建 `WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE` 无边框窗口
- `CHostWindow::_RenderThread()` / `_RenderLoop()` - 渲染线程：等待事件信号 → 读取 SharedRenderHeader → 跳过过期帧 → `_RenderFrame()`
- `CHostWindow::_RenderFrame()` - 将共享内存像素数据通过 `UpdateLayeredWindow` 渲染到分层窗口
- `CHostWindow::_HideWindow()` - 隐藏窗口（候选框消失时调用）

## For AI Agents

### Working In This Directory

When implementing or debugging:

1. **Understand edit sessions** - TSF APIs like SetText, SetCaret must be called within RequestEditSession context
2. **Barrier mechanism** - For Space/Enter/number select, use commit request with barrier sequence number to match response
3. **Async reader thread** - Runs in background to receive state pushes; use callbacks and message window for thread-safe UI updates
4. **Reference counting** - All COMobjects need AddRef/Release; use SafeRelease() to avoid leaks
5. **Named pipe timeouts** - Connection 100ms, read/write 50-100ms; circuit breaker opens after 3 failures, resets after 3 seconds

### Common Patterns

**Sending a Key Event:**
```cpp
// In CKeyEventSink::OnKeyDown():
uint32_t mods = _GetModsSnapshot();
uint8_t toggles = _GetTogglesSnapshot();
uint16_t seq = _GetNextEventSeq();
_pTextService->GetIPCClient()->SendKeyEvent(wParam, scanCode, mods, KEY_EVENT_DOWN, toggles, seq);
_HandleServiceResponse();  // Check if consumed or pass through
```

**Updating Composition:**
```cpp
// In CTextService::UpdateComposition():
CUpdateCompositionEditSession* pEditSession = new CUpdateCompositionEditSession(...);
_pThreadMgr->RequestEditSession(_tfClientId, pEditSession, TF_ES_SYNC, NULL);
pEditSession->Release();
```

**Receiving Async State Push:**
```cpp
// In CIPCClient::_AsyncReaderLoop():
ServiceResponse response;
_ReceiveBinaryMessage(header, payload);
_ParseResponse(header, payload, response);
if (_statePushCallback) {
    _statePushCallback(response);  // Call registered callback
}
```

**Circuit Breaker Logic:**
```cpp
// In CIPCClient::_ShouldAttemptOperation():
if (_circuitState == CircuitState::Open) {
    DWORD elapsed = GetTickCount() - _lastFailureTime;
    if (elapsed >= IPCConfig::CIRCUIT_RESET_INTERVAL_MS) {
        _circuitState = CircuitState::HalfOpen;  // Try again
    } else {
        return FALSE;  // Skip operation
    }
}
```

### Testing Requirements

**Build Verification:**
- 所有 .cpp 文件（含 HostWindow.cpp）必须以 /utf-8 /W3 编译进 wind_tsf.dll
- wind_tsf.dll must export 4 functions via wind_tsf.def
- No C5260 warnings about pragma pack mismatch

**Key Event Testing:**
1. Register DLL and switch to 清风输入法
2. Press key in text editor
3. Verify Go service receives KEY_EVENT in IPC logs
4. Verify composition appears in TSF context
5. Press Space -> verify commit request sent with barrier

**IPC Testing:**
1. Monitor named pipe with NamedPipeMon
2. Verify binary protocol format matches BinaryProtocol.h
3. Test circuit breaker: kill Go service -> verify circuit opens -> restart service -> verify recovery
4. Test async reader: send state push from Go -> verify callback fires and UI updates

**Composition Testing:**
1. Type a composition-requiring sequence (e.g., "shng" for 上)
2. Verify UpdateComposition is called with correct text/caret position
3. Verify display attribute (underline) is applied
4. Press Enter -> verify InsertText and composition ends
5. Verify caret position is correct after commit

## Dependencies

### Internal
- All `.cpp` files include their corresponding `.h` header
- TextService.cpp includes KeyEventSink, IPCClient, LangBarItemButton, etc.
- dllmain.cpp includes Globals, TextService, ClassFactory, Register

### External
- Windows SDK: kernel32, ole32, user32 (linked via pragma comment in source)
- MSVC Runtime: libc, libcmt (C runtime)
- TSF Libraries: msctf.lib, ctfutb.lib
- DirectWrite: 不再由 C++ 侧链接；Go 侧通过 CGO 直接调用系统 dwrite.dll

<!-- MANUAL: Any manually added notes below this line are preserved on regeneration -->
