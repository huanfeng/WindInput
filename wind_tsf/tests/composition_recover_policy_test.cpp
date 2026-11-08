// CompositionRecoverPolicy 单元测试
//
// 跑法（纯 C++17、不含 Win32 头）：
//   g++ -std=c++17 -I../include -o composition_recover_policy_test composition_recover_policy_test.cpp && ./composition_recover_policy_test

#include "CompositionRecoverPolicy.h"

#include <cstdio>

namespace
{
int g_failures = 0;

void Expect(bool cond, const char* what)
{
    if (!cond)
    {
        std::printf("FAIL: %s\n", what);
        ++g_failures;
    }
}
} // namespace

int main()
{
    using wind::recover::kMaxLeftoverLen;
    using wind::recover::kWindowMs;
    using wind::recover::ShouldRecover;

    Expect(ShouldRecover(13, false, 0, 1), "受控输入框回写：写组合后 13ms 被终止，恢复");
    Expect(ShouldRecover(kWindowMs, false, 0, 1), "窗口边界上仍恢复");
    Expect(!ShouldRecover(kWindowMs + 1, false, 0, 1), "超过窗口：当真正的结束处理");
    Expect(!ShouldRecover(13, true, 0, 1), "智能符号预上屏中：终止即提交，不恢复");
    Expect(!ShouldRecover(13, false, 2, 3), "有顶码待提交前缀：不恢复");
    Expect(!ShouldRecover(13, false, 0, 0), "组合是空的：没有可接的东西");
    Expect(ShouldRecover(13, false, 0, kMaxLeftoverLen), "长度上限内恢复");
    Expect(!ShouldRecover(13, false, 0, kMaxLeftoverLen + 1), "残留过长：不恢复");

    if (g_failures == 0)
        std::printf("composition_recover_policy_test: all passed\n");
    return g_failures == 0 ? 0 : 1;
}
