// keysym → VK、修饰位、单击检测单测。VK 期望值按 wind-keys/src/keymap.rs 的常量写。

#include "KeyMap.h"
#include "Protocol.h"
#include "TestHarness.h"

using namespace windlinux;

namespace {

void TestLettersDigits()
{
    CASE("字母大小写同 VK；数字；Shift 符号反查物理键（US）");
    CHECK_EQ(keysymToVK('a'), 0x41u);
    CHECK_EQ(keysymToVK('z'), 0x5Au);
    CHECK_EQ(keysymToVK('N'), 0x4Eu);
    CHECK_EQ(keysymToVK('0'), 0x30u);
    CHECK_EQ(keysymToVK('9'), 0x39u);
    CHECK_EQ(keysymToVK('!'), 0x31u);
    CHECK_EQ(keysymToVK(')'), 0x30u);
    CHECK_EQ(keysymToVK('@'), 0x32u);
    CHECK_EQ(keysymToVK(' '), 0x20u);
}

void TestOem()
{
    CASE("OEM 标点：本键与 Shift 键同 VK（VK_SEMICOLON..VK_QUOTE）");
    CHECK_EQ(keysymToVK(';'), 0xBAu);
    CHECK_EQ(keysymToVK(':'), 0xBAu);
    CHECK_EQ(keysymToVK('='), 0xBBu);
    CHECK_EQ(keysymToVK('+'), 0xBBu);
    CHECK_EQ(keysymToVK(','), 0xBCu);
    CHECK_EQ(keysymToVK('<'), 0xBCu);
    CHECK_EQ(keysymToVK('-'), 0xBDu);
    CHECK_EQ(keysymToVK('_'), 0xBDu);
    CHECK_EQ(keysymToVK('.'), 0xBEu);
    CHECK_EQ(keysymToVK('/'), 0xBFu);
    CHECK_EQ(keysymToVK('?'), 0xBFu);
    CHECK_EQ(keysymToVK('`'), 0xC0u);
    CHECK_EQ(keysymToVK('~'), 0xC0u);
    CHECK_EQ(keysymToVK('['), 0xDBu);
    CHECK_EQ(keysymToVK('{'), 0xDBu);
    CHECK_EQ(keysymToVK('\\'), 0xDCu);
    CHECK_EQ(keysymToVK('|'), 0xDCu);
    CHECK_EQ(keysymToVK(']'), 0xDDu);
    CHECK_EQ(keysymToVK('\''), 0xDEu);
    CHECK_EQ(keysymToVK('"'), 0xDEu);
}

void TestControlKeys()
{
    CASE("控制/导航/功能键");
    CHECK_EQ(keysymToVK(0xff08), 0x08u); // BackSpace
    CHECK_EQ(keysymToVK(0xff09), 0x09u); // Tab
    CHECK_EQ(keysymToVK(0xfe20), 0x09u); // ISO_Left_Tab（Shift+Tab）
    CHECK_EQ(keysymToVK(0xff0d), 0x0Du); // Return
    CHECK_EQ(keysymToVK(0xff1b), 0x1Bu); // Escape
    CHECK_EQ(keysymToVK(0xffff), 0x2Eu); // Delete
    CHECK_EQ(keysymToVK(0xff55), 0x21u); // Prior
    CHECK_EQ(keysymToVK(0xff56), 0x22u); // Next
    CHECK_EQ(keysymToVK(0xff50), 0x24u); // Home
    CHECK_EQ(keysymToVK(0xff57), 0x23u); // End
    CHECK_EQ(keysymToVK(0xff51), 0x25u);
    CHECK_EQ(keysymToVK(0xff52), 0x26u);
    CHECK_EQ(keysymToVK(0xff53), 0x27u);
    CHECK_EQ(keysymToVK(0xff54), 0x28u);
    CHECK_EQ(keysymToVK(0xffbe), 0x70u); // F1
    CHECK_EQ(keysymToVK(0xffc9), 0x7Bu); // F12
    CHECK_EQ(keysymToVK(0xffe1), 0xA0u); // Shift_L
    CHECK_EQ(keysymToVK(0xffe4), 0xA3u); // Control_R
    CHECK_EQ(keysymToVK(0xffe5), 0x14u); // Caps_Lock
    CHECK_EQ(keysymToVK(0x1008ff13), 0u); // XF86AudioRaiseVolume：未覆盖 → 0（透传）
    CHECK_EQ(keysymToVK(0x4e2d), 0u);     // 非 ASCII 字符 keysym
}

void TestNumpad()
{
    CASE("小键盘：数字映射 VK_NUMPAD*（不折算成主键盘，否则架空 numpad_behavior）");
    CHECK_EQ(keysymToVK(0xffb0), 0x60u);
    CHECK_EQ(keysymToVK(0xffb9), 0x69u);
    CHECK_EQ(keysymToVK(0xffaa), 0x6Au);
    CHECK_EQ(keysymToVK(0xffab), 0x6Bu);
    CHECK_EQ(keysymToVK(0xffad), 0x6Du);
    CHECK_EQ(keysymToVK(0xffae), 0x6Eu);
    CHECK_EQ(keysymToVK(0xffaf), 0x6Fu);
    CHECK_EQ(keysymToVK(0xff8d), 0x0Du); // KP_Enter
    CHECK_EQ(keysymToVK(0xff95), 0x24u); // KP_Home（NumLock 关）
}

void TestModifiers()
{
    CASE("修饰位只报通用位；CapsLock/NumLock 进 toggles；宿主快捷键判定");
    CHECK_EQ(statesToModifiers(xstate::Shift), uint32_t(KEYMOD_SHIFT));
    CHECK_EQ(statesToModifiers(xstate::Ctrl | xstate::Alt), uint32_t(KEYMOD_CTRL | KEYMOD_ALT));
    CHECK_EQ(statesToModifiers(xstate::Super), uint32_t(KEYMOD_WIN));
    CHECK_EQ(statesToModifiers(xstate::CapsLock | xstate::NumLock), 0u);
    CHECK_EQ(statesToToggles(xstate::CapsLock), uint8_t(TOGGLE_CAPSLOCK));
    CHECK_EQ(statesToToggles(xstate::NumLock), uint8_t(TOGGLE_NUMLOCK));
    CHECK(isHostShortcut(xstate::Ctrl));
    CHECK(isHostShortcut(xstate::Alt));
    CHECK(isHostShortcut(xstate::Super));
    CHECK(!isHostShortcut(xstate::Shift));
    CHECK(!isHostShortcut(xstate::CapsLock | xstate::NumLock));
}

void TestToggleTap()
{
    CASE("单击 Shift → VK_LSHIFT；中间按过别的键 / 长按 → 不算");
    ToggleTapDetector d;
    d.onPress(0xffe1, 1000);
    CHECK_EQ(d.onRelease(0xffe1, 1100), 0xA0u);

    d.onPress(0xffe1, 2000);
    d.onPress('A', 2010); // Shift+A
    d.onRelease('A', 2020);
    CHECK_EQ(d.onRelease(0xffe1, 2030), 0u);

    d.onPress(0xffe2, 3000);
    CHECK_EQ(d.onRelease(0xffe2, 3000 + ToggleTapDetector::kThresholdMs + 1), 0u);

    CASE("修饰键自动重复不重置计时；两个修饰键叠按不算");
    d.onPress(0xffe3, 4000);
    d.onPress(0xffe3, 4400); // 自动重复
    CHECK_EQ(d.onRelease(0xffe3, 4600), 0u); // 距首次按下 600ms > 阈值

    d.onPress(0xffe1, 5000);
    d.onPress(0xffe3, 5010); // Shift+Ctrl
    CHECK_EQ(d.onRelease(0xffe3, 5020), 0u);
    CHECK_EQ(d.onRelease(0xffe1, 5030), 0u);

    CASE("非切换修饰键（Alt）不参与");
    d.onPress(0xffe9, 6000);
    CHECK_EQ(d.onRelease(0xffe9, 6010), 0u);
}

void TestAltHold()
{
    CASE("Alt_L / Alt_R → VK_LMENU / VK_RMENU");
    CHECK_EQ(altVK(0xffe9), 0xA4u);
    CHECK_EQ(altVK(0xffea), 0xA5u);
    CHECK_EQ(altVK(0xffe1), 0u);

    CASE("按住 Alt 期间按过 Shift 又先松开：松开 Alt 时仍带 Shift；Alt 位不算");
    AltHold h;
    h.onAltDown(true);
    h.noteKey(0xffe1, xstate::Alt); // Shift_L 按下（按下事件的 states 还不含 Shift）
    h.noteKey('a', xstate::Alt);
    CHECK_EQ(h.stickyMods(), KEYMOD_SHIFT);

    CASE("Super / Ctrl 自身与 states 里的位都记");
    h.noteKey(0xffeb, 0);                     // Super_L
    h.noteKey('x', xstate::Ctrl | xstate::Alt); // Ctrl 已按着的组合
    CHECK_EQ(h.stickyMods(), KEYMOD_SHIFT | KEYMOD_WIN | KEYMOD_CTRL);

    CASE("自动重复不清粘滞位；复位后清");
    h.onAltDown(true);
    CHECK_EQ(h.stickyMods(), KEYMOD_SHIFT | KEYMOD_WIN | KEYMOD_CTRL);
    h.reset();
    CHECK_EQ(h.stickyMods(), 0u);

    CASE("Alt 按下前的修饰键不算");
    AltHold before;
    before.noteKey(0xffe1, 0);
    before.onAltDown(true);
    CHECK_EQ(before.stickyMods(), 0u);

    CASE("按下转发过：组字已结束（Alt+数字上屏）仍转发松开；都不在组字不转发");
    CHECK(before.shouldForwardUp(false));
    AltHold idle;
    idle.onAltDown(false);
    CHECK(!idle.shouldForwardUp(false));
    CHECK(idle.shouldForwardUp(true));
    idle.reset();
    CHECK(!idle.shouldForwardUp(false));
}

} // namespace

int main()
{
    TestLettersDigits();
    TestOem();
    TestControlKeys();
    TestNumpad();
    TestModifiers();
    TestToggleTap();
    TestAltHold();
    TEST_MAIN_END();
}
