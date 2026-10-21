// ServiceReadyRetryPolicy 单元测试
//
// 跑法（纯 C++17、不含 Win32 头）：
//   g++ -std=c++17 -I../include -o service_ready_retry_policy_test service_ready_retry_policy_test.cpp && ./service_ready_retry_policy_test
//
// 守的是「SERVICE_READY 后补发 IME_ACTIVATED 失败要有限次重试、间隔有界且各线程错开」。

#include "ServiceReadyRetryPolicy.h"

#include <cstdio>
#include <set>

namespace
{
int g_failures = 0;

void Expect(bool cond, const char* what)
{
    if (!cond)
    {
        std::printf("FAIL: %s\n", what);
        ++g_failures;
    }
}
} // namespace

int main()
{
    using namespace wind::svcready;

    Expect(!ShouldRetry(0), "没失败过不重试");
    Expect(ShouldRetry(1), "首次失败要重试");
    Expect(ShouldRetry(10), "第 10 次失败仍重试");
    Expect(!ShouldRetry(11), "超过上限放弃");

    bool inRange = true;
    for (uint32_t seed = 1; seed < 5000; seed += 7)
        for (uint32_t n = 1; n <= kMaxRetries; ++n)
        {
            uint32_t d = RetryDelayMs(seed, n);
            if (d < 300 || d > 500)
                inRange = false;
        }
    Expect(inRange, "间隔落在 300–500ms");

    // 20 个宿主线程（id 相邻，Windows 线程 id 是 4 的倍数）首次重试不能挤在一两个时刻上。
    std::set<uint32_t> firstDelays;
    for (uint32_t tid = 10000; tid < 10080; tid += 4)
        firstDelays.insert(RetryDelayMs(tid, 1));
    Expect(firstDelays.size() >= 15, "相邻线程 id 的首次重试间隔要散开");

    std::set<uint32_t> perAttempt;
    for (uint32_t n = 1; n <= kMaxRetries; ++n)
        perAttempt.insert(RetryDelayMs(12345, n));
    Expect(perAttempt.size() >= 5, "同一线程各次重试间隔要变化");

    // Decide(isRetry, hasFocus, hasThreadFocus, composing, connected, stateEstablished)
    Expect(Decide(false, false, true, false, false, false) == Action::StopNoFocus, "无焦点不同步");
    Expect(Decide(true, false, true, false, false, false) == Action::StopNoFocus, "重试时无焦点也停");
    Expect(Decide(false, true, false, false, false, false) == Action::Sync, "首次同步不看前台");
    Expect(Decide(false, true, true, false, true, true) == Action::Sync,
           "首次同步不看已同步标记（服务重启后旧连接的标记不可信）");
    Expect(Decide(true, true, true, false, true, true) == Action::StopAlreadySynced,
           "按键路径已在本连接发过 IME_ACTIVATED：过期重试不得再发（会重置用户刚切的中英）");
    Expect(Decide(true, true, true, false, true, false) == Action::Sync,
           "连上了但还没同步：照常重试");
    Expect(Decide(true, true, true, false, false, true) == Action::Sync,
           "未连接时已同步标记无意义：照常重试");
    Expect(Decide(true, true, false, false, false, false) == Action::StopBackground, "后台线程不重试");
    Expect(Decide(true, true, true, true, false, false) == Action::DeferComposition, "重试遇组合态推迟");
    Expect(Decide(false, true, false, true, false, false) == Action::DeferComposition, "首次遇组合态推迟");

    // DecideFocusRecovery(needsRecovery, hasFocus, hasThreadFocus, connected, stateEstablished)
    Expect(DecideFocusRecovery(true, true, true, false, false) == FocusAction::Resend,
           "焦点补发：断开着就连上补发 focus_gained");
    Expect(DecideFocusRecovery(true, true, true, true, false) == FocusAction::Resend,
           "焦点补发：连上了但本连接还没往来，照常补发");
    Expect(DecideFocusRecovery(true, true, true, true, true) == FocusAction::StopAlreadySynced,
           "焦点补发：窗口期内按键已在本连接往来过，停手（不得再发任何会重置状态的消息）");
    Expect(DecideFocusRecovery(false, true, true, false, false) == FocusAction::StopRecovered,
           "焦点补发：别的路径已补上就停");
    Expect(DecideFocusRecovery(true, false, true, false, false) == FocusAction::StopNoFocus,
           "焦点补发：焦点没了就停");
    Expect(DecideFocusRecovery(true, true, false, false, false) == FocusAction::StopBackground,
           "焦点补发：后台线程不补");

    // 焦点补发的判决里没有「全量同步」这一档：穷举输入，结果只可能是补发或停，
    // 且只要本连接已有往来（含按键）就绝不补发。
    bool neverResendAfterTraffic = true;
    for (int m = 0; m < 32; ++m)
    {
        const bool needs = m & 1, focus = m & 2, thread = m & 4, conn = m & 8, est = m & 16;
        if (conn && est && DecideFocusRecovery(needs, focus, thread, conn, est) == FocusAction::Resend)
            neverResendAfterTraffic = false;
    }
    Expect(neverResendAfterTraffic, "焦点补发：本连接已有往来时任何输入组合都不补发");

    if (g_failures == 0)
        std::printf("service_ready_retry_policy_test: all passed\n");
    return g_failures == 0 ? 0 : 1;
}
