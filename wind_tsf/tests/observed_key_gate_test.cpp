// ObservedKeyGate 单元测试
//
// 跑法（纯 C++17、不含 Win32 头）：
//   g++ -std=c++17 -I../include -o observed_key_gate_test observed_key_gate_test.cpp && ./observed_key_gate_test
//
// 守的是扩展旁路上报的去重：同一次按键只报一次，长按连发照报，宿主跳过 Test 的也不漏。
// 接线（守卫挂在哪、enabled 怎么算）测不到，见 KeyEventSink.cpp。

#include "ObservedKeyGate.h"

#include <cstdio>

namespace
{

using WindObserve::ObservedKeyGate;

constexpr uint32_t VK_A_ = 0x41;
constexpr uint32_t VK_B_ = 0x42;
constexpr uint32_t VK_SHIFT_ = 0x10;
constexpr uint32_t VK_PACKET_ = 0xE7;

int g_failures = 0;

void check(bool ok, const char* what)
{
    if (!ok)
    {
        std::printf("FAIL: %s\n", what);
        ++g_failures;
    }
}

void test_then_down_reports_once()
{
    ObservedKeyGate g;
    check(g.OnTestExit(VK_A_, false, false, false, true), "Test 出口报");
    check(!g.OnDownExit(VK_A_, false, false, false, true), "紧随的 Down 不重复报");
}

void repeated_test_without_keyup_is_deduped()
{
    ObservedKeyGate g;
    check(g.OnTestExit(VK_A_, false, false, false, true), "首次 Test 报");
    check(!g.OnTestExit(VK_A_, false, false, false, true), "宿主重复 Test 不报");
}

void auto_repeat_is_reported()
{
    ObservedKeyGate g;
    g.OnTestExit(VK_A_, false, false, false, true);
    g.OnDownExit(VK_A_, false, false, false, true);
    check(g.OnTestExit(VK_A_, false, false, true, true), "长按连发照报");
}

void keyup_rearms_same_key()
{
    ObservedKeyGate g;
    g.OnTestExit(VK_A_, false, false, false, true);
    g.OnKeyUp(VK_A_);
    check(g.OnTestExit(VK_A_, false, false, false, true), "松开后再按同一键照报");
}

void down_without_test_is_reported()
{
    ObservedKeyGate g;
    check(g.OnDownExit(VK_B_, false, false, false, true), "宿主跳过 Test 时 Down 出口报");
    check(!g.OnDownExit(VK_B_, false, false, false, true), "同一键没松开、非连发不重复报");
}

void down_only_auto_repeat_is_reported()
{
    ObservedKeyGate g;
    check(g.OnDownExit(VK_B_, false, false, false, true), "宿主跳过 Test：首按报");
    check(g.OnDownExit(VK_B_, false, false, true, true), "宿主跳过 Test：长按连发照报");
    check(g.OnDownExit(VK_B_, false, false, true, true), "宿主跳过 Test：连发每次都报");
}

void test_then_down_on_repeat_reports_once()
{
    ObservedKeyGate g;
    g.OnTestExit(VK_A_, false, false, false, true);
    g.OnDownExit(VK_A_, false, false, false, true);
    check(g.OnTestExit(VK_A_, false, false, true, true), "连发的 Test 报");
    check(!g.OnDownExit(VK_A_, false, false, true, true), "同一次连发的 Down 不重复报");
}

void key_sent_to_core_is_not_reported_again()
{
    ObservedKeyGate g;
    // Test 先吃下，Down 发给核心，核心回放行：核心已报过，旁路不再报。
    check(!g.OnTestExit(VK_A_, true, false, false, true), "Test 吃下");
    g.OnSentToCore(VK_A_);
    check(!g.OnDownExit(VK_A_, false, false, false, true), "核心放行的键不重复报");
    // 连发同样走核心：每一下都不旁路报。
    check(!g.OnTestExit(VK_A_, true, false, true, true), "连发 Test 吃下");
    g.OnSentToCore(VK_A_);
    check(!g.OnDownExit(VK_A_, false, false, true, true), "连发的核心放行键不报");
    g.OnKeyUp(VK_A_);
    // 下一按没发给核心（比如切到了英文）：照报。
    check(g.OnTestExit(VK_A_, false, false, false, true), "没发给核心的键照报");
}

void key_sent_to_core_during_test_is_not_reported()
{
    ObservedKeyGate g;
    g.OnSentToCore(VK_B_);
    check(!g.OnTestExit(VK_B_, false, false, false, true), "Test 里已发给核心");
    check(!g.OnDownExit(VK_B_, false, false, false, true), "同一按的 Down 也不报");
}

void eaten_modifier_self_injected_or_disabled_are_silent()
{
    ObservedKeyGate g;
    check(!g.OnTestExit(VK_A_, true, false, false, true), "吃下的键走核心，不旁路报");
    check(!g.OnTestExit(VK_SHIFT_, false, false, false, true), "纯修饰键不报");
    check(!g.OnTestExit(VK_PACKET_, false, false, false, true), "自注入键不报");
    check(!g.OnTestExit(VK_A_, false, true, false, true), "skip 表放行的键不报");
    check(!g.OnTestExit(VK_A_, false, false, false, false), "未推开 / 密码框 / 禁用时不报");
}

void different_key_in_between_rearms()
{
    ObservedKeyGate g;
    g.OnTestExit(VK_A_, false, false, false, true);
    g.OnTestExit(VK_B_, false, false, false, true);
    check(g.OnTestExit(VK_A_, false, false, false, true), "中间按过别的键，A 再按照报");
}

} // namespace

int main()
{
    test_then_down_reports_once();
    repeated_test_without_keyup_is_deduped();
    auto_repeat_is_reported();
    keyup_rearms_same_key();
    down_without_test_is_reported();
    down_only_auto_repeat_is_reported();
    test_then_down_on_repeat_reports_once();
    key_sent_to_core_is_not_reported_again();
    key_sent_to_core_during_test_is_not_reported();
    eaten_modifier_self_injected_or_disabled_are_silent();
    different_key_in_between_rearms();
    if (g_failures == 0)
        std::printf("observed_key_gate_test: all passed\n");
    return g_failures == 0 ? 0 : 1;
}
