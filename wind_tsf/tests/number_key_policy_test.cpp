// NumberKeyPolicy 判据测试（论坛 t285 与「PIN 框一键两个字」系列）
//
// 跑法（纯 C++17、不含 Win32 头）：
//   g++ -std=c++17 -Wall -Wextra -I../include -o number_key_test number_key_policy_test.cpp && ./number_key_test
//
// 变异检验：去掉 hasInputSession ⇒ TestSessionEats 红；去掉 chineseMode ⇒ TestEnglishFullWidthNotHere 红；
// 去掉 fullWidth ⇒ TestChineseIdleHalfWidthPasses 红。

#include "NumberKeyPolicy.h"

#include <cstdio>
#include <initializer_list>

namespace
{
using wind::numberkey::ShouldEatNumberKey;

int g_failures = 0;

void Expect(bool cond, const char* what)
{
    if (!cond)
    {
        std::printf("FAIL: %s\n", what);
        ++g_failures;
    }
}

void TestSessionEats()
{
    for (bool chinese : {false, true})
        for (bool full : {false, true})
            Expect(ShouldEatNumberKey(true, chinese, full), "有会话一律吃（选词 / 翻页）");
}

void TestChineseIdleHalfWidthPasses()
{
    Expect(!ShouldEatNumberKey(false, true, false), "中文空闲半角放行：宿主出字，不转发");
}

void TestChineseIdleFullWidthEats()
{
    Expect(ShouldEatNumberKey(false, true, true), "中文空闲全角吃：服务端出全角数字");
}

void TestEnglishFullWidthNotHere()
{
    Expect(!ShouldEatNumberKey(false, false, true), "英文全角不由本判据吃（另有 english_fullwidth 分支）");
    Expect(!ShouldEatNumberKey(false, false, false), "英文半角放行");
}

void TestSpaceBeforeDigit()
{
    using wind::numberkey::SpaceBeforeDigit;
    Expect(ShouldEatNumberKey(false, true, false, true), "汉字后开了中数间空格：中文空闲半角也吃");
    Expect(!ShouldEatNumberKey(false, false, false, true), "英文模式不吃（中数间空格只管中文模式）");
    Expect(SpaceBeforeDigit(true, true, '3', false, false), "汉字后主键盘数字");
    Expect(!SpaceBeforeDigit(false, true, '3', false, false), "开关关");
    Expect(!SpaceBeforeDigit(true, false, '3', false, false), "上一笔不是汉字");
    Expect(SpaceBeforeDigit(true, true, 0x63, false, false), "汉字后小键盘数字");
    Expect(SpaceBeforeDigit(true, true, 0x60, false, false), "小键盘 0");
    Expect(!SpaceBeforeDigit(true, true, 0x6A, false, false), "小键盘运算符不在其列");
    Expect(!SpaceBeforeDigit(true, true, 0x6E, false, false), "小键盘小数点不在其列");
    Expect(!SpaceBeforeDigit(true, true, '3', true, false), "带修饰键（Shift+3 是 #）");
    Expect(!SpaceBeforeDigit(true, true, '3', false, true), "CapsLock 开");
}

} // namespace

int main()
{
    TestSpaceBeforeDigit();
    TestSessionEats();
    TestChineseIdleHalfWidthPasses();
    TestChineseIdleFullWidthEats();
    TestEnglishFullWidthNotHere();
    if (g_failures == 0)
        std::printf("number_key_policy_test: all passed\n");
    return g_failures == 0 ? 0 : 1;
}
