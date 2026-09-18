// ContextIsEditable 判据测试
//
// 跑法（纯 C++17、不含 Win32 头，本机不需要 MSVC）：
//   g++ -std=c++17 -Iwind_tsf/include -o ecp_test wind_tsf/tests/editable_context_policy_test.cpp && ./ecp_test
// 或经 CMake：
//   cmake -S wind_tsf -B build/tsf-tests -DWIND_TSF_TESTS=ON
//   cmake --build build/tsf-tests && ctest --test-dir build/tsf-tests
//
// 变异检验已做（2026-09-18），逐条命中：
//   - 把空上下文那条去掉（失败一律 TRUE，即改动前那版）      → TestEmptyContext 红
//   - 把失败一律判成 FALSE（不区分错误码）                   → TestUnknownFailureStaysLenient 红
//   - 成功分支改成看 TS_SS_TRANSITORY 而非 TF_SD_READONLY    → TestTransitoryIsNotAVerdict 红
//   - 把失败判据从取失败位改成 `hr != 0`                     → TestSuccessCodesOtherThanZero 红
//   - 把失败判据写成 `(long)hr < 0`（Linux 上 long 是 64 位） → TestEmptyContext 红
// 改测试时请保住这个性质——一条永远绿的测试不会告诉你任何事。

#include "EditableContextPolicy.h"

#include <cstdio>

namespace
{

using namespace wind::editable;

int g_failures = 0;

void Check(bool cond, const char* what)
{
    if (!cond)
    {
        std::printf("  FAIL: %s\n", what);
        ++g_failures;
    }
}

/// ★ 本次修的那条：空上下文 = 焦点不在可编辑元素上。
///
/// Chromium 家族给 `TEXT_INPUT_TYPE_NONE` 留的 DocMgr 照常建 context、但不挂 text store，
/// 于是 `GetStatus` 返回 `TF_E_EMPTYCONTEXT`。改动前这里兜底成「可编辑」，点网页空白处
/// 照发 focus_gained，工具栏永不隐藏（GH#134 后半条）。
void TestEmptyContext()
{
    std::printf("TestEmptyContext\n");
    Check(!ContextIsEditable(kEmptyContextHr, 0), "空上下文必须判不可编辑");
    // 失败时 dynFlags 是没被填过的初值，给什么都不该改变判决。
    Check(!ContextIsEditable(kEmptyContextHr, 0x40), "空上下文与 dynFlags 无关");
    Check(!ContextIsEditable(kEmptyContextHr, kReadOnlyDynFlag), "空上下文与 READONLY 位无关");
}

/// 未知失败码仍走宽松兜底：两个方向的代价不对称——多显示一会儿只是碍眼，
/// 而把能打字的地方判成不能打会让工具栏在那个宿主里永不出现。
void TestUnknownFailureStaysLenient()
{
    std::printf("TestUnknownFailureStaysLenient\n");
    Check(ContextIsEditable(0x80004005U, 0), "E_FAIL 属未知失败码，保持宽松");
    Check(ContextIsEditable(0x80040501U, 0), "TF_E_NOLOCK 同理");
    Check(ContextIsEditable(0x80070005U, 0), "E_ACCESSDENIED 同理");
}

/// 成功分支只认 READONLY。
void TestReadOnlyIsTheOnlySuccessVerdict()
{
    std::printf("TestReadOnlyIsTheOnlySuccessVerdict\n");
    Check(ContextIsEditable(0, 0), "普通可编辑 context");
    Check(!ContextIsEditable(0, kReadOnlyDynFlag), "READONLY 判不可编辑");
    Check(!ContextIsEditable(0, kReadOnlyDynFlag | 0x40), "READONLY 与别的位共存时仍判不可编辑");
    // 实测值：Edge 的真输入框 dynFlags=0x40（TS_SD_INPUTPANEMANUALDISPLAYENABLE）。
    Check(ContextIsEditable(0, 0x40), "Edge 真输入框（dynFlags=0x40）必须判可编辑");
}

/// TS_SS_TRANSITORY 是**静态**标志，压根不进本判据——Chrome 与 JetBrains 都把它挂在真能
/// 打字的 context 上。这条测的是「静态位没有从别处溜进来当判据」。
void TestTransitoryIsNotAVerdict()
{
    std::printf("TestTransitoryIsNotAVerdict\n");
    // 0x4 若被当成 READONLY 之外的否决位，这一条会红。
    Check(ContextIsEditable(0, 0x4), "静态 TRANSITORY 的位值出现在 dynFlags 上也不否决");
    Check(ContextIsEditable(0, 0x80000000U), "Edge 过渡型 DocMgr 的 dynFlags 同样不否决");
}

/// HRESULT 的成功不止 `S_OK`：失败判据必须取失败位，不能是「非零即失败」。
/// 写成 `hr != 0` 的话，`S_FALSE`(1) 这类成功码会被当失败，落到空上下文之外的宽松兜底上——
/// 方向虽然侥幸不错，但判据已经错了，换个错误码就会翻车。
void TestSuccessCodesOtherThanZero()
{
    std::printf("TestSuccessCodesOtherThanZero\n");
    Check(!ContextIsEditable(1U, kReadOnlyDynFlag), "S_FALSE 是成功码，READONLY 仍须生效");
    Check(ContextIsEditable(1U, 0), "S_FALSE + 无 READONLY = 可编辑");
}

} // namespace

int main()
{
    TestEmptyContext();
    TestUnknownFailureStaysLenient();
    TestReadOnlyIsTheOnlySuccessVerdict();
    TestTransitoryIsNotAVerdict();
    TestSuccessCodesOtherThanZero();

    if (g_failures == 0)
    {
        std::printf("all passed\n");
        return 0;
    }
    std::printf("%d check(s) failed\n", g_failures);
    return 1;
}
