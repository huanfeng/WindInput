// 智能符号 hold 预览态（组合态预览）下「吃键 → 收口 → 重放」的触发判据。
//
// 背景：hold 期间符号挂在 TSF 组合里，OnTestKeyDown 按「有会话」把键吃下；OnKeyDown 里服务端
// 因缓冲为空回 PassThrough、并已同步收口符号。此时若吐成 pfEaten=FALSE 就是「吃了再吐」——
// 补发 WM_KEYDOWN 的宿主无感，不补发的宿主直接丢键。凡命中本判据的键，改为保持吃下 + SendInput
// 重放，宿主先看到收口后的文档、再看到一个与组合无关的普通按键。
//
// 本头文件刻意**不含任何 Win32 头**，好用 g++ 在 Linux 上单测（tests/hold_replay_policy_test.cpp）。
#pragma once

namespace wind
{
namespace holdreplay
{

/// 本次 keydown 是否走 hold 重放。
///
/// - holdActiveBeforeResponse：处理服务端响应**之前**采样的 hold 状态（PassThrough 分支会收口，
///   之后就查不到了）。
/// - eatenByResponse：服务端响应处理后的 pfEaten。为真说明服务端自己出了字（全角空格等），
///   键已被正当消费，不能再重放。
/// - isHoldReplayKey：`CKeyEventSink::_IsHoldReplayKey` 的结论（回车 / 空格 / 数字 / 方向键等）。
/// - isCtrlAltCleanup：OnKeyDown 的 `isCtrlAltCleanup`——会话中的 Ctrl/Alt 组合（不含修饰键
///   本身、不含已注册热键），与 OnTestKeyDown 的 `ctrl_alt_cleanup` 吃键分支同一判据。
///   A2-65 / t263：漏了这一条时，hold 期间的 Ctrl+V 在微信、WPS 表格里粘贴无效。
inline bool ShouldReplayAfterHold(bool holdActiveBeforeResponse, bool eatenByResponse,
                                  bool isHoldReplayKey, bool isCtrlAltCleanup)
{
    return holdActiveBeforeResponse && !eatenByResponse && (isHoldReplayKey || isCtrlAltCleanup);
}

} // namespace holdreplay
} // namespace wind
