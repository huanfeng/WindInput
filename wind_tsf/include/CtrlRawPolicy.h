#pragma once

// 候选显示时 Ctrl 的如实上报（KEY_EVENT_MOD_DOWN / KEY_EVENT_MOD_UP）的按住记账。
//
// 规则（协议权威见 protocol.rs 的 EVENT_MOD_DOWN）：
// - **按下那一刻**有输入会话 ⇒ 这次按住走如实上报：按下发 MOD_DOWN，匹配的松开发 MOD_UP（哪怕
//   那时候选已关），**不再**记旧的待判单击；否则走旧路（_MarkPendingToggleKey → 判过单击才送
//   KEY_EVENT_UP）。一次按住只走一条路，服务端不会从两边各收到一次。
// - 平台不吃 Ctrl，两向都放行给宿主。
// - 按住期间 TSF 看见的别的键（含不送服务端的透传键、WM_HOTKEY 的 Ctrl+数字）与鼠标操作记成
//   dirty，随 MOD_UP 以 TOGGLE_MOD_NOT_TAP 报给服务端；时长由服务端判。
//
// 纯 C++17、不含 Win32 头：判据在 tests/ctrl_raw_policy_test.cpp 里用 g++ 直接测。

#include <cstdint>

namespace wind::ctrlraw
{
constexpr uint32_t kVkLControl = 0xA2;
constexpr uint32_t kVkRControl = 0xA3;

// 一次如实上报中的按住。vk == 0 = 没有。
struct Press
{
    uint32_t vk = 0;
    bool dirty = false;
};

enum class Down
{
    Legacy, // 按下时无会话（或认不出左右）：走旧的待判单击
    Start,  // 新的一次如实上报：发 MOD_DOWN
    Repeat, // 同一次按住的重复 keydown（自动连发、宿主重复回调首按）：什么都不发
};

// Ctrl 按下。vk 是解析出的左右键（认不出给 0）；autoRepeat = lParam 第 30 位（这个键此前已按着）；
// legacyPendingVk = 旧路的待判单击（_pendingKeyUpKey）。
//
// 路只在一次按住的**第一下**定：旧路按住中（自动连发、或旧路还记着这个键）即使此刻有了会话也
// 不改走如实上报——否则松开时两边各报一次。
//
// 同侧记账还在时：自动连发、或记账干净（宿主重复回调首按）才算 Repeat；弄脏过又来一个非连发的
// 按下 = 上次松开丢了（Ctrl+Alt+Del、Ctrl+点击切走），丢掉残留按新的一次按下重判，与服务端
// `on_ctrl_raw_down` 同一判据。
inline Down OnCtrlDown(Press& p, uint32_t vk, bool hasSession, bool autoRepeat, uint32_t legacyPendingVk)
{
    if (vk != kVkLControl && vk != kVkRControl)
        return Down::Legacy;
    if (p.vk == vk)
    {
        if (autoRepeat || !p.dirty)
            return Down::Repeat;
        p = {};
    }
    if (!hasSession || autoRepeat || legacyPendingVk == vk)
    {
        // 另一侧 Ctrl 正在如实上报：这一侧按下对它是「别的键」。
        if (p.vk != 0)
            p.dirty = true;
        return Down::Legacy;
    }
    p.vk = vk;
    p.dirty = false;
    return Down::Start;
}

// 按住期间的别的键（含另一个修饰键、WM_HOTKEY 分发）。
inline void OnOtherKey(Press& p)
{
    if (p.vk != 0)
        p.dirty = true;
}

// WM_HOTKEY 分发的组合键（Ctrl+数字置顶、Ctrl+Shift+数字删除、加词）：不经按键回调，
// 旧的待判单击（pendingKey）与如实上报都在这里作废——否则 500ms 内松开 Ctrl 会被当成单击。
inline void OnHotkeyDispatched(Press& p, uint32_t& pendingKey)
{
    pendingKey = 0;
    OnOtherKey(p);
}

// 线程失焦（OnKillThreadFocus）：松开多半送不到了，丢掉记账（服务端失焦时也会收起按住翻转）。
inline void OnFocusLost(Press& p)
{
    p = {};
}

struct Up
{
    bool report = false; // 要发 MOD_UP
    bool notTap = false; // 带 TOGGLE_MOD_NOT_TAP
    uint32_t vk = 0;
};

// Ctrl 松开。matches = 松开的正是 p.vk 那一侧（调用方按 wParam / 异步键态判）。
// 命中即清掉记账；没命中（不是如实上报的那次按住）返回 report=false，交给旧路。
inline Up OnCtrlUp(Press& p, bool matches, bool mouseCancelled)
{
    if (p.vk == 0 || !matches)
        return {};
    Up r;
    r.report = true;
    r.notTap = p.dirty || mouseCancelled;
    r.vk = p.vk;
    p = {};
    return r;
}
} // namespace wind::ctrlraw
