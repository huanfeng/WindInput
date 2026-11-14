#pragma once
#include <cstdint>

#include "PassthroughNote.h"

// ============================================================================
// ObservedKeyGate —— 扩展旁路上报（CMD_KEY_OBSERVED）的「这一出口该不该报」
// ============================================================================
//
// 扩展程序要按键事件（按键音等），而英文模式下的字母数字、无会话时的退格回车这些键 DLL
// 直接放行给宿主，核心根本看不见。服务端有扩展订阅按键时推开 CONFIG_KEY_EXTENSION_KEY_OBSERVE，
// DLL 就把**放行给宿主的用户按键**异步报一份（只读通知，不进按键处理）。设计见
// docs/design/extension-hook-channel.md §8.1。
//
// 挂点是 PassthroughNoter 的出口（OnTestKeyDown / OnKeyDown 两处），「是不是用户的键进了宿主」
// 与透传上报同一判据（WindPassthrough::IsUserKeyIntoHost）。本单元只管一件事：**去重**。
//
// ── 为什么要去重 ────────────────────────────────────────────────────────────────
//
// Chrome / QQ 等宿主会无视 OnTestKeyDown 的 pfEaten=FALSE 仍调 OnKeyDown，同一次按键两个出口
// 都判「进了宿主」。OnKeyDown 对放行键做的是与 Test 对称的判定（见 KeyEventSink.cpp 各处
// 「与 OnTestKeyDown 对称」），所以同一个键不会一边报旁路、一边又送核心。
//
// 规则：
//   · Test 出口报过某键，紧随其后的 Down（同一 VK）不再报——那是同一次按键。
//   · Test 被同一 VK 重复调用、中间既没有 keyup 也不是自动重复（lParam 第 30 位为 0），
//     视为宿主重复测试，不再报。
//   · 自动重复（第 30 位为 1）照报：长按连发在宿主里确实打出了多个字。
//   · 宿主跳过 Test 直接调 Down 的，Down 出口照报，连发也照报（有的宿主只在首按调 Test，
//     连发只调 Down；按「上次报过同一 VK」去重会把长按退格的连发全吞掉）。
//   · 发给过核心的键不报：核心不处理而放行时，它自己已经给扩展报过一次（handled=false）；
//     旁路再报，同一键就响两遍——前后差十几毫秒的两个同样的音叠起来是一声「滋」。
//     典型路径：Test 判「可能要处理」先吃下，Down 发给核心，核心回放行，Down 出口未吃。
//   · keyup 清掉记忆。
//
// 不含 Win32 头，tests/observed_key_gate_test.cpp 用 g++ 直接跑。
namespace WindObserve
{

class ObservedKeyGate
{
public:
    /// OnTestKeyDown 出口。`enabled` = 服务端推开 && 非禁用 && 非密码框。返回该不该报。
    bool OnTestExit(uint32_t vk, bool eaten, bool suppressed, bool repeat, bool enabled)
    {
        eaten = eaten || vk == _coreVk;
        if (!enabled || !WindPassthrough::IsUserKeyIntoHost(vk, eaten, suppressed))
        {
            _lastVk = 0;
            _testedVk = 0;
            return false;
        }
        _testedVk = vk; // 紧随的 Down 与本次 Test 是同一次按键
        if (vk == _lastVk && !repeat)
            return false;
        _lastVk = vk;
        return true;
    }

    /// OnKeyDown 出口。
    bool OnDownExit(uint32_t vk, bool eaten, bool suppressed, bool repeat, bool enabled)
    {
        if (vk == _coreVk)
        {
            _coreVk = 0; // 这一按到此结束；下一按（含连发）若再发给核心会重新记上
            eaten = true;
        }
        if (vk == _testedVk)
        {
            _testedVk = 0; // 与刚才的 Test 是同一次按键
            return false;
        }
        if (!enabled || !WindPassthrough::IsUserKeyIntoHost(vk, eaten, suppressed))
            return false;
        if (vk == _lastVk && !repeat)
            return false;
        _lastVk = vk;
        return true;
    }

    /// 这次 keydown 发给了核心（`_SendKeyToService`）。
    void OnSentToCore(uint32_t vk) { _coreVk = vk; }

    void OnKeyUp(uint32_t vk)
    {
        if (vk == _coreVk)
            _coreVk = 0;
        if (vk == _lastVk)
            _lastVk = 0;
        if (vk == _testedVk)
            _testedVk = 0;
    }

private:
    uint32_t _lastVk = 0;
    uint32_t _testedVk = 0;
    uint32_t _coreVk = 0;
};

} // namespace WindObserve
