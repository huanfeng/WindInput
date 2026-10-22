// 组字中 Alt 转发给服务（切换注释态）的**纯判据**。协议权威在协调器 `handle_comment_alt`：
// 只在组字中转发 Alt 的按下与松开（0xA4 / 0xA5），修饰位带上这次按住期间按过的
// Shift / Ctrl / Win（粘滞，按过就一直带到松开）；吃不吃看回包。
//
// ⚠️ 只测判据；按键怎么接、回包怎么用见 KeyEventSink.cpp 的 _TryForwardAlt。
#pragma once

#include <cstdint>

namespace wind
{
namespace altkey
{

constexpr uint32_t kVkMenu = 0x12;
constexpr uint32_t kVkLMenu = 0xA4;
constexpr uint32_t kVkRMenu = 0xA5;

// 与 BinaryProtocol.h 的 KEYMOD_* 同值；此处不含 Win32 头，故自带一份。
constexpr uint32_t kModShift = 0x0001;
constexpr uint32_t kModCtrl = 0x0002;
constexpr uint32_t kModWin = 0x0008;
constexpr uint32_t kModLShift = 0x0010;
constexpr uint32_t kModRShift = 0x0020;
constexpr uint32_t kModLCtrl = 0x0040;
constexpr uint32_t kModRCtrl = 0x0080;
constexpr uint32_t kOtherMods =
    kModShift | kModCtrl | kModWin | kModLShift | kModRShift | kModLCtrl | kModRCtrl;

/// Alt 的 VK 解析成左右（TSF 送的多是笼统 VK_MENU，扩展位为 1 是右 Alt）；不是 Alt 返回 0。
inline uint32_t MapAltVk(uintptr_t wParam, intptr_t lParam)
{
    if (wParam == kVkLMenu || wParam == kVkRMenu)
        return (uint32_t)wParam;
    if (wParam != kVkMenu)
        return 0;
    return (lParam & 0x01000000) ? kVkRMenu : kVkLMenu;
}

/// lParam 的「之前已按下」位：自动连发。
inline bool IsRepeat(intptr_t lParam)
{
    return (lParam & 0x40000000) != 0;
}

/// Alt 按下是否转发：只在组字中。
inline bool ShouldForwardAlt(bool composing)
{
    return composing;
}

/// 修饰键自身的 VK 对应的修饰位（宿主报的修饰位可能取不到刚按下的那个键）。
inline uint32_t ModBitForVk(uint32_t vk)
{
    switch (vk)
    {
    case 0x10: return kModShift;              // VK_SHIFT
    case 0xA0: return kModShift | kModLShift; // VK_LSHIFT
    case 0xA1: return kModShift | kModRShift; // VK_RSHIFT
    case 0x11: return kModCtrl;               // VK_CONTROL
    case 0xA2: return kModCtrl | kModLCtrl;   // VK_LCONTROL
    case 0xA3: return kModCtrl | kModRCtrl;   // VK_RCONTROL
    case 0x5B:                                // VK_LWIN
    case 0x5C: return kModWin;                // VK_RWIN
    default: return 0;
    }
}

/// 一次 Alt 按住的记账：粘滞修饰位，以及按下有没有转发过。
class AltHold
{
public:
    /// Alt 按下。非自动连发即新的一次按住（上次松开没收到时的残留一并清掉）。
    void OnAltDown(bool isRepeat, bool forwarded)
    {
        if (!_active || !isRepeat)
        {
            _active = true;
            _sticky = 0;
            _forwarded = false;
        }
        _forwarded = _forwarded || forwarded;
    }

    /// 按住期间来了别的键（含 WM_HOTKEY 送来的组合键）。
    void NoteKey(uint32_t vk, uint32_t mods)
    {
        if (_active)
            _sticky |= (mods & kOtherMods) | ModBitForVk(vk);
    }

    uint32_t StickyMods() const { return _sticky; }

    /// 松开是否转发：组字中，或按下转发过（按住翻转要靠松开复原，哪怕期间已上屏）。
    bool ShouldForwardUp(bool composing) const { return composing || (_active && _forwarded); }

    void OnAltUp() { *this = AltHold{}; }

private:
    bool _active = false;
    bool _forwarded = false;
    uint32_t _sticky = 0;
};

} // namespace altkey
} // namespace wind
