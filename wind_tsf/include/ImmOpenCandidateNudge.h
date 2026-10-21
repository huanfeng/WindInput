// 代 CUAS 向 IMM32 宿主补发 IMN_OPENCANDIDATE。
//
// 背景（2026-10-04 靶机实测，JDK 7u45 + 复现程序，消息钩子抓 WM_IME_*）：
//   - 走 IMM32→TSF 兼容层（CUAS）的 Java 7 宿主，经 TSF 报不出插入点（GetTextExt 三次查询给
//     同一个工作区右下角像素），它报位置的唯一渠道是收到 IMN_OPENCANDIDATE 后经
//     ImmSetCandidateWindow 写 CANDIDATEFORM。
//   - CUAS 对我们的候选 UIElement **从不发** IMN_OPENCANDIDATE / IMN_CLOSECANDIDATE，
//     BeginUIElement 不发任何通知，UpdateUIElement 只翻成 IMN_CHANGECANDIDATE；微软拼音
//     同条件下收得到 OPEN（5）、SETCANDIDATEPOS（9）、CLOSE（4）。判据在 CUAS 内部，
//     去掉 Begin 后那次立即 Update 也不改变（已实测）。
//   - JDK 7（修 JDK-8019990 之前，客户的 7u21 即是）的 AWT 只认 OPEN；JDK 8 连 CHANGE 也认，
//     所以同一套代码在 JDK 8 下正常、在 7u21 下候选窗恒落右下角。
//   - 补发一条 OPEN 后，AWT 按 getTextLocation 写 CANDIDATEFORM，CUAS 的 GetTextExt 随即
//     也改答这个点——候选窗定位与微软拼音一致（用户实测确认）。
//
// 只发给「已证实是这类宿主」的线程：本线程出现过宿主默认位置指纹（见
// `wind::caret::IsHostDefaultPosition`）才置位。理由：IMN_OPENCANDIDATE 会让自绘候选的
// IMM32 宿主（不少游戏）打开它自己的候选框，不能对所有宿主广播。
//
// ⚠ 判定按线程记，但一个线程上不止一种输入框：Edge 所有网页输入框都在浏览器 UI 线程，
//   某个框出过一次指纹，此后整个进程不再报组合起点，候选窗跟着插入点逐字抖动。所以
//   本线程出现「确实在排版」的帧（`wind::caret::ShowsLaidOutComposition`）时解除判定。
//   Java 每帧都给出指纹，误解除也会在下次组合首帧重新置位。
//   「不取 GUI caret」不随之解除（见 IsGuiCaretUnreliable）：它管的是组合开始前那一刻，
//   那时没有组合矩形，判据既置不了位也解不了；解除后切回 JTable / 画布，首键就会拿到
//   窗口原点那个系统光标。
//
// ⚠ AWT 只在收到 OPEN 时刷新 CANDIDATEFORM，之后 CUAS 就一直答这个点——所以**每次组合
//   开始与结束都要补发**：只在首次指纹时发，后续组合会全部停在第一次的位置（换输入框后
//   实测如此）。结束时发是为了让 AWT 在两次组合之间按上屏后的光标刷新一次，下次组合
//   首帧读到的就已是新位置。
// ⚠ 回写是异步的（AWT 投递到 EDT 再回来），组合首帧读到的仍可能是旧值，因此这类宿主
//   **不上报组合起点**（见 CaretEditSession），否则旧值会被服务端锁成整场组合的锚点。
#pragma once

#include <windows.h>
#include <imm.h>

#include "Globals.h"

namespace wind
{
namespace caret
{

/// 本线程是否已证实为「CUAS 下经 TSF 报不出插入点、靠 IMN_OPENCANDIDATE 才报位置」的宿主。
/// TSF 文本服务每线程一个实例，edit session 也在该线程执行，故按线程记。
inline thread_local bool t_immOpenCandidateHost = false;

inline bool IsImmOpenCandidateHost()
{
    return t_immOpenCandidateHost;
}

/// 本线程出现过宿主默认位置指纹，其系统光标不可信（Java 恒在窗口客户区原点）。只置不清。
inline thread_local bool t_guiCaretUnreliable = false;

inline bool IsGuiCaretUnreliable()
{
    return t_guiCaretUnreliable;
}

/// 向本线程的焦点窗口补发 IMN_OPENCANDIDATE；`thenClose` 时紧跟一条 IMN_CLOSECANDIDATE。
/// 用 Post 而非 Send：调用点多在 edit session 内，同步把消息送进宿主窗口过程有重入风险；
/// AWT 本来也是投递到 EDT 异步处理。两条按投递顺序处理。
inline void PostImmOpenCandidate(const wchar_t* reason, bool thenClose = false)
{
    const HWND hwnd = GetFocus();
    if (hwnd == nullptr)
        return;
    const BOOL posted = PostMessageW(hwnd, WM_IME_NOTIFY, IMN_OPENCANDIDATE, 1);
    const BOOL closed = thenClose ? PostMessageW(hwnd, WM_IME_NOTIFY, IMN_CLOSECANDIDATE, 1) : FALSE;
    WIND_LOG_DEBUG_FMT(L"IMN_OPENCANDIDATE 补发(%s) hwnd=0x%p posted=%d close=%d\n", reason, (void*)hwnd,
                       (int)posted, (int)closed);
}

/// 观测到宿主默认位置指纹时调用：首次置位并立即补发一次（让当前这次组合也能拿到位置）。
inline void NoteHostDefaultPositionForImm()
{
    t_guiCaretUnreliable = true;
    if (t_immOpenCandidateHost)
        return;
    t_immOpenCandidateHost = true;
    WIND_LOG_INFO(L"本线程判定为 IMM32 宿主（TSF 报不出插入点），此后每次组合起止补发 IMN_OPENCANDIDATE\n");
    PostImmOpenCandidate(L"首次识别");
}

/// 观测到宿主确实在排版时调用：解除本线程的判定，恢复上报组合起点、停止补发。
///
/// 只会在组合中被调用（判据要组合矩形），而判定为真时本次组合已发过 OPEN（组合开始或首次识别），
/// 结束时却不再补发——所以这里补一条 CLOSE 配对，理由同 PostImmCandidateRefreshIfHost。
inline void NoteLaidOutCompositionForImm()
{
    if (!t_immOpenCandidateHost)
        return;
    t_immOpenCandidateHost = false;
    const HWND hwnd = GetFocus();
    const BOOL closed = hwnd != nullptr ? PostMessageW(hwnd, WM_IME_NOTIFY, IMN_CLOSECANDIDATE, 1) : FALSE;
    WIND_LOG_INFO_FMT(L"本线程出现正常排版的组合矩形，解除 IMM32 宿主判定 hwnd=0x%p close=%d\n", (void*)hwnd,
                      (int)closed);
}

/// 组合开始时调用：已证实的宿主才补发。
inline void PostImmOpenCandidateIfHost(const wchar_t* reason)
{
    if (t_immOpenCandidateHost)
        PostImmOpenCandidate(reason);
}

/// 组合结束 / 上屏时调用：OPEN 让 AWT 按上屏后的光标刷新 CANDIDATEFORM，紧跟的 CLOSE 让
/// 消息流与微软拼音一致地成对（OPEN…CLOSE）。AWT 不处理 CLOSE，对 Java 无影响；它防的是
/// 「命中指纹、又在 OPEN 时弹自己候选框」的 IMM32 宿主——只发 OPEN 不发 CLOSE，那个框会
/// 在组合结束后留在屏幕上。
inline void PostImmCandidateRefreshIfHost(const wchar_t* reason)
{
    if (t_immOpenCandidateHost)
        PostImmOpenCandidate(reason, /*thenClose=*/true);
}

} // namespace caret
} // namespace wind
