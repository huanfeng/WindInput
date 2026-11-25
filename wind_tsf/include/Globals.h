#pragma once

#include <windows.h>
#include <msctf.h>
#include <ctfutb.h>
#include <cstdio>
#include <cstdarg>
#include <string>

// 宿主给不出真实行高时，候选窗锚点默认往下让开的高度（逻辑像素，用前须按宿主视角的 DPI
// 换算）。`TextService.cpp` 与 `CaretEditSession.cpp` 共用，放在公共头里以免两处各抄一份字面量。
#define WIND_DEFAULT_CARET_HEIGHT 20

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

    // 该级别是否可能产生输出。INFO 及以上恒答 true（可能进环形缓冲，不看 dump_hotkey）。
    // 用于给「收集实参本身就很贵」的日志加前置闸门：宏能延后格式化，挡不住实参求值。
    inline bool IsEnabled(int level) {
        auto fileLevel = _ToFileLevel(level);
        return fileLevel <= CFileLogger::LogLevel::Info
            || CFileLogger::Instance().IsEnabled(fileLevel);
    }

    // 高精度耗时测量。GetTickCount 分辨率约 15.6ms，测不了个位数毫秒的热点。
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
        // 环形缓冲受 dump_hotkey 门控（见 FileLogger.h），关着时别在输入线程上白拷贝。
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
    // 窗口标题只留长度（InternalGetWindowText 取，上限 255）：标题是用户数据，原文不进日志。
    int windowTitleLen = 0;
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
// 本键下的值名同样是跨语言约定，改一侧不会编译失败也不会测试失败：
//   InstallDir        ← 三个部署方写、本 DLL 与 core 读
//   Dota2CompatAlias  ← wind-coordinator::tsf_profile_name::ALIAS_VALUE 写、Register.cpp 的
//                        kAliasValueName 读。写错则重装时用户的兼容别名被冲回真名。
//   InstallerRunning       ← 安装器写、IPCClient.cpp 读，安装期间不让本 DLL 拉起服务。
//                            值必须恒为 "1"：旧版读端用 WCHAR[8] 定长缓冲，值长了会读成
//                            ERROR_MORE_DATA、把「标记存在」看成「不存在」。
//   InstallerRunningOwner  ← 同上一对写读。格式 "<pid>|<进程创建时间 FILETIME>"，让读端判断
//                            立标记的进程是否还活着，中途失败的安装不会把标记永久留下。
//                            判定规则见 InstallerGuard.h。
//
// InstallDir 是本 DLL 唯一可靠的安装目录来源：DLL 部署到 System32\IME\<app>\ 后
// GetModuleFileName 推不出安装目录。三个部署方（wind-installer / scripts\dev.ps1 /
// wind-portable）在注册 COM 前都必须写该值。
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
// per-user 隔离：管道名字空间是机器级的，故在扁平后缀追加当前用户 SID（`..._S-1-5-...`，
// 不引入 `\` 路径段），与 Rust wind-bridge::pipe_scope 用同一 API（ConvertSidToStringSidW）
// 算出同名。含 SID 须运行时求值，函数内惰性缓存，宏转发以保持调用点不变。
const wchar_t* WindPipeName();
const wchar_t* WindPushPipeName();
#define PIPE_NAME               WindPipeName()
#define PUSH_PIPE_NAME          WindPushPipeName()

// 语言栏图标共享内存名（服务端预渲染图标位图 → 本 DLL 的 GetIcon 取用）。
//
// 跨仓命名契约，无编译期约束：必须与 Rust 侧 `wind_ipc::protocol::icon_shm_name()` 逐字一致，
// 否则 OpenFileMappingW 恒失败、图标永远停在「加载中」且无报错。
// 不含 SID：`Local\` 前缀已提供会话级隔离，与 host-render 的 SHM 同策略（wind-bridge 的 shm_name_for）。
//
// 名字里的 `_v1` 是 ICON_SHM_VERSION，必须跟着改：命名 section 存在期间尺寸被钉死，仍有进程
// （explorer.exe、SearchHost.exe 等）持着映射时以更大的 view 映射会返回 ACCESS_DENIED。
// 版本进名字后新旧两代各用各的内核对象。IconShmReader.cpp 的 static_assert 守这一点。
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

// 采集并记录「当前进程」信息，带级别前置闸门：采集（OpenProcess + 令牌 + 映像路径）远贵于
// 一条日志。别写成裸 WindQueryCurrentProcessInfo + WindLogHostProcessInfo 两步，那样采集照跑。
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
