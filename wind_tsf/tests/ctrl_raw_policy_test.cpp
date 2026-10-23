// CtrlRawPolicy 判据测试
//
// 跑法（纯 C++17、不含 Win32 头）：
//   g++ -std=c++17 -I../include -o ctrl_raw_test ctrl_raw_policy_test.cpp && ./ctrl_raw_test
//
// 变异检验：OnCtrlDown 去掉 Repeat 判断 ⇒ TestRepeatDoesNotRestart 红；Repeat 不看 dirty ⇒
// TestStaleDirtyPressIsReplaced 红；OnHotkeyDispatched 不清
// pendingKey ⇒ TestHotkeyCancelsLegacyPending 红；OnCtrlUp 不看 matches ⇒ TestOtherSideUpIsNotOurs 红。

#include "CtrlRawPolicy.h"

#include <cstdio>

namespace
{
using namespace wind::ctrlraw;

int g_failures = 0;

void Expect(bool cond, const char* what)
{
    if (!cond)
    {
        std::printf("FAIL: %s\n", what);
        ++g_failures;
    }
}

void TestSessionStartsRawPress()
{
    Press p;
    Expect(OnCtrlDown(p, kVkLControl, true, false, 0) == Down::Start, "有会话按下 ⇒ 如实上报");
    Up u = OnCtrlUp(p, true, false);
    Expect(u.report && !u.notTap && u.vk == kVkLControl, "干净松开 ⇒ 报 MOD_UP、不带 NOT_TAP");
    Expect(p.vk == 0, "松开后清掉记账");
}

void TestNoSessionStaysLegacy()
{
    Press p;
    Expect(OnCtrlDown(p, kVkRControl, false, false, 0) == Down::Legacy, "无会话按下 ⇒ 旧路");
    Expect(!OnCtrlUp(p, true, false).report, "旧路的松开不报 MOD_UP");
}

// 配对规则：按下时定路，松开时会话变了也不改。
void TestPairingDecidedAtPress()
{
    Press p;
    OnCtrlDown(p, kVkLControl, true, false, 0);
    // 按住期间上屏关了窗：松开仍报 MOD_UP（松开不看会话）。
    Expect(OnCtrlUp(p, true, false).report, "按下时有会话 ⇒ 松开照报，哪怕候选已关");

    // 旧路按住中候选窗出现（按住期间异步开了组合）：后续 keydown 不改走如实上报。
    Press q;
    Expect(OnCtrlDown(q, kVkLControl, false, false, 0) == Down::Legacy, "第一下无会话 ⇒ 旧路");
    Expect(OnCtrlDown(q, kVkLControl, true, true, 0) == Down::Legacy, "自动连发不改路");
    Expect(OnCtrlDown(q, kVkLControl, true, false, kVkLControl) == Down::Legacy,
           "旧路还记着这个键（宿主重复回调首按）不改路");
    Expect(q.vk == 0, "旧路不留如实上报记账 ⇒ 松开只走旧路");
}

void TestRepeatDoesNotRestart()
{
    Press p;
    OnCtrlDown(p, kVkLControl, true, false, 0);
    Expect(OnCtrlDown(p, kVkLControl, true, false, 0) == Down::Repeat, "干净时宿主重复回调首按 ⇒ Repeat");
    OnOtherKey(p);
    Expect(OnCtrlDown(p, kVkLControl, true, true, 0) == Down::Repeat, "自动连发 ⇒ Repeat，不再发 MOD_DOWN");
    Expect(p.dirty, "Repeat 不洗掉 dirty");
}

void TestOtherKeyMarksNotTap()
{
    Press p;
    OnCtrlDown(p, kVkLControl, true, false, 0);
    OnOtherKey(p); // Ctrl+C 透传给宿主，服务端看不见
    Up u = OnCtrlUp(p, true, false);
    Expect(u.report && u.notTap, "期间按过别的键 ⇒ NOT_TAP");
}

void TestMouseMarksNotTap()
{
    Press p;
    OnCtrlDown(p, kVkLControl, true, false, 0);
    Expect(OnCtrlUp(p, true, true).notTap, "Ctrl+点击 ⇒ NOT_TAP");
}

void TestOtherKeyWithoutPressIsNoop()
{
    Press p;
    OnOtherKey(p);
    Expect(p.vk == 0 && !p.dirty, "没在按住时别的键不留痕");
}

void TestHotkeyCancelsLegacyPending()
{
    // 旧路：候选窗不显示时 lctrl 记了待判单击，Ctrl+1 经 WM_HOTKEY 到达。
    Press p;
    uint32_t pending = kVkLControl;
    OnHotkeyDispatched(p, pending);
    Expect(pending == 0, "WM_HOTKEY 作废旧的待判单击");

    // 如实上报：同样作废（报 NOT_TAP）。
    Press q;
    uint32_t none = 0;
    OnCtrlDown(q, kVkLControl, true, false, 0);
    OnHotkeyDispatched(q, none);
    Expect(OnCtrlUp(q, true, false).notTap, "WM_HOTKEY 之后的松开 ⇒ NOT_TAP");
}

void TestOtherSideUpIsNotOurs()
{
    Press p;
    OnCtrlDown(p, kVkLControl, true, false, 0);
    Expect(!OnCtrlUp(p, false, false).report, "另一侧的松开不算这次按住");
    Expect(p.vk == kVkLControl, "记账保留到本侧松开");
}

void TestOtherSideDownWithoutSessionDirties()
{
    Press p;
    OnCtrlDown(p, kVkLControl, true, false, 0);
    Expect(OnCtrlDown(p, kVkRControl, false, false, 0) == Down::Legacy, "另一侧无会话按下 ⇒ 旧路");
    Expect(OnCtrlUp(p, true, false).notTap, "左 Ctrl 的这次按住被打断");
}

// 丢了一次松开（Ctrl+Alt+Del、Ctrl+点击切走）：残留是脏的，下一次同侧按下按新的一次重判。
void TestStaleDirtyPressIsReplaced()
{
    Press p;
    OnCtrlDown(p, kVkLControl, true, false, 0);
    OnOtherKey(p); // Alt / Del
    // 松开没送到。回来时无会话：走旧路（lctrl 中英切换照常），残留不再拦着。
    Expect(OnCtrlDown(p, kVkLControl, false, false, 0) == Down::Legacy, "残留 + 无会话 ⇒ 旧路");
    Expect(p.vk == 0, "残留已丢");
    Expect(!OnCtrlUp(p, true, false).report, "旧路的松开不报 MOD_UP");

    // 回来时有会话：重新起一次干净的如实上报。
    Press q;
    OnCtrlDown(q, kVkLControl, true, false, 0);
    OnOtherKey(q);
    Expect(OnCtrlDown(q, kVkLControl, true, false, 0) == Down::Start, "残留 + 有会话 ⇒ 新的一次");
    Up u = OnCtrlUp(q, true, false);
    Expect(u.report && !u.notTap, "新的一次是干净的");

    // 脏但是自动连发：仍是同一次按住。
    Press r;
    OnCtrlDown(r, kVkLControl, true, false, 0);
    OnOtherKey(r);
    Expect(OnCtrlDown(r, kVkLControl, true, true, 0) == Down::Repeat, "脏 + 自动连发 ⇒ Repeat");
}

void TestFocusLostDropsPress()
{
    Press p;
    OnCtrlDown(p, kVkLControl, true, false, 0);
    OnFocusLost(p);
    Expect(OnCtrlDown(p, kVkLControl, false, false, 0) == Down::Legacy, "失焦后再按下按新的一次判");
}

void TestUnresolvedSideIsLegacy()
{
    Press p;
    Expect(OnCtrlDown(p, 0, true, false, 0) == Down::Legacy, "认不出左右 ⇒ 旧路");
    Expect(p.vk == 0, "不留记账");
}
} // namespace

int main()
{
    TestSessionStartsRawPress();
    TestNoSessionStaysLegacy();
    TestPairingDecidedAtPress();
    TestRepeatDoesNotRestart();
    TestOtherKeyMarksNotTap();
    TestMouseMarksNotTap();
    TestOtherKeyWithoutPressIsNoop();
    TestHotkeyCancelsLegacyPending();
    TestOtherSideUpIsNotOurs();
    TestOtherSideDownWithoutSessionDirties();
    TestUnresolvedSideIsLegacy();
    TestStaleDirtyPressIsReplaced();
    TestFocusLostDropsPress();
    if (g_failures == 0)
        std::printf("ctrl_raw_policy_test: all passed\n");
    return g_failures == 0 ? 0 : 1;
}
