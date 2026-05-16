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
    constexpr DWORD CONNECT_TIMEOUT_MS = 200;      // Connection timeout
    constexpr DWORD WRITE_TIMEOUT_MS = 50;         // Write operation timeout
    constexpr DWORD READ_TIMEOUT_MS = 200;         // Read operation timeout

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

// Response from Go Service (simplified for binary protocol)
struct ServiceResponse
{
    ResponseType type = ResponseType::Error;

    // For CommitText
    std::wstring text;
    std::wstring newComposition;
    bool modeChanged = false;
    bool chineseMode = false;
    bool restartComposition = false; // 提交后需重启编排（嵌入/非嵌入模式统一，非嵌入时 newComposition 为空走占位符路径）

    // For InsertTextWithCursor
    int cursorOffset = 0;

    // For UpdateComposition
    std::wstring composition;
    int caretPos = 0;

    // For StatusUpdate / ModeChanged
    uint32_t statusFlags = 0;

    // Icon label for taskbar display (from Go service, e.g., "中", "英", "A", "拼", "五")
    std::wstring iconLabel;

    // For SyncHotkeys / StatusUpdate
    std::vector<uint32_t> keyDownHotkeys;
    std::vector<uint32_t> keyUpHotkeys;

    // For HostRenderSetup
    std::wstring shmName;
    std::wstring eventName;
    uint32_t maxBufferSize = 0;

    // Error
    std::wstring error;

    // Helper methods
    bool IsHostRenderAvailable() const { return (statusFlags & STATUS_HOST_RENDER_AVAIL) != 0; }
    bool IsChineseMode() const { return (statusFlags & STATUS_CHINESE_MODE) != 0 || chineseMode; }
    bool IsFullWidth() const { return (statusFlags & STATUS_FULL_WIDTH) != 0; }
    bool IsChinesePunct() const { return (statusFlags & STATUS_CHINESE_PUNCT) != 0; }
    bool IsToolbarVisible() const { return (statusFlags & STATUS_TOOLBAR_VISIBLE) != 0; }
    bool IsCapsLock() const { return (statusFlags & STATUS_CAPS_LOCK) != 0; }
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

    // Send key event to Go Service (binary protocol)
    // Uses state machine snapshot for modifiers/toggles
    // prevChar: character before caret from ITfTextEditSink cache (0 if unavailable)
    BOOL SendKeyEvent(uint32_t keyCode, uint32_t scanCode, uint32_t modifiers, uint8_t eventType,
                      uint8_t toggles = 0, uint16_t eventSeq = 0, uint16_t prevChar = 0);

    // Send commit request (barrier mechanism for Space/Enter/number selection)
    // Returns TRUE if request was sent successfully
    BOOL SendCommitRequest(const uint8_t* payload, uint32_t payloadSize);

    // Send caret position update to Go Service
    BOOL SendCaretUpdate(int x, int y, int height, int compositionStartX = 0, int compositionStartY = 0);

    // Send caret-pending handshake: composition just started, real caret coming after app reflow.
    // Tells Go to extend its first-show fallback timeout so it doesn't fall back to pre-key cursor.
    BOOL SendCaretPending();

    // Send selection changed notification (from ITfTextEditSink::OnEndEdit)
    // Async: notifies Go that the caret moved outside of composition (e.g., mouse click)
    BOOL SendSelectionChanged(uint16_t prevChar = 0);

    // Send focus lost notification
    BOOL SendFocusLost();

    // Send focus gained notification (with optional caret position)
    BOOL SendFocusGained(int caretX = 0, int caretY = 0, int caretHeight = 0);

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
    // This notifies Go that TSF has locally toggled the mode
    BOOL SendModeNotify(bool chineseMode, bool clearInput);

    // Send toggle mode request (sync, from UI click)
    // Go service will toggle mode and return ModeChanged response
    BOOL SendToggleMode(ServiceResponse& response);

    // System mode switch (Ctrl+Space): sync request to Go with target mode
    // Go will check CommitOnSwitch and return commitText if needed
    BOOL SendSystemModeSwitch(bool chineseMode, ServiceResponse& response);

    // Check if connected
    BOOL IsConnected() const { return _hPipe != INVALID_HANDLE_VALUE; }

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
    // Async Reader Thread (for receiving Go's proactive state pushes)
    // ========================================================================

    // Callback type for state push notifications
    using StatePushCallback = std::function<void(const ServiceResponse&)>;

    // Callback type for commit text from Go (mouse click on candidate)
    using CommitTextCallback = std::function<void(const std::wstring&)>;

    // Callback type for clear composition from Go (mode toggle via menu)
    using ClearCompositionCallback = std::function<void()>;

    // Callback type for update composition from Go (mouse click partial confirm)
    using UpdateCompositionCallback = std::function<void(const std::wstring& text, int caretPos)>;

    // Callback type for config sync from Go (generic key/value push)
    using SyncConfigCallback = std::function<void(const std::string& key, const std::vector<uint8_t>& value)>;

    // Callback type for service-ready notification (Go service connected push pipe)
    using ServiceReadyCallback = std::function<void()>;

    // Set callback for receiving state push from Go
    void SetStatePushCallback(StatePushCallback callback);

    // Set callback for receiving commit text from Go (mouse click on candidate)
    void SetCommitTextCallback(CommitTextCallback callback);

    // Set callback for receiving clear composition from Go (mode toggle via menu)
    void SetClearCompositionCallback(ClearCompositionCallback callback);

    // Set callback for receiving update composition from Go (mouse click partial confirm)
    void SetUpdateCompositionCallback(UpdateCompositionCallback callback);

    // Set callback for receiving config sync from Go (generic key/value push)
    void SetSyncConfigCallback(SyncConfigCallback callback);

    // Set callback for service-ready notification (Go service connected push pipe)
    void SetServiceReadyCallback(ServiceReadyCallback callback);

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
    // full state sync with Go service hasn't happened yet.
    BOOL NeedsStateSync() const { return _needsStateSync; }
    void ClearNeedsSyncFlag() { _needsStateSync = FALSE; }

private:
    // Pipe handle
    HANDLE _hPipe;

    // Overlapped I/O event
    HANDLE _hEvent;

    // Service start flag
    BOOL _serviceStartAttempted;

    // State sync flag: set TRUE on new connection, cleared after full sync
    BOOL _needsStateSync;

    // Circuit breaker state
    CircuitState _circuitState;
    int _consecutiveFailures;
    DWORD _lastFailureTime;

    // Static log level
    static IPCLogLevel s_logLevel;

    // Per-instance token: (uint64_t)PID << 32 | per-process-counter (uint32)
    // Used to correlate push pipe connection with request/response commands.
    // 64-bit form preserves the full PID — Windows 10/11 PIDs routinely exceed
    // 65535, so truncating to 16 bits would cause cross-process collisions.
    uint64_t _clientToken;
    static std::atomic<uint32_t> s_instanceCounter;

    // Start the Go service if not running
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

    // Circuit breaker helpers
    void _RecordSuccess();
    void _RecordFailure();
    BOOL _ShouldAttemptOperation();

    // Encoding helpers
    static std::wstring _Utf8ToWide(const char* utf8, size_t length);
    static std::string _WideToUtf8(const std::wstring& wide);

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
    CommitTextCallback _commitTextCallback;
    ClearCompositionCallback _clearCompositionCallback;
    UpdateCompositionCallback _updateCompositionCallback;
    SyncConfigCallback _syncConfigCallback;
    ServiceReadyCallback _serviceReadyCallback;
    CRITICAL_SECTION _asyncLock;         // Lock for thread-safe access
    volatile BOOL _asyncReaderRunning = FALSE;

    // Async reader thread function
    static DWORD WINAPI _AsyncReaderThread(LPVOID lpParam);
    void _AsyncReaderLoop();
};
