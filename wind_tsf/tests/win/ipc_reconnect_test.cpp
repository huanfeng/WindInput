// CIPCClient 服务端重启自愈的集成测试（Windows 目标，链接真实 IPCClient.cpp）。
//
// 场景：服务端重启后，客户端手里的主管道句柄是旧连接（IsConnected() 仍为真），
// 下一次发送会以 ERROR_NO_DATA 失败。要求：
//   - 陈旧句柄上的发送自愈（断开 → 重连 → 重写一次），服务端恰好收到一次；
//   - 陈旧失败不计入熔断；
//   - DiscardStaleConnection 只丢已断的连接，不碰活连接；
//   - 服务端彻底不在时返回 FALSE，不挂死、不无限重试。
//
// 管道名由本文件的 WindPipeName() 桩提供（含 pid 的测试专用名），绝不会连到真实服务；
// WindResolveInstallRoot 桩恒失败，_StartService 因此拉不起任何进程。
//
// 跑法：make -C wind_tsf/tests/win run（交叉编译后用 wine 执行），或把 exe 拷到 Windows 上直接跑。
// -DIPC_TEST_LEGACY：去掉对新接口的调用，用于在改前代码上复现。

#include "IPCClient.h"
#include "FileLogger.h"
#include "InstallPaths.h"

#include <atomic>
#include <cstdio>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

// ============================================================================
// 链接桩：替代 Globals.cpp / FileLogger.cpp / InstallPaths.cpp 里 IPCClient.cpp 用到的符号
// ============================================================================

namespace
{
std::wstring g_pipeName;
std::wstring g_pushPipeName;
} // namespace

const wchar_t* WindPipeName() { return g_pipeName.c_str(); }
const wchar_t* WindPushPipeName() { return g_pushPipeName.c_str(); }

BOOL WindResolveInstallRoot(WCHAR*, DWORD) { return FALSE; }
BOOL WindQueryCurrentProcessInfo(WindHostProcessInfo*) { return FALSE; }
void WindLogHostProcessInfo(int, const wchar_t*, const WindHostProcessInfo&) {}
void WindLogForegroundProcessInfo(int, const wchar_t*) {}

CFileLogger::CFileLogger() : _mode(LogMode::DebugString), _level(LogLevel::Info) {}
CFileLogger::~CFileLogger() {}
CFileLogger& CFileLogger::Instance()
{
    static CFileLogger s;
    return s;
}
void CFileLogger::Write(LogLevel level, const wchar_t* message)
{
    static std::mutex mu;
    std::lock_guard<std::mutex> lock(mu);
    fwprintf(stderr, L"    [log %d] %ls\n", (int)level, message);
}
void CFileLogger::WriteToRingBuffer(LogLevel, const wchar_t*) {}

// ============================================================================
// 测试用命名管道服务端：最小协议（同步命令回 ACK，异步命令不回）
// ============================================================================

namespace
{

struct Received
{
    uint16_t command;
    bool async;
};

class TestServer
{
public:
    void Start()
    {
        _stop = CreateEventW(nullptr, TRUE, FALSE, nullptr);
        _listener = std::thread([this] { _Listen(); });
        // 等首个监听实例挂上，否则客户端 WaitNamedPipe 会直接 FILE_NOT_FOUND。
        for (int i = 0; i < 200 && !_listening.load(); i++)
            Sleep(5);
    }

    // 模拟服务进程退出：关掉所有实例（含已连上的），客户端那端随之成为陈旧句柄。
    void Stop()
    {
        SetEvent(_stop);
        if (_listener.joinable())
            _listener.join();
        for (auto& t : _clients)
            if (t.joinable())
                t.join();
        _clients.clear();
        CloseHandle(_stop);
        _stop = nullptr;
        _listening = false;
    }

    void Restart()
    {
        Stop();
        Sleep(50);
        ClearReceived();
        Start();
    }

    // 等异步消息落地（服务端线程记账与客户端返回之间有先后）。
    int CountAfterSettle(uint16_t command)
    {
        Sleep(100);
        std::lock_guard<std::mutex> lock(_mu);
        int n = 0;
        for (const auto& r : _received)
            if (r.command == command)
                n++;
        return n;
    }

    void ClearReceived()
    {
        std::lock_guard<std::mutex> lock(_mu);
        _received.clear();
    }

private:
    void _Listen()
    {
        for (;;)
        {
            HANDLE h = CreateNamedPipeW(g_pipeName.c_str(), PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
                                        PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
                                        PIPE_UNLIMITED_INSTANCES, 64 * 1024, 64 * 1024, 0, nullptr);
            if (h == INVALID_HANDLE_VALUE)
            {
                fwprintf(stderr, L"    [server] CreateNamedPipe failed: %lu\n", GetLastError());
                return;
            }
            HANDLE ev = CreateEventW(nullptr, TRUE, FALSE, nullptr);
            OVERLAPPED ov = {};
            ov.hEvent = ev;
            _listening = true;
            BOOL connected = ConnectNamedPipe(h, &ov);
            DWORD err = GetLastError();
            if (!connected && err == ERROR_IO_PENDING)
            {
                HANDLE waits[2] = {ev, _stop};
                DWORD w = WaitForMultipleObjects(2, waits, FALSE, INFINITE);
                if (w != WAIT_OBJECT_0)
                {
                    CancelIo(h);
                    CloseHandle(ev);
                    CloseHandle(h);
                    return;
                }
                DWORD dummy = 0;
                connected = GetOverlappedResult(h, &ov, &dummy, FALSE);
            }
            else if (!connected && err == ERROR_PIPE_CONNECTED)
            {
                connected = TRUE;
            }
            CloseHandle(ev);
            if (!connected)
            {
                CloseHandle(h);
                continue;
            }
            // 与真实服务一样：先让下一个监听实例挂上，再服务本连接。
            _clients.emplace_back([this, h] { _Serve(h); });
        }
    }

    void _Serve(HANDLE h)
    {
        HANDLE ev = CreateEventW(nullptr, TRUE, FALSE, nullptr);
        std::vector<uint8_t> buf(64 * 1024);
        for (;;)
        {
            OVERLAPPED ov = {};
            ov.hEvent = ev;
            ResetEvent(ev);
            DWORD n = 0;
            BOOL ok = ReadFile(h, buf.data(), (DWORD)buf.size(), &n, &ov);
            if (!ok && GetLastError() == ERROR_IO_PENDING)
            {
                HANDLE waits[2] = {ev, _stop};
                if (WaitForMultipleObjects(2, waits, FALSE, INFINITE) != WAIT_OBJECT_0)
                {
                    CancelIo(h);
                    break;
                }
                ok = GetOverlappedResult(h, &ov, &n, FALSE);
            }
            if (!ok || n < sizeof(IpcHeader))
                break;
            IpcHeader hdr;
            memcpy(&hdr, buf.data(), sizeof(hdr));
            const bool async = (hdr.version & ASYNC_FLAG) != 0;
            {
                std::lock_guard<std::mutex> lock(_mu);
                _received.push_back({hdr.command, async});
            }
            if (!async)
            {
                IpcHeader ack = {PROTOCOL_VERSION, CMD_ACK, 0};
                OVERLAPPED wov = {};
                wov.hEvent = ev;
                ResetEvent(ev);
                DWORD written = 0;
                if (!WriteFile(h, &ack, sizeof(ack), &written, &wov) && GetLastError() == ERROR_IO_PENDING)
                    GetOverlappedResult(h, &wov, &written, TRUE);
            }
        }
        CloseHandle(ev);
        CloseHandle(h);
    }

    HANDLE _stop = nullptr;
    std::thread _listener;
    std::vector<std::thread> _clients;
    std::atomic<bool> _listening{false};
    std::mutex _mu;
    std::vector<Received> _received;
};

int g_failures = 0;
int g_checks = 0;

void Expect(bool cond, const char* what)
{
    g_checks++;
    if (!cond)
    {
        g_failures++;
        printf("  FAIL: %s\n", what);
    }
    else
    {
        printf("  ok:   %s\n", what);
    }
    fflush(stdout);
}

// 建好连接并在其上成功发过一次（模拟重启前一直在用的旧连接）。
void Warmup(CIPCClient& client, TestServer& server)
{
    Expect(client.SendFocusGained() == TRUE, "warmup: focus_gained on live connection");
    Expect(server.CountAfterSettle(CMD_FOCUS_GAINED) == 1, "warmup: server got focus_gained once");
    server.ClearReceived();
}

void TestFocusGainedAfterRestart()
{
    printf("[focus_gained after restart]\n");
    TestServer server;
    server.Start();
    CIPCClient client;
    Warmup(client, server);

    server.Restart();
    Expect(client.IsConnected() == TRUE, "stale handle still looks connected");
    Expect(client.SendFocusGained() == TRUE, "focus_gained self-heals on stale handle");
    Expect(server.CountAfterSettle(CMD_FOCUS_GAINED) == 1, "new server got focus_gained exactly once");
    Expect(client.GetCircuitState() == CircuitState::Closed, "circuit stays closed");
    server.Stop();
}

void TestImeActivatedAfterRestart()
{
    printf("[ime_activated after restart]\n");
    TestServer server;
    server.Start();
    CIPCClient client;
    Warmup(client, server);

    server.Restart();
    Expect(client.SendIMEActivated() == TRUE, "ime_activated self-heals on stale handle");
    Expect(server.CountAfterSettle(CMD_IME_ACTIVATED) == 1, "new server got ime_activated exactly once");
    Expect(client.StateEstablished() == TRUE, "state established on the new connection");
    server.Stop();
}

void TestKeyEventAfterRestart()
{
    printf("[key_event after restart]\n");
    TestServer server;
    server.Start();
    CIPCClient client;
    Warmup(client, server);

    server.Restart();
    Expect(client.SendKeyEvent('A', 0x1E, 0, 0) == TRUE, "key_event self-heals on stale handle");
    ServiceResponse resp;
    Expect(client.ReceiveResponse(resp) == TRUE, "key_event response read on the new connection");
    Expect(server.CountAfterSettle(CMD_KEY_EVENT) == 1, "new server got key_event exactly once");
#ifndef IPC_TEST_LEGACY
    Expect(client.StateEstablished() == TRUE, "key traffic on the new connection marks state established");
#endif
    server.Stop();
}

void TestRepeatedRestartsKeepCircuitClosed()
{
    printf("[repeated restarts do not trip circuit breaker]\n");
    TestServer server;
    server.Start();
    CIPCClient client;
    Warmup(client, server);

    int ok = 0;
    for (int i = 0; i < IPCConfig::MAX_CONSECUTIVE_FAILURES + 1; i++)
    {
        server.Restart();
        if (client.SendFocusGained())
            ok++;
    }
    Expect(ok == IPCConfig::MAX_CONSECUTIVE_FAILURES + 1, "every send after each restart succeeded");
    Expect(client.GetCircuitState() == CircuitState::Closed, "circuit stays closed");
    server.Stop();
}

#ifndef IPC_TEST_LEGACY
void TestDiscardStaleConnection()
{
    printf("[DiscardStaleConnection]\n");
    TestServer server;
    server.Start();
    CIPCClient client;
    Warmup(client, server);

    Expect(client.DiscardStaleConnection() == FALSE, "live connection is kept");
    Expect(client.IsConnected() == TRUE, "still connected after probing a live connection");

    server.Restart();
    Expect(client.DiscardStaleConnection() == TRUE, "stale connection is dropped");
    Expect(client.IsConnected() == FALSE, "not connected after drop");
    Expect(client.SendFocusGained() == TRUE, "next send connects fresh");
    Expect(client.NeedsStateSync() == TRUE, "fresh connection asks for full state sync");
    Expect(server.CountAfterSettle(CMD_FOCUS_GAINED) == 1, "server got focus_gained once");
    server.Stop();
}
#endif

void TestServiceGone()
{
    printf("[service gone: fail fast, bounded]\n");
    TestServer server;
    server.Start();
    CIPCClient client;
    Warmup(client, server);
    server.Stop();

    const DWORD t0 = GetTickCount();
    Expect(client.SendFocusGained() == FALSE, "focus_gained returns FALSE when service is gone");
    const DWORD first = GetTickCount() - t0;
    Expect(client.IsConnected() == FALSE, "handle dropped after failure");
    int falseCount = 0;
    for (int i = 0; i < 5; i++)
        if (!client.SendKeyEvent('A', 0x1E, 0, 0))
            falseCount++;
    const DWORD total = GetTickCount() - t0;
    printf("  (first send %lums, six sends %lums)\n", first, total);
    Expect(falseCount == 5, "key_event keeps returning FALSE");
    Expect(total < 5000, "six failing sends finish within 5s (no hang, no unbounded retry)");
    Expect(client.GetCircuitState() == CircuitState::Open, "circuit opens after repeated real failures");
}

} // namespace

int main()
{
    wchar_t name[128];
    swprintf(name, 128, L"\\\\.\\pipe\\wind_tsf_ipctest_%lu", GetCurrentProcessId());
    g_pipeName = name;
    swprintf(name, 128, L"\\\\.\\pipe\\wind_tsf_ipctest_push_%lu", GetCurrentProcessId());
    g_pushPipeName = name;
    printf("test pipe: %ls\n", g_pipeName.c_str());

    TestFocusGainedAfterRestart();
    TestImeActivatedAfterRestart();
    TestKeyEventAfterRestart();
    TestRepeatedRestartsKeepCircuitClosed();
#ifndef IPC_TEST_LEGACY
    TestDiscardStaleConnection();
#endif
    TestServiceGone();

    printf("\n%d/%d checks passed\n", g_checks - g_failures, g_checks);
    if (g_failures != 0)
    {
        printf("FAILED\n");
        return 1;
    }
    printf("ALL PASS\n");
    return 0;
}
