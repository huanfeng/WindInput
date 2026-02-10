#pragma once

#include <windows.h>
#include <msctf.h>
#include <ctfutb.h>
#include <cstdio>
#include <cstdarg>

// ============================================================================
// Logging Configuration
// ============================================================================
// Log levels (higher value = more verbose):
//   0 = OFF    - No logging
//   1 = ERROR  - Critical errors only
//   2 = WARN   - Warnings and errors
//   3 = INFO   - Important information, warnings, and errors
//   4 = DEBUG  - Debug information (verbose)
//   5 = TRACE  - Trace-level (very verbose, for development only)
//
// Set WIND_LOG_LEVEL to control logging verbosity at compile time.
// Default: INFO (3) in release, DEBUG (4) when WIND_DEBUG_LOG is defined.
// ============================================================================

// Log level constants
#define WIND_LOG_LEVEL_OFF   0
#define WIND_LOG_LEVEL_ERROR 1
#define WIND_LOG_LEVEL_WARN  2
#define WIND_LOG_LEVEL_INFO  3
#define WIND_LOG_LEVEL_DEBUG 4
#define WIND_LOG_LEVEL_TRACE 5

// Uncomment to enable verbose debug logging
// #define WIND_DEBUG_LOG

// Set default log level based on build configuration
#ifndef WIND_LOG_LEVEL
    #ifdef WIND_DEBUG_LOG
        #define WIND_LOG_LEVEL WIND_LOG_LEVEL_DEBUG
    #else
        #define WIND_LOG_LEVEL WIND_LOG_LEVEL_INFO
    #endif
#endif

// Internal logging implementation
namespace WindLog {
    inline void Output(const wchar_t* level, const wchar_t* msg) {
        WCHAR buf[600];
        swprintf(buf, 600, L"[WindInput][%s] %s", level, msg);
        OutputDebugStringW(buf);
    }

    inline void OutputFmt(const wchar_t* level, const wchar_t* fmt, ...) {
        WCHAR msgBuf[512];
        va_list args;
        va_start(args, fmt);
        _vsnwprintf_s(msgBuf, _countof(msgBuf), _TRUNCATE, fmt, args);
        va_end(args);

        WCHAR buf[600];
        swprintf(buf, 600, L"[WindInput][%s] %s", level, msgBuf);
        OutputDebugStringW(buf);
    }
}

// ============================================================================
// Log macros - use these throughout the codebase
// ============================================================================

// ERROR level - Critical errors (always enabled unless OFF)
#if WIND_LOG_LEVEL >= WIND_LOG_LEVEL_ERROR
    #define WIND_LOG_ERROR(msg) WindLog::Output(L"ERROR", msg)
    #define WIND_LOG_ERROR_FMT(fmt, ...) WindLog::OutputFmt(L"ERROR", fmt, __VA_ARGS__)
#else
    #define WIND_LOG_ERROR(msg) ((void)0)
    #define WIND_LOG_ERROR_FMT(fmt, ...) ((void)0)
#endif

// WARN level - Warnings
#if WIND_LOG_LEVEL >= WIND_LOG_LEVEL_WARN
    #define WIND_LOG_WARN(msg) WindLog::Output(L"WARN", msg)
    #define WIND_LOG_WARN_FMT(fmt, ...) WindLog::OutputFmt(L"WARN", fmt, __VA_ARGS__)
#else
    #define WIND_LOG_WARN(msg) ((void)0)
    #define WIND_LOG_WARN_FMT(fmt, ...) ((void)0)
#endif

// INFO level - Important information
#if WIND_LOG_LEVEL >= WIND_LOG_LEVEL_INFO
    #define WIND_LOG_INFO(msg) WindLog::Output(L"INFO", msg)
    #define WIND_LOG_INFO_FMT(fmt, ...) WindLog::OutputFmt(L"INFO", fmt, __VA_ARGS__)
#else
    #define WIND_LOG_INFO(msg) ((void)0)
    #define WIND_LOG_INFO_FMT(fmt, ...) ((void)0)
#endif

// DEBUG level - Debug information
#if WIND_LOG_LEVEL >= WIND_LOG_LEVEL_DEBUG
    #define WIND_LOG_DEBUG(msg) WindLog::Output(L"DEBUG", msg)
    #define WIND_LOG_DEBUG_FMT(fmt, ...) WindLog::OutputFmt(L"DEBUG", fmt, __VA_ARGS__)
#else
    #define WIND_LOG_DEBUG(msg) ((void)0)
    #define WIND_LOG_DEBUG_FMT(fmt, ...) ((void)0)
#endif

// TRACE level - Very verbose trace information
#if WIND_LOG_LEVEL >= WIND_LOG_LEVEL_TRACE
    #define WIND_LOG_TRACE(msg) WindLog::Output(L"TRACE", msg)
    #define WIND_LOG_TRACE_FMT(fmt, ...) WindLog::OutputFmt(L"TRACE", fmt, __VA_ARGS__)
#else
    #define WIND_LOG_TRACE(msg) ((void)0)
    #define WIND_LOG_TRACE_FMT(fmt, ...) ((void)0)
#endif

// Legacy compatibility macros (map to DEBUG level)
#define WIND_LOG(msg) WIND_LOG_DEBUG(msg)
#define WIND_LOG_FMT(fmt, ...) WIND_LOG_DEBUG_FMT(fmt, __VA_ARGS__)

// ============================================================================

// 全局变量声明
extern HINSTANCE g_hInstance;
extern LONG g_lServerLock;

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
#define PIPE_NAME               L"\\\\.\\pipe\\wind_input"
#define PUSH_PIPE_NAME          L"\\\\.\\pipe\\wind_input_push"

// Modifier key flags (using KEY_ prefix to avoid Windows macro conflicts)
constexpr int KEY_MOD_SHIFT = 0x01;
constexpr int KEY_MOD_CTRL  = 0x02;
constexpr int KEY_MOD_ALT   = 0x04;

// 工具函数
LONG DllAddRef();
LONG DllRelease();

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
