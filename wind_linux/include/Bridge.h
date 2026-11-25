// UDS 客户端：请求/响应通道（BridgeClient）与服务端推送通道（PushClient），外加端点路径。
// 纯 POSIX，不依赖 Fcitx5。对位 Swift BridgeClient.swift / PushClient.swift 与 Rust
// wind-bridge/src/endpoint.rs。
#pragma once

#include "Codec.h"

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <functional>
#include <mutex>
#include <string>
#include <thread>

namespace windlinux {

// ── 端点 ────────────────────────────────────────────────────────────

/// 变体后缀：dev 为 "Dev"，release 为 ""。环境变量 `WIND_VARIANT=dev`（忽略大小写）强制
/// dev、其它非空值强制 release（与 Rust `wind_config::variant::is_dev` 同一覆盖口）；
/// 未设置时取编译期默认（CMake 选项 WIND_VARIANT_DEV）。
std::string variantSuffix();

/// 运行时目录，与 Rust `endpoint.rs::runtime_dir` 逐条对齐：
/// `$WIND_INPUT_RUNTIME_DIR` → `$XDG_RUNTIME_DIR/WindInput{Dev}` → `/tmp/wind_input{_dev}-<uid>`。
std::string runtimeDir();
/// 无 `$XDG_RUNTIME_DIR` 时的兜底目录 `/tmp/wind_input{_dev}-<uid>`。`/tmp` 人人可写：别的用户
/// 可以抢先建这个目录、在里面监听 socket，收走全部按键（含密码框）再经推送通道注入文本 / 按键。
/// 所以连这个目录下的 socket 之前先 `privateDirProblem` 校验（服务端建目录时同一套规则）。
/// `$XDG_RUNTIME_DIR`（规范要求本用户 0700）与显式覆盖不校验目录；两条通道一律校验对端 uid。
std::string fallbackRuntimeDir();
/// 目录是否「本用户私有」：`lstat` 是目录（不是符号链接）、属主 = `uid`、组与其他人无任何权限。
/// 合格返回空串，否则返回原因。与 Rust `endpoint::check_private_dir` 同一规则。
std::string privateDirProblem(const std::string& dir, uint32_t uid);
/// 已连上的 UDS 对端进程的 uid（`SO_PEERCRED`）；取不到返回 -1。
int64_t peerUid(int fd);
std::string requestSocketPath();
std::string pushSocketPath();
/// POSIX SHM 名：`/WindInput{Dev}.<uid>`（与 Rust `endpoint.rs::shm_name` 的 Linux 外部宿主
/// 分支对齐；macOS 是 `/WindInput_SHM{Dev}`，Swift 写死）。
///
/// 带 uid：`/dev/shm` 全系统共用，不带则同机第二个用户建不了段，别的用户还能抢先建同名段。
/// 名字连层后缀最长 28 字节，须守 macOS PSHMNAMLEN 的 31，Rust 侧 debug_assert 同一个数。
std::string shmName();
/// 同上，uid 显式给（单测钉样例用）。
std::string shmNameForUid(uint32_t uid);
/// 光栅浮层某一层的 SHM 名：`base`（缺省 `shmName()`）+ `_TIP` / `_STS` / `_TST`，菜单第 k 级
/// `_MN<k>`，工具栏 `_TB`、软键盘 `_SKB`（与 Rust `endpoint.rs::overlay_shm_name` 对齐）。未知层返回空串。
std::string overlayShmName(uint32_t kind, const std::string& base = shmName());

// ── 请求/响应 ────────────────────────────────────────────────────────

/// 阻塞式 UDS 客户端，不是线程安全的：同一连接上「发一帧、读一帧」必须配对，别的线程插帧会
/// 把读写配对错开。
///
/// addon 跑在 fcitx5 主进程里，这里卡住就是整个桌面的输入卡住，所以：
///   - I/O 超时（默认 2000ms）在 connect 之前就设好：服务停住时 listen 队列塞满，AF_UNIX 的
///     connect 会一直阻塞，它认 SO_SNDTIMEO。
///   - 熔断：任何一次超时后 `breakerMs`（默认 3000ms）内 connect 直接失败，服务卡死期间每个键
///     至多付一次超时；到期后下一键再试。
///   - 写用 MSG_NOSIGNAL：向死连接写不会 SIGPIPE 杀掉 fcitx5。
class BridgeClient {
public:
    /// 最近一次失败的种类：决定能不能重试（`retryable`）、要不要熔断（`isTimeout`）。
    enum class Failure {
        None,
        NotConnected,
        Suspended,      // 熔断期内，没去连
        Connect,        // 连不上（ENOENT / ECONNREFUSED…）：服务多半没在跑
        ConnectTimeout, // 连接排队超时：服务在，但不 accept
        Untrusted,      // 兜底目录不是本用户私有，或对端进程不是本用户的（SO_PEERCRED）
        Send,          // 写失败（EPIPE / ECONNRESET）：对端没收到这一帧
        SendTimeout,
        ReadTimeout,    // 发出去了、迟迟没回：服务可能已经处理了这一帧
        PeerClosed,     // 一个响应字节都没读到就 EOF / ECONNRESET（服务读帧前后崩了）
        Broken,         // 响应读到一半断了、或帧头不合法
    };

    explicit BridgeClient(int ioTimeoutMs = 2000, int breakerMs = 3000);
    ~BridgeClient() { close(); }
    BridgeClient(const BridgeClient&) = delete;
    BridgeClient& operator=(const BridgeClient&) = delete;

    bool connect(const std::string& path);
    bool isConnected() const { return fd_ >= 0; }
    /// 对端必须是这个 uid（缺省本用户）。只给单测造「对端是别人」用。
    void setExpectedPeerUid(uint32_t uid) { expectedPeerUid_ = uid; }
    void close();

    bool send(const Bytes& frame);
    bool readFrame(Frame& out);
    /// 发一帧并读一帧响应。任一步失败即关闭连接并返回 false（交上层重连）。
    bool request(const Bytes& frame, Frame& out);

    const std::string& lastError() const { return lastError_; }
    Failure lastFailure() const { return lastFailure_; }
    /// 熔断中：上一次超时之后还没过 `breakerMs`。期间 connect 直接失败。
    bool suspended() const;
    /// 自上次取走以来发生过超时：服务恢复后可能还会处理掉积在旧连接里的帧（它当时没回，
    /// 不代表没收），两端的组字状态因此可能错开——上层在重新连上时据此对齐一次。
    bool takeStallFlag();

    /// 这种失败能否原样重发同一帧：只有「对端确定没处理」的才行——写失败、或一个响应字节
    /// 都没读到就断（服务在读帧前后崩溃，崩了就不会上屏）。读超时不能重发：服务可能已经
    /// 处理、只是回得慢，协调器不按 event_seq 去重，重发就是重复上屏。
    static bool retryable(Failure f) { return f == Failure::Send || f == Failure::PeerClosed; }
    static bool isTimeout(Failure f)
    {
        return f == Failure::ConnectTimeout || f == Failure::SendTimeout
            || f == Failure::ReadTimeout;
    }

private:
    bool fail(Failure kind, const std::string& what);

    int fd_ = -1;
    int timeoutMs_;
    int breakerMs_;
    uint32_t expectedPeerUid_;
    std::string lastError_;
    Failure lastFailure_ = Failure::None;
    std::chrono::steady_clock::time_point unhealthyUntil_{};
    bool stalled_ = false;
};

/// 发一帧、读响应；失败且 `BridgeClient::retryable` 时调 `reconnect` 换新连接再试一次
/// （服务重启后的第一个键就自愈、不丢字——对位 macOS `handle` 的同名策略）。
/// `reconnect` 返回 false（连不上 / 熔断中）则放弃。
bool requestWithRetry(BridgeClient& client, const Bytes& frame, Frame& out,
                      const std::function<bool()>& reconnect);

/// 从已连接的 fd 读满 n 字节；EOF/错误返回 false。PushClient 与 BridgeClient 共用。
bool readFully(int fd, uint8_t* buf, size_t n);

// ── 推送 ────────────────────────────────────────────────────────────

/// 订阅 `bridge_push.sock` 的后台线程：连上即收 SERVICE_READY，之后逐帧回调。
/// 断开（服务重启）后每秒重连一次，直到 stop()。
///
/// 回调在推送线程上执行；调用方负责把它转回自己的主线程（Fcitx5 侧用
/// EventDispatcher）。push 连接不设读超时：它长期空闲等推送，设了会被误判断连。
///
/// 协议上客户端只读不写（服务端 accept 即登记，见 wind-bridge/src/push_unix.rs）。
class PushClient {
public:
    using FrameHandler = std::function<void(Frame)>;
    using StateHandler = std::function<void(bool connected)>;

    PushClient(std::string path, FrameHandler onFrame, StateHandler onState = {});
    ~PushClient() { stop(); }
    PushClient(const PushClient&) = delete;
    PushClient& operator=(const PushClient&) = delete;

    void start();
    void stop();

private:
    void run();

    std::string path_;
    FrameHandler onFrame_;
    StateHandler onState_;
    std::thread thread_;
    std::atomic<bool> stopping_{false};
    std::mutex mu_;
    std::condition_variable cv_;
    int fd_ = -1; // 受 mu_ 保护（stop 要 shutdown 它来打断阻塞读）
};

} // namespace windlinux
