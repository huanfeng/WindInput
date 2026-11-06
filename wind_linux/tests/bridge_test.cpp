// UDS 客户端单测：用进程内的假服务端验证请求/响应、读超时、断线自愈、push 重连。
// socket 放 $TMPDIR（未设则 /tmp）下的短路径——sun_path 上限 108 字节。

#include "Bridge.h"
#include "ExtProtocol.h"
#include "Protocol.h"
#include "TestHarness.h"

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdlib>
#include <cstring>
#include <mutex>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <thread>
#include <fstream>
#include <sstream>
#include <unistd.h>
#include <vector>

using namespace windlinux;

namespace {

std::string tmpPath(const char* name)
{
    const char* t = std::getenv("TMPDIR");
    return std::string(t && *t ? t : "/tmp") + "/wl_" + std::to_string(getpid()) + "_" + name;
}

int listenOn(const std::string& path)
{
    ::unlink(path.c_str());
    int fd = ::socket(AF_UNIX, SOCK_STREAM, 0);
    sockaddr_un a{};
    a.sun_family = AF_UNIX;
    std::strncpy(a.sun_path, path.c_str(), sizeof(a.sun_path) - 1);
    if (::bind(fd, reinterpret_cast<sockaddr*>(&a), sizeof(a)) != 0 || ::listen(fd, 4) != 0) {
        std::printf("  listen %s 失败: %s\n", path.c_str(), std::strerror(errno));
        ::close(fd);
        return -1;
    }
    return fd;
}

void writeAll(int fd, const Bytes& b)
{
    size_t off = 0;
    while (off < b.size()) {
        ssize_t w = ::send(fd, b.data() + off, b.size() - off, MSG_NOSIGNAL);
        if (w <= 0) {
            return;
        }
        off += size_t(w);
    }
}

void TestRequestResponse()
{
    CASE("request：发 KeyEvent、收 Consumed；服务端看到的帧完整");
    std::string path = tmpPath("rr.sock");
    int lfd = listenOn(path);
    CHECK(lfd >= 0);
    std::atomic<uint16_t> seenCmd{0};
    std::atomic<uint32_t> seenLen{0};
    std::thread srv([&] {
        int c = ::accept(lfd, nullptr, nullptr);
        uint8_t hdr[8];
        if (readFully(c, hdr, 8)) {
            HeaderInfo info;
            decodeHeader(hdr, info);
            Bytes p(info.length);
            readFully(c, p.data(), p.size());
            seenCmd = info.cmd;
            seenLen = info.length;
            writeAll(c, encodeEmptyFrame(CMD_CONSUMED));
        }
        ::close(c);
    });
    BridgeClient bc(1000);
    CHECK(bc.connect(path));
    KeyEvent e;
    e.keyCode = 0x4E;
    Frame resp;
    CHECK(bc.request(encodeKeyEventFrame(e), resp));
    CHECK_EQ(resp.cmd, uint16_t(CMD_CONSUMED));
    srv.join();
    CHECK_EQ(seenCmd.load(), uint16_t(CMD_KEY_EVENT));
    CHECK_EQ(seenLen.load(), 20u);

    CASE("对端已关：下一次 request 失败并自动断开（上层据此重连），进程不被 SIGPIPE 杀掉");
    Frame r2;
    CHECK(!bc.request(encodeKeyEventFrame(e), r2));
    CHECK(!bc.isConnected());
    ::close(lfd);
    ::unlink(path.c_str());
}

void TestReadTimeout()
{
    CASE("服务卡死不回：读在超时后失败，而不是无限挂住 fcitx5 主线程");
    std::string path = tmpPath("to.sock");
    int lfd = listenOn(path);
    std::mutex m;
    std::condition_variable cv;
    bool done = false;
    std::thread srv([&] {
        int c = ::accept(lfd, nullptr, nullptr);
        std::unique_lock<std::mutex> lk(m);
        cv.wait(lk, [&] { return done; }); // 收下但永不回复
        ::close(c);
    });
    BridgeClient bc(300);
    CHECK(bc.connect(path));
    auto t0 = std::chrono::steady_clock::now();
    Frame f;
    CHECK(!bc.request(encodeEmptyFrame(CMD_FOCUS_LOST), f));
    auto ms = std::chrono::duration_cast<std::chrono::milliseconds>(
                  std::chrono::steady_clock::now() - t0)
                  .count();
    CHECK(ms >= 250 && ms < 2000);
    {
        std::lock_guard<std::mutex> lk(m);
        done = true;
    }
    cv.notify_all();
    srv.join();
    ::close(lfd);
    ::unlink(path.c_str());
}

/// 一个只 accept、把收到的帧数记下来的假服务端：`reply` 决定收到帧后怎么办。
enum class Reply { Never, CloseWithoutReply, Consumed };

struct FakeServer {
    std::string path;
    int lfd = -1;
    std::atomic<int> frames{0};
    std::atomic<int> conns{0};
    std::thread th;

    FakeServer(const char* name, std::vector<Reply> plan) : path(tmpPath(name))
    {
        lfd = listenOn(path);
        th = std::thread([this, plan] {
            for (Reply r : plan) {
                int c = ::accept(lfd, nullptr, nullptr);
                if (c < 0) {
                    return;
                }
                ++conns;
                uint8_t hdr[8];
                if (readFully(c, hdr, 8)) {
                    HeaderInfo info;
                    decodeHeader(hdr, info);
                    Bytes p(info.length);
                    readFully(c, p.data(), p.size());
                    ++frames;
                    if (r == Reply::Consumed) {
                        writeAll(c, encodeEmptyFrame(CMD_CONSUMED));
                    } else if (r == Reply::Never) {
                        char b;
                        while (::read(c, &b, 1) > 0) { // 收下不回，直到客户端放弃（关连接）
                        }
                    }
                }
                ::close(c);
            }
        });
    }
    ~FakeServer()
    {
        ::shutdown(lfd, SHUT_RDWR); // 打断还在等的 accept
        th.join();
        ::close(lfd);
        ::unlink(path.c_str());
    }
};

long msSince(std::chrono::steady_clock::time_point t0)
{
    return long(std::chrono::duration_cast<std::chrono::milliseconds>(
                    std::chrono::steady_clock::now() - t0)
                    .count());
}

void TestReadTimeoutIsNotRetried()
{
    CASE("读超时不重发：服务收下了却迟迟不回（可能已处理），重发就是重复上屏");
    FakeServer srv("noretry.sock", {Reply::Never, Reply::Consumed});
    BridgeClient bc(300, 60000);
    int reconnects = 0;
    auto reconnect = [&] {
        ++reconnects;
        return bc.connect(srv.path);
    };
    KeyEvent e;
    e.keyCode = 0x4E;
    Frame f;
    auto t0 = std::chrono::steady_clock::now();
    CHECK(!requestWithRetry(bc, encodeKeyEventFrame(e), f, reconnect));
    const long ms = msSince(t0);
    CHECK(bc.lastFailure() == BridgeClient::Failure::ReadTimeout);
    CHECK(!BridgeClient::retryable(bc.lastFailure()));
    CHECK_EQ(reconnects, 1); // 只有起手那次连接，没有因失败再连
    CHECK(ms >= 250 && ms < 1000);
    std::this_thread::sleep_for(std::chrono::milliseconds(50));
    CHECK_EQ(srv.frames.load(), 1);
}

void TestPeerClosedIsRetried()
{
    CASE("一个响应字节都没读到就断（服务读帧后崩溃）：换新连接重发一次，成功");
    FakeServer srv("retry.sock", {Reply::CloseWithoutReply, Reply::Consumed});
    BridgeClient bc(1000);
    auto reconnect = [&] { return bc.connect(srv.path); };
    Frame f;
    CHECK(requestWithRetry(bc, encodeEmptyFrame(CMD_FOCUS_LOST), f, reconnect));
    CHECK_EQ(f.cmd, uint16_t(CMD_CONSUMED));
    CHECK_EQ(srv.frames.load(), 2);
    CHECK_EQ(srv.conns.load(), 2);
}

void TestSendToDeadConnectionIsRetried()
{
    CASE("向死连接写（服务重启过）：写失败可重发，换新连接后成功");
    FakeServer srv("dead.sock", {Reply::CloseWithoutReply, Reply::Consumed});
    BridgeClient bc(1000);
    CHECK(bc.connect(srv.path));
    Frame f;
    // 先让第一条连接被服务端关掉：发一帧它收下就关。
    CHECK(!bc.request(encodeEmptyFrame(CMD_FOCUS_LOST), f));
    CHECK(bc.lastFailure() == BridgeClient::Failure::PeerClosed);
    CHECK(BridgeClient::retryable(bc.lastFailure()));
    CHECK(!bc.suspended()); // 不是超时，不熔断
    CHECK(!bc.takeStallFlag());
    CHECK(bc.connect(srv.path));
    CHECK(bc.request(encodeEmptyFrame(CMD_FOCUS_LOST), f));
}

void TestBreaker()
{
    CASE("熔断：超时之后 breakerMs 内 connect 立即失败（不再付一次超时），到期恢复");
    FakeServer srv("breaker.sock", {Reply::Never, Reply::Consumed});
    BridgeClient bc(200, 600);
    CHECK(bc.connect(srv.path));
    Frame f;
    CHECK(!bc.request(encodeEmptyFrame(CMD_FOCUS_LOST), f));
    CHECK(bc.suspended());
    CHECK(bc.takeStallFlag());
    CHECK(!bc.takeStallFlag()); // 取走即清
    auto t0 = std::chrono::steady_clock::now();
    CHECK(!bc.connect(srv.path));
    CHECK(bc.lastFailure() == BridgeClient::Failure::Suspended);
    CHECK(msSince(t0) < 50);
    std::this_thread::sleep_for(std::chrono::milliseconds(650));
    CHECK(!bc.suspended());
    CHECK(bc.connect(srv.path));
    CHECK(bc.request(encodeEmptyFrame(CMD_FOCUS_LOST), f));
}

void TestConnectTimeout()
{
    CASE("服务停住、listen 队列已满：connect 在超时后返回（不无限阻塞），并熔断");
    std::string path = tmpPath("full.sock");
    ::unlink(path.c_str());
    int lfd = ::socket(AF_UNIX, SOCK_STREAM, 0);
    sockaddr_un a{};
    a.sun_family = AF_UNIX;
    std::strncpy(a.sun_path, path.c_str(), sizeof(a.sun_path) - 1);
    CHECK(::bind(lfd, reinterpret_cast<sockaddr*>(&a), sizeof(a)) == 0);
    CHECK(::listen(lfd, 0) == 0); // 从不 accept
    // 把队列塞满：非阻塞地连到 EAGAIN 为止。
    std::vector<int> fillers;
    for (int i = 0; i < 64; ++i) {
        int fd = ::socket(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK, 0);
        if (::connect(fd, reinterpret_cast<sockaddr*>(&a), sizeof(a)) != 0) {
            ::close(fd);
            break;
        }
        fillers.push_back(fd);
    }
    CHECK(!fillers.empty());
    BridgeClient bc(300, 60000);
    auto t0 = std::chrono::steady_clock::now();
    CHECK(!bc.connect(path));
    const long ms = msSince(t0);
    CHECK(bc.lastFailure() == BridgeClient::Failure::ConnectTimeout);
    CHECK(ms >= 250 && ms < 1500);
    CHECK(bc.suspended());
    for (int fd : fillers) {
        ::close(fd);
    }
    ::close(lfd);
    ::unlink(path.c_str());
}

void TestPrivateDir()
{
    CASE("兜底运行时目录的私有性：目录、本用户、0700，符号链接 / 文件 / 放开权限 / 别人的都拒绝");
    char tmpl[] = "/tmp/wl-priv-XXXXXX";
    const std::string root = mkdtemp(tmpl);
    const uint32_t me = uint32_t(getuid());
    const std::string ok = root + "/ok";
    CHECK(::mkdir(ok.c_str(), 0700) == 0);
    CHECK(privateDirProblem(ok, me).empty());
    CHECK(!privateDirProblem(ok, me + 1).empty()); // 属主不是期望的 uid
    const std::string loose = root + "/loose";
    CHECK(::mkdir(loose.c_str(), 0700) == 0);
    CHECK(::chmod(loose.c_str(), 0755) == 0);
    CHECK(privateDirProblem(loose, me).find("权限") != std::string::npos);
    CHECK(::chmod(loose.c_str(), 0710) == 0);
    CHECK(!privateDirProblem(loose, me).empty());
    const std::string link = root + "/link";
    CHECK(::symlink(ok.c_str(), link.c_str()) == 0);
    CHECK(privateDirProblem(link, me).find("符号链接") != std::string::npos);
    const std::string file = root + "/file";
    { std::ofstream(file) << "x"; }
    CHECK(::chmod(file.c_str(), 0600) == 0);
    CHECK(privateDirProblem(file, me).find("不是目录") != std::string::npos);
    CHECK(!privateDirProblem(root + "/missing", me).empty());
    if (std::system(("rm -rf " + root).c_str()) != 0) {
        std::printf("(清理临时目录失败，忽略)\n");
    }

    CASE("兜底目录名带 uid（/tmp 下各用户各一个）");
    unsetenv("WIND_VARIANT");
    setenv("WIND_VARIANT", "release", 1);
    CHECK_EQ(fallbackRuntimeDir(), "/tmp/wind_input-" + std::to_string(me));
    setenv("WIND_VARIANT", "dev", 1);
    CHECK_EQ(fallbackRuntimeDir(), "/tmp/wind_input_dev-" + std::to_string(me));
    unsetenv("WIND_VARIANT");
}

void TestPeerUid()
{
    CASE("对端 uid（SO_PEERCRED）：本用户的服务照连；对端不是期望的 uid 就拒绝，连接不留");
    FakeServer srv("peer.sock", {Reply::Consumed, Reply::Consumed});
    BridgeClient bc(1000);
    bc.setExpectedPeerUid(uint32_t(getuid()) + 1);
    CHECK(!bc.connect(srv.path));
    CHECK(bc.lastFailure() == BridgeClient::Failure::Untrusted);
    CHECK(!bc.isConnected());
    CHECK(!bc.suspended()); // 不是超时，不熔断
    bc.setExpectedPeerUid(uint32_t(getuid()));
    CHECK(bc.connect(srv.path));
    Frame f;
    CHECK(bc.request(encodeEmptyFrame(CMD_FOCUS_LOST), f));
    int sv[2];
    CHECK(::socketpair(AF_UNIX, SOCK_STREAM, 0, sv) == 0);
    CHECK_EQ(peerUid(sv[0]), int64_t(getuid()));
    ::close(sv[0]);
    ::close(sv[1]);
}

void TestFallbackDirRejected()
{
    CASE("兜底目录不私有（如被放开成 0755）：连都不连，报不可信；收紧成 0700 后照连");
    unsetenv("WIND_INPUT_RUNTIME_DIR");
    unsetenv("XDG_RUNTIME_DIR");
    // 本机可能真有本用户某个变体的兜底目录在用：挑一个不存在的变体来造，不碰在用的那个。
    std::string dir;
    for (const char* v : {"dev", "release"}) {
        setenv("WIND_VARIANT", v, 1);
        struct stat st {};
        if (::lstat(fallbackRuntimeDir().c_str(), &st) != 0) {
            dir = fallbackRuntimeDir();
            break;
        }
    }
    CHECK(!dir.empty()); // 两个变体的兜底目录都在用：这台机器上没法不碰它们来测
    if (!dir.empty()) {
        CHECK(::mkdir(dir.c_str(), 0700) == 0);
        CHECK(::chmod(dir.c_str(), 0755) == 0);
        const std::string sock = dir + "/bridge.sock";
        int lfd = listenOn(sock);
        BridgeClient bc(500);
        CHECK(!bc.connect(sock));
        CHECK(bc.lastFailure() == BridgeClient::Failure::Untrusted);
        CHECK(bc.lastError().find("不可信") != std::string::npos);
        CHECK(::chmod(dir.c_str(), 0700) == 0);
        std::thread srv([&] {
            int c = ::accept(lfd, nullptr, nullptr);
            ::close(c);
        });
        CHECK(bc.connect(sock));
        srv.join();
        ::close(lfd);
        ::unlink(sock.c_str());
        ::rmdir(dir.c_str());
    }
    unsetenv("WIND_VARIANT");
}

void TestConnectFailures()
{
    CASE("连接失败：socket 不存在 / 路径超长都如实报错");
    BridgeClient bc;
    CHECK(!bc.connect(tmpPath("nope.sock")));
    CHECK(!bc.lastError().empty());
    CHECK(!bc.connect("/tmp/" + std::string(200, 'x')));
    CHECK(bc.lastError().find("字节") != std::string::npos);
}

void TestPushReconnect()
{
    CASE("push：收帧回调；服务重启后自动重连再收");
    std::string path = tmpPath("push.sock");
    std::mutex m;
    std::condition_variable cv;
    std::vector<uint16_t> got;
    int connects = 0;
    PushClient pc(
        path,
        [&](Frame f) {
            std::lock_guard<std::mutex> lk(m);
            got.push_back(f.cmd);
            cv.notify_all();
        },
        [&](bool up) {
            std::lock_guard<std::mutex> lk(m);
            if (up) {
                ++connects;
            }
        });
    pc.start(); // 服务还没起：客户端应每秒重试

    auto serveOnce = [&](uint16_t cmd) {
        int lfd = listenOn(path);
        int c = ::accept(lfd, nullptr, nullptr);
        writeAll(c, encodeEmptyFrame(CMD_SERVICE_READY));
        writeAll(c, encodeEmptyFrame(cmd));
        std::unique_lock<std::mutex> lk(m);
        cv.wait_for(lk, std::chrono::seconds(3), [&] {
            return !got.empty() && got.back() == cmd;
        });
        lk.unlock();
        ::close(c);
        ::close(lfd);
        ::unlink(path.c_str());
    };
    serveOnce(CMD_STATE_PUSH);
    serveOnce(0x0512); // CMD_KEY_TYPE；第二次 = 服务重启后

    pc.stop();
    std::lock_guard<std::mutex> lk(m);
    CHECK(got.size() == 4);
    if (got.size() == 4) {
        CHECK_EQ(got[0], uint16_t(CMD_SERVICE_READY));
        CHECK_EQ(got[1], uint16_t(CMD_STATE_PUSH));
        CHECK_EQ(got[3], uint16_t(0x0512));
    }
    CHECK_EQ(connects, 2);
}

void TestStopWhileIdle()
{
    CASE("stop：连着但空闲时也能立即退出（shutdown 打断阻塞读）");
    std::string path = tmpPath("idle.sock");
    int lfd = listenOn(path);
    std::thread srv([&] {
        int c = ::accept(lfd, nullptr, nullptr);
        char b;
        while (::read(c, &b, 1) > 0) {
        }
        ::close(c);
    });
    PushClient pc(path, [](Frame) {});
    pc.start();
    std::this_thread::sleep_for(std::chrono::milliseconds(100));
    auto t0 = std::chrono::steady_clock::now();
    pc.stop();
    auto ms = std::chrono::duration_cast<std::chrono::milliseconds>(
                  std::chrono::steady_clock::now() - t0)
                  .count();
    CHECK(ms < 500);
    srv.join();
    ::close(lfd);
    ::unlink(path.c_str());
}

void TestEndpoints()
{
    CASE("端点：WIND_INPUT_RUNTIME_DIR 优先；XDG_RUNTIME_DIR/WindInput[Dev]；/tmp 兜底");
    setenv("WIND_INPUT_RUNTIME_DIR", "/tmp/rt1", 1);
    CHECK(requestSocketPath() == "/tmp/rt1/bridge.sock");
    CHECK(pushSocketPath() == "/tmp/rt1/bridge_push.sock");
    unsetenv("WIND_INPUT_RUNTIME_DIR");
    setenv("XDG_RUNTIME_DIR", "/run/user/1000", 1);
    setenv("WIND_VARIANT", "release", 1);
    CHECK(runtimeDir() == "/run/user/1000/WindInput");
    const std::string uid = std::to_string(getuid());
    CHECK(shmName() == "/WindInput." + uid);
    setenv("WIND_VARIANT", "DEV", 1);
    CHECK(runtimeDir() == "/run/user/1000/WindInputDev");
    CHECK(shmName() == "/WindInputDev." + uid);
    CHECK(overlayShmName(OVERLAY_KIND_STATUS) == "/WindInputDev." + uid + "_STS");
    CHECK(overlayShmName(OVERLAY_KIND_TOOLTIP) == "/WindInputDev." + uid + "_TIP");
    CHECK(overlayShmName(OVERLAY_KIND_TOAST) == "/WindInputDev." + uid + "_TST");
    CHECK(overlayShmName(0).empty());

    CASE("SHM 名带 uid：与 Rust endpoint.rs 钉同一组样例；最长的也不超过 31 字节");
    // 样例同时写在 endpoint.rs 的 Linux 测试里（下面读源码核对），两侧规则改一边当场红。
    const std::string longest =
        overlayShmName(OVERLAY_KIND_MENU + OVERLAY_MENU_LEVELS - 1, shmNameForUid(4294967295u));
    CHECK_EQ(longest, std::string("/WindInputDev.4294967295_MN5"));
    CHECK(longest.size() <= 31);
    CHECK_EQ(overlayShmName(OVERLAY_KIND_SOFTKEYBOARD, shmNameForUid(4294967295u)),
             std::string("/WindInputDev.4294967295_SKB"));
    setenv("WIND_VARIANT", "release", 1);
    CHECK_EQ(shmNameForUid(1000), std::string("/WindInput.1000"));
    CHECK_EQ(overlayShmName(OVERLAY_KIND_STATUS, shmNameForUid(1000)),
             std::string("/WindInput.1000_STS"));
    CHECK_EQ(overlayShmName(OVERLAY_KIND_TOOLBAR, shmNameForUid(1000)),
             std::string("/WindInput.1000_TB"));
    setenv("WIND_VARIANT", "DEV", 1);
    {
        std::ifstream in(std::string(WIND_REPO_DIR) + "/wind_input/crates/wind-bridge/src/endpoint.rs");
        std::stringstream ss;
        ss << in.rdbuf();
        const std::string rs = ss.str();
        CHECK(!rs.empty());
        for (const char* sample : {"\"/WindInputDev.4294967295_MN5\"", "\"/WindInput.1000\"",
                                   "\"/WindInput.1000_STS\"", "\"/WindInput.1000_TB\"",
                                   "\"/WindInputDev.4294967295_SKB\""}) {
            if (rs.find(sample) == std::string::npos) {
                std::printf("  endpoint.rs 里没有样例 %s\n", sample);
                CHECK(false);
            }
        }
    }
    unsetenv("XDG_RUNTIME_DIR");
    CHECK(runtimeDir() == "/tmp/wind_input_dev-" + uid);
    unsetenv("WIND_VARIANT");
}

} // namespace

int main()
{
    TestRequestResponse();
    TestReadTimeout();
    TestReadTimeoutIsNotRetried();
    TestPeerClosedIsRetried();
    TestSendToDeadConnectionIsRetried();
    TestBreaker();
    TestConnectTimeout();
    TestPrivateDir();
    TestPeerUid();
    TestFallbackDirRejected();
    TestConnectFailures();
    TestPushReconnect();
    TestStopWhileIdle();
    TestEndpoints();
    TEST_MAIN_END();
}
