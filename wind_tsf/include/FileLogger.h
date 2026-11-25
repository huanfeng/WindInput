#pragma once

#include <windows.h>
#include <cstdint>
#include <string>

// Log path macros for debug variant coexistence
//
// 变体隔离只做在目录一层（`WindInputDev` / `WindInput`），文件名与配置名两版本同名，
// 切换变体时排查命令原样可用。
#ifdef WIND_DEV_VARIANT
#define WIND_LOG_DIR_NAME       L"WindInputDev"
#else
#define WIND_LOG_DIR_NAME       L"WindInput"
#endif
// 日志文件名前缀。实际文件是 `<前缀>.<宿主名>.<pid>.log`（轮转产物再加 `.old`），
// 每进程一个，见 CFileLogger::_BuildPaths。core 侧清理旧日志时按这个前缀匹配。
#define WIND_LOG_FILE_PREFIX    L"wind_tsf"
// TSF 日志的专属子目录（`logs\tsf_log\`）：按进程拆开后文件很多，别淹没 core 的主日志。
#define WIND_LOG_SUBDIR_NAME    L"tsf_log"
// 配置文件留在 `logs\` 下、不进子目录：它是用户手工创建的总开关，搬走会让存量开关静默失效。
#define WIND_LOG_CONFIG_NAME    L"tsf_log_config"

// ============================================================================
// FileLogger - Multi-process safe logging for TSF DLL
//
// Output modes (controlled by config file):
//   none        - No output (default, near-zero overhead)
//   file        - Write to <日志根>\tsf_log\<前缀>.<宿主名>.<pid>.log
//   debugstring - OutputDebugStringW only (viewable in DebugView)
//   all         - Both file and OutputDebugStringW
//
// Config file: <日志根>\<WIND_LOG_CONFIG_NAME>
//   mode=none
//   level=debug
//   dump_hotkey=0
//
// <日志根> 有两种，判据见 _BuildPaths，与 Rust 侧 `Config::log_dir()` 逐字对齐：
//   便携部署（安装根下有 portable_mode 标记）→ <安装根>\localdata\logs
//   其余                                    → %LOCALAPPDATA%\<WIND_LOG_DIR_NAME>\logs
// 便携版绝不可写 %LOCALAPPDATA%：日志带宿主进程名，会破坏「拔盘不留痕」。
//
// 每进程一个日志文件，无需跨进程同步；进程内仍要 _fileLock（TSF 输入线程与 CIPCClient
// 异步读线程都写日志）。5MB 轮转到 <前缀>.<宿主名>.<pid>.old.log。
//
// 支持外部改动日志文件（每秒自检，见 _ResyncFile）：删除文件或 tsf_log 目录一秒内自动重建；
// 截断到 0 则从头续写。
//
// 环形缓冲与 Ctrl+Shift+F12 导出由 dump_hotkey=1 打开，默认关是硬要求：该热键的拦截早于
// OnTestKeyDown / OnKeyDown 的所有闸门，开着时 Ctrl+Shift+F12 永远到不了宿主（VS / JetBrains
// 常用），还会往焦点处插入提示文本。CFileLogger::ReloadConfig 唯一调用点也挂在这个热键上，
// 关掉后即无重读入口；配置文件本就在取证时一次写好。
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
    // Also writes to the in-memory ring buffer when dump_hotkey is on.
    void Write(LogLevel level, const wchar_t* message);

    // Fast-path check: is logging enabled at this level?
    // Inlined for minimal overhead when mode=none
    bool IsEnabled(LogLevel level) const {
        return _mode != LogMode::None && level != LogLevel::Off && level <= _level;
    }

    // 环形缓冲与 Ctrl+Shift+F12 导出的同一个总开关（配置项 dump_hotkey，默认关）。
    // 两者必须共用：缓冲是导出的唯一数据源，只开一半要么在输入线程上白记，要么导出永远为空。
    bool IsDumpHotkeyEnabled() const { return _dumpHotkey; }

    // Write directly to ring buffer (bypasses mode/level checks)
    void WriteToRingBuffer(LogLevel level, const wchar_t* message);

    // Dump all ring buffer entries as a single wstring, then clear
    std::wstring DumpRingBuffer();

    // Accessors
    LogLevel GetLevel() const { return _level; }
    LogMode GetMode() const { return _mode; }
    void SetLevel(LogLevel level) { _level = level; }
    void SetMode(LogMode mode) { _mode = mode; }

    // 重新读取配置文件（mode/level）。DLL 在宿主内常驻，没有它改配置要退出宿主才生效。
    // 唯一调用点是 Ctrl+Shift+F12（见 KeyEventSink::OnKeyDown）。
    void ReloadConfig() { _ReadConfig(); }

private:
    CFileLogger();
    ~CFileLogger();

    CFileLogger(const CFileLogger&) = delete;
    CFileLogger& operator=(const CFileLogger&) = delete;

    // Read config from file
    void _ReadConfig();

    // Build log directory and file paths
    void _BuildPaths();

    // 写文件。持 _fileLock，真正的实现在 _WriteToFileLocked。
    void _WriteToFile(const char* utf8Line, int utf8Len);
    // 写文件的实现体。调用方须持 _fileLock（下面三个同）。
    void _WriteToFileLocked(const char* utf8Line, int utf8Len);

    // Write to OutputDebugStringW（不碰 _hFile/_written，无需持锁）
    void _WriteToDebugString(LogLevel level, const wchar_t* message);
    // 打开常开的追加句柄；失败返回 nullptr。顺带把已有文件大小计入 _written。
    HANDLE _OpenLogFile();
    // 与磁盘真实状态对表：文件被删则重开，被外部清空则校正 _written。每秒最多一次。
    void _ResyncFile();
    // 立即轮转（关句柄 → 改名到 .old → 重开）。本进程独占文件，无需与他人协调。
    void _RotateNow();

    // Format timestamp
    static void _FormatTimestamp(wchar_t* buf, size_t bufSize);

    // Level to string
    static const wchar_t* _LevelStr(LogLevel level);

    LogMode _mode;
    LogLevel _level;
    bool    _initialized;
    // 环形缓冲 + Ctrl+Shift+F12 导出总开关。见 IsDumpHotkeyEnabled。
    bool    _dumpHotkey;
    // 常开的追加句柄。别改回每行开关文件：那在 TSF 输入线程上每行要多两个数量级耗时
    // （主要是 Defender 逐次扫描），成分见 FileLogger.cpp 的 _WriteToFile。
    HANDLE  _hFile;
    // 文件段的进程内锁，保护 _hFile / _written / _lastCheckTick 三者。
    // TSF 输入线程与 CIPCClient::_AsyncReaderThread 都会走 Write()；_ResyncFile/_RotateNow 会
    // CloseHandle(_hFile)，无锁时另一线程的 WriteFile 可能写进句柄值被复用的别的内核对象。
    // 与 _ringLock 分开：环形缓冲不受 mode 控制（mode=none 也写），不该被文件 I/O 挡住。
    CRITICAL_SECTION _fileLock;
    // 已写入字节数，用于轮转判定。本进程独占该文件，故是精确值，无需每行查文件大小。
    DWORD   _written;
    // 上次与磁盘对表的时刻（GetTickCount），见 _ResyncFile。按时间节流，自愈延迟与日志频率无关。
    DWORD   _lastCheckTick;
    DWORD   _pid;
    // `...\logs`：配置文件所在目录（日志文件已下沉到 _fileDir）
    wchar_t _logDir[MAX_PATH];
    // `...\logs\tsf_log`：日志文件所在目录，与 core 的主日志分开
    wchar_t _fileDir[MAX_PATH];
    wchar_t _logPath[MAX_PATH];
    wchar_t _oldPath[MAX_PATH];
    wchar_t _configPath[MAX_PATH];

    // Ring buffer for in-memory log capture（受 _dumpHotkey 门控，默认不记录）
    static constexpr int RING_BUFFER_LINES = 200;
    static constexpr int RING_LINE_MAX = 256;
    wchar_t _ringBuffer[RING_BUFFER_LINES][RING_LINE_MAX];
    int _ringHead;   // Next write position
    int _ringCount;  // Total entries written (capped at RING_BUFFER_LINES)
    CRITICAL_SECTION _ringLock;

    static constexpr DWORD MAX_LOG_SIZE = 5 * 1024 * 1024; // 5MB
    // 自检间隔。日志文件被外部删掉后最多丢这么久的日志。
    static constexpr DWORD FILE_CHECK_INTERVAL_MS = 1000;
    // 宿主进程名在文件名里的截断长度（超长的 exe 名不该把路径顶到 MAX_PATH）。
    static constexpr size_t HOST_NAME_MAX = 48;
};
