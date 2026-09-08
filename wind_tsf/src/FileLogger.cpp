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
    , _dumpHotkey(false)
    , _hFile(nullptr)
    , _written(0)
    , _lastCheckTick(0)
    , _pid(0)
    , _ringHead(0)
    , _ringCount(0)
{
    _logDir[0] = L'\0';
    _fileDir[0] = L'\0';
    _logPath[0] = L'\0';
    _configPath[0] = L'\0';
    memset(_ringBuffer, 0, sizeof(_ringBuffer));
    InitializeCriticalSection(&_ringLock);
    // 必须在 Init 之前就绪：Init 末尾会 Write 一条启动标记，那条已经要持锁。
    // InitializeCriticalSection 只分配内核对象、不加载 DLL，loader lock 下可用。
    InitializeCriticalSection(&_fileLock);
}

CFileLogger::~CFileLogger()
{
    Shutdown();
    DeleteCriticalSection(&_fileLock);
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

    // Ensure log directory exists（配置文件就在这一层；日志文件的子目录留到真要写时再建，
    // 见 _OpenLogFile —— mode=none 时不该给每个宿主进程都平白造一个空目录）
    CreateDirectoryW(_logDir, nullptr);

    // Read config (mode + level)
    _ReadConfig();

    // If mode is none, skip file handle creation entirely
    if (_mode == LogMode::None)
        return;

    // 打开**常开**的追加句柄。
    //
    // ⚠ 本函数跑在 `DllMain(DLL_PROCESS_ATTACH)` 里，即 loader lock 之下。这里只做内核
    // 对象操作（开文件），**不能**加载别的 DLL、创建线程或等待同步对象——旧日志的清理
    // 因此不放在 DLL 侧，交给 core 服务启动时做。
    //
    // 句柄常开是主要收益：此前每行日志都要开关一次文件，而 Defender 在每次文件打开时
    // 插一次扫描，实测单次 CreateFile+CloseHandle 就是 203μs（详见 _WriteToFile 的实测表）。
    //
    // 顺带不再需要互斥锁：日志文件已按 pid 拆开（见 _BuildPaths），本进程独占该文件，
    // 没有可竞争的对象。那把 `Local\WindInput*TSFLogMutex` 本身很便宜（无竞争 293ns），
    // 它的害处是把上面那 246μs 的临界区跨进程串行化了。
    if (_mode == LogMode::File || _mode == LogMode::All)
    {
        // 此刻本 DLL 还没有第二个线程（异步读线程要等 CIPCClient 起来才建），
        // 持锁纯粹是为了让「碰 _hFile 必持 _fileLock」没有例外——例外正是日后
        // 被人照抄的那一条。
        EnterCriticalSection(&_fileLock);
        _hFile = _OpenLogFile();
        LeaveCriticalSection(&_fileLock);
        if (_hFile == nullptr)
        {
            OutputDebugStringW(L"[WindInput][FileLogger] Failed to open log file, file logging disabled\n");
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

    // 关句柄同样持锁：宿主退出时异步读线程未必已经停稳，它那边可能还在 WriteFile。
    EnterCriticalSection(&_fileLock);
    if (_hFile != nullptr)
    {
        CloseHandle(_hFile);
        _hFile = nullptr;
    }
    LeaveCriticalSection(&_fileLock);

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

// 文件段的进程内串行化。实现体拆成 _WriteToFileLocked 而不是在原函数里 Enter/Leave：
// 那里面有四处提前 return，逐个配 Leave 迟早漏一个，而漏掉的表现是整个日志子系统死锁。
void CFileLogger::_WriteToFile(const char* utf8Line, int utf8Len)
{
    EnterCriticalSection(&_fileLock);
    _WriteToFileLocked(utf8Line, utf8Len);
    LeaveCriticalSection(&_fileLock);
}

void CFileLogger::_WriteToFileLocked(const char* utf8Line, int utf8Len)
{
    if (_hFile == nullptr)
        return;

    // 文件可能在我们眼皮底下被外部删除（手动清日志、core 回收过期日志）。Windows 删掉的
    // 是**目录里的名字**，文件对象本身还被我们的句柄吊着，于是 WriteFile 照样返回成功，
    // 每一行都静默写进一个谁也看不见的幽灵文件——不主动发现就永远发现不了。
    //
    // 节流用 GetTickCount：它读的是内核共享的用户页，**不是系统调用**，可以每行都问；
    // 真正的查询每秒最多一次，与日志频率无关。安静的宿主同样能在一秒内接回来——而它恰恰
    // 是最容易被 core 按修改时间当成过期文件删掉的那类。
    DWORD now = GetTickCount();
    if (now - _lastCheckTick >= FILE_CHECK_INTERVAL_MS)
    {
        _lastCheckTick = now;
        _ResyncFile();
        if (_hFile == nullptr)
            return; // 重开失败，本行丢弃；下次写入会再试一次
    }

    // 常规路径只剩**一次 WriteFile**。
    //
    // 此前这里是：抢跨进程互斥锁 → GetFileAttributesExW 查大小 → CreateFileW →
    // WriteFile → CloseHandle → ReleaseMutex，全在 TSF 输入线程上。
    //
    // 实测各成分（Win11 + Defender 实时保护开启，单进程无竞争，ns/行）：
    //     互斥锁 抢+放                 293
    //     GetFileAttributesExW      43,167
    //     CreateFileW + CloseHandle 203,426
    //     合计                     230,695   →  现在 3,579
    //
    // ⚠ 别把账算在锁上：无竞争时它只占 0.1%。真正的开销是**每行开一次文件**——
    // Defender 会在每次文件打开时插一次扫描，200μs 由此而来。那把**跨进程**互斥量的
    // 害处是放大：246μs 的临界区被它跨进程串行化，N 个宿主同时写就是 N×246μs 的排队。
    // 所以「拆文件」才是主要收益。
    //
    // ⚠⚠ 但「不需要跨进程锁」≠「不需要锁」：进程内仍有两个线程走这条路（见
    // _fileLock 的注释）。现在这把 CRITICAL_SECTION 只在本进程内串行化 3.6μs 的
    // 临界区，与被删掉的那把不是同一回事，别再顺手把它也当成历史包袱删掉。
    DWORD written = 0;
    if (!WriteFile(_hFile, utf8Line, (DWORD)utf8Len, &written, nullptr))
        return;

    // 轮转判定用**累计字节数**，不再查文件大小：本进程独占该文件，写了多少自己最清楚。
    // 溢出保护：_written 是 DWORD，MAX_LOG_SIZE 只有 5MB，正常路径下轮转会先发生；
    // 但若某次 WriteFile 异常地大，饱和累加可避免绕回后长期不轮转。
    if (_written > MAXDWORD - written)
        _written = MAXDWORD;
    else
        _written += written;

    if (_written >= MAX_LOG_SIZE)
        _RotateNow();
}

// 打开常开的追加句柄。失败返回 nullptr（而非 INVALID_HANDLE_VALUE，省得每个调用点
// 各判一次哨兵值）。**调用方须持 _fileLock**（会写 _written / _lastCheckTick）。
//
// FILE_APPEND_DATA 而非 GENERIC_WRITE：追加语义由内核保证，不必自己维护写指针。
// FILE_SHARE_READ 让排查时能一边跑一边 tail 日志；FILE_SHARE_DELETE 让文件在被打开
// 的状态下仍可被改名或删除——core 侧清理旧日志时不会因为某个宿主还开着而失败。
HANDLE CFileLogger::_OpenLogFile()
{
    // 目录可能还不存在：core 未必启动过，用户也可能刚清理过日志目录。
    // CreateDirectoryW **不建中间层**，故父子各建一次；已存在时返回 FALSE，忽略即可。
    // 放在这里而不是 Init：轮转重开也走这条路，删了目录仍能自愈。
    CreateDirectoryW(_logDir, nullptr);
    CreateDirectoryW(_fileDir, nullptr);

    HANDLE h = CreateFileW(
        _logPath,
        FILE_APPEND_DATA,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        nullptr,
        OPEN_ALWAYS,
        FILE_ATTRIBUTE_NORMAL,
        nullptr
    );
    if (h == INVALID_HANDLE_VALUE)
        return nullptr;

    // 续写已有文件时把已有大小计入，否则重启宿主会让轮转阈值从零重新开始数，
    // 文件可以长到远超 MAX_LOG_SIZE。
    LARGE_INTEGER size;
    _written = GetFileSizeEx(h, &size) && size.QuadPart < MAXDWORD
        ? (DWORD)size.QuadPart
        : 0;
    // 刚对过表，自检窗口从现在起算（省掉开完就立刻再查一次）
    _lastCheckTick = GetTickCount();
    return h;
}

// 与磁盘上的真实状态对一次表。**调用方须持 _fileLock**——本函数会 CloseHandle(_hFile)
// 并重开，与别的线程的 WriteFile 撞上就是往已关闭（且可能已被复用）的句柄里写。
//
// 一次 GetFileInformationByHandleEx 同时办三件事：
// 认出文件被删（重开）、认出文件被外部清空（校正累计字节数）、拿到真实大小。
//
// 判据实测（Win11/NTFS，见提交说明）：外部删除后 `NumberOfLinks` 归零 **且**
// `DeletePending` 置位。前者对应现代的 POSIX 删除语义（名字立即从目录消失），后者对应
// 经典语义（名字留到最后一个句柄关闭）；两个一起判，两种语义都盖住。
void CFileLogger::_ResyncFile()
{
    FILE_STANDARD_INFO info;
    if (!GetFileInformationByHandleEx(_hFile, FileStandardInfo, &info, sizeof(info)))
        return; // 查不到就维持原状——不值得为一次查询失败把日志关掉

    if (info.NumberOfLinks == 0 || info.DeletePending)
    {
        // 已成幽灵。丢掉旧句柄按原路径重开，顺带把可能被一起删掉的目录建回来
        // （_OpenLogFile 里有父子两次 CreateDirectoryW）。幽灵里的内容找不回来，
        // 但那本来就是被人主动删掉的。
        CloseHandle(_hFile);
        _hFile = _OpenLogFile();
        return;
    }

    // 没被删，但可能被**清空**——排查时截断日志是常规操作。追加句柄天然从新的文件末尾
    // 写起（实测内容干净、无 NUL 空洞），不需要做什么；只有 _written 会停在旧的累计值上
    // 导致轮转提前触发，所以这里以真实大小为准。
    _written = info.EndOfFile.QuadPart < (LONGLONG)MAXDWORD
        ? (DWORD)info.EndOfFile.QuadPart
        : MAXDWORD;
}

// 立即轮转：关句柄 → 覆盖式改名到 .old → 重开。
//
// 只有本进程持有这个文件，所以不需要与**别的进程**协调——这正是「每进程一个文件」
// 换来的简化。此前的 _RotateIfNeeded 要在共享文件上做，还得先靠跨进程互斥量排他。
//
// **调用方须持 _fileLock**：进程内的另一个日志线程仍要挡住，理由同 _ResyncFile。
void CFileLogger::_RotateNow()
{
    if (_hFile != nullptr)
    {
        CloseHandle(_hFile);
        _hFile = nullptr;
    }
    DeleteFileW(_oldPath);
    MoveFileW(_logPath, _oldPath);
    _hFile = _OpenLogFile();
    // 重开失败则文件日志就此停摆（_hFile 为 nullptr，后续 _WriteToFile 直接返回）。
    // 不改 _mode：DebugString 那一路与文件无关，不该被文件问题连累关掉。
}

void CFileLogger::_BuildPaths()
{
    wchar_t appData[MAX_PATH];
    if (FAILED(SHGetFolderPathW(nullptr, CSIDL_LOCAL_APPDATA, nullptr, 0, appData)))
        return;

    _snwprintf_s(_logDir, _countof(_logDir), _TRUNCATE,
        L"%ls\\" WIND_LOG_DIR_NAME L"\\logs", appData);

    // 日志文件再下沉一层到 `logs\tsf_log\`：按进程拆开后文件数是「用过的宿主 × pid」
    // 量级，跟 core 的 wind_input.log 平铺在一起会把主日志淹掉。
    _snwprintf_s(_fileDir, _countof(_fileDir), _TRUNCATE,
        L"%ls\\" WIND_LOG_SUBDIR_NAME, _logDir);

    // 日志文件**每进程一个**：`wind_tsf.<宿主名>.<pid>.log`。
    //
    // TSF DLL 被每个宿主进程各加载一份，此前它们共写一个文件，于是既要靠一把跨进程互斥锁
    // 串行化，也没法常开句柄（别人要写同一个文件）。拆开之后进程间没有共享资源，句柄可以
    // 常开，**跨进程**那把锁不再需要——真正省下的是「每行开关文件」那 246μs，见
    // _WriteToFile 的实测表。进程**内**的线程同步是另一回事，仍由 _fileLock 承担。
    //
    // 带宿主名是为了排查时一眼认出是谁（wind_tsf.feishu.12345.log）；**还要带 pid**，
    // 因为同一个程序可以多开，Chrome 那类多进程宿主更是一开就是一串——只带名字会撞回
    // 共享，跨进程那把锁也就白去了。
    wchar_t hostExe[MAX_PATH] = {};
    wchar_t hostName[HOST_NAME_MAX + 1];
    wcscpy_s(hostName, L"host");
    if (GetModuleFileNameW(nullptr, hostExe, _countof(hostExe)) > 0)
    {
        const wchar_t* base = wcsrchr(hostExe, L'\\');
        base = base ? base + 1 : hostExe;
        size_t n = 0;
        // 取文件名主干，且**逐字符白名单**：进程名要进文件路径，混进路径分隔符或通配符
        // 会把日志写去别的目录，也会让 core 侧按前缀清理时匹配到意外的文件。
        for (const wchar_t* c = base; *c != L'\0' && *c != L'.' && n < HOST_NAME_MAX; ++c)
        {
            if ((*c >= L'a' && *c <= L'z') || (*c >= L'A' && *c <= L'Z') ||
                (*c >= L'0' && *c <= L'9') || *c == L'_' || *c == L'-')
            {
                hostName[n++] = *c;
            }
        }
        if (n > 0)
            hostName[n] = L'\0';
    }

    _snwprintf_s(_logPath, _countof(_logPath), _TRUNCATE,
        L"%ls\\" WIND_LOG_FILE_PREFIX L".%ls.%lu.log", _fileDir, hostName, _pid);

    _snwprintf_s(_oldPath, _countof(_oldPath), _TRUNCATE,
        L"%ls\\" WIND_LOG_FILE_PREFIX L".%ls.%lu.old.log", _fileDir, hostName, _pid);

    _snwprintf_s(_configPath, _countof(_configPath), _TRUNCATE,
        L"%ls\\" WIND_LOG_CONFIG_NAME, _logDir);
}

void CFileLogger::_ReadConfig()
{
    // Default: mode=none, level=info, dump_hotkey=off
    //
    // ⛔ _dumpHotkey 必须在这里也归零：本函数同时是 ReloadConfig 的实现，
    // 用户删掉配置行后重读，开关要跟着落回关——「打开容易关不掉」的开关等于没有开关。
    _mode = LogMode::None;
    _level = LogLevel::Info;
    _dumpHotkey = false;

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
            else if (_stricmp(key, "dump_hotkey") == 0)
            {
                // 环形缓冲 + Ctrl+Shift+F12 导出总开关。只认显式的真值，
                // 其余（含拼错的值）一律留在关的一侧——这个开关抢的是全局按键。
                _dumpHotkey = (_stricmp(val, "1") == 0 || _stricmp(val, "true") == 0
                               || _stricmp(val, "on") == 0 || _stricmp(val, "yes") == 0);
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
    // 开关关着就一个字都不记：没有出口的缓冲只是在 TSF 输入线程上白付临界区 + 拷贝。
    if (!_dumpHotkey || message == nullptr)
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
