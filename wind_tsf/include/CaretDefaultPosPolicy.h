// 「宿主对本 context 没有插入点可报」的纯判据：什么形态的退化矩形算宿主的默认位置，以及判决
// 在什么范围内有效。抽出来是为了能做真值表单测：放宽会把别的宿主「还没排完版」甩到屏幕角落，
// 收紧会让 Illustrator 画布退回左上角。
// 覆盖边界：这里只有判据；取矩形、置闩、上报哪个 source 都在 TextService.cpp，判据全绿不等于
// 候选窗定对了。背景见 BinaryProtocol.h 的 CARET_SRC_TSF_DEFAULT_POS 与
// docs/architecture/caret-position-host-compat-notes.md「二·七」节。
#pragma once

namespace wind
{
namespace caret
{

/// 两个矩形四个角全同。
///
/// 别换成 Win32 EqualRect：它把任意两个空矩形判成相等，而本判据比的全是 h=0 的空矩形，
/// 「三者全同」指纹会因此失效。手写比较也让本头文件零 Win32 依赖、可用 g++ 单测。
template <class RectT>
constexpr bool SameRect(const RectT& a, const RectT& b)
{
    return a.left == b.left && a.top == b.top && a.right == b.right && a.bottom == b.bottom;
}

/// 这一帧是不是宿主在说「本 context 没有插入点可报」。
///
/// 指纹：选区矩形、组合起点、组合整体矩形三者全同且高度为 0（如 Illustrator 画布恒为工作区
/// 右下角）。不能见退化就采信：别的宿主排版中也会给退化矩形，但它们的组合矩形宽度随编码串
/// 增长；三者全同才说明宿主没在回答位置问题。
///
/// `hasCompStart` / `hasCompRect` 为假时不算（失败关闭）。
/// `hasCompRect` 还兜着一条隐性不变式：CaretEditSession.cpp 的一级/二级降级改过 caret 后都会把
/// `_hasCompositionRect` 置假，所以「组合矩形还在」蕴含「本帧没被降级改过」（单向，组合 range
/// 的 GetTextExt 失败也会置假）。去掉这一维，降级合成的帧会和退化帧交替，候选窗间歇跳左上角。
template <class RectT>
constexpr bool IsHostDefaultPosition(const RectT& caret, bool hasCompStart, const RectT& compStart,
                                     bool hasCompRect, const RectT& compRect)
{
    return caret.bottom <= caret.top && hasCompStart && hasCompRect && SameRect(caret, compStart)
           && SameRect(caret, compRect);
}

/// 这一帧是不是宿主「确实在排版」的证据：选区有高度，组合整体矩形有宽有高、与选区不是同一个
/// 矩形，且选区落在组合矩形的水平范围内、与它垂直重叠。
///
/// 用来解除 IMM32 宿主的线程级判定（见 ImmOpenCandidateNudge.h）。CUAS 上的 Java 三次查询都是
/// 同一个 h=0 的点，只答固定矩形或两个不相干矩形的宿主也都满足不了。
template <class RectT>
constexpr bool ShowsLaidOutComposition(bool caretOk, const RectT& caret, bool hasCompRect, const RectT& compRect)
{
    return caretOk && caret.bottom > caret.top && hasCompRect && compRect.right > compRect.left
           && compRect.bottom > compRect.top && !SameRect(caret, compRect) && caret.left >= compRect.left
           && caret.left <= compRect.right && caret.top < compRect.bottom && caret.bottom > compRect.top;
}

/// 二级降级（拿组合整体矩形的左上角当锚点）能不能走。
///
/// 前提是宿主算完了布局、只是不填高度：矩形有宽度、落在宿主声明的显示区内。
/// `hostDefaultPos` 命中时不走：CUAS 的 Java 客户端角点矩形 w=1，窗口最大化后显示区会把角点
/// 包进去，越界校验单独挡不住。`hasScreenExt` 为假一律不走（失败关闭），见 CaretEditSession.cpp。
constexpr bool AllowCompositionRectFallback(bool hostDefaultPos, bool hasScreenExt, bool hasCompRect,
                                            long compRectWidth, bool compRectInsideScreenExt)
{
    return !hostDefaultPos && hasScreenExt && hasCompRect && compRectWidth > 0 && compRectInsideScreenExt;
}

/// 「没有插入点」的判决现在还作不作数。
///
/// 判决由 CARET_RETRY 定时器那次异步取坐标作出，只供同一次组合内后续按键的同步路径使用
/// （它们每次都拿到同一个退化矩形，不认闩就跌回 GUI_CARET），所以作用域就是「组合还在」。
/// 作废刻意做在读闩处而非各结束出口：出口有四个（EndComposition、CommitText、
/// OnCompositionTerminated、焦点切换），漏复位一个，同实例里下一个控件（如 Illustrator 面板
/// 输入框）的首帧退化就会被误报到屏幕角落。
constexpr bool LatchApplies(bool latched, bool hasComposition)
{
    return latched && hasComposition;
}

} // namespace caret
} // namespace wind
