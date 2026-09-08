#pragma once

#include <windows.h>
#include <cstdint>
#include <string>

// Log path macros for debug variant coexistence
//
// 变体隔离**只做在目录一层**：dev 与正式版的数据目录本就分开
// （`WindInputDev` / `WindInput`），目录内的文件名再带 `_dev` 是重复隔离，
// 只会让两版本的排查步骤（找哪个文件、写哪个配置）无谓地不一致。
// 故文件名与配置名两版本一律同名，切换变体时排查命令原样可用。
#ifdef WIND_DEV_VARIANT
#define WIND_LOG_DIR_NAME       L"WindInputDev"
#else
#define WIND_LOG_DIR_NAME       L"WindInput"
#endif
// 日志文件名前缀。实际文件是 `<前缀>.<宿主名>.<pid>.log`（轮转产物再加 `.old`）——
// 每进程一个，见 CFileLogger::_BuildPaths。core 侧清理旧日志时按这个前缀匹配。
#define WIND_LOG_FILE_PREFIX    L"wind_tsf"
// TSF 日志的**专属子目录**（`logs\tsf_log\`）。日志按进程拆开后，文件数是
// 「用过的宿主 × pid」量级，跟 core 的 wind_input.log 混在一层会把主日志淹掉。
#define WIND_LOG_SUBDIR_NAME    L"tsf_log"
// 配置文件仍留在 `logs\` 下，**不跟着进子目录**：它是用户手工创建的日志总开关，
// 路径散见于各 AGENTS.md 与排查步骤；跟着搬会让存量用户的开关静默失效。
#define WIND_LOG_CONFIG_NAME    L"tsf_log_config"

// ============================================================================
// FileLogger - Multi-process safe logging for TSF DLL
//
// Output modes (controlled by config file):
//   none        - No output (default, near-zero overhead)
//   file        - Write to %LOCALAPPDATA%\<WIND_LOG_DIR_NAME>\logs\tsf_log\<前缀>.<宿主名>.<pid>.log
//   debugstring - OutputDebugStringW only (viewable in DebugView)
//   all         - Both file and OutputDebugStringW
//
// Config file: %LOCALAPPDATA%\<WIND_LOG_DIR_NAME>\logs\<WIND_LOG_CONFIG_NAME>
//   mode=none
//   level=debug
//
// Multi-process safety: 每进程一个日志文件，本进程独占 → 无需任何跨进程同步
// Thread safety:  进程**内**仍需同步（_fileLock）——至少两个线程会写日志：TSF 输入线程
//                 与 CIPCClient 的异步读线程。见 _fileLock 的注释。
// Auto-rotation: 5MB max, rotates to <前缀>.<宿主名>.<pid>.old.log
//
// 外部改动日志文件是**受支持**的（每秒自检一次，见 _ResyncFile）：
//   删除文件或整个 tsf_log 目录 → 一秒内自动重建，不必重启宿主
//   清空文件（截断到 0）        → 直接从头续写，不留空洞；排查时可随时截断
//
// Ring Buffer + Ctrl+Shift+F12 导出：**默认整体关闭**，由配置项 dump_hotkey 打开。
//
//   dump_hotkey=1        # 环形缓冲开始记录，且 Ctrl+Shift+F12 被 TSF 吃下用于导出
//
// ⛔ 默认关是硬要求，不是省开销的优化。这个热键的拦截位于 OnTestKeyDown / OnKeyDown
//    的**所有闸门之前**（早于 IsKeyboardDisabled、密码框抑制、只读上下文），所以只要
//    本输入法处于激活态，Ctrl+Shift+F12 就永远到不了宿主——而它是 VS / JetBrains 等
//    宿主的常用快捷键，且我们的处理还会往焦点处插入一行提示文本。默认开等于向所有用户
//    收取一个他们既不知道也用不上的按键。取证时让用户临时写一行配置即可。
//
// ⚠ 关掉后 CFileLogger::ReloadConfig 也就失去了唯一调用点（那也挂在这个热键上）。这不是
//    遗漏：配置文件本就是取证时手工创建的，建它的那次把 mode / level / dump_hotkey 一起
//    写好，之后再改才需要重读。
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

    // 环形缓冲与 Ctrl+Shift+F12 导出的**同一个**总开关（配置项 dump_hotkey，默认关）。
    //
    // ★ 缓冲与导出必须共用一个开关：缓冲是导出的唯一数据源，导出是缓冲的唯一出口。
    // 只关一半，剩下的那半要么白记（每条 INFO 日志都进临界区拷 256 字符，且发生在
    // TSF 输入线程上），要么按下去永远是空的。
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

    // 重新读取配置文件（mode/level）。
    //
    // 提供这个入口是因为 DLL 在宿主进程内常驻、构造函数只跑一次：没有它，改完
    // 日志配置必须完全退出宿主才生效，而取证时这是最高频的操作。
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
    // 写文件的实现体。**调用方须持 _fileLock**（下面三个同）。
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
    // 常开的追加句柄。此前是「每行 CreateFile/WriteFile/CloseHandle 各一次 + 抢一把
    // 跨进程互斥锁」，而这一切都发生在 **TSF 输入线程**上，实测 230μs/行（现在 3.6μs，
    // 成分表见 FileLogger.cpp 的 _WriteToFile）。大头是每行开关文件被 Defender 逐次扫描，
    // 不是那把锁；锁只是把这段临界区跨进程串行化，让多宿主同时写时雪上加霜。
    HANDLE  _hFile;
    // 文件段的进程内锁，保护 _hFile / _written / _lastCheckTick 三者。
    //
    // ★ 「每进程一个文件」消掉的是**跨进程**同步，不是跨线程的：至少两个线程会走
    // Write() —— TSF 输入线程（每次按键）与 CIPCClient::_AsyncReaderThread（每收一条
    // 推送就 _LogDebug）。此前那把命名互斥量同时兼着这两个职责，删它时只论证了跨进程
    // 那半边，跨线程这半边就此裸奔：_ResyncFile/_RotateNow 会 CloseHandle(_hFile)，
    // 而另一个线程可能正停在 WriteFile(_hFile, ...) 上——Windows 的句柄值会被激进复用，
    // 最坏情形是日志字节写进另一个刚被打开的内核对象。
    //
    // 与 _ringLock 分开：环形缓冲恒开（mode=none 也写），不该被文件 I/O 挡住。
    // 无竞争的 EnterCriticalSection 是 20-50ns 量级，对照现在 3.6μs/行可以忽略。
    CRITICAL_SECTION _fileLock;
    // 已写入字节数，用于轮转判定。**精确而非估算**：本进程独占该文件，没有别人往里写。
    // 此前每行都要 GetFileAttributesExW 查一次真实大小，现在一次系统调用都不需要。
    DWORD   _written;
    // 上次与磁盘对表的时刻（GetTickCount）。见 _ResyncFile —— 外部删除/清空只能主动
    // 发现，而按**时间**节流的好处是自愈延迟与日志频率无关：安静的宿主同样一秒内接回来。
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
