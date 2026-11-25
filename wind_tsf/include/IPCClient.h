#pragma once

#include "Globals.h"
#include "BinaryProtocol.h"
#include <atomic>
#include <string>
#include <vector>
#include <functional>

// IPC Configuration
namespace IPCConfig
{
    // Timeout settings (milliseconds)
    // 这些超时是防止卡死宿主的保险丝，不是延迟预算。系统繁忙时正常按键也可能偶发数百毫秒，
    // 取宽松值以免把偶发慢误判为服务挂死而断连；代价是真卡死时用户感知顿一下。
    constexpr DWORD CONNECT_TIMEOUT_MS = 500;      // Connection timeout
    constexpr DWORD WRITE_TIMEOUT_MS = 300;        // Write operation timeout
    constexpr DWORD READ_TIMEOUT_MS = 1500;        // Read operation timeout

    // Circuit breaker settings
    constexpr int MAX_CONSECUTIVE_FAILURES = 3;    // Failures before circuit opens
    constexpr DWORD CIRCUIT_RESET_INTERVAL_MS = 3000; // Time before retry after circuit opens

    // Buffer sizes (64KB like Weasel)
    constexpr DWORD PIPE_BUFFER_SIZE = 64 * 1024;
    constexpr DWORD MAX_MESSAGE_SIZE = 1024 * 1024; // 1MB max message
}

// Log levels for debug output
enum class IPCLogLevel
{
    None = 0,      // No logging
    Error = 1,     // Only errors
    Info = 2,      // Errors + important info
    Debug = 3      // Everything including verbose debug
};

// One parsed CMD_HOST_RENDER_SETUP list entry: an SHM channel for one host window kind
// (candidate / tooltip / status). The DLL creates one band window per entry.
struct HostRenderSetupInfo
{
    uint32_t windowKind = 0;    // HostWindowKind
    uint32_t maxBufferSize = 0; // Max shared-memory size for this channel
    std::wstring shmName;
    std::wstring eventName;
};

// Response from the service (simplified for binary protocol)
struct ServiceResponse
{
    ResponseType type = ResponseType::Error;

    // For CommitText
    std::wstring text;
    std::wstring newComposition;
    bool modeChanged = false;
    bool chineseMode = false;
    bool restartComposition = false; // 提交后需重启编排（嵌入/非嵌入模式统一，非嵌入时 newComposition 为空走占位符路径）
    // 本次提交要替换掉 hold 预览态里待定的中文符号（智能符号 press2），而非追加在它后面。
    bool replacingHeld = false;
    // 上屏后处于无占位的联想会话（COMMIT_FLAG_ASSOC_SESSION）：不重启组合，但要同步记会话。
    bool assocSession = false;

    // For InsertTextWithCursor
    int cursorOffset = 0;

    // For MoveCursorRight: 向右移动的格数（配对跳出）。默认 1 = 单字符配对的老行为，
    // 也是旧版 core（不带 count 载荷）时的兜底。
    uint32_t moveCount = 1;

    // For ReplaceBackward: 删除光标前的字符数（v1 固定 1），随后插入 text
    int replaceCount = 0;

    // For UpdateComposition
    std::wstring composition;
    int caretPos = 0;

    // For StatusUpdate
    uint32_t statusFlags = 0;

    // Icon label for taskbar display (from the service, e.g., "中", "英", "A", "拼", "五")
    std::wstring iconLabel;

    // For SyncHotkeys / StatusUpdate
    std::vector<uint32_t> keyDownHotkeys;
    std::vector<uint32_t> keyUpHotkeys;

    // For HostRenderSetup — one entry per host window kind (candidate/tooltip/status).
    // The legacy single fields below mirror the candidate (first) entry for back-compat
    // logging; _EnsureHostRenderSetup iterates hostRenderSetups to create all band windows.
    std::vector<HostRenderSetupInfo> hostRenderSetups;
    std::wstring shmName;
    std::wstring eventName;
    uint32_t maxBufferSize = 0;
    // This connection's instance ID (bridge clientID). Stamped onto every band window so the
    // render thread can match it against SharedRenderHeader.targetInstanceId — only the
    // targeted instance renders a frame; siblings in the same process hide. 0 = unset.
    uint32_t hostRenderInstanceId = 0;

    // For HoldComposition
    uint32_t holdTimeoutMs = 0;

    // For UiElementPage（CMD_UIELEMENT_QUERY 的响应）：候选文本为当页候选，
    // uiSelected 为页内下标，uiPageSize ≥ 1，uiCurrentPage 恒 0（快照只带当页）。
    std::vector<std::wstring> uiCandidates;
    uint32_t uiSelected = 0;
    uint32_t uiPageSize = 1;
    uint32_t uiCurrentPage = 0;

    // Error
    std::wstring error;

    // Helper methods
    bool IsHostRenderAvailable() const { return (statusFlags & STATUS_HOST_RENDER_AVAIL) != 0; }
    bool IsChineseMode() const { return (statusFlags & STATUS_CHINESE_MODE) != 0 || chineseMode; }
    bool IsFullWidth() const { return (statusFlags & STATUS_FULL_WIDTH) != 0; }
    bool IsChinesePunct() const { return (statusFlags & STATUS_CHINESE_PUNCT) != 0; }
    bool IsToolbarVisible() const { return (statusFlags & STATUS_TOOLBAR_VISIBLE) != 0; }
    bool IsCapsLock() const { return (statusFlags & STATUS_CAPS_LOCK) != 0; }
    // 软键盘面板开着。吃键判定要用：中文模式无 input session 时数字键本是交还宿主的，
    // 软键盘的数字行需要它们被吃下来。
    bool IsSoftKeyboard() const { return (statusFlags & STATUS_SOFT_KEYBOARD) != 0; }
    bool IsSoftKeyboardKeys() const { return (statusFlags & STATUS_SOFT_KEYBOARD_KEYS) != 0; }
    bool IsHotkeySession() const { return (statusFlags & STATUS_HOTKEY_SESSION) != 0; }
    bool HasHotkeys() const { return !keyDownHotkeys.empty() || !keyUpHotkeys.empty(); }
};

// Circuit breaker state
enum class CircuitState
{
    Closed,     // Normal operation
    Open,       // Failing, skip IPC calls
    HalfOpen    // Testing if service recovered
};

class CIPCClient
{
public:
    CIPCClient();
    ~CIPCClient();

    // Connect to named pipe server (with timeout)
    BOOL Connect();

    // Disconnect from named pipe
    void Disconnect();

    // Check if service is available (considers circuit breaker)
    BOOL IsServiceAvailable();

    // Send key event to the service (binary protocol)
    // Uses state machine snapshot for modifiers/toggles
    // prevChar: character before caret from ITfTextEditSink cache (0 if unavailable)
    // seqChar: last char of the IME's own input sequence (InputSequenceChar.h, 0 if unknown)
    BOOL SendKeyEvent(uint32_t keyCode, uint32_t scanCode, uint32_t modifiers, uint8_t eventType,
                      uint8_t toggles = 0, uint16_t eventSeq = 0, uint16_t prevChar = 0,
                      uint16_t seqChar = 0);

    // Send commit request (barrier mechanism for Space/Enter/number selection)
    // Returns TRUE if request was sent successfully
    BOOL SendCommitRequest(const uint8_t* payload, uint32_t payloadSize);

    // Send caret position update to the service
    // source: CARET_SRC_*，标明这组坐标来自哪个通道。消费端据此决定能否当权威坐标、
    //         能否与组合起点做距离比较（GUI 回退值与组合起点本就不同源，比较无意义）。
    // compRect*：整个组合 range 的包围矩形，四值全 0 = 本帧未取到（服务端据此忽略）。
    // 见 BinaryProtocol.h 的 CaretPayloadV3。
    BOOL SendCaretUpdate(int x, int y, int height, int compositionStartX = 0, int compositionStartY = 0,
                         int source = CARET_SRC_UNKNOWN,
                         int compRectLeft = 0, int compRectTop = 0,
                         int compRectRight = 0, int compRectBottom = 0);

    // Send caret-pending handshake: composition just started, real caret coming after app reflow.
    // Tells the service to extend its first-show fallback timeout so it doesn't fall back to pre-key cursor.
    BOOL SendCaretPending();

    // Send a first-show caret probe: one pre-reflow sample taken inside an OnLayoutChange
    // burst iteration. Fire-and-forget; the service decides whether the sample is trustworthy
    // (see CMD_CARET_PROBE in BinaryProtocol.h). Never affects local composition state.
    BOOL SendCaretProbe(int x, int y, int height, int compositionStartX, int compositionStartY,
                        int source = CARET_SRC_UNKNOWN);

    // Send selection changed notification (from ITfTextEditSink::OnEndEdit)
    // Async: notifies the service that the caret moved outside of composition (e.g., mouse click)
    BOOL SendSelectionChanged(uint16_t prevChar = 0);

    // Send focus lost notification.
    // Carries this instance's clientToken so the service can discard a *stale* focus_lost —
    // OnKillThreadFocus emits it ~100ms after the DocMgr-level focus loss, i.e. after the next
    // host's focus_gained has already arrived.
    // reason 取 FOCUS_LOST_REASON_*，决定服务端清哪些状态；三种语义的区别见
    // BinaryProtocol.h。默认 THREAD＝最保守的「整个应用离开了」。
    BOOL SendFocusLost(uint8_t reason = FOCUS_LOST_REASON_THREAD);

    // Send focus gained notification (with optional caret position and InputScope bitmask).
    // disabled/reason: input diagnostics HUD state for the newly focused control (Task 7).
    // reason: 0 None / 1 CompartmentDisabled / 2 InputScopePassword / 3 NumericPassword.
    //
    // `disabled` 恒为线程级 GUID_COMPARTMENT_KEYBOARD_DISABLED，语义是「系统禁用输入法，DLL 已在
    // OnTestKeyDown 全放行」。不要传 context 级的密码框判定（_focusIsPassword）：core 会据此跳过
    // 强制英文，而网页密码框的键并没被放行，会照打中文。密码框信号走 inputScopeMask 的 IS_PASSWORD 位。
    //
    // caretSource: CARET_SRC_*，标明 caretX/Y 出自回退链的哪一级。OnSetFocus 里同步 edit session
    // 必被拒绝、回退值是跨窗口的 Win32 光标，焦点气泡只能靠它分辨可信度。
    //
    // windowClass: 焦点所在顶层窗口的类名（nullptr / 空 = 未知）。服务端据此区分 explorer.exe 的
    // 过渡型窗口（任务栏 / Alt+Tab）与停留型窗口（桌面 / 文件管理器）；空值回落到照常重算初始模式。
    //
    // windowTitle: 同一个顶层窗口的标题（可为 nullptr / 空）。调用方只在 core 推开了
    // CONFIG_KEY_COMPAT_TITLE_MATCH 时才取并传入（见 WindowTitlePolicy.h）；用户数据，日志只记长度。
    BOOL SendFocusGained(int caretX = 0, int caretY = 0, int caretHeight = 0, UINT64 inputScopeMask = 0,
                         bool disabled = false, uint8_t reason = 0,
                         int caretSource = CARET_SRC_UNKNOWN,
                         const wchar_t* windowClass = nullptr,
                         const wchar_t* windowTitle = nullptr);

    // Send input state report (async, fire-and-forget): standalone disabled/reason change
    // for the currently focused control, e.g. a GUID_COMPARTMENT_KEYBOARD_DISABLED flip that
    // happens without a new OnSetFocus (SPA navigating into a password field in-place).
    BOOL SendInputStateReport(uint32_t pid, bool disabled, uint8_t reason, uint64_t inputScopeMask);

    // Send observed key (async, fire-and-forget): a user keydown passed straight to the host,
    // reported for extensions only (CONFIG_KEY_EXTENSION_KEY_OBSERVE). Key content is never logged.
    BOOL SendKeyObserved(uint32_t keyCode, uint32_t modifiers);

    // Send diagnostics snapshot (async, fire-and-forget): focus window chain + foreground
    // window + TSF context instance ids. Header 后接三段 `len u16 LE + UTF-8` 类名。
    // 仅在 core 经 CONFIG_KEY_DIAG_SNAPSHOT 推开采集后才会被调用（调用方门控）。
    BOOL SendDiagSnapshot(const DiagSnapshotHeader& head,
                          const std::wstring& focusClass,
                          const std::wstring& rootClass,
                          const std::wstring& fgClass);

    // Send composition unexpectedly terminated notification
    // (e.g., user clicked in input field to change cursor position)
    BOOL SendCompositionTerminated();

    // Send IME deactivated notification (when user switches to another IME)
    BOOL SendIMEDeactivated();

    // Send IME activated notification (when user switches back to this IME)
    BOOL SendIMEActivated();

    // Send host render request (after activation with HOST_RENDER_AVAIL flag)
    // Gets back shared memory and event names for host rendering
    BOOL SendHostRenderRequest(ServiceResponse& response);

    // Send mode change notification (async, for local mode toggle)
    // This notifies the service that TSF has locally toggled the mode
    BOOL SendModeNotify(bool chineseMode, bool clearInput);

    // Send toggle mode request (sync, from UI click)
    // The service will toggle mode and return StatusUpdate response (full state + iconLabel)
    BOOL SendToggleMode(ServiceResponse& response);

    // System mode switch (Ctrl+Space): sync request to the service with target mode
    // The service will check CommitOnSwitch and return commitText if needed
    // source 只进服务端日志：让「宿主写 compartment」与「按键兜底」在服务端日志里可分辨。
    // ctrlHeld 是判据不是日志：服务端的 ignore_host_ime_switch 靠它放行 Ctrl+Space
    // （系统热键与宿主切中英走同一条 compartment 通路，不交代就分不出）。
    BOOL SendSystemModeSwitch(bool chineseMode, ModeSwitchSource source, bool ctrlHeld,
                              ServiceResponse& response);

    // Check if connected
    BOOL IsConnected() const { return _hPipe != INVALID_HANDLE_VALUE; }

    // 主管道对端已断（服务端重启后留下的旧连接，IsConnected() 仍为真）时关掉句柄，
    // 让下一次发送走 Connect 新建；活连接不动。返回是否丢弃了句柄。TSF 线程调用。
    BOOL DiscardStaleConnection();

    // Receive response from service (call this after sending)
    BOOL ReceiveResponse(ServiceResponse& response);

    // ========================================================================
    // Async and Batch support (for performance optimization)
    // ========================================================================

    // Send async message (fire-and-forget, no response expected)
    BOOL SendAsync(uint16_t command, const void* payload, uint32_t size);

    // Send sync message (waits for response)
    BOOL SendSync(uint16_t command, const void* payload, uint32_t size, ServiceResponse& response);

    // Batch event support
    void BeginBatch();
    void AddBatchEvent(uint16_t command, const void* payload, uint32_t size, bool needResponse);
    BOOL SendBatch(std::vector<ServiceResponse>& responses);

    // Receive batch response
    BOOL ReceiveBatchResponse(std::vector<ServiceResponse>& responses, int expectedCount);

    // ========================================================================
    // Async Reader Thread (for receiving the service's proactive state pushes)
    // ========================================================================

    // Callback type for state push notifications
    using StatePushCallback = std::function<void(const ServiceResponse&)>;

    // Callback type for activation status push notifications (CMD_ACTIVATION_STATUS_PUSH).
    // 服务端处理完 IME_ACTIVATED / FOCUS_GAINED 后经 push pipe 推送的完整 activation 状态
    // （含 hotkeys + hostRenderAvail + iconLabel）。回调在 AsyncReader 线程上调用，TextService
    // 据此 Post 到 TSF 线程做 _SyncStateFromResponse + _EnsureHostRenderSetup。
    using ActivationPushCallback = std::function<void(const ServiceResponse&)>;

    // Callback type for commit text from the service (mouse click on candidate)
    using CommitTextCallback = std::function<void(const std::wstring&)>;

    // Callback type for clear composition from the service (mode toggle via menu)
    using ClearCompositionCallback = std::function<void()>;

    // Callback type for replace-backward push from service (undo commit):
    // delete `count` chars before caret, then insert text (may be empty).
    using ReplaceBackwardCallback = std::function<void(int, const std::wstring&)>;

    // Callback type for pair-commit push from service (直通 ime.pair):
    // 上屏 text 后向左移 moveLeft 格，并记一层待跳出深度。
    using PairCommitCallback = std::function<void(const std::wstring&, uint32_t)>;

    // Callback type for update composition from the service (mouse click partial confirm)
    using UpdateCompositionCallback = std::function<void(const std::wstring& text, int caretPos)>;

    // Callback type for config sync from the service (generic key/value push)
    using SyncConfigCallback = std::function<void(const std::string& key, const std::vector<uint8_t>& value)>;

    // Callback type for service-ready notification (the service connected push pipe)
    using ServiceReadyCallback = std::function<void()>;

    // Callback type for icon-refresh push (CMD_REFRESH_ICON), no payload.
    // 位图换了而状态没变时由服务端发来。回调在 AsyncReader 线程上，实现只能 PostMessage 到
    // TSF 线程再发 OnUpdate，不得在此碰 COM。
    using RefreshIconCallback = std::function<void()>;

    // Callback type for mode-only push (CMD_MODE_PUSH).
    // 携带 chineseMode + fullWidth + chinesePunct，用于 FocusGained 竞态窗口优化。
    // 回调在 AsyncReader 线程上调用；实现必须用 InterlockedExchange 写
    // _bChineseMode/_bFullWidth/_bChinesePunct，不得调用 _SyncStateFromResponse（会清热键）。
    // 须带 chinesePunct：标点透传判据依赖它，漏了则切完焦点的第一个标点会按旧态判。
    using ModePushCallback =
        std::function<void(bool chineseMode, bool fullWidth, bool chinesePunct)>;

    // Callback type for shell exec push (CMD_SHELL_EXEC).
    // 在 TSF 侧（前台应用进程）调用 ShellExecuteW，解决 Service 进程无前台权限问题。
    // 回调在 AsyncReader 线程上调用，ShellExecuteW 线程安全，可直接调用。
    //
    // dir = 子进程的工作目录。空串会继承宿主进程的当前目录（不可控），故服务端总会发一个
    // 确定目录；为空只出现在对接旧版服务时。
    // verb = ShellExecute 动词（open/runas/edit/print/explore/properties），空串 = open。
    // show = 初始窗口状态（normal/min/max/hidden），空串 = normal。
    // verb/show 的取值已由服务端按白名单校验，收到的一定合法或为空。
    using ShellExecCallback = std::function<void(const std::wstring& target, const std::wstring& params, const std::wstring& dir, const std::wstring& verb, const std::wstring& show)>;

    // Set callback for receiving state push from the service
    void SetStatePushCallback(StatePushCallback callback);

    // Set callback for receiving activation status push from the service
    void SetActivationPushCallback(ActivationPushCallback callback);

    // Set callback for receiving commit text from the service (mouse click on candidate)
    void SetCommitTextCallback(CommitTextCallback callback);

    // Set callback for receiving clear composition from the service (mode toggle via menu)
    void SetClearCompositionCallback(ClearCompositionCallback callback);

    // Set callback for receiving replace-backward push from service (undo commit)
    void SetReplaceBackwardCallback(ReplaceBackwardCallback callback);

    // Set callback for receiving pair-commit push from service (直通 ime.pair)
    void SetPairCommitCallback(PairCommitCallback callback);

    // Set callback for receiving update composition from the service (mouse click partial confirm)
    void SetUpdateCompositionCallback(UpdateCompositionCallback callback);

    // Set callback for receiving config sync from the service (generic key/value push)
    void SetSyncConfigCallback(SyncConfigCallback callback);

    // Set callback for service-ready notification (the service connected push pipe)
    void SetServiceReadyCallback(ServiceReadyCallback callback);

    // Set callback for mode-only push (CMD_MODE_PUSH, FocusGained 竞态优化)
    void SetModePushCallback(ModePushCallback callback);

    // Set callback for icon-refresh push (CMD_REFRESH_ICON, 位图变而状态未变)
    void SetRefreshIconCallback(RefreshIconCallback callback);

    // Set callback for shell exec push (CMD_SHELL_EXEC)
    void SetShellExecCallback(ShellExecCallback callback);

    // Start async reader thread (call after successful connection)
    BOOL StartAsyncReader();

    // Stop async reader thread
    void StopAsyncReader();

    // Check if async reader is running
    BOOL IsAsyncReaderRunning() const;

    // Log level control
    static void SetLogLevel(IPCLogLevel level) { s_logLevel = level; }
    static IPCLogLevel GetLogLevel() { return s_logLevel; }

    // Get circuit breaker state (for debugging/UI)
    CircuitState GetCircuitState() const { return _circuitState; }

    // Force circuit breaker reset (e.g., user manually triggered)
    void ResetCircuitBreaker();

    // State sync tracking: TRUE when a new connection is established and
    // full state sync with the service hasn't happened yet.
    BOOL NeedsStateSync() const { return _needsStateSync; }
    void ClearNeedsSyncFlag() { _needsStateSync = FALSE; }

    // 本连接上 IME_ACTIVATED、FOCUS_GAINED 或按键是否已发出去过（新连接 / 断开时清零）。
    // SERVICE_READY 的过期重试与焦点补发据此停手：服务端收到 IME_ACTIVATED 会按初始模式重置中英。
    BOOL StateEstablished() const { return _stateEstablished; }

    // SERVICE_READY 同步期间置 TRUE：管道忙（各宿主同时来连）不计入熔断。按键路径照旧计数。
    void SetPipeBusyExempt(BOOL exempt) { _pipeBusyExempt = exempt; }

private:
    // Pipe handle
    HANDLE _hPipe;

    // Overlapped I/O event
    HANDLE _hEvent;

    // Service start flag
    BOOL _serviceStartAttempted;

    // State sync flag: set TRUE on new connection, cleared after full sync
    BOOL _needsStateSync;

    BOOL _stateEstablished;
    BOOL _pipeBusyExempt;

    // 本连接上是否成功写过消息。写过的连接才可能是服务端重启留下的陈旧句柄，
    // 发送失败时只对它重连重发；刚建的连接失败照旧算失败。
    BOOL _pipeWritten;
    // 最近一次 _WriteWithTimeout 失败的错误码，_SendBinaryMessage 据此判对端是否已断。
    DWORD _lastWriteError;

    // Circuit breaker state
    CircuitState _circuitState;
    int _consecutiveFailures;
    DWORD _lastFailureTime;

    // Static log level
    static IPCLogLevel s_logLevel;

    // Per-instance token: (uint64_t)PID << 32 | per-process-counter (uint32)
    // Used to correlate push pipe connection with request/response commands.
    // 64-bit form keeps the full PID (PIDs exceed 65535; truncating collides across processes).
    uint64_t _clientToken;
    static std::atomic<uint32_t> s_instanceCounter;

    // Start the the service if not running
    BOOL _StartService();

    // Send binary message (header + payload)
    BOOL _SendBinaryMessage(uint16_t command, const void* payload, uint32_t payloadSize, bool async = false);

    // Receive binary message
    BOOL _ReceiveBinaryMessage(IpcHeader& header, std::vector<uint8_t>& payload);

    // Parse binary response
    BOOL _ParseResponse(const IpcHeader& header, const std::vector<uint8_t>& payload, ServiceResponse& response);

    // Overlapped I/O helpers
    BOOL _WriteWithTimeout(const void* data, DWORD size, DWORD timeoutMs);
    BOOL _ReadWithTimeout(void* buffer, DWORD size, DWORD* bytesRead, DWORD timeoutMs);

    // Drain stale (late) responses left in the pipe by a previous timed-out request,
    // so request/response pairing cannot shift off-by-one. Called before every send.
    void _DrainStaleData();

    // Circuit breaker helpers
    void _RecordSuccess();
    void _RecordFailure();
    BOOL _ShouldAttemptOperation();

    // Encoding helpers
    static std::wstring _Utf8ToWide(const char* utf8, size_t length);
    static std::string _WideToUtf8(const std::wstring& wide);

    /// 安装器闸门：`HKLM\Software\<app>\InstallerRunning` 是否**当前有效**地要求不启动服务。
    ///
    /// 还校验 `InstallerRunningOwner` 记下的进程是否仍在，以免安装器异常死亡后标记永久生效。
    /// 判定规则见 InstallerGuard.h。
    static bool _InstallerGuardBlocks();

    // Logging helpers
    static void _Log(IPCLogLevel level, const wchar_t* format, ...);
    static void _LogError(const wchar_t* format, ...);
    static void _LogInfo(const wchar_t* format, ...);
    static void _LogDebug(const wchar_t* format, ...);

    // Batch state
    std::vector<uint8_t> _batchBuffer;
    std::vector<bool> _batchNeedResponse;
    uint16_t _batchCount = 0;

    // Async reader thread state
    HANDLE _hAsyncThread = NULL;
    HANDLE _hStopEvent = NULL;           // Event to signal thread to stop
    HANDLE _hReadPipe = INVALID_HANDLE_VALUE;  // Separate pipe for async reading
    StatePushCallback _statePushCallback;
    ActivationPushCallback _activationPushCallback;
    CommitTextCallback _commitTextCallback;
    ClearCompositionCallback _clearCompositionCallback;
    ReplaceBackwardCallback _replaceBackwardCallback;
    PairCommitCallback _pairCommitCallback;
    UpdateCompositionCallback _updateCompositionCallback;
    SyncConfigCallback _syncConfigCallback;
    ServiceReadyCallback _serviceReadyCallback;
    ModePushCallback _modePushCallback;
    RefreshIconCallback _refreshIconCallback;
    ShellExecCallback _shellExecCallback;
    CRITICAL_SECTION _asyncLock;         // Lock for thread-safe access
    volatile BOOL _asyncReaderRunning = FALSE;

    // Async reader thread function
    static DWORD WINAPI _AsyncReaderThread(LPVOID lpParam);
    void _AsyncReaderLoop();
};
