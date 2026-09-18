// 「焦点落在的这个 context 里有没有可编辑的东西」的**纯判据**：把
// `ITfContext::GetStatus` 的返回码 + 动态标志翻译成一个布尔。
//
// 从 `CTextService::_DocMgrHasEditableContext` 里抽出来只为一件事：让它可测。判据跑在 TSF
// 宿主进程里、要真机才观察得到，而两个失败方向都很贵——**放宽**了就是把「焦点不在输入框」
// 当成「在输入框」，工具栏永不隐藏（GH#134 的后半条，实测 Chromium 系宿主全中）；**收紧**
// 了则是把真能打字的地方判成不能打，工具栏永不显示。真值表单测是唯一能钉住它的手段。
//
// ⚠️ 覆盖边界：这里只有判据，没有取数。`GetTop` 怎么拿、拿不到算什么、判决之后发哪条
// IPC，全在 TextService.cpp 里，本文件一行也覆盖不到。判据全绿 ≠ 工具栏隐对了。
#pragma once

#include <cstdint>

namespace wind
{
namespace editable
{

/// `TF_E_EMPTYCONTEXT`（msctf.idl: `MAKE_HRESULT(SEVERITY_ERROR, FACILITY_ITF, 0x0509)`）。
///
/// 本头文件刻意**不含任何 Win32 头**（才能用 g++ 在非 Windows 机器上单测），故在这里
/// 按数值重述。改动前先与 SDK 对一遍。
inline constexpr std::uint32_t kEmptyContextHr = 0x80040509U;

/// HRESULT 的失败位（`FAILED()` 宏看的那一位）。
///
/// ⚠ **不能**写成 `hr < 0`：`HRESULT` 在 Windows 上是 32 位 `long`，而本头文件要能用 g++
/// 在 Linux 上单测，那里 `long` 是 64 位，`0x80040509L` 是个**正数**，符号位判据当场失效
/// （2026-09-18 写这条测试时实测踩到）。统一按无符号取位，与宿主平台的 `long` 宽度无关。
inline constexpr std::uint32_t kFailureBit = 0x80000000U;

/// `TF_SD_READONLY` = `TS_SD_READONLY`，动态标志第 0 位。
inline constexpr std::uint32_t kReadOnlyDynFlag = 0x1U;

/// 这个 context 里有没有可编辑的东西。
///
/// - `GetStatus` **成功**：只认 `TF_SD_READONLY`。`TS_SS_TRANSITORY` 单独判可编辑性是不
///   可靠的（Chrome 与 JetBrains 都会把它挂在真能打字的 context 上），故本判据不看静态位。
/// - `GetStatus` 返回 **`TF_E_EMPTYCONTEXT`**：这不是「查询出错」，而是宿主在说**这个
///   context 根本没有文本存储**——Chromium 家族给「焦点不在可编辑元素上」准备的就是这样
///   一个 DocMgr（`TEXT_INPUT_TYPE_NONE`：照常建 context，但不挂 text store）。判成不可
///   编辑正是它的字面语义。
/// - 其余失败：保持宽松兜底（判成可编辑）。两个方向的代价不对称：判宽了只是工具栏多显示
///   一会儿；判窄了则是工具栏在那个宿主里不显示，**且本焦点会话内不会自愈**（`no_edit_ctx`
///   分支明确清掉 `_needsFocusRecovery`，要等下一次 `OnSetFocus` / `OnSetThreadFocus` 才
///   重判）。故未知的失败码一律归到便宜的那一侧。
///
///   `TF_E_EMPTYCONTEXT` 之所以敢归到贵的那一侧，是因为它不只是「这一个查询失败了」：
///   实测（Illustrator 的 CEPHtmlEngine，Win11）同一个 context 上 `RequestEditSession`
///   也返回 `0x80040509`，连插入点都取不到 —— 那里本来就打不了字。
///
/// ★ 实测证据（2026-09-18，Win10 22H2 + Edge，TSF 日志）：点进搜索框 → DocMgr A
/// (`dynFlags=0x40 statFlags=0xC inputScope=0x20`)，点网页空白处 → DocMgr B，B 上
/// `GetStatus` 恒返回 `0x80040509`。改动前 B 落在「其余失败」那一侧被判成可编辑，于是
/// 每次点空白处都照发 `focus_gained`，服务端的 `has_edit_context` 再也回不到假。
/// 同一指纹在 Win11 的 CEF 宿主（CEPHtmlEngine）日志里同样存在 —— **这条不是 Win10 专属**。
constexpr bool ContextIsEditable(std::uint32_t getStatusHr, std::uint32_t dynamicFlags)
{
    if ((getStatusHr & kFailureBit) != 0U)
    {
        return getStatusHr != kEmptyContextHr;
    }
    return (dynamicFlags & kReadOnlyDynFlag) == 0U;
}

} // namespace editable
} // namespace wind
