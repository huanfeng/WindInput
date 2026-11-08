// 宿主过早终止组合后「保留缓冲、下一键接上」的纯判据（不含 Win32 头，g++ 可跑，
// 见 tests/composition_recover_policy_test.cpp）。
//
// # 场景
//
// React 等受控输入框在每次 input 事件后先把 value 回写成旧值、再写成新值。组合中途
// value 被脚本改写，Chromium 就直接取消组合（不派发 compositionend），组合文字作为
// 普通文字留在输入框里。若照「宿主终止」清空缓冲，每一键都从零开始，框里只剩一串字母。
//
// 微软拼音在同一页面上的做法（Edge 事件日志对照）：被取消后保留自己的缓冲，残留不动；
// 下一键先删掉光标前那段残留，再用完整缓冲重开组合；上屏时同样先删残留再插入。
//
// # 为什么进入恢复时不清残留
//
// 删残留靠的是「光标恰在记下的位置末尾、前面恰好是记下的那段文字」（位置用 ACP 偏移，
// 光标挪走即解除恢复，见 OnEndEdit）。终止回调里清空在 Chromium 上并不生效，
// 在别的宿主上却可能生效；一旦生效，光标前的文字就是用户原有的内容，碰巧相同时会被
// 误删。所以进入恢复就不去清，残留一定还在，核对才可靠。
#pragma once

#include <cstddef>
#include <cstdint>

namespace wind
{
namespace recover
{

/// 距上次写组合多久之内被终止才算「过早终止」。受控输入框的回写在 input 事件里同步发生，
/// 实测 2~15ms；用户点别处、切窗口这类真正的结束远在这之后。
constexpr uint64_t kWindowMs = 200;

/// 残留最长多少个 UTF-16 单元。超过就不恢复：编码区不会这么长，长了多半不是我们的组合。
constexpr size_t kMaxLeftoverLen = 64;

/// 这次宿主终止能否进入恢复。
///
/// - `msSinceUpdate`：距上次写组合文字的毫秒数；
/// - `holdActive`：智能符号预上屏中（那时终止＝提交，不能恢复）；
/// - `prefixLen`：顶码待提交前缀长度（前缀已按上屏记账，残留里混着它就不能当编码删）；
/// - `textLen`：被终止时组合里的文字长度。
inline bool ShouldRecover(uint64_t msSinceUpdate, bool holdActive, size_t prefixLen, size_t textLen)
{
    return !holdActive && prefixLen == 0 && textLen > 0 && textLen <= kMaxLeftoverLen &&
           msSinceUpdate <= kWindowMs;
}

} // namespace recover
} // namespace wind
