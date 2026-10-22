#include "KeyMap.h"

#include "Protocol.h"

namespace windlinux {

namespace {

// VK 常量取值对照 wind-keys/src/keymap.rs（SSOT）与 Win32 winuser.h。这里只列本文件用到的，
// 用具名常量而非裸十六进制，理由同仓库 AGENTS.md「虚拟键码——用常量」。
namespace vk {
constexpr uint32_t BACK = 0x08;
constexpr uint32_t TAB = 0x09;
constexpr uint32_t RETURN = 0x0D;
constexpr uint32_t CAPITAL = 0x14;
constexpr uint32_t ESCAPE = 0x1B;
constexpr uint32_t SPACE = 0x20;
constexpr uint32_t PRIOR = 0x21;
constexpr uint32_t NEXT = 0x22;
constexpr uint32_t END = 0x23;
constexpr uint32_t HOME = 0x24;
constexpr uint32_t LEFT = 0x25;
constexpr uint32_t UP = 0x26;
constexpr uint32_t RIGHT = 0x27;
constexpr uint32_t DOWN = 0x28;
constexpr uint32_t INSERT = 0x2D;
constexpr uint32_t DEL = 0x2E;
constexpr uint32_t K0 = 0x30;
constexpr uint32_t A = 0x41;
constexpr uint32_t LWIN = 0x5B;
constexpr uint32_t RWIN = 0x5C;
constexpr uint32_t APPS = 0x5D;
constexpr uint32_t NUMPAD0 = 0x60;
constexpr uint32_t MULTIPLY = 0x6A;
constexpr uint32_t ADD = 0x6B;
constexpr uint32_t SEPARATOR = 0x6C;
constexpr uint32_t SUBTRACT = 0x6D;
constexpr uint32_t DECIMAL = 0x6E;
constexpr uint32_t DIVIDE = 0x6F;
constexpr uint32_t F1 = 0x70;
constexpr uint32_t LSHIFT = 0xA0;
constexpr uint32_t RSHIFT = 0xA1;
constexpr uint32_t LCONTROL = 0xA2;
constexpr uint32_t RCONTROL = 0xA3;
constexpr uint32_t LMENU = 0xA4;
constexpr uint32_t RMENU = 0xA5;
constexpr uint32_t OEM_1 = 0xBA;      // ; :
constexpr uint32_t OEM_PLUS = 0xBB;   // = +
constexpr uint32_t OEM_COMMA = 0xBC;  // , <
constexpr uint32_t OEM_MINUS = 0xBD;  // - _
constexpr uint32_t OEM_PERIOD = 0xBE; // . >
constexpr uint32_t OEM_2 = 0xBF;      // / ?
constexpr uint32_t OEM_3 = 0xC0;      // ` ~
constexpr uint32_t OEM_4 = 0xDB;      // [ {
constexpr uint32_t OEM_5 = 0xDC;      // \ |
constexpr uint32_t OEM_6 = 0xDD;      // ] }
constexpr uint32_t OEM_7 = 0xDE;      // ' "
} // namespace vk

// X11 keysym 取值（X11/keysymdef.h）。
namespace ks {
constexpr uint32_t BackSpace = 0xff08;
constexpr uint32_t Tab = 0xff09;
constexpr uint32_t Return = 0xff0d;
constexpr uint32_t Escape = 0xff1b;
constexpr uint32_t Delete = 0xffff;
constexpr uint32_t Home = 0xff50;
constexpr uint32_t Left = 0xff51;
constexpr uint32_t Up = 0xff52;
constexpr uint32_t Right = 0xff53;
constexpr uint32_t Down = 0xff54;
constexpr uint32_t Prior = 0xff55;
constexpr uint32_t Next = 0xff56;
constexpr uint32_t End = 0xff57;
constexpr uint32_t Insert = 0xff63;
constexpr uint32_t Menu = 0xff67;
constexpr uint32_t ISO_Left_Tab = 0xfe20;
constexpr uint32_t KP_Enter = 0xff8d;
constexpr uint32_t KP_Home = 0xff95;
constexpr uint32_t KP_Left = 0xff96;
constexpr uint32_t KP_Up = 0xff97;
constexpr uint32_t KP_Right = 0xff98;
constexpr uint32_t KP_Down = 0xff99;
constexpr uint32_t KP_Prior = 0xff9a;
constexpr uint32_t KP_Next = 0xff9b;
constexpr uint32_t KP_End = 0xff9c;
constexpr uint32_t KP_Insert = 0xff9e;
constexpr uint32_t KP_Delete = 0xff9f;
constexpr uint32_t KP_Multiply = 0xffaa;
constexpr uint32_t KP_Add = 0xffab;
constexpr uint32_t KP_Separator = 0xffac;
constexpr uint32_t KP_Subtract = 0xffad;
constexpr uint32_t KP_Decimal = 0xffae;
constexpr uint32_t KP_Divide = 0xffaf;
constexpr uint32_t KP_0 = 0xffb0;
constexpr uint32_t KP_9 = 0xffb9;
constexpr uint32_t F1 = 0xffbe;
constexpr uint32_t F12 = 0xffc9;
constexpr uint32_t Shift_L = 0xffe1;
constexpr uint32_t Shift_R = 0xffe2;
constexpr uint32_t Control_L = 0xffe3;
constexpr uint32_t Control_R = 0xffe4;
constexpr uint32_t Caps_Lock = 0xffe5;
constexpr uint32_t Alt_L = 0xffe9;
constexpr uint32_t Alt_R = 0xffea;
constexpr uint32_t Super_L = 0xffeb;
constexpr uint32_t Super_R = 0xffec;
} // namespace ks

/// 可打印 ASCII keysym（0x20..0x7E，与 Latin-1 码点相同）→ VK。
/// Shift 过的符号按 US 布局反查到它所在的物理键：`!` → VK '1'，`{` → VK_OEM_4 …
uint32_t asciiToVK(uint32_t c)
{
    if (c >= 'a' && c <= 'z') {
        return vk::A + (c - 'a');
    }
    if (c >= 'A' && c <= 'Z') {
        return vk::A + (c - 'A');
    }
    if (c >= '0' && c <= '9') {
        return vk::K0 + (c - '0');
    }
    switch (c) {
    case ' ': return vk::SPACE;
    // Shift + 顶排数字（US 布局）
    case '!': return vk::K0 + 1;
    case '@': return vk::K0 + 2;
    case '#': return vk::K0 + 3;
    case '$': return vk::K0 + 4;
    case '%': return vk::K0 + 5;
    case '^': return vk::K0 + 6;
    case '&': return vk::K0 + 7;
    case '*': return vk::K0 + 8;
    case '(': return vk::K0 + 9;
    case ')': return vk::K0;
    // OEM 键（本键 / Shift 键）
    case ';': case ':': return vk::OEM_1;
    case '=': case '+': return vk::OEM_PLUS;
    case ',': case '<': return vk::OEM_COMMA;
    case '-': case '_': return vk::OEM_MINUS;
    case '.': case '>': return vk::OEM_PERIOD;
    case '/': case '?': return vk::OEM_2;
    case '`': case '~': return vk::OEM_3;
    case '[': case '{': return vk::OEM_4;
    case '\\': case '|': return vk::OEM_5;
    case ']': case '}': return vk::OEM_6;
    case '\'': case '"': return vk::OEM_7;
    default: return 0;
    }
}

} // namespace

uint32_t keysymToVK(uint32_t keysym)
{
    if (keysym >= 0x20 && keysym <= 0x7E) {
        return asciiToVK(keysym);
    }
    // 小键盘数字。**必须映射成 VK_NUMPAD* 而不是主键盘数字**：「数字小键盘功能」
    // （input.numpad_behavior）的归一化在服务端做，这里折算成主键盘数字等于把那个开关
    // 架空（同 KeyHandler.swift 的同名注释）。NumLock 关时 XKB 给的是 KP_Home 之类，
    // 走下面导航键那一路——与 Windows 小键盘 NumLock 关时出 VK_HOME 一致。
    if (keysym >= ks::KP_0 && keysym <= ks::KP_9) {
        return vk::NUMPAD0 + (keysym - ks::KP_0);
    }
    if (keysym >= ks::F1 && keysym <= ks::F12) {
        return vk::F1 + (keysym - ks::F1);
    }
    switch (keysym) {
    case ks::BackSpace: return vk::BACK;
    case ks::Tab:
    case ks::ISO_Left_Tab: return vk::TAB; // Shift+Tab 在 XKB 里是 ISO_Left_Tab
    case ks::Return:
    case ks::KP_Enter: return vk::RETURN; // 小键盘 Enter 与 Windows 同，不单列
    case ks::Escape: return vk::ESCAPE;
    case ks::Delete:
    case ks::KP_Delete: return vk::DEL;
    case ks::Home:
    case ks::KP_Home: return vk::HOME;
    case ks::End:
    case ks::KP_End: return vk::END;
    case ks::Prior:
    case ks::KP_Prior: return vk::PRIOR;
    case ks::Next:
    case ks::KP_Next: return vk::NEXT;
    case ks::Left:
    case ks::KP_Left: return vk::LEFT;
    case ks::Up:
    case ks::KP_Up: return vk::UP;
    case ks::Right:
    case ks::KP_Right: return vk::RIGHT;
    case ks::Down:
    case ks::KP_Down: return vk::DOWN;
    case ks::Insert:
    case ks::KP_Insert: return vk::INSERT;
    case ks::Menu: return vk::APPS;
    case ks::KP_Multiply: return vk::MULTIPLY;
    case ks::KP_Add: return vk::ADD;
    case ks::KP_Separator: return vk::SEPARATOR;
    case ks::KP_Subtract: return vk::SUBTRACT;
    case ks::KP_Decimal: return vk::DECIMAL;
    case ks::KP_Divide: return vk::DIVIDE;
    case ks::Shift_L: return vk::LSHIFT;
    case ks::Shift_R: return vk::RSHIFT;
    case ks::Control_L: return vk::LCONTROL;
    case ks::Control_R: return vk::RCONTROL;
    case ks::Caps_Lock: return vk::CAPITAL;
    case ks::Alt_L: return vk::LMENU;
    case ks::Alt_R: return vk::RMENU;
    case ks::Super_L: return vk::LWIN;
    case ks::Super_R: return vk::RWIN;
    default: return 0;
    }
}

uint32_t statesToModifiers(uint32_t states)
{
    uint32_t m = 0;
    if (states & xstate::Shift) {
        m |= KEYMOD_SHIFT;
    }
    if (states & xstate::Ctrl) {
        m |= KEYMOD_CTRL;
    }
    if (states & xstate::Alt) {
        m |= KEYMOD_ALT;
    }
    if (states & (xstate::Super | xstate::Super2)) {
        m |= KEYMOD_WIN;
    }
    return m;
}

uint8_t statesToToggles(uint32_t states)
{
    uint8_t t = 0;
    if (states & xstate::CapsLock) {
        t |= TOGGLE_CAPSLOCK;
    }
    if (states & xstate::NumLock) {
        t |= TOGGLE_NUMLOCK;
    }
    return t;
}

bool isHostShortcut(uint32_t states)
{
    return (states & (xstate::Ctrl | xstate::Alt | xstate::Super | xstate::Super2)) != 0;
}

uint32_t toggleModifierVK(uint32_t keysym)
{
    switch (keysym) {
    case ks::Shift_L: return vk::LSHIFT;
    case ks::Shift_R: return vk::RSHIFT;
    case ks::Control_L: return vk::LCONTROL;
    case ks::Control_R: return vk::RCONTROL;
    default: return 0;
    }
}

void ToggleTapDetector::onPress(uint32_t keysym, uint64_t nowMs)
{
    uint32_t v = toggleModifierVK(keysym);
    if (v != 0 && v == pendingVK_) {
        return; // 同一修饰键的自动重复按下：不重置计时，长按仍按长按判
    }
    if (v != 0 && pendingVK_ == 0) {
        pendingVK_ = v;
        pressedAtMs_ = nowMs;
        return;
    }
    // 别的键（含第二个修饰键）按下：这次不再是单击。
    pendingVK_ = 0;
}

uint32_t ToggleTapDetector::onRelease(uint32_t keysym, uint64_t nowMs)
{
    uint32_t v = toggleModifierVK(keysym);
    if (v == 0 || v != pendingVK_) {
        if (v != 0) {
            pendingVK_ = 0;
        }
        return 0;
    }
    pendingVK_ = 0;
    if (nowMs - pressedAtMs_ > kThresholdMs) {
        return 0;
    }
    return v;
}

uint32_t altVK(uint32_t keysym)
{
    switch (keysym) {
    case ks::Alt_L: return vk::LMENU;
    case ks::Alt_R: return vk::RMENU;
    default: return 0;
    }
}

void AltHold::onAltDown(bool forwarded)
{
    if (!active_) {
        active_ = true;
        sticky_ = 0;
        forwarded_ = false;
    }
    forwarded_ = forwarded_ || forwarded;
}

void AltHold::noteKey(uint32_t keysym, uint32_t states)
{
    if (!active_) {
        return;
    }
    sticky_ |= statesToModifiers(states) & ~KEYMOD_ALT;
    switch (keysym) {
    case ks::Shift_L:
    case ks::Shift_R: sticky_ |= KEYMOD_SHIFT; break;
    case ks::Control_L:
    case ks::Control_R: sticky_ |= KEYMOD_CTRL; break;
    case ks::Super_L:
    case ks::Super_R: sticky_ |= KEYMOD_WIN; break;
    default: break;
    }
}

} // namespace windlinux
