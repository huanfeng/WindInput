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
//   %LOCALAPPDATA%\<WIND_LOG_DIR_NAME>\logs\<WIND_LOG_CONFIG_NAME>
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

    // 该级别是否会真正产生输出（文件/DebugString，或 INFO 及以上进环形缓冲）。
    // 用于在调用点为「收集实参本身就很贵」的日志加前置闸门：日志宏能延后格式化，
    // 但挡不住实参位置的函数调用——那是 C++ 求值顺序保证要先做的事。
    inline bool IsEnabled(int level) {
        auto fileLevel = _ToFileLevel(level);
        return fileLevel <= CFileLogger::LogLevel::Info
            || CFileLogger::Instance().IsEnabled(fileLevel);
    }

    // 高精度耗时测量。GetTickCount 分辨率约 15.6ms——用它测个位数毫秒的热点，
    // 得到的只有 0 或 15 两种值（一个节拍量子），无法区分「很快」与「有点慢」。
    inline LONGLONG PerfNow() {
        LARGE_INTEGER c;
        QueryPerformanceCounter(&c);
        return c.QuadPart;
    }
    inline double PerfMsSince(LONGLONG start) {
        LARGE_INTEGER f;
        QueryPerformanceFrequency(&f);
        if (f.QuadPart == 0)
            return 0.0;
        return (double)(PerfNow() - start) * 1000.0 / (double)f.QuadPart;
    }

    inline void Output(int level, const wchar_t* msg) {
        auto& logger = CFileLogger::Instance();
        auto fileLevel = _ToFileLevel(level);

        // Quick exit: skip TRACE/DEBUG for ring buffer to avoid per-keystroke overhead
        //
        // 环形缓冲默认**不记录**（dump_hotkey 开关，见 FileLogger.h）：它唯一的出口
        // Ctrl+Shift+F12 默认关着，不问开关就等于给每条 INFO 日志白付一次
        // 512 字符拷贝 + 临界区，而这发生在 TSF 输入线程上。
        bool ringWorthy = logger.IsDumpHotkeyEnabled() && (fileLevel <= CFileLogger::LogLevel::Info);
        if (!ringWorthy && !logger.IsEnabled(fileLevel))
            return;

        // Strip trailing \n\r for clean message
        WCHAR cleanMsg[512];
        wcsncpy_s(cleanMsg, msg, _TRUNCATE);
        size_t len = wcslen(cleanMsg);
        while (len > 0 && (cleanMsg[len - 1] == L'\n' || cleanMsg[len - 1] == L'\r'))
            cleanMsg[--len] = L'\0';

        // Write to ring buffer for INFO and above (Ctrl+Shift+F12 dump; 默认关)
        if (ringWorthy)
            logger.WriteToRingBuffer(fileLevel, cleanMsg);

        // Normal file/debugstring output only if enabled
        if (logger.IsEnabled(fileLevel))
            logger.Write(fileLevel, cleanMsg);
    }

    inline void OutputFmt(int level, const wchar_t* fmt, ...) {
        auto& logger = CFileLogger::Instance();
        auto fileLevel = _ToFileLevel(level);

        bool ringWorthy = logger.IsDumpHotkeyEnabled() && (fileLevel <= CFileLogger::LogLevel::Info);
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
// {99C2EE30-5C57-45A2-9C63-FB54B34FD90A}
extern const CLSID c_clsidTextService;

// {99C2EE31-5C57-45A2-9C63-FB54B34FD90A}
extern const GUID c_guidProfile;

// {99C2EE32-5C57-45A2-9C63-FB54B34FD90A}
extern const GUID c_guidLangBarItemButton;

// {99C2EE33-5C57-45A2-9C63-FB54B34FD90A}
extern const GUID c_guidDisplayAttributeInput;

// {99C2EE34-5C57-45A2-9C63-FB54B34FD90A}
extern const GUID c_guidDisplayAttributeConverted;

// 输入法名称
#ifdef WIND_DEV_VARIANT
#define TEXTSERVICE_NAME        L"清风输入法 (开发版)"
#define TEXTSERVICE_DESC        L"清风输入法 Dev (WindInputDev)"
#else
#define TEXTSERVICE_NAME        L"清风输入法"
#define TEXTSERVICE_DESC        L"清风输入法 (WindInput)"
#endif
#define TEXTSERVICE_ICON_INDEX  0

// 应用标识与安装信息落点。
//
// WIND_APP_NAME 须与 Rust 侧 wind-config::variant::app_dir_name() 及安装器清单
// [app] id 逐字一致（三处同名，无编译期约束）。
//
// ⚠️ WIND_APP_REGKEY 下的 InstallDir 是**本 DLL 唯一可靠的安装目录来源**：
// 本 DLL 被部署到系统目录（System32\IME\<app>\，见 docs 与安装器 system_subdir）后，
// GetModuleFileName 取到的是系统副本路径，**推不出安装目录**。三个部署方
// （wind-installer / scripts\dev.ps1 / wind-portable）在注册 COM 前都必须写该值。
#ifdef WIND_DEV_VARIANT
#define WIND_APP_NAME           L"WindInputDev"
#define WIND_SERVICE_EXE        L"wind_input_dev.exe"
#else
#define WIND_APP_NAME           L"WindInput"
#define WIND_SERVICE_EXE        L"wind_input.exe"
#endif
#define WIND_APP_REGKEY         L"Software\\" WIND_APP_NAME

// 语言 ID (简体中文)
#define TEXTSERVICE_LANGID      0x0804

// 命名管道名称 (与 Rust core 通信)
// 注意：不使用 LOCAL\ 前缀，AppContainer 进程可能无法访问带目录前缀的管道。
//
// per-user 隔离：命名管道名字空间是**机器级**的，故在**扁平后缀**位置追加当前
// 用户 SID（`..._S-1-5-...`，不引入 `\` 路径段以免 AppContainer 打不开），
// 与 Rust wind-bridge::pipe_scope 用同一 OS API（ConvertSidToStringSidW）算出同名，
// 两端才在同名管道上会合。含 SID 故须运行时求值：由函数返回（进程内惰性缓存一次），
// 宏转发以保持所有调用点不变。
const wchar_t* WindPipeName();
const wchar_t* WindPushPipeName();
#define PIPE_NAME               WindPipeName()
#define PUSH_PIPE_NAME          WindPushPipeName()

// 语言栏图标共享内存名（服务端预渲染图标位图 → 本 DLL 的 GetIcon 取用）。
//
// ⚠️ **跨仓命名契约，无编译期约束**：必须与 Rust 侧
// `wind_ipc::protocol::icon_shm_name()` 的结果逐字一致。不一致的表现是
// OpenFileMappingW 恒失败、图标静默退回 DLL 本地绘制——**能正常显示，只是永远
// 不跟随标点状态变化**，没有任何报错，极难反查。改名必须两端同步。
//
// 与管道名不同，这里**不含 SID**：`Local\` 前缀已提供终端服务会话级隔离，
// 与 host-render 的 SHM 同策略（见 wind-bridge 的 shm_name_for）。
// 故可编译期常量，无需运行时求值。
// ★ 名字里的 `_v1` 是 ICON_SHM_VERSION，**必须跟着它一起改**。命名 section 的名字
// 一旦存在，尺寸也被钉死：还有进程持着映射时，以更大的 view 去映射会返回
// ACCESS_DENIED（指向权限，完全不指向真正的原因）。开发期把 SHM 提到 128 KiB 时就是
// 这样卡住的，而持有者名单里有 explorer.exe 与 SearchHost.exe，腾干净约等于注销一次。
// 版本进名字后新旧两代各用各的内核对象，互不阻塞。
// 漏改的后果是良性的（打不开 → 退回本地绘制，图标在、没角标），但同样无声。
// IconShmReader.cpp 有一条 static_assert 会在 ICON_SHM_VERSION 变动时编译失败。
#ifdef WIND_DEV_VARIANT
#define WIND_ICON_SHM_NAME      L"Local\\WindInput_IconShm_v1_dev"
#else
#define WIND_ICON_SHM_NAME      L"Local\\WindInput_IconShm_v1"
#endif

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

// 采集并记录「当前进程」信息。与 WindLogForegroundProcessInfo 同款前置闸门——
// 采集本身（OpenProcess + 令牌 + 映像路径）远贵于一条日志，级别关闭时一个 syscall 都不该做。
// 调用点原本是「裸 WindQueryCurrentProcessInfo + WindLogHostProcessInfo」两步，
// 闸门只挡得住后一步，前一步照跑。用本函数替换那个两步写法。
void WindLogCurrentProcessInfo(int level, const wchar_t* prefix);

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
