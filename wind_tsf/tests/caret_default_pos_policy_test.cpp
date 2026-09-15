// CaretDefaultPosPolicy 判据测试
//
// 跑法（纯 C++17、不含 Win32 头，本机不需要 MSVC）：
//   g++ -std=c++17 -I../include -o cdp_test caret_default_pos_policy_test.cpp && ./cdp_test
// 或经 CMake：
//   cmake -S wind_tsf -B build/tsf-tests -DWIND_TSF_TESTS=ON
//   cmake --build build/tsf-tests && ctest --test-dir build/tsf-tests
//
// 变异检验已做（2026-09-15），逐条命中：
//   - IsHostDefaultPosition 去掉 `SameRect(caret, compRect)`            → TestFingerprint 红
//   - IsHostDefaultPosition 去掉 `SameRect(caret, compStart)`           → TestFingerprint 红
//   - IsHostDefaultPosition 去掉 `caret.bottom <= caret.top`            → TestFingerprint 红
//   - IsHostDefaultPosition 把 hasCompStart/hasCompRect 缺席当放行      → TestFingerprint 红
//   - LatchApplies 改成只看 latched（= 忘了加组合作用域那一版）         → TestLatchScope 红
//   - SameRect 任一成员比较去掉                                          → TestSameRect 红
// 改测试时请保住这个性质——一条永远绿的测试不会告诉你任何事。
//
// ⚠️ 覆盖边界：这里测的只是**判据**。矩形怎么取、闩什么时候置、`_pComposition` 在哪几个
// 出口变空，全在 TextService.cpp 里，这里一行也覆盖不到。判据全绿 ≠ 候选窗定对了。

#include "CaretDefaultPosPolicy.h"

#include <cstdio>

namespace
{

using namespace wind::caret;

int g_failures = 0;
const char* g_case = "";

#define CHECK(expr)                                                                      \
    do                                                                                   \
    {                                                                                    \
        if (!(expr))                                                                     \
        {                                                                                \
            std::printf("  FAIL  %s:%d  [%s]  %s\n", __FILE__, __LINE__, g_case, #expr); \
            g_failures++;                                                                \
        }                                                                                \
    } while (0)

#define CASE(name)                   \
    do                               \
    {                                \
        g_case = name;               \
        std::printf("  %s\n", name); \
    } while (0)

// Win32 RECT 的最小替身：本头文件按成员名取值（模板），不依赖 windows.h。
struct Rect
{
    long left;
    long top;
    long right;
    long bottom;
};

// Illustrator 30.8 画布文字实测值：三次布局查询同一个退化矩形，
// 正是工作区（2560×1368）右下角最后一个像素。
constexpr Rect kIllustrator{2559, 1367, 2560, 1367};

void TestSameRect()
{
    CASE("SameRect：四个角全同才算同，缺一不可");
    CHECK(SameRect(kIllustrator, kIllustrator));
    CHECK(!SameRect(kIllustrator, Rect{2558, 1367, 2560, 1367}));
    CHECK(!SameRect(kIllustrator, Rect{2559, 1366, 2560, 1367}));
    CHECK(!SameRect(kIllustrator, Rect{2559, 1367, 2561, 1367}));
    CHECK(!SameRect(kIllustrator, Rect{2559, 1367, 2560, 1368}));
    // 两个都是空矩形但坐标不同 ⇒ 不同。Win32 EqualRect 对空矩形另有语义，
    // 这正是本仓手写逐成员比较的理由（见 SameRect 的注释）。
    CHECK(!SameRect(Rect{0, 0, 0, 0}, kIllustrator));
}

void TestFingerprint()
{
    CASE("IsHostDefaultPosition：Illustrator 实测形态必须命中");
    // ★ 这组值就是 D-3 的实测数据。指纹改动后它若不再命中，Illustrator 画布会退回
    //   「候选窗钉在屏幕左上角」——本条是整个修复的存活判据。
    CHECK(IsHostDefaultPosition(kIllustrator, true, kIllustrator, true, kIllustrator));

    CASE("IsHostDefaultPosition：有高度就不是「没有插入点」");
    // 正常宿主：矩形有高度，无论三者同不同都不该走这条路。
    constexpr Rect tall{473, 189, 478, 217};
    CHECK(!IsHostDefaultPosition(tall, true, tall, true, tall));

    CASE("IsHostDefaultPosition：三者不全同 ⇒ 宿主是在排版，不是在答「没有」");
    // shell 临时输入小窗实测（CaretEditSession.cpp 的一级降级注释）：selection 恒退化成
    // (2559,1367,2560,1367)，而组合起点给出有效的 (473,189,473,217)。那时手上有真起点，
    // 该走一级降级，绝不能当成「没有插入点」。
    CHECK(!IsHostDefaultPosition(kIllustrator, true, Rect{473, 189, 473, 217}, true, kIllustrator));
    // 洛克王国实测：组合整体矩形宽度随编码串增长（16→19→25→28→46），说明宿主算完了布局
    // 只是不填高度。那是二级降级的地盘，不是本判据的。
    CHECK(!IsHostDefaultPosition(Rect{1353, 1647, 1353, 1647}, true, Rect{1353, 1647, 1353, 1647},
                                 true, Rect{1353, 1647, 1399, 1647}));

    CASE("IsHostDefaultPosition：缺任何一维都不算（失败关闭）");
    // 缺了组合起点或组合矩形就无从判断「三者全同」，此时按既有行为丢弃。
    CHECK(!IsHostDefaultPosition(kIllustrator, false, kIllustrator, true, kIllustrator));
    CHECK(!IsHostDefaultPosition(kIllustrator, true, kIllustrator, false, kIllustrator));
    CHECK(!IsHostDefaultPosition(kIllustrator, false, kIllustrator, false, kIllustrator));
}

void TestLatchScope()
{
    CASE("LatchApplies：判决只在同一次组合内作数");
    CHECK(LatchApplies(true, true));
    // ★ 组合没了，判决就不作数——上屏走 CommitText 不经 EndComposition，闩会活过那次组合。
    //   活过之后若还认，Illustrator 面板输入框的首帧退化会被当成「这里没有插入点」报到
    //   屏幕角落，而那里正确的行为是回退 GUI_CARET。
    CHECK(!LatchApplies(true, false));
    CHECK(!LatchApplies(false, true));
    CHECK(!LatchApplies(false, false));
}

} // namespace

int main()
{
    std::printf("CaretDefaultPosPolicy tests\n");
    TestSameRect();
    TestFingerprint();
    TestLatchScope();

    if (g_failures == 0)
    {
        std::printf("OK\n");
        return 0;
    }
    std::printf("%d FAILURE(S)\n", g_failures);
    return 1;
}
