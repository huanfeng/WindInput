// AltKey 判据测试
//
// 跑法（纯 C++17、不含 Win32 头）：
//   g++ -std=c++17 -I../include -o alt_key_test alt_key_test.cpp && ./alt_key_test

#include "AltKey.h"

#include <cstdio>

namespace
{
using namespace wind::altkey;

int g_failures = 0;

void Expect(bool cond, const char* what)
{
    if (!cond)
    {
        std::printf("FAIL: %s\n", what);
        ++g_failures;
    }
}

constexpr uintptr_t kVkMenu = 0x12;
constexpr intptr_t kExtended = 0x01000000;
constexpr intptr_t kRepeat = 0x40000000;
constexpr uint32_t kShift = 0x0001, kCtrl = 0x0002, kAlt = 0x0004, kWin = 0x0008;
constexpr uint32_t kLShift = 0x0010;

void TestMapAltVk()
{
    Expect(MapAltVk(kVkMenu, kExtended) == 0xA5, "VK_MENU + 扩展位 ⇒ 右 Alt");
    Expect(MapAltVk(kVkMenu, 0) == 0xA4, "VK_MENU 无扩展位 ⇒ 左 Alt");
    Expect(MapAltVk(0xA4, kExtended) == 0xA4, "已是左右 Alt 原样返回");
    Expect(MapAltVk(0xA5, 0) == 0xA5, "已是左右 Alt 原样返回");
    Expect(MapAltVk(0x10, 0) == 0, "非 Alt ⇒ 0");
}

void TestShouldForwardAlt()
{
    Expect(!ShouldForwardAlt(false), "不在组字不转发");
    Expect(ShouldForwardAlt(true), "组字中转发");
}

void TestStickyModifierSurvivesRelease()
{
    AltHold h;
    h.OnAltDown(false, true);
    h.NoteKey(0xA0, kShift | kLShift | kAlt); // 按下左 Shift
    h.NoteKey(0xA0, kAlt);                    // 修饰位已读不到（先松了 Shift）
    Expect((h.StickyMods() & kShift) != 0, "按住期间按过 Shift，松开 Alt 时仍带 Shift");
    Expect((h.StickyMods() & kAlt) == 0, "Alt 位不算别的修饰位");
}

void TestStickyFromVkAlone()
{
    AltHold h;
    h.OnAltDown(false, true);
    h.NoteKey(0x5B, 0); // 左 Win，修饰位没取到
    Expect(h.StickyMods() == kWin, "Win 键本身记 Win 位");
    h.NoteKey(0x11, 0); // 笼统 Ctrl
    Expect(h.StickyMods() == (kWin | kCtrl), "Ctrl 键本身记 Ctrl 位");
}

void TestOtherKeysOutsideHoldIgnored()
{
    AltHold h;
    h.NoteKey(0xA0, kShift);
    h.OnAltDown(false, true);
    Expect(h.StickyMods() == 0, "Alt 按下前的修饰键不算");
}

void TestRepeatKeepsStickyFreshDownResets()
{
    AltHold h;
    h.OnAltDown(false, true);
    h.NoteKey(0xA0, kShift);
    h.OnAltDown(IsRepeat(kRepeat), true);
    Expect(h.StickyMods() == (kShift | kLShift), "自动连发不清粘滞位");
    h.OnAltDown(IsRepeat(0), true); // 松开没收到（Alt+Tab 切走），下一次是新的按下
    Expect(h.StickyMods() == 0, "新的一次按下清掉残留");
}

void TestUpForwardedAfterForwardedDown()
{
    AltHold h;
    h.OnAltDown(false, true);
    Expect(h.ShouldForwardUp(false), "按下转发过，组字已结束（Alt+数字上屏）仍转发松开");
    h.OnAltUp();
    Expect(!h.ShouldForwardUp(false), "松开后复位");
    Expect(h.StickyMods() == 0, "松开后清粘滞位");

    AltHold idle;
    idle.OnAltDown(false, false);
    Expect(!idle.ShouldForwardUp(false), "按下与松开都不在组字 ⇒ 不转发");
    Expect(idle.ShouldForwardUp(true), "松开时在组字 ⇒ 转发");
}

void TestHotkeyComboMarksSticky()
{
    AltHold h;
    h.OnAltDown(false, true);
    h.NoteKey(0x31, kAlt | kCtrl); // WM_HOTKEY 送来的组合键
    Expect(h.StickyMods() == kCtrl, "热键组合里的 Ctrl 记进粘滞位");
}

} // namespace

int main()
{
    TestMapAltVk();
    TestShouldForwardAlt();
    TestStickyModifierSurvivesRelease();
    TestStickyFromVkAlone();
    TestOtherKeysOutsideHoldIgnored();
    TestRepeatKeepsStickyFreshDownResets();
    TestUpForwardedAfterForwardedDown();
    TestHotkeyComboMarksSticky();
    if (g_failures == 0)
        std::printf("alt_key_test: all passed\n");
    return g_failures == 0 ? 0 : 1;
}
