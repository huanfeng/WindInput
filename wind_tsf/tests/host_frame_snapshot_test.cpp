// HostFrameSnapshot 测试：帧头只读一次、拷完复核 sequence
//
// 跑法（纯 C++17、不含 Win32 头）：
//   g++ -std=c++17 -I../include -o host_frame_snapshot_test host_frame_snapshot_test.cpp && ./host_frame_snapshot_test

#include "HostFrameSnapshot.h"

#include <cstdio>
#include <cstring>
#include <fstream>
#include <sstream>
#include <string>
#include <vector>

namespace
{
using namespace wind::hostframe;

// BinaryProtocol.h 的 SharedRenderHeader / Rect 同布局的镜像（那个头文件本机编不了）。
// 字段与偏移由 TestMirrorMatchesProtocol 对着源码核。
struct Header
{
    uint32_t magic, version, sequence, flags;
    int32_t x, y;
    uint32_t width, height, stride, dataSize, rectCount, rectsOffset;
    int32_t renderedHoverIndex;
    uint32_t targetInstanceId, passOffset, reserved;
};
static_assert(sizeof(Header) == 64, "帧头 64 字节");
static_assert(offsetof(Header, sequence) == 8, "sequence @8");
struct Rect
{
    int32_t index, x, y, w, h;
};
static_assert(sizeof(Rect) == 20, "命中矩形 20 字节");
using Snap = FrameSnapshot<Header, Rect>;
constexpr uint32_t kMaxRects = 256;
constexpr uint32_t kMagic = 0x57494E44; // 'WIND'
constexpr uint32_t kFlagVisible = 0x0001;

int g_failures = 0;

void Expect(bool cond, const char* what)
{
    if (!cond)
    {
        std::printf("FAIL: %s\n", what);
        ++g_failures;
    }
}

constexpr uint32_t kBuf = 1024;
constexpr uint32_t kData = 16; // 2×2 BGRA

// 一段 SHM：帧头 + 像素（0x11）+ 1 个命中矩形 + 穿透层（0x22）。
std::vector<uint8_t> MakeShm(uint32_t seq)
{
    std::vector<uint8_t> shm(kBuf, 0);
    Header h{};
    h.magic = kMagic;
    h.sequence = seq;
    h.flags = kFlagVisible | wind::hostpass::kFlagPassLayer;
    h.width = 2;
    h.height = 2;
    h.stride = 8;
    h.dataSize = kData;
    h.rectCount = 1;
    h.rectsOffset = sizeof(Header) + kData;
    h.passOffset = h.rectsOffset + sizeof(Rect);
    std::memcpy(shm.data(), &h, sizeof(h));
    std::memset(shm.data() + sizeof(h), 0x11, kData);
    Rect r{};
    r.index = 3;
    std::memcpy(shm.data() + h.rectsOffset, &r, sizeof(r));
    std::memset(shm.data() + h.passOffset, 0x22, kData);
    return shm;
}

void TestCopiesFrame()
{
    std::vector<uint8_t> shm = MakeShm(5);
    const Header h = CopyHeader<Header>(shm.data());
    Snap s;
    Expect(CopyPayload(shm.data(), kBuf, h, kMaxRects, s), "完整一帧 ⇒ 拷出");
    Expect(s.header.sequence == 5, "帧头随快照");
    Expect(s.pixels.size() == kData && s.pixels[0] == 0x11 && s.pixels[kData - 1] == 0x11, "像素拷出");
    Expect(s.rects.size() == 1 && s.rects[0].index == 3, "命中矩形拷出");
    Expect(s.pass.size() == kData && s.pass[0] == 0x22, "穿透层拷出");
}

void TestUsesHeaderCopy()
{
    // 校验用的帧头拷贝之后，SHM 里的 dataSize 被改大：拷贝仍按拷贝的帧头取，不越过它校验过的长度。
    std::vector<uint8_t> shm = MakeShm(5);
    const Header h = CopyHeader<Header>(shm.data());
    Header grown = h;
    grown.dataSize = kBuf; // 若改按 SHM 现读，像素会越界
    std::memcpy(shm.data(), &grown, sizeof(grown));
    Snap s;
    Expect(CopyPayload(shm.data(), kBuf, h, kMaxRects, s), "sequence 没变 ⇒ 拷出");
    Expect(s.pixels.size() == kData, "像素长度按帧头拷贝，不按 SHM 现值");
}

void TestTornFrameDropped()
{
    std::vector<uint8_t> shm = MakeShm(5);
    const Header h = CopyHeader<Header>(shm.data());
    Snap s;
    const bool ok = CopyPayload(shm.data(), kBuf, h, kMaxRects, s, [&] {
        // 拷的途中服务端写了新帧（sequence 先变）
        const uint32_t next = 6;
        std::memcpy(shm.data() + offsetof(Header, sequence), &next, sizeof(next));
    });
    Expect(!ok, "拷完 sequence 变了 ⇒ 这一帧丢掉");
}

void TestRejectsOversize()
{
    std::vector<uint8_t> shm = MakeShm(5);
    Header h = CopyHeader<Header>(shm.data());
    h.dataSize = kBuf; // 64 + 1024 > 1024
    Snap s;
    Expect(!CopyPayload(shm.data(), kBuf, h, kMaxRects, s), "像素越过缓冲区 ⇒ 拒收");
}

void TestGeometryMustAgree()
{
    // 帧头读到新旧混杂的一份：新的宽高 + 旧的（更大的）dataSize。按 dataSize 拷进按宽高建的 DIB 会写越界。
    std::vector<uint8_t> shm = MakeShm(5);
    Header h = CopyHeader<Header>(shm.data());
    Snap s;
    h.height = 1;
    Expect(!CopyPayload(shm.data(), kBuf, h, kMaxRects, s), "新高 + 旧 dataSize ⇒ 丢帧");
    h = CopyHeader<Header>(shm.data());
    h.width = 1;
    h.stride = 4;
    Expect(!CopyPayload(shm.data(), kBuf, h, kMaxRects, s), "新宽（stride 也新）+ 旧 dataSize ⇒ 丢帧");
    h = CopyHeader<Header>(shm.data());
    h.stride = 12;
    Expect(!CopyPayload(shm.data(), kBuf, h, kMaxRects, s), "stride 不是宽 × 4 ⇒ 丢帧");
    h = CopyHeader<Header>(shm.data());
    h.width = 0x40000000u; // 宽 × 高 × 4 在 32 位里会绕回
    h.height = 4;
    h.stride = 0;
    h.dataSize = 0;
    Expect(!CopyPayload(shm.data(), kBuf, h, kMaxRects, s), "宽高乘积按 64 位算，不绕回");
}

void TestReleaseBuffers()
{
    std::vector<uint8_t> shm = MakeShm(5);
    Snap s;
    CopyPayload(shm.data(), kBuf, CopyHeader<Header>(shm.data()), kMaxRects, s);
    ReleaseBuffers(s);
    Expect(s.pixels.capacity() == 0 && s.pass.capacity() == 0 && s.rects.capacity() == 0,
           "收起后拷帧缓冲放掉");
}

void TestRectsAndPassBounds()
{
    std::vector<uint8_t> shm = MakeShm(5);
    Header h = CopyHeader<Header>(shm.data());
    h.rectsOffset = kBuf - 4; // 表越界
    h.flags &= ~wind::hostpass::kFlagPassLayer;
    Snap s;
    Expect(CopyPayload(shm.data(), kBuf, h, kMaxRects, s), "像素正常 ⇒ 照拷");
    Expect(s.rects.empty(), "命中矩形表越界 ⇒ 当没有");
    Expect(s.pass.empty(), "没带穿透层 ⇒ 空");
}

void TestMirrorMatchesProtocol()
{
    const std::string self = __FILE__;
    const std::string dir = self.substr(0, self.find_last_of("/\\") + 1);
    std::ifstream f(dir + "../include/BinaryProtocol.h");
    std::stringstream ss;
    ss << f.rdbuf();
    const std::string cpp = ss.str();
    Expect(!cpp.empty(), "读得到 BinaryProtocol.h");
    // 帧头字段按声明顺序出现（镜像照这个顺序排）
    const char* fields[] = {"uint32_t magic;", "uint32_t version;", "uint32_t sequence;",
                            "uint32_t flags;", "int32_t  x;", "int32_t  y;", "uint32_t width;",
                            "uint32_t height;", "uint32_t stride;", "uint32_t dataSize;",
                            "uint32_t rectCount;", "uint32_t rectsOffset;",
                            "int32_t  renderedHoverIndex;", "uint32_t targetInstanceId;",
                            "uint32_t passOffset;", "uint32_t reserved;"};
    size_t at = cpp.find("struct SharedRenderHeader");
    Expect(at != std::string::npos, "找得到 SharedRenderHeader");
    for (const char* fd : fields)
    {
        const size_t next = at == std::string::npos ? at : cpp.find(fd, at);
        Expect(next != std::string::npos, fd);
        at = next;
    }
    Expect(cpp.find("SHARED_RENDER_MAGIC   = 0x57494E44") != std::string::npos, "magic 同值");
    Expect(cpp.find("MAX_HOST_RENDER_RECTS = 256;") != std::string::npos, "矩形上限同值");
}

} // namespace

int main()
{
    TestMirrorMatchesProtocol();
    TestCopiesFrame();
    TestUsesHeaderCopy();
    TestTornFrameDropped();
    TestRejectsOversize();
    TestGeometryMustAgree();
    TestReleaseBuffers();
    TestRectsAndPassBounds();
    if (g_failures == 0)
    {
        std::printf("host_frame_snapshot: all passed\n");
    }
    return g_failures == 0 ? 0 : 1;
}
