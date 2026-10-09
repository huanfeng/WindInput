// HostBandPolicy 测试：host 候选窗 band 复核判据 + 落位上报编码
//
// 跑法（纯 C++17、不含 Win32 头，本机不需要 MSVC）：
//   g++ -std=c++17 -I../include -o host_band_policy_test host_band_policy_test.cpp && ./host_band_policy_test
// 或经 CMake：
//   cmake -B build -DWIND_TSF_TESTS=ON && cmake --build build --target host_band_policy_test
//
// ⚠️ 覆盖边界：这里测的只是**判据与字节**。前台 band 怎么取、重建时 IPC 与建窗的
// 时序全在 TextService.cpp / HostWindow.cpp 里，只能上真机（SearchHost 开机预启动）验。

#include "HostBandPolicy.h"

#include <cstdio>
#include <cstring>

namespace
{

using namespace wind::hostband;

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

#define CASE(name) g_case = (name)

void TestNoBasisNoRebuild()
{
    CASE("前台不是本进程 / 普通 band：没有依据，不动");
    // 0 = 前台不是本进程；1 = ZBID_DESKTOP 普通窗口。哪怕旧窗全错也不重建——
    // 否则白名单里的普通宿主每段输入都会白白重建一次。
    CHECK(!NeedsRebuild(0, 1, false, false));
    CHECK(!NeedsRebuild(1, 13, false, true));
}

void TestBootPrelaunchCase()
{
    CASE("开机预启动：建在 band 1 且无 owner，打开开始菜单后前台 band 13 → 重建");
    CHECK(NeedsRebuild(13, 1, false, true));
}

void TestEachReasonAlone()
{
    CASE("三个理由各自单独成立");
    CHECK(NeedsRebuild(13, 6, true, true));  // 只有 band 不符（开始菜单 ↔ 任务栏搜索）
    CHECK(NeedsRebuild(13, 13, false, true)); // 只缺 owner
    CHECK(NeedsRebuild(13, 13, true, false)); // 只是窗口被连带销毁
}

void TestStableAfterRebuild()
{
    CASE("重建后稳定：不会每个键都重建");
    // 重建时按前台建：requested = 前台 band、owner = 前台窗口。
    CHECK(!NeedsRebuild(13, 13, true, true));
}

void TestPlacedBody()
{
    CASE("落位 body：setup 不带 prev，recheck 带上旧窗依据");
    Placement now = { 13, 13, 13, true };
    CHECK(PlacedBody(42, now, nullptr) ==
          "{\"pid\":42,\"trigger\":\"setup\",\"probed\":13,\"requested\":13,\"actual\":13,\"owner\":true}");
    Placement prev = { 0, 1, 1, false };
    CHECK(PlacedBody(42, now, &prev) ==
          "{\"pid\":42,\"trigger\":\"recheck\",\"probed\":13,\"requested\":13,\"actual\":13,\"owner\":true,"
          "\"prev_probed\":0,\"prev_requested\":1,\"prev_actual\":1,\"prev_owner\":false}");
}

void TestEncodeExt()
{
    CASE("CMD_EXT 信封布局：kindLen u32 LE + kind + bodyLen u32 LE + body");
    std::vector<uint8_t> p = EncodeExt("ab", "xyz");
    const uint8_t want[] = { 2, 0, 0, 0, 'a', 'b', 3, 0, 0, 0, 'x', 'y', 'z' };
    CHECK(p.size() == sizeof(want));
    CHECK(p.size() == sizeof(want) && std::memcmp(p.data(), want, sizeof(want)) == 0);
    CHECK(std::strcmp(kPlacedKind, "diag.host_render_placed") == 0);
}

} // namespace

int main()
{
    std::printf("HostBandPolicy tests\n");
    TestNoBasisNoRebuild();
    TestBootPrelaunchCase();
    TestEachReasonAlone();
    TestStableAfterRebuild();
    TestPlacedBody();
    TestEncodeExt();

    if (g_failures == 0)
    {
        std::printf("OK\n");
        return 0;
    }
    std::printf("%d FAILURE(S)\n", g_failures);
    return 1;
}
