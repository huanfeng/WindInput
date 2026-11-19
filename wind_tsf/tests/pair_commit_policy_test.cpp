// PairCommitPolicy 判据测试（配对插入提交方式：直写 vs 按键注入）
//
// 跑法（纯 C++17、不含 Win32 头）：
//   g++ -std=c++17 -Wall -Wextra -I../include -o pair_commit_test pair_commit_policy_test.cpp && ./pair_commit_test
//
// 变异检验：DecodePairViaComp 空值不保持 ⇒ TestDecode 红。

#include "PairCommitPolicy.h"

#include <cstdint>
#include <cstdio>

namespace
{
using namespace wind::paircommit;

int g_failures = 0;

void Expect(bool cond, const char* what)
{
    if (!cond)
    {
        std::printf("FAIL: %s\n", what);
        ++g_failures;
    }
}

void TestDecode()
{
    const std::uint8_t one = 1, zero = 0;
    Expect(DecodePairViaComp(&one, 1, false), "1 = 开");
    Expect(!DecodePairViaComp(&zero, 1, false), "0 = 关");
    Expect(!DecodePairViaComp(&zero, 1, true), "0 显式盖掉现值");
    // 空值保持现值：重连从默认起步后服务端必推，格式意外不该翻转状态。
    Expect(!DecodePairViaComp(nullptr, 0, false), "空值保持 false");
    Expect(DecodePairViaComp(nullptr, 0, true), "空值保持 true");
    Expect(DecodePairViaComp(nullptr, 1, true), "空指针保持现值");
}

void TestDefault()
{
    Expect(!kDefaultPairViaComp, "出厂必须关：直写是长期现状，全局翻转待实测矩阵");
}
} // namespace

int main()
{
    TestDecode();
    TestDefault();
    if (g_failures == 0)
    {
        std::printf("pair_commit_policy_test: all pass\n");
        return 0;
    }
    std::printf("pair_commit_policy_test: %d failure(s)\n", g_failures);
    return 1;
}
