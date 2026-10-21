// 收到 SERVICE_READY 后补发 IME_ACTIVATED 失败时的**重试次数与间隔**纯判据。
//
// 从 `CLangBarItemButton::_MsgWndProc` 的 WM_SERVICE_READY / TIMER_ID_SERVICE_READY 分支
// 抽出来只为可测（不含 Win32 头，g++ 可跑，见 tests/service_ready_retry_policy_test.cpp）。
//
// # 为什么要重试
//
// 服务就绪时给所有已连上 push 管道的客户端一起补发 SERVICE_READY，每个焦点非空的 TSF 线程
// （含后台宿主）同时去连主管道；服务端一次只挂一个监听实例，宿主一多，前台那个常拿到
// ERROR_PIPE_BUSY，IME_ACTIVATED 发不出去。SERVICE_READY 只来一次，服务端又只在收到
// IME_ACTIVATED 时显示工具栏——不重试的话工具栏要等到用户按键才出来。
//
// # 为什么带抖动
//
// N 个宿主同一时刻失败，固定间隔重试会再次同时撞上；按线程 id 与次数散开 0–200ms。
#pragma once

#include <cstdint>

namespace wind
{
namespace svcready
{

/// 失败后最多重试几次（不含首次）。
constexpr uint32_t kMaxRetries = 10;
constexpr uint32_t kBaseDelayMs = 300;
constexpr uint32_t kJitterMs = 200;

/// 已失败 `failures` 次（含本次）后是否还要再试。
inline bool ShouldRetry(uint32_t failures)
{
    return failures >= 1 && failures <= kMaxRetries;
}

/// 第 `failures` 次失败后的重试间隔，落在 [kBaseDelayMs, kBaseDelayMs + kJitterMs]。
/// `seed` 取线程 id：同一线程每次不同，不同线程彼此错开。
inline uint32_t RetryDelayMs(uint32_t seed, uint32_t failures)
{
    uint32_t h = seed * 2654435761u ^ failures * 40503u;
    h ^= h >> 15;
    h *= 2246822519u;
    h ^= h >> 13;
    return kBaseDelayMs + h % (kJitterMs + 1);
}

/// 本轮（首次或定时器到点）该怎么做。
enum class Action
{
    StopNoFocus,         ///< 无输入焦点：不弹工具栏，等 OnSetFocus 建立服务端状态
    StopAlreadySynced,   ///< 重试时发现本连接已由按键 / 焦点路径同步过：再发会重置中英状态
    StopBackground,      ///< 重试时本线程不在前台：后台宿主不值得反复阻塞 UI 线程
    DeferComposition,    ///< 组合进行中：推迟，免得清掉服务端输入缓冲（不计失败次数）
    Sync,                ///< 发 IME_ACTIVATED
};

/// - `isRetry`：之前已失败过（失败重试，而非首次或组合态推迟）。
/// - `connected` / `stateEstablished`：主管道已连上，且本连接上 IME_ACTIVATED 或
///   FOCUS_GAINED 已发出去过。
inline Action Decide(bool isRetry, bool hasFocus, bool hasThreadFocus, bool composing,
                     bool connected, bool stateEstablished)
{
    if (!hasFocus)
        return Action::StopNoFocus;
    if (isRetry && connected && stateEstablished)
        return Action::StopAlreadySynced;
    if (isRetry && !hasThreadFocus)
        return Action::StopBackground;
    if (composing)
        return Action::DeferComposition;
    return Action::Sync;
}

/// focus_gained 发不出去后的定时补发（与上面的就绪重试分开计数、分开定时器）该怎么做。
///
/// 只补 focus_gained，**没有**退回全量同步这一档：IME_ACTIVATED 会让服务端按初始模式重置
/// 中英 / 全半角 / 标点，而补发窗口里用户可能已手动切过。工具栏晚出比重置用户状态好接受。
enum class FocusAction
{
    StopRecovered,       ///< 已没有待补的焦点（别的路径补上了，或焦点已换）
    StopNoFocus,         ///< 无输入焦点
    StopBackground,      ///< 本线程不在前台：补发会冒充前台焦点
    StopAlreadySynced,   ///< 本连接上已有 IME_ACTIVATED / FOCUS_GAINED / 按键往来
    Resend,              ///< 连上（必要时新建）后补发 focus_gained
};

/// - `needsRecovery`：焦点同步仍欠着（TextService 的 _needsFocusRecovery）。
/// - `connected` / `stateEstablished`：同 Decide；按键发出去也算已建立。
inline FocusAction DecideFocusRecovery(bool needsRecovery, bool hasFocus, bool hasThreadFocus,
                                       bool connected, bool stateEstablished)
{
    if (!needsRecovery)
        return FocusAction::StopRecovered;
    if (!hasFocus)
        return FocusAction::StopNoFocus;
    if (!hasThreadFocus)
        return FocusAction::StopBackground;
    if (connected && stateEstablished)
        return FocusAction::StopAlreadySynced;
    return FocusAction::Resend;
}

} // namespace svcready
} // namespace wind
