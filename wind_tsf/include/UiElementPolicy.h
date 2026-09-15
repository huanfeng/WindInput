// TSF 候选 UI 元素的**纯判据**：什么时候答快照、什么时候补拉、谁算在画候选。
//
// 从 CTextService 里抽出来只为一件事：让它们可测。这个状态机有四个互相咬合的输入
// （宿主声明 / 读取闩 / 快照空否 / 脏位）× 两种调用来路（取内容 vs 取元信息），
// 而它跑在 TSF 宿主进程里、需要真机才能观察——真值表单测是唯一能把它钉住的手段。
//
// ⚠️ 覆盖边界：这里只有判据，没有取数。快照怎么拉、IPC 超时多久、UpdateUIElement
// 什么时候发，全在 TextService.cpp 里，本文件一行也覆盖不到。判据全绿 ≠ 候选喂对了。
//
// 背景与实测证据见 docs/design/game-compat-tsf-uielement.md §1.4。
#pragma once

namespace wind
{
namespace uielement
{

/// 「宿主在画候选」——声明接管**或**实际读走过候选串。
///
/// 两个入参语气不同，合并只发生在这一个函数里：`declared` 是宿主自己说的
/// （`BeginUIElement` 回 `pbShow=FALSE` / `Show(FALSE)`），是事实；
/// `readCandidates` 是「它把候选文本取走了」的推断。上报给服务端时**必须分两位**，
/// 因为推断那一半在 core 侧默认就不收窗（opt-in，compat 写 `host_drawn_candidates = true`
/// 才生效）。
/// ⚠ `declared` **刻意不含 `_uiLessThread`**（调用方只传 `_uiHostDraws`）：UI-less 线程的
/// `BeginUIElement` 按规范必回 `pbShow=FALSE`，`_uiHostDraws` 随即置真，这里无须再并一次；
/// 并进来反而会在「UI-less 线程的 Begin 意外回了 TRUE」时改变行为。
/// 同一份代码里还有两处口径**更宽**，勿混用：
///   - `CTextService::_CaretQuerySuppressed()` = `_uiLessThread || _uiHostDraws`；
///   - 服务端 `UiElementStatePayload::host_draws()` = `HOST_DRAWS | UI_LESS_THREAD`。
constexpr bool HostDraws(bool declared, bool readCandidates)
{
    return declared || readCandidates;
}

/// getter 答真快照（true）还是答占位（false）。
///
/// - 已认定宿主在画 ⇒ 恒答快照。与 `SetSelection` / `Finalize` / `Abort` 的放行判据
///   同源；两边不同源会出现「GetCount 说有 1 条、GetString 给占位『…』、宿主照着选
///   却被 SetSelection 回 E_INVALIDARG」这种自相矛盾。
/// - 尚未认定 ⇒ 要求快照**非空且不脏**。脏意味着候选已经变了而我们刻意没去拉
///   （见 [`ShouldRefreshOnGet`] 的成本论证），这时答旧快照就是在交付上一帧。
constexpr bool UseSnapshot(bool declared, bool readCandidates, bool snapshotEmpty, bool dirty)
{
    if (HostDraws(declared, readCandidates))
    {
        return true;
    }
    return !snapshotEmpty && !dirty;
}

/// getter 入口要不要现拉一次快照。`contentRead` = 本次调用是不是在取候选内容本身
/// （只有 `GetString` 为 true）。
///
/// ⚠️ `contentRead` 不是洁癖：`GetCount` 是 **msctf 自己**也会问的（它据此判断候选 UI
/// 「有没有意义」，Chromium 的 IME-first 调度就靠这个），在那里无条件补拉等于给每个
/// 宿主的每一次按键都加一次宿主 UI 线程上的同步 IPC。
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

/// 「脏位 ∧ 已认定在画」这个组合是否出现了——出现即**调用方的不变量被破坏**。
///
/// 不变量本身不在这里，在调用方：脏位只在 [`ShouldRefreshEagerly`] 回 false 那一支被
/// 置起（`NotifyCandidatesVisibilityChanged` 第三分支），而读取闩只在 `GetString` 里合上、
/// 合闩之前必先经 [`ShouldRefreshOnGet`] 补拉并清脏。于是这个组合进不来，取元信息那 5 个
/// getter 里的补拉是**防御性死代码**。
///
/// ⛔ 别写成 `constexpr bool DirtyCanCoexistWithHostDraws() { return false; }` 再拿单测
/// 断言它为假——那是拿一个字面量自证其说，与 `TextService.cpp` 没有任何耦合：真在急刷
/// 那支加一句 `_uiSnapshotDirty = TRUE` 把不变量破掉，整个测试套一条都不会红
/// （2026-09-14 三轮审查实测）。看守必须落在**会被执行到的那条路**上，所以做成一个
/// 谓词，由 `_EnsureUiSnapshotFresh` 在运行时调用、破了就记 WARN 进环形缓冲。
constexpr bool InvariantBroken(bool dirty, bool declared, bool readCandidates)
{
    return dirty && HostDraws(declared, readCandidates);
}

} // namespace uielement
} // namespace wind
