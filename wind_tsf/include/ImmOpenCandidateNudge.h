// 代 CUAS 向 IMM32 宿主补发 IMN_OPENCANDIDATE。
//
// 走 CUAS 的 Java 7 AWT 宿主经 TSF 报不出插入点，只在收到 IMN_OPENCANDIDATE 后经
// ImmSetCandidateWindow 写 CANDIDATEFORM，而 CUAS 对我们的候选 UIElement 从不发这条通知。
// 只发给已证实是这类宿主的线程（出现过 `wind::caret::IsHostDefaultPosition` 指纹）：OPEN 会让
// 自绘候选的 IMM32 宿主弹出自己的候选框。每次组合起止都要补发；判定在线程出现正常排版的组合
// 矩形时解除（Edge 所有网页输入框共用一个线程）。这类宿主不上报组合起点（见 CaretEditSession）。
// 依据见 docs/architecture/caret-position-host-compat-notes.md 的「代 CUAS 补发 IMN_OPENCANDIDATE」。
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

/// 组合结束 / 上屏时调用：OPEN 让 AWT 按上屏后的光标刷新 CANDIDATEFORM，紧跟 CLOSE 配对。
/// AWT 不处理 CLOSE；它防的是在 OPEN 时弹自己候选框的 IMM32 宿主，只发 OPEN 那个框会残留。
inline void PostImmCandidateRefreshIfHost(const wchar_t* reason)
{
    if (t_immOpenCandidateHost)
        PostImmOpenCandidate(reason, /*thenClose=*/true);
}

} // namespace caret
} // namespace wind
