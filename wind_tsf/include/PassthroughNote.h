#pragma once

#include <cstdint>

// ============================================================================
// PassthroughNote —— 「自上一个 keydown 送达服务端以来，有键进了宿主」的记账
// ============================================================================
//
// 服务端的智能符号 press2 判定要知道「press1 与 press2 之间有没有别的输入」。中文模式下
// 它自己看得见（编码缓冲、按键事件），但**英文半角**下看不见：TSF 只吃标点键，字母数字
// 直接透传给宿主，服务端既无事件也无缓冲。只有 DLL 知道这件事，于是由它经 KeyPayload
// 的 `toggles` 位（`TOGGLE_PASSTHROUGH_KEY`）如实上报。分工同 `_lastPassthroughDigit`：
// **DLL 报事实，服务端持策略**。
//
// ── 判据必须失效安全，但「多报」的代价分两种 ────────────────────────────────────
//
// 漏报 ⇒ 服务端把 press2 判成真，`ReplaceBackward` 删掉用户刚打的字。**不可接受**。
// 多报 ⇒ 武装态被解除，用户重按一次 press1。可接受——**但只在触发源是用户按键时**。
//
// ★ 若触发源是**我们自己的注入机制**，多报就从「一次性不便」变成「系统性失效」：
//   `CTextService::CommitText` 在 TSF 提交失败时回退 SendInput（每字符一个 `VK_PACKET`），
//   于是 press1 每次上屏都把自己注入的那串字符记成「用户输入」，press2 必被解除 ⇒ 智能
//   符号在那类宿主上整条功能失效。而走这条兜底的恰恰是 `prev_char` 读不回的那批宿主
//   （微信 / 终端），也就是本功能最需要生效的地方，且换个宿主就好了、极难归因。
//
// ⇒ 记账的排除项按这条规则取舍：**凡「我们自己造出来的键」一律不记**，其余一律记。
//
// ── 本文件不含任何 Win32 头 ─────────────────────────────────────────────────────
//
// 同 `SkipKeyTable.h`：为的是能被 `tests/passthrough_note_test.cpp` 在非 Windows 工具链下
// 直接编译运行（`g++ -std=c++17 -Iwind_tsf/include`）。VK 常量在此以字面量给出，
// `KeyEventSink.h` 里有 `static_assert` 把它们与 `<winuser.h>` 的 `VK_*` 钉死，漂了编不过。
//
// ⚠️ 与 `SkipKeyTable.h` 同一条局限：**本文件只管记账语义，管不了接线**。守卫挂在哪几个
// 函数、`Suppress()` 在哪个分支调，这些单测覆盖不到，只能靠 `KeyEventSink.cpp` 那边的
// 注释与真机验证。
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

/// 纯修饰键：按下它本身不往文档里写东西，故不记账——否则「按住 Shift 连按两次 `？`」
/// 这类正常 press2 会被自己中间的 Shift 解除掉。
///
/// 比 `KeyEventSink.cpp` 的 `_IsPureModifierKey` **多 Win 键**：那个谓词答的是「该不该
/// 放行给宿主」（Win 键有系统级语义，不归它管），这个答的是「按下它文档会不会变」。
/// 两个问题不同，集合因此不同；共用一个反而会在将来某次「统一」时静默改掉其中一边。
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

/// 「这个键是我们自己造出来的」——**按 VK 值判，与 skip 表无关**。
///
/// skip 表那条通路（`_TryConsumeSkipKey` 命中即 `Suppress()`）只在 `OnTestKeyDown` 上，
/// 而 Chrome / QQ 等宿主会**无视 `pfEaten=FALSE` 仍调 `OnKeyDown`**（见 KeyEventSink.cpp
/// 里那两处点名注释）。那时 skip 条目已被 test 消费掉，`OnKeyDown` 的守卫无从抑制 ⇒
/// 注入的 `VK_PACKET` 照样置位 ⇒ 上面说的系统性失效在那批宿主上并没有被堵住。
///
/// 故这里再加一道**与消费点无关**的兜底。只列 0xE7 / 0xE8：两者都没有物理键能产生，
/// 误抑制真实用户输入的风险为零。
///
/// ⚠️ 为什么不做「记住刚抑制过的 vk + 时间窗」那种更通用的备忘：那会把
/// `_SimulatePairKey` 的 `VK_RIGHT`、`ReplacePrecedingChars` 的 `VK_BACK` 一并纳入，而它们
/// 是**真实存在的物理键**——备忘残留（宿主没调 OnKeyDown 时必然残留）会让紧随其后的一次
/// 真实按键被误抑制，那是**漏报**方向。用「多报一次」换「可能漏报一次」是亏的：那两者
/// 造成的多报都只是一次性的（配对跳出、press2 的删改都发生在动作之后），本就在可接受侧。
inline bool IsSelfInjectedVk(uint32_t vk)
{
    return vk == kVkPacket || vk == kVkAsyncCommitTrigger;
}

/// 记账状态：一个布尔 + 三条规则。放进类里是为了让规则可被单测钉住（见文件头的局限说明）。
class PassthroughState
{
public:
    /// 一次 keydown 的结论。
    ///   `eaten`      —— 最终的 `*pfEaten`；吃下了就不是「进了宿主」。
    ///   `suppressed` —— skip 表命中（我们自己注入的键）。
    ///
    /// ★ 抑制**优先于**记账：两个排除项（`suppressed` 与 `IsSelfInjectedVk`）任一成立即不记。
    void NoteKeyDown(uint32_t vk, bool eaten, bool suppressed)
    {
        if (eaten || suppressed || IsSelfInjectedVk(vk) || IsBareModifier(vk))
            return;
        _pending = true;
    }

    /// keydown 事件发往服务端时消费：返回该不该置 `TOGGLE_PASSTHROUGH_KEY`，并清零。
    ///
    /// ⚠️ **keyup 不得调用本函数**。toggle 键的 keyup 也会走 `_SendKeyToService`，让它顺手
    /// 消费会把事实丢在一个服务端根本不读该位的事件上——症状是「中间按过 Shift 就又能误删
    /// 一次」。事实的归属者永远是下一个 keydown。
    bool TakeOnKeyDownSend()
    {
        bool p = _pending;
        _pending = false;
        return p;
    }

    /// 发送失败（IPC 断开 / 服务端重启）：事实从未送达，必须放回去。
    /// 不放回就是**漏报**——按本文件开头立的失效方向，那是唯一不可接受的方向。
    void RestoreOnSendFailure() { _pending = true; }

    /// 焦点 / 文档切换：与 skip 表同处清理（`ResetComposingState`）。跨应用带一个陈旧的
    /// 事实过去没有意义，虽然服务端那边焦点变更也会解除武装、实际无害。
    void Reset() { _pending = false; }

    bool Pending() const { return _pending; }

private:
    bool _pending = false;
};

} // namespace WindPassthrough
