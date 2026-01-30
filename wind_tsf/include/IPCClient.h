#pragma once

#include "Globals.h"
#include "BinaryProtocol.h"
#include <string>
#include <vector>
#include <functional>
#include <atomic>
#include <unordered_set>

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

// Response from Go Service (simplified for binary protocol)
struct ServiceResponse
{
    ResponseType type = ResponseType::Error;

    // For CommitText
    std::wstring text;
    std::wstring newComposition;
    bool modeChanged = false;
    bool chineseMode = false;

    // For UpdateComposition
    std::wstring composition;
    int caretPos = 0;

    // For StatusUpdate / ModeChanged
    uint32_t statusFlags = 0;

    // For SyncHotkeys / StatusUpdate
    std::vector<uint32_t> keyDownHotkeys;
    std::vector<uint32_t> keyUpHotkeys;

    // Error
    std::wstring error;

    // Helper methods
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

    // Send key event to Go Service (binary protocol)
    BOOL SendKeyEvent(uint32_t keyCode, uint32_t scanCode, uint32_t modifiers, uint8_t eventType);

    // Send caret position update to Go Service
    BOOL SendCaretUpdate(int x, int y, int height);

    // Send focus lost notification
    BOOL SendFocusLost();

    // Send focus gained notification (with optional caret position)
    BOOL SendFocusGained(int caretX = 0, int caretY = 0, int caretHeight = 0);

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

    // Send binary message (header + payload)
    BOOL _SendBinaryMessage(uint16_t command, const void* payload, uint32_t payloadSize);

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
};
