// HoldReplayPolicy 判据测试（A2-65 / 论坛 t263：组合态预览期间 Ctrl+V 在微信、WPS 表格粘贴无效）
//
// 跑法（纯 C++17、不含 Win32 头）：
//   g++ -std=c++17 -Wall -Wextra -I../include -o hold_replay_test hold_replay_policy_test.cpp && ./hold_replay_test
//
// 变异检验：去掉 isCtrlAltCleanup 项 ⇒ TestCtrlComboReplayed 红；去掉 holdActiveBeforeResponse
// ⇒ TestNoHoldNoReplay 红；去掉 !eatenByResponse ⇒ TestServiceConsumedNoReplay 红。

#include "HoldReplayPolicy.h"

#include <cstdio>

namespace
{
using wind::holdreplay::ShouldReplayAfterHold;

int g_failures = 0;

void Expect(bool cond, const char* what)
{
    if (!cond)
    {
        std::printf("FAIL: %s\n", what);
        ++g_failures;
    }
}

// 既有行为：hold + PassThrough + 回车 / 数字等功能键 ⇒ 重放。
void TestReplayKeyReplayed()
{
    Expect(ShouldReplayAfterHold(true, false, true, false), "hold 期间回车类键应重放");
}

// A2-65：hold + PassThrough + Ctrl/Alt 组合（Ctrl+V / Ctrl+S / Alt+F）⇒ 重放，不能吐成 FALSE。
void TestCtrlComboReplayed()
{
    Expect(ShouldReplayAfterHold(true, false, false, true), "hold 期间 Ctrl/Alt 组合应重放");
}

// 无 hold：普通会话里的 Ctrl 组合不归本判据管（那条路的服务端响应未实测，不动）。
void TestNoHoldNoReplay()
{
    Expect(!ShouldReplayAfterHold(false, false, false, true), "无 hold 时 Ctrl 组合不重放");
    Expect(!ShouldReplayAfterHold(false, false, true, false), "无 hold 时回车类键不重放");
}

// 服务端已消费（pfEaten 为真，如全角空格出字、候选操作）⇒ 不重放，否则宿主多收一次。
void TestServiceConsumedNoReplay()
{
    Expect(!ShouldReplayAfterHold(true, true, false, true), "服务端已消费的 Ctrl 组合不重放");
    Expect(!ShouldReplayAfterHold(true, true, true, false), "服务端已消费的回车类键不重放");
}

// 普通字母等既非重放键、也非 Ctrl/Alt 组合 ⇒ 不重放（保持原行为）。
void TestOtherKeysUnchanged()
{
    Expect(!ShouldReplayAfterHold(true, false, false, false), "非重放键、非 Ctrl/Alt 组合不重放");
}

} // namespace

int main()
{
    TestReplayKeyReplayed();
    TestCtrlComboReplayed();
    TestNoHoldNoReplay();
    TestServiceConsumedNoReplay();
    TestOtherKeysUnchanged();

    if (g_failures == 0)
    {
        std::printf("hold_replay_policy_test: all passed\n");
        return 0;
    }
    std::printf("hold_replay_policy_test: %d failure(s)\n", g_failures);
    return 1;
}
