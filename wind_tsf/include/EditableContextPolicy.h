// 「焦点所在 context 有没有可编辑的东西」的纯判据：把 `ITfContext::GetStatus` 的返回码与
// 动态标志翻译成布尔。抽出来是为了能单测：判宽了工具栏永不隐藏，判窄了永不显示。
// 覆盖边界：只有判据，没有取数；`GetTop` 怎么拿、判决后发哪条 IPC 在 TextService.cpp，
// 判据全绿不等于工具栏隐显正确。
#pragma once

#include <cstdint>

namespace wind
{
namespace editable
{

/// `TF_E_EMPTYCONTEXT`（msctf.idl: `MAKE_HRESULT(SEVERITY_ERROR, FACILITY_ITF, 0x0509)`）。
///
/// 本头文件刻意不含 Win32 头（以便 g++ 在非 Windows 上单测），故按数值重述，改动前先与 SDK 对一遍。
inline constexpr std::uint32_t kEmptyContextHr = 0x80040509U;

/// HRESULT 的失败位（`FAILED()` 宏看的那一位）。
///
/// 不能写成 `hr < 0`：Linux 上 `long` 是 64 位，`0x80040509L` 是正数，单测里符号位判据失效。
inline constexpr std::uint32_t kFailureBit = 0x80000000U;

/// `TF_SD_READONLY` = `TS_SD_READONLY`，动态标志第 0 位。
inline constexpr std::uint32_t kReadOnlyDynFlag = 0x1U;

/// 这个 context 里有没有可编辑的东西。
///
/// - `GetStatus` 成功：只认 `TF_SD_READONLY`，不看静态位（`TS_SS_TRANSITORY` 不可靠）。
/// - 返回 `TF_E_EMPTYCONTEXT`：宿主在说这个 context 没有文本存储（Chromium 系焦点不在可编辑元素
///   上时就是这样），判不可编辑。
/// - 其余失败：判可编辑。判窄了工具栏在本焦点会话内不会自愈，故未知失败码归到代价小的一侧。
///
/// 依据与宿主指纹见 docs/architecture/tsf-docmgr-focus-semantics.md 的「可编辑性判据」。
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
