// HostPassLayerPolicy 判据测试
//
// 跑法（纯 C++17、不含 Win32 头）：
//   g++ -std=c++17 -I../include -o host_pass_test host_pass_layer_policy_test.cpp && ./host_pass_test

#include "HostPassLayerPolicy.h"

#include <algorithm>
#include <cstdio>
#include <fstream>
#include <sstream>
#include <string>

namespace
{
using namespace wind::hostpass;

int g_failures = 0;

void Expect(bool cond, const char* what)
{
    if (!cond)
    {
        std::printf("FAIL: %s\n", what);
        ++g_failures;
    }
}

void TestOffset()
{
    const uint32_t buf = 4096;
    Expect(PassLayerOffset(kFlagPassLayer, 100, 16, buf) == 100, "带层、偏移在帧头后且放得下 ⇒ 用它");
    Expect(PassLayerOffset(0, 100, 16, buf) == 0, "没置位 ⇒ 没有");
    Expect(PassLayerOffset(kFlagPassLayer, 0, 16, buf) == 0, "偏移 0 ⇒ 没有");
    Expect(PassLayerOffset(kFlagPassLayer, 60, 16, buf) == 0, "偏移落在帧头里 ⇒ 不信");
    Expect(PassLayerOffset(kFlagPassLayer, 64, 16, buf) == 64, "紧贴帧头 ⇒ 可以");
    Expect(PassLayerOffset(kFlagPassLayer, 102, 16, buf) == 0, "没对齐 ⇒ 不信");
    Expect(PassLayerOffset(kFlagPassLayer, 4080, 16, buf) == 4080, "恰好到段尾 ⇒ 可以");
    Expect(PassLayerOffset(kFlagPassLayer, 4084, 16, buf) == 0, "越过段尾 ⇒ 不信");
    Expect(PassLayerOffset(kFlagPassLayer, 0xFFFFFFF0u, 0x20, buf) == 0, "相加溢出 ⇒ 不信");
}

void TestBelow()
{
    Expect(PassLayerBelow(kFlagPassLayer | kFlagPassBelow), "BELOW 位 ⇒ 在下");
    Expect(!PassLayerBelow(kFlagPassLayer), "没 BELOW 位 ⇒ 在上");
}

void TestComposite()
{
    // 每像素 BGRA（预乘）：主层 = 半透明白（128），穿透层 = 不透明蓝 / 全透明 / 半透明红。
    uint8_t main[12] = {128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128};
    const uint8_t pass[12] = {255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 128, 128};
    uint8_t over[12];
    std::copy(main, main + 12, over);
    CompositePass(over, pass, sizeof over, false);
    Expect(over[0] == 255 && over[1] == 0 && over[3] == 255, "在上：不透明穿透像素盖住主层");
    Expect(over[4] == 128 && over[7] == 128, "在上：透明穿透像素不动主层");
    Expect(over[8] == 64 && over[10] == 192 && over[11] == 192, "在上：半透明按 over 混合");
    uint8_t under[12];
    std::copy(main, main + 12, under);
    CompositePass(under, pass, sizeof under, true);
    Expect(under[0] == 128 + 127 && under[1] == 128 && under[3] == 255, "在下：主层压在穿透像素上");
    Expect(under[4] == 128 && under[7] == 128, "在下：透明穿透像素不动主层");
    Expect(under[8] == 128 && under[10] == 192 && under[11] == 192, "在下：半透明按 under 混合");
}

// 常量与 BinaryProtocol.h、服务端 protocol.rs 同值（改了一边这里当场红）。
void TestSyncedWithProtocol()
{
    // 按本文件的路径找（CI 在仓库根以相对路径编、在同一目录跑）。
    const std::string self = __FILE__;
    const std::string dir = self.substr(0, self.find_last_of("/\\") + 1);
    auto read = [&](const std::string& rel) {
        std::ifstream f(dir + rel);
        std::stringstream s;
        s << f.rdbuf();
        return s.str();
    };
    const std::string cpp = read("../include/BinaryProtocol.h");
    const std::string rs = read("../../wind_input/crates/wind-ipc/src/protocol.rs");
    Expect(!cpp.empty() && !rs.empty(), "读得到两份协议文件");
    Expect(cpp.find("SHARED_FLAG_PASS_LAYER = 0x0020;") != std::string::npos, "C++ PASS_LAYER 同值");
    Expect(cpp.find("SHARED_FLAG_PASS_BELOW = 0x0040;") != std::string::npos, "C++ PASS_BELOW 同值");
    Expect(rs.find("FLAG_PASS_LAYER: u32 = 0x0020;") != std::string::npos, "Rust PASS_LAYER 同值");
    Expect(rs.find("FLAG_PASS_BELOW: u32 = 0x0040;") != std::string::npos, "Rust PASS_BELOW 同值");
    Expect(cpp.find("HOST_RENDER_CAP_PASS_LAYER = 0x0001;") != std::string::npos, "C++ 能力位同值");
    Expect(rs.find("HOST_RENDER_CAP_PASS_LAYER: u32 = 0x0001;") != std::string::npos, "Rust 能力位同值");
    Expect(rs.find("pub pass_offset: u32,") != std::string::npos, "Rust 帧头 @56 是 pass_offset");
}

} // namespace

int main()
{
    TestOffset();
    TestBelow();
    TestComposite();
    TestSyncedWithProtocol();
    if (g_failures == 0)
    {
        std::printf("host_pass_layer_policy: all passed\n");
    }
    return g_failures == 0 ? 0 : 1;
}
