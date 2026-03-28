#pragma once

#include <windows.h>
#include <cstdint>
#include <string>

// ============================================================================
// FileLogger - Multi-process safe logging for TSF DLL
//
// Output modes (controlled by config file):
//   none        - No output (default, near-zero overhead)
//   file        - Write to %LOCALAPPDATA%\WindInput\logs\wind_tsf.log
//   debugstring - OutputDebugStringW only (viewable in DebugView)
//   all         - Both file and OutputDebugStringW
//
// Config file: %LOCALAPPDATA%\WindInput\logs\tsf_log_config
//   mode=none
//   level=debug
//
// Multi-process safety: Named Mutex + append-mode file I/O
// Auto-rotation: 5MB max, rotates to wind_tsf.old.log
//
// Ring Buffer: Always captures last RING_BUFFER_LINES log entries in memory,
//   regardless of mode. Press Ctrl+Shift+F11 to dump via text insertion.
// ============================================================================

class CFileLogger
{
public:
    enum class LogLevel : int
    {
        Off = 0,
        Error = 1,
        Warn = 2,
        Info = 3,
        Debug = 4,
        Trace = 5
    };

    enum class LogMode : int
    {
        None = 0,         // No output (default)
        File = 1,         // File only
        DebugString = 2,  // OutputDebugStringW only
        All = 3           // Both file and OutputDebugStringW
    };

    // Get singleton instance
    static CFileLogger& Instance();

    // Initialize logger (call once at DLL_PROCESS_ATTACH)
    void Init();

    // Shutdown logger (call at DLL_PROCESS_DETACH)
    void Shutdown();

    // Write a log entry (thread-safe, multi-process safe)
    // Also always writes to the in-memory ring buffer.
    void Write(LogLevel level, const wchar_t* message);

    // Fast-path check: is logging enabled at this level?
    // Inlined for minimal overhead when mode=none
    bool IsEnabled(LogLevel level) const {
        return _mode != LogMode::None && level != LogLevel::Off && level <= _level;
    }

    // Ring buffer: always enabled (captures even when mode=none)
    // Returns true if ring buffer has captured entries
    bool IsRingBufferEnabled() const { return true; }

    // Write directly to ring buffer (bypasses mode/level checks)
    void WriteToRingBuffer(LogLevel level, const wchar_t* message);

    // Dump all ring buffer entries as a single wstring, then clear
    std::wstring DumpRingBuffer();

    // Accessors
    LogLevel GetLevel() const { return _level; }
    LogMode GetMode() const { return _mode; }
    void SetLevel(LogLevel level) { _level = level; }
    void SetMode(LogMode mode) { _mode = mode; }

private:
    CFileLogger();
    ~CFileLogger();

    CFileLogger(const CFileLogger&) = delete;
    CFileLogger& operator=(const CFileLogger&) = delete;

    // Read config from file
    void _ReadConfig();

    // Build log directory and file paths
    void _BuildPaths();

    // Rotate log file if needed (caller must hold mutex)
    void _RotateIfNeeded();

    // Write to file (caller must hold mutex)
    void _WriteToFile(const char* utf8Line, int utf8Len);

    // Write to OutputDebugStringW
    void _WriteToDebugString(LogLevel level, const wchar_t* message);

    // Format timestamp
    static void _FormatTimestamp(wchar_t* buf, size_t bufSize);

    // Level to string
    static const wchar_t* _LevelStr(LogLevel level);

    LogMode _mode;
    LogLevel _level;
    bool    _initialized;
    HANDLE  _hMutex;        // For file write synchronization
    DWORD   _pid;
    wchar_t _logDir[MAX_PATH];
    wchar_t _logPath[MAX_PATH];
    wchar_t _configPath[MAX_PATH];

    // Ring buffer for in-memory log capture (always active)
    static constexpr int RING_BUFFER_LINES = 200;
    static constexpr int RING_LINE_MAX = 256;
    wchar_t _ringBuffer[RING_BUFFER_LINES][RING_LINE_MAX];
    int _ringHead;   // Next write position
    int _ringCount;  // Total entries written (capped at RING_BUFFER_LINES)
    CRITICAL_SECTION _ringLock;

    static constexpr DWORD MAX_LOG_SIZE = 5 * 1024 * 1024; // 5MB
    static constexpr DWORD MUTEX_TIMEOUT_MS = 500;
};
