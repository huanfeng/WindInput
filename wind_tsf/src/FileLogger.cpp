#include "FileLogger.h"
#include <shlobj.h>  // SHGetFolderPathW
#include <cstdio>
#include <cstring>

// ============================================================================
// CFileLogger implementation
// ============================================================================

CFileLogger::CFileLogger()
    : _mode(LogMode::None)
    , _level(LogLevel::Info)
    , _initialized(false)
    , _hMutex(nullptr)
    , _pid(0)
    , _ringHead(0)
    , _ringCount(0)
{
    _logDir[0] = L'\0';
    _logPath[0] = L'\0';
    _configPath[0] = L'\0';
    memset(_ringBuffer, 0, sizeof(_ringBuffer));
    InitializeCriticalSection(&_ringLock);
}

CFileLogger::~CFileLogger()
{
    Shutdown();
    DeleteCriticalSection(&_ringLock);
}

CFileLogger& CFileLogger::Instance()
{
    static CFileLogger instance;
    return instance;
}

void CFileLogger::Init()
{
    if (_initialized)
        return;

    _initialized = true;
    _pid = GetCurrentProcessId();

    // Build paths first
    _BuildPaths();
    if (_logDir[0] == L'\0')
        return;

    // Ensure log directory exists
    CreateDirectoryW(_logDir, nullptr);

    // Read config (mode + level)
    _ReadConfig();

    // If mode is none, skip mutex creation entirely
    if (_mode == LogMode::None)
        return;

    // Create named mutex for file write synchronization (only needed for file mode)
    if (_mode == LogMode::File || _mode == LogMode::All)
    {
        _hMutex = CreateMutexW(nullptr, FALSE, L"Local\\WindInputTSFLogMutex");
        if (_hMutex == nullptr)
        {
            OutputDebugStringW(L"[WindInput][FileLogger] Failed to create mutex, file logging disabled\n");
            // Fall back to debugstring-only if we had All mode
            if (_mode == LogMode::All)
                _mode = LogMode::DebugString;
            else
                _mode = LogMode::None;
        }
    }

    // Write startup marker
    wchar_t startMsg[256];
    _snwprintf_s(startMsg, _countof(startMsg), _TRUNCATE,
        L"FileLogger initialized (mode=%d, level=%ls, pid=%lu)",
        (int)_mode, _LevelStr(_level), _pid);
    Write(LogLevel::Info, startMsg);
}

void CFileLogger::Shutdown()
{
    if (!_initialized)
        return;

    if (_mode != LogMode::None)
    {
        Write(LogLevel::Info, L"FileLogger shutdown");
    }

    if (_hMutex != nullptr)
    {
        CloseHandle(_hMutex);
        _hMutex = nullptr;
    }

    _mode = LogMode::None;
    _initialized = false;
}

void CFileLogger::Write(LogLevel level, const wchar_t* message)
{
    if (!IsEnabled(level) || message == nullptr)
        return;

    // Format: "2026-03-17 07:11:02.985 [DEBUG] [PID: 1234] message"
    wchar_t timestamp[32];
    _FormatTimestamp(timestamp, _countof(timestamp));

    // OutputDebugStringW path
    if (_mode == LogMode::DebugString || _mode == LogMode::All)
    {
        _WriteToDebugString(level, message);
    }

    // File path
    if (_mode == LogMode::File || _mode == LogMode::All)
    {
        wchar_t line[1200];
        int len = _snwprintf_s(line, _countof(line), _TRUNCATE,
            L"%ls [%-5ls] [PID: %5lu] %ls\r\n",
            timestamp, _LevelStr(level), _pid, message);

        if (len <= 0)
            return;

        // Convert to UTF-8
        char utf8Line[2400];
        int utf8Len = WideCharToMultiByte(CP_UTF8, 0, line, len, utf8Line, sizeof(utf8Line) - 1, nullptr, nullptr);
        if (utf8Len <= 0)
            return;

        _WriteToFile(utf8Line, utf8Len);
    }
}

void CFileLogger::_WriteToDebugString(LogLevel level, const wchar_t* message)
{
    WCHAR buf[600];
    _snwprintf_s(buf, _countof(buf), _TRUNCATE,
        L"[WindInput][%ls] %ls\n", _LevelStr(level), message);
    OutputDebugStringW(buf);
}

void CFileLogger::_WriteToFile(const char* utf8Line, int utf8Len)
{
    if (_hMutex == nullptr)
        return;

    // Acquire mutex (with timeout to avoid blocking input)
    DWORD waitResult = WaitForSingleObject(_hMutex, MUTEX_TIMEOUT_MS);
    if (waitResult != WAIT_OBJECT_0 && waitResult != WAIT_ABANDONED)
        return; // Skip rather than block

    // Check rotation before opening
    _RotateIfNeeded();

    // Open file in append mode
    HANDLE hFile = CreateFileW(
        _logPath,
        FILE_APPEND_DATA,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        nullptr,
        OPEN_ALWAYS,
        FILE_ATTRIBUTE_NORMAL,
        nullptr
    );

    if (hFile != INVALID_HANDLE_VALUE)
    {
        DWORD written;
        WriteFile(hFile, utf8Line, (DWORD)utf8Len, &written, nullptr);
        CloseHandle(hFile);
    }

    ReleaseMutex(_hMutex);
}

void CFileLogger::_BuildPaths()
{
    wchar_t appData[MAX_PATH];
    if (FAILED(SHGetFolderPathW(nullptr, CSIDL_LOCAL_APPDATA, nullptr, 0, appData)))
        return;

    _snwprintf_s(_logDir, _countof(_logDir), _TRUNCATE,
        L"%ls\\WindInput\\logs", appData);

    _snwprintf_s(_logPath, _countof(_logPath), _TRUNCATE,
        L"%ls\\wind_tsf.log", _logDir);

    _snwprintf_s(_configPath, _countof(_configPath), _TRUNCATE,
        L"%ls\\tsf_log_config", _logDir);
}

void CFileLogger::_ReadConfig()
{
    // Default: mode=none, level=info
    _mode = LogMode::None;
    _level = LogLevel::Info;

    HANDLE hFile = CreateFileW(
        _configPath,
        GENERIC_READ,
        FILE_SHARE_READ,
        nullptr,
        OPEN_EXISTING,
        FILE_ATTRIBUTE_NORMAL,
        nullptr
    );

    if (hFile == INVALID_HANDLE_VALUE)
        return; // No config file → mode=none

    char buf[256] = {};
    DWORD bytesRead = 0;
    ReadFile(hFile, buf, sizeof(buf) - 1, &bytesRead, nullptr);
    CloseHandle(hFile);

    // Parse line by line
    char* ctx = nullptr;
    char* line = strtok_s(buf, "\r\n", &ctx);
    while (line != nullptr)
    {
        // Skip leading whitespace
        while (*line == ' ' || *line == '\t') line++;

        // Skip comments and empty lines
        if (*line == '#' || *line == '\0')
        {
            line = strtok_s(nullptr, "\r\n", &ctx);
            continue;
        }

        // Parse key=value
        char* eq = strchr(line, '=');
        if (eq != nullptr)
        {
            *eq = '\0';
            char* key = line;
            char* val = eq + 1;

            // Trim key
            char* kEnd = eq - 1;
            while (kEnd > key && (*kEnd == ' ' || *kEnd == '\t')) *kEnd-- = '\0';

            // Trim value
            while (*val == ' ' || *val == '\t') val++;
            char* vEnd = val + strlen(val) - 1;
            while (vEnd > val && (*vEnd == ' ' || *vEnd == '\t')) *vEnd-- = '\0';

            if (_stricmp(key, "mode") == 0)
            {
                if (_stricmp(val, "none") == 0 || _stricmp(val, "off") == 0)
                    _mode = LogMode::None;
                else if (_stricmp(val, "file") == 0)
                    _mode = LogMode::File;
                else if (_stricmp(val, "debugstring") == 0 || _stricmp(val, "debug_string") == 0)
                    _mode = LogMode::DebugString;
                else if (_stricmp(val, "all") == 0)
                    _mode = LogMode::All;
            }
            else if (_stricmp(key, "level") == 0)
            {
                if (_stricmp(val, "off") == 0) _level = LogLevel::Off;
                else if (_stricmp(val, "error") == 0) _level = LogLevel::Error;
                else if (_stricmp(val, "warn") == 0) _level = LogLevel::Warn;
                else if (_stricmp(val, "info") == 0) _level = LogLevel::Info;
                else if (_stricmp(val, "debug") == 0) _level = LogLevel::Debug;
                else if (_stricmp(val, "trace") == 0) _level = LogLevel::Trace;
            }
        }

        line = strtok_s(nullptr, "\r\n", &ctx);
    }
}

void CFileLogger::_RotateIfNeeded()
{
    WIN32_FILE_ATTRIBUTE_DATA fad;
    if (!GetFileAttributesExW(_logPath, GetFileExInfoStandard, &fad))
        return;

    LARGE_INTEGER fileSize;
    fileSize.LowPart = fad.nFileSizeLow;
    fileSize.HighPart = fad.nFileSizeHigh;

    if (fileSize.QuadPart < MAX_LOG_SIZE)
        return;

    wchar_t oldPath[MAX_PATH];
    _snwprintf_s(oldPath, _countof(oldPath), _TRUNCATE,
        L"%ls\\wind_tsf.old.log", _logDir);

    DeleteFileW(oldPath);
    MoveFileW(_logPath, oldPath);
}

void CFileLogger::_FormatTimestamp(wchar_t* buf, size_t bufSize)
{
    SYSTEMTIME st;
    GetLocalTime(&st);
    _snwprintf_s(buf, bufSize, _TRUNCATE,
        L"%04d-%02d-%02d %02d:%02d:%02d.%03d",
        st.wYear, st.wMonth, st.wDay,
        st.wHour, st.wMinute, st.wSecond, st.wMilliseconds);
}

const wchar_t* CFileLogger::_LevelStr(LogLevel level)
{
    switch (level)
    {
    case LogLevel::Error: return L"ERROR";
    case LogLevel::Warn:  return L"WARN";
    case LogLevel::Info:  return L"INFO";
    case LogLevel::Debug: return L"DEBUG";
    case LogLevel::Trace: return L"TRACE";
    default:              return L"?????";
    }
}

// ============================================================================
// Ring Buffer (always active, no file system / debug string dependency)
// ============================================================================

void CFileLogger::WriteToRingBuffer(LogLevel level, const wchar_t* message)
{
    if (message == nullptr)
        return;

    EnterCriticalSection(&_ringLock);

    wchar_t timestamp[32];
    _FormatTimestamp(timestamp, _countof(timestamp));

    _snwprintf_s(_ringBuffer[_ringHead], RING_LINE_MAX, _TRUNCATE,
        L"%ls [%-5ls] %ls",
        timestamp, _LevelStr(level), message);

    _ringHead = (_ringHead + 1) % RING_BUFFER_LINES;
    if (_ringCount < RING_BUFFER_LINES)
        _ringCount++;

    LeaveCriticalSection(&_ringLock);
}

std::wstring CFileLogger::DumpRingBuffer()
{
    EnterCriticalSection(&_ringLock);

    std::wstring result;
    result.reserve(_ringCount * 128);

    // Header
    wchar_t header[128];
    _snwprintf_s(header, _countof(header), _TRUNCATE,
        L"=== WindInput TSF Log Dump (PID:%lu, entries:%d) ===\r\n", _pid, _ringCount);
    result += header;

    if (_ringCount == 0)
    {
        result += L"(empty)\r\n";
    }
    else
    {
        // Start from oldest entry
        int start = (_ringCount < RING_BUFFER_LINES) ? 0 : _ringHead;
        for (int i = 0; i < _ringCount; i++)
        {
            int idx = (start + i) % RING_BUFFER_LINES;
            result += _ringBuffer[idx];
            result += L"\r\n";
        }
    }

    result += L"=== End Log Dump ===\r\n";

    // Clear buffer after dump
    _ringHead = 0;
    _ringCount = 0;

    LeaveCriticalSection(&_ringLock);
    return result;
}
