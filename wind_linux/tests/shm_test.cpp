// SHM 帧读端与候选窗落位几何单测。SHM 用本进程建一段假装是服务端写的（布局按 Rust
// `shared_render_frame.rs::encode_frame_into`：64 字节头 + 紧跟像素）。

#include "ExtProtocol.h"
#include "Protocol.h"
#include "ShmFrame.h"
#include "TestHarness.h"

#include <cstring>
#include <utility>
#include <fcntl.h>
#include <sys/mman.h>
#include <unistd.h>

using namespace windlinux;

namespace {

struct FakeServiceShm {
    std::string name;
    int fd = -1;
    uint8_t* p = nullptr;
    size_t size;

    FakeServiceShm(size_t sz) : size(sz)
    {
        name = "/wl_test_" + std::to_string(getpid());
        shm_unlink(name.c_str());
        fd = shm_open(name.c_str(), O_CREAT | O_RDWR | O_EXCL, 0600);
        if (ftruncate(fd, off_t(size)) != 0) {
            std::printf("ftruncate 失败\n");
        }
        p = static_cast<uint8_t*>(mmap(nullptr, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0));
    }
    ~FakeServiceShm()
    {
        munmap(p, size);
        close(fd);
        shm_unlink(name.c_str());
    }

    void write(uint32_t seq, int32_t x, int32_t y, uint32_t w, uint32_t h, uint8_t fill,
               uint32_t dataSizeOverride = 0)
    {
        SharedRenderHeader hdr{};
        hdr.magic = SHARED_RENDER_MAGIC;
        hdr.version = SHARED_RENDER_VERSION;
        hdr.sequence = seq;
        hdr.flags = SHARED_FLAG_VISIBLE | SHARED_FLAG_CONTENT_READY;
        hdr.x = x;
        hdr.y = y;
        hdr.width = w;
        hdr.height = h;
        hdr.stride = w * 4;
        hdr.dataSize = dataSizeOverride ? dataSizeOverride : w * h * 4;
        size_t pix = size_t(w) * h * 4;
        std::memset(p + 64, fill, pix + 64 <= size ? pix : size - 64); // 越界帧只写头，像素写到映射尾为止
        std::memcpy(p, &hdr, sizeof(hdr));
    }
};

void TestReadFrame()
{
    CASE("SHM：读出头部字段与像素拷贝");
    FakeServiceShm svc(1 << 20);
    ShmFrameReader r;
    CHECK(r.open(svc.name, svc.size));
    svc.write(5, -3, 40, 10, 4, 0x7F);
    SharedFrame f;
    CHECK(r.snapshot(f));
    CHECK_EQ(f.sequence, 5u);
    CHECK_EQ(f.screenX, -3);
    CHECK_EQ(f.screenY, 40);
    CHECK_EQ(f.width, 10u);
    CHECK_EQ(f.bgra.size(), size_t(10 * 4 * 4));
    CHECK_EQ(f.bgra[0], uint8_t(0x7F));
    CHECK((f.flags & SHARED_FLAG_VISIBLE) != 0);

    CASE("拷贝与 SHM 脱钩：服务端写下一帧不改已取出的像素");
    svc.write(6, 0, 0, 10, 4, 0x11);
    CHECK_EQ(f.bgra[0], uint8_t(0x7F));
    CHECK(r.snapshot(f));
    CHECK_EQ(f.bgra[0], uint8_t(0x11));
}

void TestRejectBad()
{
    CASE("magic 不对 / 尺寸越界 / 头部自相矛盾 → 拒收，不越界读");
    FakeServiceShm svc(4096);
    ShmFrameReader r;
    CHECK(r.open(svc.name, svc.size));
    SharedFrame f;
    CHECK(!r.snapshot(f)); // 全 0：magic 不对
    svc.write(1, 0, 0, 100, 100, 0); // 40000 字节 > 4096 映射
    CHECK(!r.snapshot(f));
    svc.write(1, 0, 0, 4, 4, 0, /*dataSize=*/16); // dataSize < stride*height
    CHECK(!r.snapshot(f));

    CASE("宽高超过 16384：拒收（X11 尺寸是 16 位，截断后的请求会让 xcb 关连接）");
    {
        FakeServiceShm big(64 + 16385 * 4 + 64);
        ShmFrameReader rb;
        CHECK(rb.open(big.name, big.size));
        big.write(1, 0, 0, 16385, 1, 0x10); // 像素放得下，单看尺寸也要拒
        CHECK(!rb.snapshot(f));
        big.write(2, 0, 0, 16384, 1, 0x10);
        CHECK(rb.snapshot(f));
    }

    CASE("隐藏帧（dataSize=0）：成功、像素为空");
    SharedRenderHeader hdr{};
    hdr.magic = SHARED_RENDER_MAGIC;
    hdr.sequence = 9;
    std::memcpy(svc.p, &hdr, sizeof(hdr));
    CHECK(r.snapshot(f));
    CHECK(f.bgra.empty());
    CHECK_EQ(f.sequence, 9u);
}

void TestOpenMissing()
{
    CASE("服务还没建段：open 失败而不是崩");
    ShmFrameReader r;
    CHECK(!r.open("/wl_definitely_missing_" + std::to_string(getpid())));
    CHECK(!r.isOpen());
    SharedFrame f;
    CHECK(!r.snapshot(f));
}

void TestOpenGuards()
{
    CASE("段还没定长（O_CREAT 与 ftruncate 之间，大小 0）：拒绝映射——按 4MB 映射一读就 SIGBUS");
    {
        std::string name = "/wl_zero_" + std::to_string(getpid());
        shm_unlink(name.c_str());
        int fd = shm_open(name.c_str(), O_CREAT | O_RDWR | O_EXCL, 0600);
        CHECK(fd >= 0);
        ShmFrameReader r;
        CHECK(!r.open(name));
        CHECK(!r.isOpen());
        CHECK(r.lastError().find("字节") != std::string::npos);
        // 定长之后下一帧就打得开。
        CHECK(ftruncate(fd, 4096) == 0);
        CHECK(r.open(name));
        close(fd);
        shm_unlink(name.c_str());
    }

    CASE("段比 4MB 小：只映射实际大小，帧头声称的像素越过段尾时拒收（不 SIGBUS）");
    {
        FakeServiceShm svc(4096);
        ShmFrameReader r;
        CHECK(r.open(svc.name)); // 缺省按 4MB 上限，实际只映射 4096
        svc.write(1, 0, 0, 100, 100, 0); // 40000 字节像素，段只有 4096
        SharedFrame f;
        CHECK(!r.snapshot(f));
        svc.write(2, 0, 0, 4, 4, 0x33); // 放得下的帧照常读
        CHECK(r.snapshot(f));
        CHECK_EQ(f.bgra.size(), size_t(64));
    }

    CASE("段的属主不是本用户（别人抢先建的同名段）：拒绝映射");
    {
        FakeServiceShm svc(4096);
        ShmFrameReader r;
        CHECK(!r.open(svc.name, ShmFrameReader::kDefaultSize, ShmFrameReader::currentUid() + 1));
        CHECK(!r.isOpen());
        CHECK(r.lastError().find("属主") != std::string::npos);
        CHECK(r.open(svc.name, ShmFrameReader::kDefaultSize, ShmFrameReader::currentUid()));
    }
}

void TestPlacement()
{
    const Rect wa{0, 0, 1920, 1080};
    CASE("落位：放得下就照服务端给的点");
    Rect r = placePanel(100, 200, 300, 150, wa, false);
    CHECK(r.x == 100 && r.y == 200);

    CASE("右溢回拉、左溢推回");
    r = placePanel(1800, 200, 300, 150, wa, false);
    CHECK_EQ(r.x, 1620);
    r = placePanel(-50, 200, 300, 150, wa, false);
    CHECK_EQ(r.x, 0);

    CASE("下方放不下 → 翻到光标上方（让出 18px 光标高）");
    r = placePanel(100, 1000, 300, 150, wa, false);
    CHECK_EQ(r.y, 1000 - 18 - 150);

    CASE("固定位置（absolute）只钳制不翻转");
    r = placePanel(100, 1000, 300, 150, wa, true);
    CHECK_EQ(r.y, 1080 - 150);

    CASE("翻转后仍越界 → 夹进工作区");
    r = placePanel(100, 100, 300, 1000, Rect{0, 0, 1920, 1080}, false);
    CHECK(r.y >= 0 && r.y + 1000 <= 1080);

    CASE("候选窗拖动锚：锚 > 固定位置 > 跟随光标；窗口 = 锚 − 内容偏移，只钳制不翻转");
    const std::optional<std::pair<int32_t, int32_t>> none;
    r = placeCandidate(100, 200, 300, 150, wa, false, none, 8, 12);
    CHECK(r.x == 100 && r.y == 200); // 没锚：同 placePanel，不套内容偏移
    r = placeCandidate(100, 1000, 300, 150, wa, false, none, 8, 12);
    CHECK_EQ(r.y, 1000 - 18 - 150); // 没锚的跟随照旧翻转
    const auto pin = std::make_optional(std::make_pair(int32_t(500), int32_t(400)));
    r = placeCandidate(100, 200, 300, 150, wa, false, pin, 8, 12);
    CHECK(r.x == 492 && r.y == 388); // 锚压过跟随光标
    r = placeCandidate(640, 480, 300, 150, wa, true, pin, 8, 12);
    CHECK(r.x == 492 && r.y == 388); // 锚压过固定位置
    r = placeCandidate(100, 200, 300, 150, wa, false,
                       std::make_optional(std::make_pair(int32_t(500), int32_t(1000))), 0, 0);
    CHECK_EQ(r.y, 1080 - 150); // 锚贴底：钳制而不是翻到光标上方
}

OverlayFramePayload overlay(uint32_t place, int32_t x, int32_t y, int32_t altX, int32_t altY)
{
    OverlayFramePayload p;
    p.flags = 1;
    p.place = place;
    p.x = x;
    p.y = y;
    p.altX = altX;
    p.altY = altY;
    // 位图 120x40，内容盒 100x30 偏移 (8, 4)：软阴影扩边。
    p.width = 120;
    p.height = 40;
    p.contentX = 8;
    p.contentY = 4;
    p.contentW = 100;
    p.contentH = 30;
    return p;
}

void TestOverlayPlacement()
{
    const Rect wa{0, 0, 1920, 1080};
    CASE("浮层 ABSOLUTE：内容盒照搬，窗口 = 内容 − 阴影扩边");
    Rect r = placeOverlay(overlay(OVERLAY_PLACE_ABSOLUTE, 500, 600, 0, 0), wa, 0, 0);
    CHECK(r.x == 492 && r.y == 596 && r.w == 120 && r.h == 40);

    CASE("浮层 ABSOLUTE 越界只夹回（左侧副屏之类的负坐标由工作区决定）");
    r = placeOverlay(overlay(OVERLAY_PLACE_ABSOLUTE, 1900, -20, 0, 0), wa, 0, 0);
    CHECK(r.x == 1920 - 100 - 8 && r.y == -4);

    CASE("浮层 FLIP：放得下用首选点；下溢用备选点；备选越过上沿贴下沿");
    r = placeOverlay(overlay(OVERLAY_PLACE_FLIP, 100, 200, 100, 150), wa, 0, 0);
    CHECK(r.x == 92 && r.y == 196);
    r = placeOverlay(overlay(OVERLAY_PLACE_FLIP, 100, 1070, 100, 1000), wa, 0, 0);
    CHECK_EQ(r.y, 1000 - 4);
    r = placeOverlay(overlay(OVERLAY_PLACE_FLIP, 100, 1070, 100, -10), wa, 0, 0);
    CHECK_EQ(r.y, 1080 - 30 - 4);

    CASE("浮层 FLIP：右溢用备选 x（竖排 tooltip 改到候选窗左侧）");
    r = placeOverlay(overlay(OVERLAY_PLACE_FLIP, 1850, 200, 1600, 200), wa, 0, 0);
    CHECK_EQ(r.x, 1600 - 8);

    CASE("浮层 FOLLOW_CANDIDATE：随候选窗的实际位移平移后再判溢出");
    // 候选窗被翻到光标上方（dy = -300）：tooltip 首选点跟着上移，照样放得下。
    r = placeOverlay(overlay(OVERLAY_PLACE_FOLLOW_CANDIDATE, 100, 1060, 100, 990), wa, 20, -300);
    CHECK(r.x == 120 - 8 && r.y == 760 - 4);
    // FLIP 不吃平移。
    r = placeOverlay(overlay(OVERLAY_PLACE_FLIP, 100, 200, 100, 150), wa, 20, -300);
    CHECK(r.x == 92 && r.y == 196);

    CASE("浮层 ANCHOR：七个锚点 + 留白");
    auto anchored = [&](uint32_t a) {
        OverlayFramePayload p = overlay(OVERLAY_PLACE_ANCHOR, 0, 0, 0, 0);
        p.anchor = a;
        p.margin = 12;
        Rect w = placeOverlay(p, wa, 0, 0);
        return std::make_pair(w.x + 8, w.y + 4); // 换回内容左上
    };
    CHECK(anchored(OVERLAY_ANCHOR_CENTER) == std::make_pair(960 - 50, 540 - 15));
    CHECK(anchored(OVERLAY_ANCHOR_TOP_LEFT) == std::make_pair(12, 12));
    CHECK(anchored(OVERLAY_ANCHOR_TOP_RIGHT) == std::make_pair(1920 - 12 - 100, 12));
    CHECK(anchored(OVERLAY_ANCHOR_BOTTOM_LEFT) == std::make_pair(12, 1080 - 12 - 30));
    CHECK(anchored(OVERLAY_ANCHOR_BOTTOM_RIGHT) == std::make_pair(1920 - 112, 1080 - 42));
    CHECK(anchored(OVERLAY_ANCHOR_TOP_CENTER) == std::make_pair(910, 12));
    CHECK(anchored(OVERLAY_ANCHOR_BOTTOM_CENTER) == std::make_pair(910, 1038));
}

void TestHitTest()
{
    CASE("命中测试：候选下标 / 翻页 -1 -2 / 未命中");
    std::vector<CandidateHitRect> rects = {{0, 10, 5, 40, 20}, {1, 60, 5, 40, 20}, {-2, 200, 5, 16, 20}};
    CHECK_EQ(hitTest(rects, 12, 6), 0);
    CHECK_EQ(hitTest(rects, 99, 24), 1);
    CHECK_EQ(hitTest(rects, 100, 24), kNoHit); // 右边界是开区间
    CHECK_EQ(hitTest(rects, 205, 10), -2);
    CHECK_EQ(hitTest(rects, 0, 0), kNoHit);
}

void TestSkinInputRegion()
{
    CASE("皮肤输入区：没带表 → 空 optional（缺省输入区）；带表 → 裁进位图、去掉空矩形");
    CandidateSkin skin;
    CHECK(!candidateInputRegion(skin, 100, 50).has_value()); // 普通主题
    skin.flags = SKIN_FLAG_ACTIVE;
    skin.mask = {{0, 0, 10, 10}};
    CHECK(!candidateInputRegion(skin, 100, 50).has_value()); // 皮肤但没带表（macOS 形态）
    skin.flags |= SKIN_FLAG_MASK;
    skin.mask = {{-4, 2, 10, 5}, {95, 45, 20, 20}, {200, 0, 5, 5}, {3, 3, 0, 4}};
    auto r = candidateInputRegion(skin, 100, 50);
    CHECK(r.has_value());
    CHECK_EQ(r->size(), size_t(2));
    CHECK((*r)[0].x == 0 && (*r)[0].y == 2 && (*r)[0].w == 6 && (*r)[0].h == 5);
    CHECK((*r)[1].x == 95 && (*r)[1].y == 45 && (*r)[1].w == 5 && (*r)[1].h == 5);
    skin.mask.clear();
    auto empty = candidateInputRegion(skin, 100, 50);
    CHECK(empty.has_value() && empty->empty()); // 带了表但为空：整窗都不接鼠标

    CASE("位图像素 → 逻辑坐标：向外取整，宽高至少 1");
    Rect a = toLogicalCover(Rect{3, 4, 5, 1}, 2.0);
    CHECK(a.x == 1 && a.y == 2 && a.w == 3 && a.h == 1);
    Rect b = toLogicalCover(Rect{10, 10, 30, 15}, 1.5);
    CHECK(b.x == 6 && b.y == 6 && b.w == 21 && b.h == 11);
    Rect c = toLogicalCover(Rect{7, 8, 9, 10}, 1.0);
    CHECK(c.x == 7 && c.y == 8 && c.w == 9 && c.h == 10);
    Rect d = toLogicalCover(Rect{7, 8, 9, 10}, 0.0); // 坏缩放按 1
    CHECK(d.w == 9);
}

} // namespace

void TestDecorAt()
{
    CASE("装饰点击命中：最上层的 drag / block，pass 不算，窗体内的 under 不算");
    CandidateSkin skin;
    skin.frame = {20, 20, 100, 60};
    skin.decor = {
        {7, SKIN_HIT_BLOCK, SKIN_DECOR_UNDER, {0, 0, 40, 40}}, // 底层，伸进窗体左上角
        {3, SKIN_HIT_DRAG, 0, {100, 0, 40, 40}},
        {5, SKIN_HIT_BLOCK, 0, {120, 0, 30, 30}}, // 压在 3 上面
        {9, SKIN_HIT_PASS, 0, {0, 70, 30, 30}},
    };
    CHECK_EQ(decorAt(skin, 5, 5), -1); // 皮肤没启用
    skin.flags = SKIN_FLAG_ACTIVE;
    CHECK_EQ(decorAt(skin, 5, 5), 7);    // 窗体外的 under 装饰
    CHECK_EQ(decorAt(skin, 30, 30), -1); // 同一块落在窗体内：被窗体盖住
    CHECK_EQ(decorAt(skin, 105, 5), 3);
    CHECK_EQ(decorAt(skin, 125, 5), 5); // 重叠取上层
    CHECK_EQ(decorAt(skin, 130, 35), 3); // 上层下方露出的一截
    CHECK_EQ(decorAt(skin, 150, 5), -1); // 右边界开区间
    CHECK_EQ(decorAt(skin, 5, 75), -1);  // pass 不接鼠标
    CHECK_EQ(decorAt(skin, 60, 50), -1); // 窗体内空白
}

void TestNoDragDecor()
{
    CASE("不拖动区：窗体外的 block / pass 装饰；drag 装饰与窗体内照拖");
    CandidateSkin skin;
    skin.frame = {20, 20, 100, 60};
    skin.decor = {
        {7, SKIN_HIT_BLOCK, 0, {0, 0, 40, 40}}, // 伸进窗体左上角
        {3, SKIN_HIT_DRAG, 0, {100, 0, 40, 40}},
        {9, SKIN_HIT_PASS, 0, {0, 70, 30, 30}},
    };
    CHECK(!onNoDragDecor(skin, 5, 5)); // 皮肤没启用
    skin.flags = SKIN_FLAG_ACTIVE;
    CHECK(onNoDragDecor(skin, 5, 5));
    CHECK(!onNoDragDecor(skin, 30, 30)); // block 伸进窗体的那截：按窗体拖
    CHECK(!onNoDragDecor(skin, 105, 5)); // drag 装饰
    CHECK(onNoDragDecor(skin, 5, 85));   // pass（回落成 block 时也接得到鼠标）
    CHECK(!onNoDragDecor(skin, 60, 50)); // 窗体内空白
    CHECK(!onNoDragDecor(skin, 40, 5));  // 右边界开区间
}

void TestAnimInPlace()
{
    CASE("换帧照原位贴：带 ANIM 位且 SHM 就是通知的那一帧");
    CHECK(animInPlace(FRAME_FLAG_ANIM_FRAME | 1, 7, 7));
    CHECK(!animInPlace(FRAME_FLAG_ANIM_FRAME, 7, 8)); // SHM 已是更新的一帧：按普通帧
    CHECK(!animInPlace(1, 7, 7));                     // 普通帧
}

int main()
{
    TestReadFrame();
    TestRejectBad();
    TestDecorAt();
    TestNoDragDecor();
    TestOpenMissing();
    TestOpenGuards();
    TestPlacement();
    TestOverlayPlacement();
    TestHitTest();
    TestSkinInputRegion();
    TestAnimInPlace();
    TEST_MAIN_END();
}
