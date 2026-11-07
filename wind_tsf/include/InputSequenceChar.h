#pragma once

#include <cstddef>
#include <cstdint>

// ============================================================================
// InputSequenceChar —— 发往服务端的 KeyPayload.seqChar 怎么取
// ============================================================================
//
// seqChar = 「输入序列里最近进文档的那个字符」的代表值，0 = 不可知。服务端据此判中英/数字
// 交界补空格、英文后智能标点（设计见 docs/design/auto-spacing-and-latin-smart-punct.md）。
//
// 与 `_lastPassthroughDigit`（SmartPunctPrevChar.h）同一原则：**只认输入法自己看得见的输入
// 序列**，不认文档现读——「刚刚打了什么」是用户意图，「光标前碰巧是什么」不是。分工也相同：
// DLL 只报事实，补不补空格、转不转半角全由服务端定。
//
// 服务端只需要**类别**（汉字 / 拉丁字母 / 数字 / 其它），所以透传键报代表值即可，不必按键盘
// 布局还原确切字符：字母报 'a'，空格报 ' '，其余可见符号报 '?'。
//
// 本文件不含任何 Win32 头，可被 tests/input_sequence_char_test.cpp 在非 Windows 工具链下
// 直接编译运行。VK 常量以字面量给出，KeyEventSink.h 有 static_assert 与 <winuser.h> 对账。
namespace WindInputSeq
{

/// 「在记账、但不知道光标前是什么」（导航键、焦点切换、纯删除之后）。与 0 区分：0 只来自不带
/// 这个字段的旧客户端，服务端对它会退回自己的记录；对本值不会（回车换行后的行首不该补空格）。
/// 与 Rust `wind_ipc::protocol::SEQ_CHAR_UNKNOWN` 同值。
constexpr uint16_t kUnknown = 0xFFFF;

/// 是不是汉字类（与服务端 `wind_punct::spacing::classify_unit` 的 `Cjk` 同一张表）。
/// DLL 只用它决定「汉字后的数字要不要吃下交给服务端补空格」。
inline bool IsCjk(uint16_t u)
{
    return (u >= 0x2E80 && u <= 0x2FDF) || (u >= 0x3040 && u <= 0x309F) ||
           (u >= 0x30A0 && u <= 0x30FA) || (u >= 0x30FC && u <= 0x30FF) ||
           (u >= 0x3100 && u <= 0x312F) || (u >= 0x31F0 && u <= 0x31FF) ||
           (u >= 0x3200 && u <= 0x32FF) || (u >= 0x3400 && u <= 0x4DBF) ||
           (u >= 0x4E00 && u <= 0x9FFF) || (u >= 0xF900 && u <= 0xFAFF) ||
           (u >= 0xD840 && u <= 0xD8BF);
}

constexpr uint32_t kVkSpace = 0x20;
constexpr uint32_t kVkNumpad0 = 0x60;
constexpr uint32_t kVkNumpad9 = 0x69;
constexpr uint32_t kVkMultiply = 0x6A; // 小键盘 * + 分隔 - . / 连续六个
constexpr uint32_t kVkDivide = 0x6F;
constexpr uint32_t kVkOem1 = 0xBA;     // ;: 到 `~ 连续七个
constexpr uint32_t kVkOem3 = 0xC0;
constexpr uint32_t kVkOem4 = 0xDB;     // [{ 到 '" 连续四个
constexpr uint32_t kVkOem7 = 0xDE;
constexpr uint32_t kVkOem102 = 0xE2;   // 102 键布局的 \|

/// 一次透传进宿主的 keydown 的代表字符；kUnknown = 不产出可见字符（编辑/导航/功能键，或宿主快捷键）。
///
/// Ctrl/Alt 组合一律 kUnknown：它们是宿主快捷键（Ctrl+V 粘贴进来的内容我们看不见）。
/// AltGr（= Ctrl+Alt）在部分布局下也出字，同样报不知道——少补一个空格好过多补。
inline uint16_t FromVk(uint32_t vk, bool shift, bool ctrlOrAlt)
{
    if (ctrlOrAlt)
        return kUnknown;
    if (vk >= '0' && vk <= '9')
        return shift ? '?' : (uint16_t)vk; // Shift+数字是 !@#…
    if (vk >= 'A' && vk <= 'Z')
        return 'a';
    if (vk >= kVkNumpad0 && vk <= kVkNumpad9)
        return (uint16_t)('0' + (vk - kVkNumpad0));
    if (vk == kVkSpace)
        return ' ';
    if ((vk >= kVkMultiply && vk <= kVkDivide) || (vk >= kVkOem1 && vk <= kVkOem3) ||
        (vk >= kVkOem4 && vk <= kVkOem7) || vk == kVkOem102)
        return '?';
    return kUnknown;
}

/// 经输入法上屏的文本的代表字符：末尾码点的**首个** UTF-16 单元（BMP 即本字符，辅助平面取高
/// 代理——服务端据此把 2/3 号平面认成汉字、1 号平面的 emoji 认成其它）。
///
/// 空文本返回 0，调用方据此**不动**记账：没有东西写进文档，光标前是什么并没有改变。
inline uint16_t FromCommittedText(const wchar_t* text, size_t len)
{
    if (text == nullptr || len == 0)
        return 0;
    uint16_t last = (uint16_t)text[len - 1];
    if (last >= 0xDC00 && last <= 0xDFFF && len >= 2)
    {
        uint16_t hi = (uint16_t)text[len - 2];
        if (hi >= 0xD800 && hi <= 0xDBFF)
            return hi;
    }
    return last;
}

/// 一次 OnEndEdit 是不是「用户用鼠标把光标挪到了别处」：此刻记账作废（报 kUnknown）。
///
/// 选区变化本身不够：透传的字符、输入法自己的上屏也会让选区变（宿主的回声可以晚到几百毫秒，
/// 服务端按时间分不开）。鼠标按着、且这次编辑没有改动任何文本，才是点击 / 拖选。
/// 键盘挪光标不靠这里：导航键在按键侧就报了 kUnknown。
inline bool CursorMovedByMouse(bool selChanged, bool inComposition, bool mouseDown, bool textChanged)
{
    return selChanged && !inComposition && mouseDown && !textChanged;
}

} // namespace WindInputSeq
