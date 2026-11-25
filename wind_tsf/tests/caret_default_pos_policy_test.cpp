// CaretDefaultPosPolicy 判据测试
//
// 跑法（纯 C++17、不含 Win32 头，本机不需要 MSVC）：
//   g++ -std=c++17 -I../include -o cdp_test caret_default_pos_policy_test.cpp && ./cdp_test
// 或经 CMake：
//   cmake -S wind_tsf -B build/tsf-tests -DWIND_TSF_TESTS=ON
//   cmake --build build/tsf-tests && ctest --test-dir build/tsf-tests
//
// 每个判据的任一前提被去掉（含 LatchApplies 只看 latched、SameRect 少比一个成员），
// 都有对应用例变红；改测试时保住这个性质。
//
// 覆盖边界：这里只测判据。矩形怎么取、闩何时置、`_pComposition` 在哪些出口变空都在
// TextService.cpp 里，判据全绿不等于候选窗定对了。

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

// Illustrator 画布文字的取值：三次布局查询同一个退化矩形，即工作区（2560×1368）右下角像素。
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
    // 指纹改动后若不再命中，Illustrator 画布的候选窗会退回屏幕左上角。
    CHECK(IsHostDefaultPosition(kIllustrator, true, kIllustrator, true, kIllustrator));

    CASE("IsHostDefaultPosition：CUAS 上的 Java 宿主同形，必须命中");
    // Java 客户端（1920×1040 工作区）。二级降级靠本判据让路给 IMM32 那一级；不命中的话
    // 窗口最大化后这个角点会被当作组合矩形采信，候选窗钉在屏幕右缘。
    constexpr Rect kJavaCuas{1919, 1039, 1920, 1039};
    CHECK(IsHostDefaultPosition(kJavaCuas, true, kJavaCuas, true, kJavaCuas));

    CASE("IsHostDefaultPosition：有高度就不是「没有插入点」");
    // 正常宿主：矩形有高度，无论三者同不同都不该走这条路。
    constexpr Rect tall{473, 189, 478, 217};
    CHECK(!IsHostDefaultPosition(tall, true, tall, true, tall));

    CASE("IsHostDefaultPosition：三者不全同 ⇒ 宿主是在排版，不是在答「没有」");
    // shell 临时输入小窗：selection 恒退化在角点，组合起点却有效。手上有真起点就该走
    // 一级降级（见 CaretEditSession.cpp），不能当成「没有插入点」。
    CHECK(!IsHostDefaultPosition(kIllustrator, true, Rect{473, 189, 473, 217}, true, kIllustrator));
    // 洛克王国：组合矩形宽度随编码串增长，宿主算完了布局只是不填高度，归二级降级管。
    CHECK(!IsHostDefaultPosition(Rect{1353, 1647, 1353, 1647}, true, Rect{1353, 1647, 1353, 1647},
                                 true, Rect{1353, 1647, 1399, 1647}));

    CASE("IsHostDefaultPosition：缺任何一维都不算（失败关闭）");
    // 缺了组合起点或组合矩形就无从判断「三者全同」，此时按既有行为丢弃。
    CHECK(!IsHostDefaultPosition(kIllustrator, false, kIllustrator, true, kIllustrator));
    CHECK(!IsHostDefaultPosition(kIllustrator, true, kIllustrator, false, kIllustrator));
    CHECK(!IsHostDefaultPosition(kIllustrator, false, kIllustrator, false, kIllustrator));
}

void TestLaidOutComposition()
{
    CASE("ShowsLaidOutComposition：Edge 网页输入框排完版必须成立");
    // 选区落在组合末尾，组合矩形从起点到末尾、与选区同高。不成立的话，Edge 被误判成
    // IMM32 宿主后解除不了，组合起点一直不报，候选窗跟着插入点逐字抖动。
    CHECK(ShowsLaidOutComposition(true, Rect{1464, 695, 1464, 725}, true, Rect{1440, 695, 1464, 725}));
    // 记事本同形
    CHECK(ShowsLaidOutComposition(true, Rect{495, 484, 495, 539}, true, Rect{461, 484, 495, 539}));

    CASE("ShowsLaidOutComposition：CUAS 上的 Java 补发前后都不成立");
    // 补发前：工作区右下角像素；补发后：CUAS 改答 CANDIDATEFORM 点。三次查询都是同一个 h=0 的点。
    constexpr Rect kJavaCuas{1919, 1039, 1920, 1039};
    CHECK(!ShowsLaidOutComposition(true, kJavaCuas, true, kJavaCuas));
    constexpr Rect kJavaAfterNudge{314, 60, 314, 60};
    CHECK(!ShowsLaidOutComposition(true, kJavaAfterNudge, true, kJavaAfterNudge));
    CHECK(!ShowsLaidOutComposition(true, kIllustrator, true, kIllustrator));

    CASE("ShowsLaidOutComposition：只答一个固定矩形的宿主不成立");
    // 有高度但所有 range 都给同一个矩形（如按 CFS_EXCLUDE 区域作答），不是在排版。
    constexpr Rect fixed{300, 40, 320, 70};
    CHECK(!ShowsLaidOutComposition(true, fixed, true, fixed));

    CASE("ShowsLaidOutComposition：选区不在组合矩形上不成立");
    // 两个互不相干的矩形（如分别答输入框区域与 CFS_EXCLUDE 区域）不是排版证据。
    constexpr Rect area{1440, 695, 1464, 725};
    CHECK(!ShowsLaidOutComposition(true, Rect{1400, 695, 1400, 725}, true, area)); // 在左边外
    CHECK(!ShowsLaidOutComposition(true, Rect{1500, 695, 1500, 725}, true, area)); // 在右边外
    CHECK(!ShowsLaidOutComposition(true, Rect{1450, 600, 1450, 630}, true, area)); // 在上方
    CHECK(!ShowsLaidOutComposition(true, Rect{1450, 760, 1450, 790}, true, area)); // 在下方
    // 换行的组合：选区在第二行，组合矩形包住两行，仍成立
    CHECK(ShowsLaidOutComposition(true, Rect{1060, 725, 1060, 755}, true, Rect{1040, 695, 1800, 755}));
    // shell 临时输入小窗：选区退化在屏幕角落，组合起点有效——不成立
    CHECK(!ShowsLaidOutComposition(true, kIllustrator, true, Rect{473, 189, 485, 217}));

    CASE("ShowsLaidOutComposition：任一维缺失或退化都不成立");
    constexpr Rect caret{1464, 695, 1464, 725};
    constexpr Rect comp{1440, 695, 1464, 725};
    CHECK(!ShowsLaidOutComposition(false, caret, true, comp));                          // 选区没取到
    // 以下三条其余前提全满足（选区落在组合矩形内），只缺被测那一维
    CHECK(!ShowsLaidOutComposition(true, Rect{1464, 710, 1464, 710}, true, comp));      // 选区无高度
    CHECK(!ShowsLaidOutComposition(true, caret, false, comp));                          // 组合矩形没取到
    CHECK(!ShowsLaidOutComposition(true, Rect{1464, 700, 1464, 720}, true, Rect{1464, 695, 1464, 725})); // 组合矩形零宽
    CHECK(!ShowsLaidOutComposition(true, Rect{1399, 1620, 1399, 1650}, true, Rect{1353, 1647, 1399, 1647})); // 洛克王国：不填高度
}

void TestCompositionRectFallback()
{
    CASE("AllowCompositionRectFallback：洛克王国形态（算完布局、不填高度）必须放行");
    // 这是二级降级存在的理由；它若不再放行，候选窗退回兜底坐标 (640,332)。
    CHECK(AllowCompositionRectFallback(false, true, true, 46, true));

    CASE("AllowCompositionRectFallback：宿主默认位置指纹命中 ⇒ 不走，哪怕角点在显示区内");
    // Java 客户端最大化后角点落在显示区内、w=1，其它条件全满足，只有指纹这一维挡得住。
    CHECK(!AllowCompositionRectFallback(true, true, true, 1, true));

    CASE("AllowCompositionRectFallback：其余任一前提不满足 ⇒ 不走");
    CHECK(!AllowCompositionRectFallback(false, false, true, 46, true)); // 无显示区：失败关闭
    CHECK(!AllowCompositionRectFallback(false, true, false, 46, true)); // 没取到组合矩形
    CHECK(!AllowCompositionRectFallback(false, true, true, 0, true));   // 零宽
    CHECK(!AllowCompositionRectFallback(false, true, true, 1, false));  // 流放之路：落在窗外
}

void TestLatchScope()
{
    CASE("LatchApplies：判决只在同一次组合内作数");
    CHECK(LatchApplies(true, true));
    // 组合没了判决就不作数：上屏走 CommitText 不经 EndComposition，闩会活过那次组合；
    // 若仍认，Illustrator 面板输入框的首帧退化会被报到屏幕角落，而应回退 GUI_CARET。
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
    TestLaidOutComposition();
    TestCompositionRectFallback();
    TestLatchScope();

    if (g_failures == 0)
    {
        std::printf("OK\n");
        return 0;
    }
    std::printf("%d FAILURE(S)\n", g_failures);
    return 1;
}
