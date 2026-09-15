// 「宿主对本 context 没有插入点可报」的**纯判据**：什么形态的退化矩形算宿主的默认位置，
// 以及那个判决在什么范围内有效。
//
// 从 CTextService 里抽出来只为一件事：让它们可测。判据跑在 TSF 宿主进程里、要真机才观察得到，
// 而它的两个失败方向都很贵——放宽了就是拿别的宿主的「还没排完版」冒充「没有插入点」，把候选窗
// 甩到屏幕角落；收紧了 Illustrator 画布就退回钉死在左上角。真值表单测是唯一能钉住它的手段。
//
// ⚠️ 覆盖边界：这里只有判据，没有取数。矩形怎么取、闩什么时候置、上报哪个 source，
// 全在 TextService.cpp 里，本文件一行也覆盖不到。判据全绿 ≠ 候选窗定对了。
//
// 背景见 BinaryProtocol.h 的 CARET_SRC_TSF_DEFAULT_POS 与 CaretEditSession.h 的
// CaretProbeKind::RetryDeadline。
#pragma once

namespace wind
{
namespace caret
{

/// 两个矩形四个角全同。
///
/// 刻意**不用 Win32 `EqualRect`**，而且这不是风格偏好：那个 API 对**空矩形**另有一套
/// 语义，会把两个坐标完全不同的空矩形判成相等——而本判据比的恰好**全是** h=0 的空矩形。
/// 用它的话，下面那条「三者全同」指纹的三条腿会同时失效，任何一帧退化矩形都能命中，
/// 于是「指纹」退化成「见退化就采信」，正是要防的那件事。
///
/// 手写逐成员比较，语义一眼可见，也让本头文件保持零 Win32 依赖、能用 g++ 在非 Windows
/// 机器上单测。
template <class RectT>
constexpr bool SameRect(const RectT& a, const RectT& b)
{
    return a.left == b.left && a.top == b.top && a.right == b.right && a.bottom == b.bottom;
}

/// 这一帧是不是宿主在说「本 context 没有插入点可报」。
///
/// 指纹：**三次布局查询给出完全相同的退化矩形**——选区矩形、组合起点、组合整体矩形
/// 三者全同且高度为 0。Illustrator 30.8 画布文字实测恒为 (2559,1367,2560,1367)，正是
/// 工作区右下角最后一个像素；同机 10 个宿主给的都是这一个值。
///
/// ★ **为什么要指纹，不能见退化就采信**：证据只来自一家宿主，而「形态相同、期望相反」
/// 在本仓有过实打实的翻车——`compat.toml:261` 的 `pin_anchor_when_start_drifts`
/// 2026-09-05 从 per-app 改成全局，当场弄坏 Excel：Excel/WPS 表格的矩形形态与 WPS 文字
/// 一模一样，期望行为却相反。本次是同一类风险：别的宿主也会在没排完版时给退化矩形，
/// 但那是「等一下就有了」，采信就等于把候选窗甩到屏幕角落。
///
/// ★ 三者全同才是有信息量的那一维。单看 caret 退化区分不出两种情形；而「宿主连组合
/// 整体矩形都给同一个点」说明它压根没在回答位置问题——真在排版中的宿主，组合矩形的
/// 宽度会随编码串增长（洛克王国实测 16→19→25→28→46）。
///
/// `hasCompStart` / `hasCompRect` 为假时一律不算：缺了任何一维就无从判断三者是否全同，
/// 此时按既有行为丢弃（失败关闭）。
///
/// ⚠⚠ `hasCompRect` 这一维还兜着一条**隐性不变式**，删它的人多半不知道：
/// `CaretEditSession.cpp` 的一级/二级降级在动过 caret 之后都会把 `_hasCompositionRect`
/// 置假（各有各的理由，见那两处注释）。于是「组合矩形还在」**足以保证**「本帧没被降级
/// 改过」——两条路共用同一个前提。
/// ⚠ 只是单向蕴含，**不是等价**：`_hasCompositionRect` 为假另有一个完全不同的来源——
/// 组合 range 的 `GetTextExt` 本身失败或回 `TS_E_NOLAYOUT`（`CaretEditSession.cpp:184`
/// 压根没执行）。按「等价」去读会得出「没有 compRect ⇒ 一定降级过 ⇒ 手上已经有个降级
/// caret 了」，而真相往往正相反：宿主什么都没算出来，三条路全断。
/// 去掉这一维，二级降级合成出来的高度就重新能作废判决，
/// 同一次组合内会出现「降级帧 / 退化帧」交替：候选窗间歇跳左上角，且位置来源在
/// CARET_SRC_TSF_COMPOSITION 与 CARET_SRC_TSF_DEFAULT_POS 之间反复谎报。
template <class RectT>
constexpr bool IsHostDefaultPosition(const RectT& caret, bool hasCompStart, const RectT& compStart,
                                     bool hasCompRect, const RectT& compRect)
{
    return caret.bottom <= caret.top && hasCompStart && hasCompRect && SameRect(caret, compStart)
           && SameRect(caret, compRect);
}

/// 「没有插入点」的判决现在还作不作数。
///
/// 判决由 CARET_RETRY 定时器那次异步取坐标作出，用途只有一个：**同一次组合内后续按键
/// 的同步路径**（那条路每次都会拿到同一个退化矩形，不认闩就会跌回 GUI_CARET）。所以
/// 作用域就是「组合还在」。
///
/// ⚠ **判决作废刻意做在读闩处，不在各个出口复位**。组合的结束出口有四个
/// （`EndComposition`、`CommitText` 自己把 `_pComposition` 置空、`OnCompositionTerminated`、
/// 焦点切换），逐个补复位就是等着漏掉其中一个——而漏掉的代价是静默的：
///   Illustrator 画布输入 → 闩住 → 空格上屏（走 CommitText，不经 EndComposition）
///   → 闩仍为真 → 用户点进 Illustrator **面板**输入框（同线程、同一个 CTextService 实例）
///   → 面板首帧没排完版同样退化 → 被当成「这里没有插入点」报到屏幕角落，
///   而这时正确的行为恰恰是旧的那条：回退 GUI_CARET（面板上那个才是对的）。
/// 读闩处加一道作用域，四个出口就都不用管了。
constexpr bool LatchApplies(bool latched, bool hasComposition)
{
    return latched && hasComposition;
}

} // namespace caret
} // namespace wind
