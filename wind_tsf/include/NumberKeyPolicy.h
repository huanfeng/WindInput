// 数字类键（主键盘 0-9、小键盘 0-9 与运算符；HotkeyManager 归 Number）的吃键判据。
//
// OnTestKeyDown 与 OnKeyDown **调同一个函数**：Test 放行的数字键，OnKeyDown 也必须放行、不转发
// 服务端。此前 OnKeyDown 在中文模式把 Number 一律算输入键转发，靠服务端回 PassThrough 兜着；
// 服务端某条路径一旦出字（论坛 t285 小键盘、主键盘 0、follow_main 运算符先后漏过三次），
// Chrome 类宿主（Test 不吃仍照调 OnKeyDown）就宿主出一次、我们插一次，PIN 框里一键两个字。
//
// 本头文件刻意**不含任何 Win32 头**，好用 g++ 在 Linux 上单测（tests/number_key_policy_test.cpp）。
#pragma once

namespace wind
{
namespace numberkey
{

/// 数字类键该不该吃（转发服务端）。
///
/// - 有输入会话：选词 / 翻页 / 顶屏，吃。
/// - 中文 + 全角、无会话：服务端出全角数字，吃（否则纯 TSF 宿主直接得到半角）。
/// - 中文半角空闲、汉字后、开了中数间空格（`spaceBeforeDigit`，调用方算好）：吃，服务端出
///   「空格 + 数字」。服务端对「上一笔是汉字的空闲数字键」恒出字（判据见 message_handler 那条臂），
///   故 C++ 吃键集 ⊆ Rust 出字集成立。
/// - 其余（中文半角空闲、英文半角）：不吃，宿主自己出字。英文全角另有 english_fullwidth 分支，
///   不经本函数。
inline bool ShouldEatNumberKey(bool hasInputSession, bool chineseMode, bool fullWidth,
                               bool spaceBeforeDigit = false)
{
    return hasInputSession || (chineseMode && fullWidth) || (chineseMode && spaceBeforeDigit);
}

/// 汉字后的数字要不要吃下交给服务端补空格。主键盘与小键盘的 0-9（`VK_NUMPAD0..9` = 0x60..0x69）、
/// 不带修饰、CapsLock 关——与服务端 `digit_after_cjk_outputs` 的判据逐条对应（小键盘 direct 档
/// 在服务端那条臂出字，follow_main 档被改写成主键盘键后走主键盘那条臂）。运算符、Shift+数字不在其列。
inline bool SpaceBeforeDigit(bool enabled, bool seqIsCjk, unsigned vk, bool anyModifier, bool capsLock)
{
    const bool digit = (vk >= '0' && vk <= '9') || (vk >= 0x60 && vk <= 0x69);
    return enabled && seqIsCjk && digit && !anyModifier && !capsLock;
}

} // namespace numberkey
} // namespace wind
