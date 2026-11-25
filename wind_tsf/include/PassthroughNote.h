#pragma once

#include <cstdint>

// ============================================================================
// PassthroughNote —— 「自上一个 keydown 送达服务端以来，有键进了宿主」的记账
// ============================================================================
//
// 服务端的智能符号 press2 判定要知道 press1 与 press2 之间有没有别的输入。英文半角下字母数字
// 直接透传给宿主，服务端看不见，由 DLL 经 KeyPayload 的 `TOGGLE_PASSTHROUGH_KEY` 位上报。
// DLL 报事实，服务端持策略（同 `_lastPassthroughDigit`）。
//
// 失效方向：漏报会让 `ReplaceBackward` 删掉用户刚打的字，不可接受；多报只让用户重按一次
// press1，可接受。但凡是我们自己造出来的键一律不记：`CommitText` 的 SendInput 兜底
// （`VK_PACKET`）若被记账，智能符号在走兜底的宿主（微信 / 终端）上会整条失效。
//
// 不含 Win32 头，以便 `tests/passthrough_note_test.cpp` 用 g++ 直接编译；VK 字面量由
// `KeyEventSink.h` 的 `static_assert` 与 `<winuser.h>` 对账。与 `SkipKeyTable.h` 同一局限：
// 只管记账语义，守卫挂在哪、`Suppress()` 在哪调，单测覆盖不到。
namespace WindPassthrough
{

// 与 <winuser.h> 同值（KeyEventSink.h 有 static_assert 对账）。
constexpr uint32_t kVkShift = 0x10;
constexpr uint32_t kVkControl = 0x11;
constexpr uint32_t kVkMenu = 0x12;
constexpr uint32_t kVkLWin = 0x5B;
constexpr uint32_t kVkRWin = 0x5C;
constexpr uint32_t kVkLShift = 0xA0;
constexpr uint32_t kVkRShift = 0xA1;
constexpr uint32_t kVkLControl = 0xA2;
constexpr uint32_t kVkRControl = 0xA3;
constexpr uint32_t kVkLMenu = 0xA4;
constexpr uint32_t kVkRMenu = 0xA5;

// SendInput 的 Unicode 注入载体（KEYEVENTF_UNICODE 恒发这个 VK）。
constexpr uint32_t kVkPacket = 0xE7;
// 本 IME 自注入的「异步提交触发键」（KeyEventSink.h 的 VK_ASYNC_COMMIT_TRIGGER）。
constexpr uint32_t kVkAsyncCommitTrigger = 0xE8;
// 服务进程的 Alt 钩子在补发的 Alt 松开前注入的屏蔽键（压住宿主菜单栏），见 wind-keys 的
// `key_inject::VK_MENU_MASK`。宿主必须看得到它，KeyEventSink 一律放行、不转发。
constexpr uint32_t kVkMenuMask = 0x9F;

/// 纯修饰键：按下它不往文档里写东西，故不记账，否则按住 Shift 连按两次 `？` 会被 Shift 解除。
/// 比 `KeyEventSink.cpp` 的 `_IsPureModifierKey` 多 Win 键：那个答「该不该放行给宿主」，这个答
/// 「文档会不会变」。问题不同，别合并。
inline bool IsBareModifier(uint32_t vk)
{
    switch (vk)
    {
    case kVkShift:    case kVkControl:  case kVkMenu:
    case kVkLShift:   case kVkRShift:
    case kVkLControl: case kVkRControl:
    case kVkLMenu:    case kVkRMenu:
    case kVkLWin:     case kVkRWin:
        return true;
    default:
        return false;
    }
}

/// 「这个键是我们自己造出来的」，按 VK 值判，与 skip 表无关。
///
/// Chrome / QQ 等宿主无视 `pfEaten=FALSE` 仍调 `OnKeyDown`，那时 skip 条目已被 test 消费，
/// 守卫无从抑制，故需这道与消费点无关的兜底。只列 0xE7 / 0xE8 / 0x9F：没有物理键能产生它们。
///
/// 别改成「记住刚抑制过的 vk + 时间窗」：那会纳入 `VK_RIGHT` / `VK_BACK` 等物理键，备忘残留会
/// 误抑制下一次真实按键（漏报）；而它们的多报只是一次性的，本在可接受侧。
inline bool IsSelfInjectedVk(uint32_t vk)
{
    return vk == kVkPacket || vk == kVkAsyncCommitTrigger || vk == kVkMenuMask;
}

/// 这一 keydown 是否算「用户的键进了宿主」（排除项见文件头）。数字后智能标点的输入序列
/// 记账（KeyEventSink::_NoteInputSequenceAtExit）用的也是这一判据，两者必须同口径。
inline bool IsUserKeyIntoHost(uint32_t vk, bool eaten, bool suppressed)
{
    return !eaten && !suppressed && !IsSelfInjectedVk(vk) && !IsBareModifier(vk);
}

/// 记账状态：一个布尔 + 三条规则。放进类里是为了让规则可被单测钉住（见文件头的局限说明）。
class PassthroughState
{
public:
    /// 一次 keydown 的结论。
    ///   `eaten`      —— 最终的 `*pfEaten`；吃下了就不是「进了宿主」。
    ///   `suppressed` —— skip 表命中（我们自己注入的键）。
    ///
    /// 抑制优先于记账：`suppressed` 与 `IsSelfInjectedVk` 任一成立即不记。
    void NoteKeyDown(uint32_t vk, bool eaten, bool suppressed)
    {
        if (IsUserKeyIntoHost(vk, eaten, suppressed))
            _pending = true;
    }

    /// keydown 事件发往服务端时消费：返回该不该置 `TOGGLE_PASSTHROUGH_KEY`，并清零。
    ///
    /// keyup 不得调用：toggle 键的 keyup 也走 `_SendKeyToService`，服务端不读它的该位，
    /// 事实会被丢掉（中间按过 Shift 就又能误删一次）。事实永远归下一个 keydown。
    bool TakeOnKeyDownSend()
    {
        bool p = _pending;
        _pending = false;
        return p;
    }

    /// 发送失败（IPC 断开 / 服务端重启）：事实从未送达，必须放回，否则漏报。
    void RestoreOnSendFailure() { _pending = true; }

    /// 焦点 / 文档切换：与 skip 表同处清理（`ResetComposingState`），不把陈旧事实带过去。
    void Reset() { _pending = false; }

    bool Pending() const { return _pending; }

private:
    bool _pending = false;
};

} // namespace WindPassthrough
