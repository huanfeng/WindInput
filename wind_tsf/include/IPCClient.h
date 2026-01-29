#pragma once

#include "Globals.h"
#include <string>
#include <vector>
#include <functional>
#include <atomic>

// IPC Configuration
namespace IPCConfig
{
    // Timeout settings (milliseconds)
    constexpr DWORD CONNECT_TIMEOUT_MS = 100;      // Connection timeout
    constexpr DWORD WRITE_TIMEOUT_MS = 50;         // Write operation timeout
    constexpr DWORD READ_TIMEOUT_MS = 100;         // Read operation timeout

    // Circuit breaker settings
    constexpr int MAX_CONSECUTIVE_FAILURES = 3;    // Failures before circuit opens
    constexpr DWORD CIRCUIT_RESET_INTERVAL_MS = 3000; // Time before retry after circuit opens

    // Buffer sizes
    constexpr DWORD PIPE_BUFFER_SIZE = 4096;
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

// Response types from Go Service
enum class ResponseType
{
    Ack,
    InsertText,
    UpdateComposition,
    ClearComposition,
    ModeChanged,
    StatusUpdate,
    Consumed,       // Key was consumed by a hotkey (no output)
    Unknown
};

// Hotkey configuration from Go Service
struct HotkeyConfigData
{
    std::vector<std::wstring> toggleModeKeys;  // lshift, rshift, lctrl, rctrl, capslock
    std::wstring switchEngine;                  // ctrl+`, ctrl+shift+e, none
    std::wstring toggleFullWidth;               // shift+space, ctrl+shift+space, none
    std::wstring togglePunct;                   // ctrl+., ctrl+,, none
    std::vector<std::wstring> selectKeyGroups;  // semicolon_quote, comma_period, lrshift, lrctrl
    std::vector<std::wstring> pageKeys;         // pageupdown, minus_equal, brackets, shift_tab
    BOOL hasData;                               // TRUE if hotkey config was present in response

    HotkeyConfigData() : hasData(FALSE) {}
};

// Response from Go Service
struct ServiceResponse
{
    ResponseType type;
    std::wstring text;      // For InsertText
    std::wstring composition; // For UpdateComposition
    int caretPos;           // For UpdateComposition
    BOOL chineseMode;       // For ModeChanged, StatusUpdate, and InsertText (when modeChanged)
    BOOL modeChanged;       // For InsertText with mode change (CommitOnSwitch)
    BOOL fullWidth;         // For StatusUpdate
    BOOL chinesePunct;      // For StatusUpdate
    BOOL toolbarVisible;    // For StatusUpdate
    HotkeyConfigData hotkeys; // For StatusUpdate (hotkey configuration)
    std::wstring error;
};

// Circuit breaker state
enum class CircuitState
{
    Closed,     // Normal operation
    Open,       // Failing, skip IPC calls
    HalfOpen    // Testing if service recovered
};

// Callback for receiving responses
using ResponseCallback = std::function<void(const ServiceResponse&)>;

class CIPCClient
{
public:
    CIPCClient();
    ~CIPCClient();

    // Connect to named pipe server (with timeout)
    BOOL Connect();

    // Disconnect
    void Disconnect();

    // Check if service is available (considers circuit breaker)
    BOOL IsServiceAvailable();

    // Send key event to Go Service (with optional caret info for efficiency)
    // If caret is provided (px, py, pHeight all non-null), includes caret in the same request
    BOOL SendKeyEvent(const std::wstring& key, int keyCode, int modifiers = 0,
                      const LONG* px = nullptr, const LONG* py = nullptr, const LONG* pHeight = nullptr);

    // Send caret position update to Go Service (standalone, use SendKeyEvent with caret for efficiency)
    BOOL SendCaretUpdate(int x, int y, int height);

    // Send focus lost notification
    BOOL SendFocusLost();

    // Send focus gained notification (for toolbar display)
    // Includes caret position so service knows which screen to show toolbar on
    BOOL SendFocusGained(LONG caretX = 0, LONG caretY = 0, LONG caretHeight = 0);

    // Send toggle mode request
    BOOL SendToggleMode();

    // Send Caps Lock state for indicator display
    BOOL SendCapsLockState(BOOL capsLockOn);

    // Send menu command (toggle_mode, toggle_width, toggle_punct, open_settings, toggle_toolbar)
    BOOL SendMenuCommand(const char* command);

    // Send IME deactivated notification (when user switches to another IME)
    BOOL SendIMEDeactivated();

    // Send IME activated notification (when user switches back to this IME)
    BOOL SendIMEActivated();

    // Check if connected
    BOOL IsConnected() const { return _hPipe != INVALID_HANDLE_VALUE; }

    // Receive response from service (call this after sending)
    BOOL ReceiveResponse(ServiceResponse& response);

    // Log level control
    static void SetLogLevel(IPCLogLevel level) { s_logLevel = level; }
    static IPCLogLevel GetLogLevel() { return s_logLevel; }

    // Get circuit breaker state (for debugging/UI)
    CircuitState GetCircuitState() const { return _circuitState; }

    // Force circuit breaker reset (e.g., user manually triggered)
    void ResetCircuitBreaker();

private:
    // Pipe handle
    HANDLE _hPipe;

    // Overlapped I/O event
    HANDLE _hEvent;

    // Service start flag
    BOOL _serviceStartAttempted;

    // Circuit breaker state
    CircuitState _circuitState;
    int _consecutiveFailures;
    DWORD _lastFailureTime;

    // Static log level
    static IPCLogLevel s_logLevel;

    // Start the Go service if not running
    BOOL _StartService();

    // Send message with timeout (length-prefixed JSON)
    BOOL _SendMessage(const std::wstring& message);

    // Receive message with timeout (length-prefixed JSON)
    BOOL _ReceiveMessage(std::wstring& message);

    // Parse response JSON
    BOOL _ParseResponse(const std::wstring& json, ServiceResponse& response);

    // Overlapped I/O helpers
    BOOL _WriteWithTimeout(const void* data, DWORD size, DWORD timeoutMs);
    BOOL _ReadWithTimeout(void* buffer, DWORD size, DWORD* bytesRead, DWORD timeoutMs);

    // Circuit breaker helpers
    void _RecordSuccess();
    void _RecordFailure();
    BOOL _ShouldAttemptOperation();

    // Logging helpers
    static void _Log(IPCLogLevel level, const wchar_t* format, ...);
    static void _LogError(const wchar_t* format, ...);
    static void _LogInfo(const wchar_t* format, ...);
    static void _LogDebug(const wchar_t* format, ...);
};
