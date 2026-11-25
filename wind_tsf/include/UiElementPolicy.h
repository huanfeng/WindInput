// TSF 候选 UI 元素的纯判据：什么时候答快照、什么时候补拉、谁算在画候选。
//
// 从 CTextService 抽出来是为了可测：四个输入（宿主声明 / 读取闩 / 快照空否 / 脏位）
// × 两种调用来路（取内容 / 取元信息），只能靠真值表单测钉住。
// 覆盖边界：这里只有判据，取数、IPC 超时、UpdateUIElement 时机都在 TextService.cpp，
// 判据全绿不等于候选喂对了。背景见 docs/design/game-compat-tsf-uielement.md §1.4。
#pragma once

namespace wind
{
namespace uielement
{

/// 「宿主在画候选」：声明接管或实际读走过候选串。
///
/// `declared` 是宿主自己说的（`BeginUIElement` 回 `pbShow=FALSE` / `Show(FALSE)`），
/// `readCandidates` 是推断；上报服务端时必须分两位，推断那一半在 core 侧是 opt-in
/// （compat `host_drawn_candidates = true`）。
/// `declared` 刻意不含 `_uiLessThread`（调用方只传 `_uiHostDraws`），否则会改变
/// 「UI-less 线程的 Begin 意外回 TRUE」时的行为。另有两处口径更宽，勿混用：
/// `CTextService::_CaretQuerySuppressed()` 与服务端 `UiElementStatePayload::host_draws()`。
constexpr bool HostDraws(bool declared, bool readCandidates)
{
    return declared || readCandidates;
}

/// getter 答真快照（true）还是答占位（false）。
///
/// - 已认定宿主在画 ⇒ 恒答快照，须与 `SetSelection` / `Finalize` / `Abort` 的放行判据同源，
///   否则会出现 GetCount 说有 1 条、GetString 给占位、SetSelection 却回 E_INVALIDARG。
/// - 尚未认定 ⇒ 要求快照非空且不脏；脏时答旧快照就是交付上一帧。
constexpr bool UseSnapshot(bool declared, bool readCandidates, bool snapshotEmpty, bool dirty)
{
    if (HostDraws(declared, readCandidates))
    {
        return true;
    }
    return !snapshotEmpty && !dirty;
}

/// getter 入口要不要现拉一次快照。`contentRead` = 本次是否在取候选内容（只有 `GetString` 为 true）。
///
/// `GetCount` msctf 自己也会问（Chromium 的 IME-first 调度靠它），在那里无条件补拉等于
/// 给每次按键加一次宿主 UI 线程上的同步 IPC。
constexpr bool ShouldRefreshOnGet(bool dirty, bool contentRead, bool readCandidates)
{
    if (!dirty)
    {
        return false;
    }
    return contentRead || readCandidates;
}

/// 候选变化时：立刻拉快照再通知（true），还是只记脏、等宿主来读再拉（false）。
constexpr bool ShouldRefreshEagerly(bool declared, bool readCandidates)
{
    return HostDraws(declared, readCandidates);
}

/// 「脏位 ∧ 已认定在画」出现即调用方的不变量被破坏。
///
/// 脏位只在 [`ShouldRefreshEagerly`] 回 false 那支置起，读取闩只在 `GetString` 里经
/// [`ShouldRefreshOnGet`] 补拉清脏后合上，所以元信息 getter 里的补拉是防御性代码。
/// 别改成返回字面量 false 再单测断言：那与 TextService.cpp 无耦合，破了也不会红。
/// 看守由 `_EnsureUiSnapshotFresh` 在运行时调用本谓词，破了记 WARN。
constexpr bool InvariantBroken(bool dirty, bool declared, bool readCandidates)
{
    return dirty && HostDraws(declared, readCandidates);
}

} // namespace uielement
} // namespace wind
