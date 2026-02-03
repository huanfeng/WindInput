#pragma once

#include <windows.h>
#include <msctf.h>
#include <ctfutb.h>

// ============================================================================
// Debug logging configuration
// ============================================================================
// Uncomment to enable verbose debug logging (impacts performance when DebugView is active)
// #define WIND_DEBUG_LOG

#ifdef WIND_DEBUG_LOG
    #define WIND_LOG(msg) OutputDebugStringW(msg)
    #define WIND_LOG_FMT(fmt, ...) do { \
        WCHAR _buf[512]; \
        swprintf(_buf, 512, fmt, __VA_ARGS__); \
        OutputDebugStringW(_buf); \
    } while(0)
#else
    #define WIND_LOG(msg) ((void)0)
    #define WIND_LOG_FMT(fmt, ...) ((void)0)
#endif

// Error logging is always enabled (critical errors only)
#define WIND_LOG_ERROR(msg) OutputDebugStringW(L"[WindInput ERROR] " msg)
#define WIND_LOG_ERROR_FMT(fmt, ...) do { \
    WCHAR _buf[512]; \
    swprintf(_buf, 512, L"[WindInput ERROR] " fmt, __VA_ARGS__); \
    OutputDebugStringW(_buf); \
} while(0)

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
#define TEXTSERVICE_NAME        L"御风输入法"
#define TEXTSERVICE_DESC        L"WindInput Input Method"
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
