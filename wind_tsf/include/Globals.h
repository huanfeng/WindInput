#pragma once

#include <windows.h>
#include <msctf.h>
#include <ctfutb.h>
#include <cstdio>
#include <cstdarg>
#include <string>

// ============================================================================
// Logging Configuration
// ============================================================================
// All log levels are compiled in. Output is controlled at runtime via config file:
//   %LOCALAPPDATA%\WindInput\logs\tsf_log_config
//
// Config format (one key=value per line):
//   mode=none          Output mode: none(default) | file | debugstring | all
//   level=debug        Log level: off | error | warn | info | debug | trace
//
// When mode=none (or no config file), logging has near-zero overhead
// (a single branch on a global variable per log call).
// ============================================================================

#include "FileLogger.h"

namespace WindLog {
    // Map level constant to FileLogger::LogLevel
    inline CFileLogger::LogLevel _ToFileLevel(int level) {
        return static_cast<CFileLogger::LogLevel>(level);
    }

    inline void Output(int level, const wchar_t* msg) {
        auto& logger = CFileLogger::Instance();
        auto fileLevel = _ToFileLevel(level);

        // Quick exit: skip TRACE/DEBUG for ring buffer to avoid per-keystroke overhead
        bool ringWorthy = (fileLevel <= CFileLogger::LogLevel::Info);
        if (!ringWorthy && !logger.IsEnabled(fileLevel))
            return;

        // Strip trailing \n\r for clean message
        WCHAR cleanMsg[512];
        wcsncpy_s(cleanMsg, msg, _TRUNCATE);
        size_t len = wcslen(cleanMsg);
        while (len > 0 && (cleanMsg[len - 1] == L'\n' || cleanMsg[len - 1] == L'\r'))
            cleanMsg[--len] = L'\0';

        // Write to ring buffer for INFO and above (Ctrl+Shift+F11 dump)
        if (ringWorthy)
            logger.WriteToRingBuffer(fileLevel, cleanMsg);

        // Normal file/debugstring output only if enabled
        if (logger.IsEnabled(fileLevel))
            logger.Write(fileLevel, cleanMsg);
    }

    inline void OutputFmt(int level, const wchar_t* fmt, ...) {
        auto& logger = CFileLogger::Instance();
        auto fileLevel = _ToFileLevel(level);

        bool ringWorthy = (fileLevel <= CFileLogger::LogLevel::Info);
        if (!ringWorthy && !logger.IsEnabled(fileLevel))
            return;

        WCHAR msgBuf[512];
        va_list args;
        va_start(args, fmt);
        _vsnwprintf_s(msgBuf, _countof(msgBuf), _TRUNCATE, fmt, args);
        va_end(args);

        // Strip trailing \n\r
        size_t len = wcslen(msgBuf);
        while (len > 0 && (msgBuf[len - 1] == L'\n' || msgBuf[len - 1] == L'\r'))
            msgBuf[--len] = L'\0';

        if (ringWorthy)
            logger.WriteToRingBuffer(fileLevel, msgBuf);

        if (logger.IsEnabled(fileLevel))
            logger.Write(fileLevel, msgBuf);
    }
}

// ============================================================================
// Log macros - all levels always compiled in, filtered at runtime
// ============================================================================

#define WIND_LOG_ERROR(msg)            WindLog::Output(1, msg)
#define WIND_LOG_ERROR_FMT(fmt, ...)   WindLog::OutputFmt(1, fmt, __VA_ARGS__)
#define WIND_LOG_WARN(msg)             WindLog::Output(2, msg)
#define WIND_LOG_WARN_FMT(fmt, ...)    WindLog::OutputFmt(2, fmt, __VA_ARGS__)
#define WIND_LOG_INFO(msg)             WindLog::Output(3, msg)
#define WIND_LOG_INFO_FMT(fmt, ...)    WindLog::OutputFmt(3, fmt, __VA_ARGS__)
#define WIND_LOG_DEBUG(msg)            WindLog::Output(4, msg)
#define WIND_LOG_DEBUG_FMT(fmt, ...)   WindLog::OutputFmt(4, fmt, __VA_ARGS__)
#define WIND_LOG_TRACE(msg)            WindLog::Output(5, msg)
#define WIND_LOG_TRACE_FMT(fmt, ...)   WindLog::OutputFmt(5, fmt, __VA_ARGS__)

// Legacy compatibility
#define WIND_LOG(msg) WIND_LOG_DEBUG(msg)
#define WIND_LOG_FMT(fmt, ...) WIND_LOG_DEBUG_FMT(fmt, __VA_ARGS__)

// ============================================================================

// 全局变量声明
extern HINSTANCE g_hInstance;
extern LONG g_lServerLock;

struct WindHostProcessInfo
{
    DWORD processId = 0;
    DWORD threadId = 0;
    HWND hwnd = nullptr;
    BOOL isAppContainer = FALSE;
    DWORD integrityRid = 0;
    DWORD queryError = ERROR_SUCCESS;
    std::wstring processPath;
    std::wstring processName;
    std::wstring windowClass;
    std::wstring windowTitle;
    std::wstring packageFamilyName;
};

// GUID 定义
// {7E5A5C60-1234-4567-89AB-CDEF01234567}
// 注意：实际使用时应该生成唯一的 GUID
extern const CLSID c_clsidTextService;

// {7E5A5C61-1234-4567-89AB-CDEF01234567}
extern const GUID c_guidProfile;

// {7E5A5C62-1234-4567-89AB-CDEF01234567}
extern const GUID c_guidLangBarItemButton;

// 输入法名称
#define TEXTSERVICE_NAME        L"清风输入法"
#define TEXTSERVICE_DESC        L"清风输入法 (WindInput)"
#define TEXTSERVICE_ICON_INDEX  0

// 语言 ID (简体中文)
#define TEXTSERVICE_LANGID      0x0804

// 命名管道名称 (与 Go Service 通信)
// 注意：不使用 LOCAL\ 前缀，AppContainer 进程可能无法访问带目录前缀的管道
#define PIPE_NAME               L"\\\\.\\pipe\\wind_input"
#define PUSH_PIPE_NAME          L"\\\\.\\pipe\\wind_input_push"

// Modifier key flags (using KEY_ prefix to avoid Windows macro conflicts)
constexpr int KEY_MOD_SHIFT = 0x01;
constexpr int KEY_MOD_CTRL  = 0x02;
constexpr int KEY_MOD_ALT   = 0x04;

// 工具函数
LONG DllAddRef();
LONG DllRelease();

BOOL WindQueryCurrentProcessInfo(WindHostProcessInfo* info);
BOOL WindQueryWindowProcessInfo(HWND hwnd, WindHostProcessInfo* info);
void WindLogHostProcessInfo(int level, const wchar_t* prefix, const WindHostProcessInfo& info);
void WindLogForegroundProcessInfo(int level, const wchar_t* prefix);

// COM 工具函数
template<class T>
inline void SafeRelease(T*& p)
{
    if (p)
    {
        p->Release();
        p = nullptr;
    }
}

template<class T>
inline void SafeDelete(T*& p)
{
    if (p)
    {
        delete p;
        p = nullptr;
    }
}
