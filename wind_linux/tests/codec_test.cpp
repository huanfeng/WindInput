// 帧编解码单测。字节偏移按 Rust `wind-ipc` 的解码器写死——这里断言的是「服务端按它的
// 布局能不能解出来」，不是「编码再解码回来一样」（后者两边同错也会绿）。

#include "Codec.h"
#include "ExtProtocol.h"
#include "Protocol.h"
#include "TestHarness.h"

using namespace windlinux;

namespace {

uint32_t u32At(const Bytes& b, size_t off)
{
    return uint32_t(b[off]) | (uint32_t(b[off + 1]) << 8) | (uint32_t(b[off + 2]) << 16)
        | (uint32_t(b[off + 3]) << 24);
}

uint16_t u16At(const Bytes& b, size_t off)
{
    return uint16_t(b[off] | (b[off + 1] << 8));
}

Bytes le32(uint32_t v)
{
    return {uint8_t(v), uint8_t(v >> 8), uint8_t(v >> 16), uint8_t(v >> 24)};
}

void append(Bytes& b, const Bytes& more)
{
    b.insert(b.end(), more.begin(), more.end());
}

void append(Bytes& b, const std::string& s)
{
    b.insert(b.end(), s.begin(), s.end());
}

void TestHeader()
{
    CASE("header：版本/cmd/长度小端，async 置最高位");
    Bytes h = encodeHeader(CMD_KEY_EVENT, 18);
    CHECK_EQ(h.size(), size_t(8));
    CHECK_EQ(u16At(h, 0), uint16_t(0x1001));
    CHECK_EQ(u16At(h, 2), uint16_t(0x0101));
    CHECK_EQ(u32At(h, 4), 18u);
    Bytes a = encodeHeader(CMD_IME_ACTIVATED, 8, true);
    CHECK_EQ(u16At(a, 0), uint16_t(0x9001));

    HeaderInfo info;
    CHECK(decodeHeader(a.data(), info) == HeaderError::None);
    CHECK(info.isAsync);
    CHECK_EQ(info.cmd, uint16_t(CMD_IME_ACTIVATED));

    Bytes bad = encodeHeader(CMD_ACK, 0);
    bad[1] = 0x20; // major 2
    CHECK(decodeHeader(bad.data(), info) == HeaderError::VersionMismatch);
    Bytes big = encodeHeader(CMD_ACK, MAX_PAYLOAD_SIZE + 1);
    CHECK(decodeHeader(big.data(), info) == HeaderError::PayloadTooLarge);
}

void TestKeyEvent()
{
    CASE("KeyEvent：20 字节 payload，字段偏移与 KeyPayload 一致");
    KeyEvent e;
    e.keyCode = 0x4E;
    e.modifiers = KEYMOD_SHIFT;
    e.eventType = KEY_EVENT_UP;
    e.toggles = TOGGLE_CAPSLOCK;
    e.eventSeq = 0x1234;
    e.prevChar = 0x33;
    e.seqChar = 0x4F60;
    Bytes f = encodeKeyEventFrame(e);
    CHECK_EQ(f.size(), size_t(8 + 20));
    CHECK_EQ(u32At(f, 4), 20u);
    CHECK_EQ(u32At(f, 8), 0x4Eu);
    CHECK_EQ(u32At(f, 12), 0u);
    CHECK_EQ(u32At(f, 16), uint32_t(KEYMOD_SHIFT));
    CHECK_EQ(f[20], uint8_t(KEY_EVENT_UP));
    CHECK_EQ(f[21], uint8_t(TOGGLE_CAPSLOCK));
    CHECK_EQ(u16At(f, 22), uint16_t(0x1234));
    CHECK_EQ(u16At(f, 24), uint16_t(0x33));
    CHECK_EQ(u16At(f, 26), uint16_t(0x4F60));
}

void TestFocusGained()
{
    CASE("FocusGained：定长 39 字节 + bundleId 段 + windowClass 段");
    Bytes f = encodeFocusGainedFrame(0x1122334455667788ull, INPUT_SCOPE_PASSWORD_BIT, "gedit", "wc");
    Bytes p(f.begin() + 8, f.end());
    // ← 发短了 Rust `FocusGainedPayload::from_bytes` 解码恒失败（macOS 12 字节那次事故）
    CHECK(p.size() >= 39);
    CHECK_EQ(p.size(), size_t(39 + 4 + 5 + 4 + 2));
    for (int i = 0; i < 20; ++i) {
        CHECK_EQ(p[i], uint8_t(0)); // caret 段全 0
    }
    CHECK_EQ(u32At(p, 20), 0x55667788u); // clientToken 低 32
    CHECK_EQ(u32At(p, 24), 0x11223344u); // clientToken 高 32 = 宿主标识
    CHECK_EQ(u32At(p, 28), 0x80000000u); // IS_PASSWORD
    CHECK_EQ(u32At(p, 32), 0u);
    CHECK_EQ(p[36], uint8_t(0));
    CHECK_EQ(p[37], uint8_t(0));
    CHECK_EQ(p[38], uint8_t(0));
    CHECK_EQ(u32At(p, 39), 5u); // bundleIdLen，Rust VAR_SECTION_OFFSET = 39
    CHECK(std::string(p.begin() + 43, p.begin() + 48) == "gedit");
    CHECK_EQ(u32At(p, 48), 2u);
    CHECK(std::string(p.begin() + 52, p.end()) == "wc");
}

void TestFocusLostAndToken()
{
    CASE("FocusLost 9 字节同步；IME_ACTIVATED 带 AsyncFlag（服务端从不回响应）");
    Bytes f = encodeFocusLostFrame(0xAABBCCDD00000001ull, FOCUS_LOST_REASON_THREAD);
    CHECK_EQ(u16At(f, 0), uint16_t(0x1001));
    CHECK_EQ(u32At(f, 4), 9u);
    CHECK_EQ(u32At(f, 8), 1u);
    CHECK_EQ(u32At(f, 12), 0xAABBCCDDu);
    CHECK_EQ(f[16], uint8_t(0));

    Bytes a = encodeClientTokenFrame(CMD_IME_ACTIVATED, 7);
    // ← 不带 async 的话调用方会去读一个永远不来的响应，fcitx5 主线程卡 2 秒
    CHECK(u16At(a, 0) & ASYNC_FLAG);
    CHECK_EQ(u32At(a, 4), 8u);
}

void TestSmallUpstream()
{
    CASE("CaretUpdate 12 字节；Select/Hover/Scroll/MenuAction 为 i32");
    Bytes c = encodeCaretUpdateFrame(-5, 300, 18);
    CHECK_EQ(u32At(c, 4), 12u);
    CHECK_EQ(int32_t(u32At(c, 8)), -5);
    CHECK_EQ(u32At(c, 12), 300u);
    CHECK_EQ(u32At(c, 16), 18u);

    CHECK_EQ(u16At(encodeCandidateSelectFrame(2), 2), uint16_t(CMD_CANDIDATE_SELECT));
    // 「无悬停」上线时折成 INT32_MIN：-1/-2 在服务端非 Windows 分支是翻页按钮。
    CHECK_EQ(int32_t(u32At(encodeCandidateHoverFrame(-1), 8)), INT32_MIN);
    CHECK_EQ(int32_t(u32At(encodeCandidateHoverFrame(3), 8)), 3);
    CHECK_EQ(int32_t(u32At(encodeCandidateScrollFrame(-120), 8)), -120);
    CHECK_EQ(u16At(encodeMenuActionFrame(9), 2), uint16_t(CMD_MENU_ACTION));

    Bytes e = encodeExtFrame("pos.candidate", "{}");
    CHECK_EQ(u32At(e, 8), 13u);
    CHECK_EQ(u32At(e, 8 + 4 + 13), 2u);
}

void TestDecodeCommit()
{
    CASE("CommitText：flags + 两段长度 + 两段文本；截断返回空");
    Bytes p;
    append(p, le32(COMMIT_FLAG_HAS_NEW_COMPOSITION));
    append(p, le32(6));
    append(p, le32(1));
    append(p, std::string("你好"));
    append(p, std::string("n"));
    auto c = decodeCommitText(p);
    CHECK(c.has_value());
    CHECK(c->text == "你好");
    CHECK(c->newComposition == "n");
    p.pop_back();
    CHECK(!decodeCommitText(p).has_value());
}

void TestDecodeComposition()
{
    CASE("UpdateComposition：caret u32 + 剩余字节是文本");
    Bytes p = le32(3);
    append(p, std::string("nih"));
    auto u = decodeUpdateComposition(p);
    CHECK(u.has_value());
    CHECK_EQ(u->caretPos, 3u);
    CHECK(u->text == "nih");
    CHECK(!decodeUpdateComposition(Bytes{1, 2}).has_value());
}

void TestDecodeDeferred()
{
    CASE("HoldComposition / CommitAndHold 布局");
    Bytes h = le32(500);
    append(h, le32(3));
    append(h, std::string("，"));
    auto hd = decodeHoldComposition(h);
    CHECK(hd.has_value());
    CHECK_EQ(hd->timeoutMs, 500u);
    CHECK(hd->holdText == "，");
    CHECK(hd->commitText.empty());

    Bytes c = le32(0);
    append(c, le32(3));
    append(c, le32(1));
    append(c, std::string("工"));
    append(c, std::string("a"));
    auto cd = decodeCommitAndHold(c);
    CHECK(cd.has_value());
    CHECK(cd->commitText == "工");
    CHECK(cd->holdText == "a");
}

void TestDecodeMisc()
{
    CASE("ReplaceBackward / CommitTextWithCursor / MoveCursor / KeyType");
    Bytes r = le32(1);
    append(r, le32(1));
    append(r, std::string("."));
    auto rd = decodeReplaceBackward(r);
    CHECK(rd && rd->count == 1 && rd->text == ".");

    Bytes w = le32(6);
    append(w, le32(1));
    append(w, std::string("（）"));
    auto wd = decodeCommitTextWithCursor(w);
    CHECK(wd && wd->cursorOffset == 1 && wd->text == "（）");

    auto m = decodeMoveCursor(le32(1));
    CHECK(m && *m == 1);
    CHECK(decodeKeyType(Bytes{'h', 'i'}) == "hi");
}

void TestDecodeRender()
{
    CASE("HostRenderFrame 24/28 字节；OverlayFrame 68 字节；CandidateRects；Ext 信封");
    Bytes f;
    for (uint32_t v : {7u, uint32_t(-10), 20u, 100u, 50u, 0x9u}) {
        append(f, le32(v));
    }
    auto fr = decodeHostRenderFrame(f);
    CHECK(fr.has_value());
    CHECK_EQ(fr->x, -10);
    CHECK_EQ(fr->scale, 1u);
    CHECK(fr->visible());
    CHECK((fr->flags & FRAME_FLAG_ABSOLUTE_POS) != 0);
    append(f, le32(2));
    CHECK_EQ(decodeHostRenderFrame(f)->scale, 2u);
    CHECK(!decodeHostRenderFrame(Bytes(20, 0)).has_value());

    // CMD_OVERLAY_FRAME：偏移按 Rust `encode_overlay_frame` 写死（布局改了两边一起改）。
    Bytes ov;
    for (uint32_t v : {2u, 9u, 120u, 40u, 0x5u, 1u, uint32_t(-30), 200u, uint32_t(-31), 150u, 7u,
                       16u, 3u, 4u, 110u, 30u, 1500u}) {
        append(ov, le32(v));
    }
    auto od = decodeOverlayFrame(ov);
    CHECK(od.has_value());
    CHECK_EQ(od->kind, 2u);
    CHECK_EQ(od->seq, 9u);
    CHECK_EQ(od->width, 120u);
    CHECK_EQ(od->height, 40u);
    CHECK(od->visible());
    CHECK_EQ(od->place, 1u);
    CHECK_EQ(od->x, -30);
    CHECK_EQ(od->y, 200);
    CHECK_EQ(od->altX, -31);
    CHECK_EQ(od->altY, 150);
    CHECK_EQ(od->anchor, 7u);
    CHECK_EQ(od->margin, 16);
    CHECK_EQ(od->contentX, 3);
    CHECK_EQ(od->contentY, 4);
    CHECK_EQ(od->contentW, 110u);
    CHECK_EQ(od->contentH, 30u);
    CHECK_EQ(od->durationMs, 1500);
    // 服务端现在恒发 72 字节（末尾多一个 f32 scale）：多出的尾段不影响前 68 字节的读取。
    Bytes ov72 = ov;
    append(ov72, le32(0x40000000u)); // 2.0f
    auto od72 = decodeOverlayFrame(ov72);
    CHECK(od72.has_value());
    CHECK_EQ(od72->kind, 2u);
    CHECK_EQ(od72->durationMs, 1500);
    ov.pop_back();
    CHECK(!decodeOverlayFrame(ov).has_value());

    Bytes rects = le32(1);
    for (uint32_t v : {0u, 4u, 5u, 30u, 20u}) {
        append(rects, le32(v));
    }
    auto rd = decodeCandidateRects(rects);
    CHECK(rd && rd->size() == 1);
    CHECK((*rd)[0].contains(4, 5));
    CHECK(!(*rd)[0].contains(34, 5));
    rects.pop_back();
    CHECK(!decodeCandidateRects(rects).has_value());

    Bytes e = le32(4);
    append(e, std::string("shot"));
    append(e, le32(2));
    append(e, std::string("{}"));
    auto ed = decodeExt(e);
    CHECK(ed && ed->kind == "shot" && ed->body.size() == 2);
    e.pop_back();
    CHECK(!decodeExt(e).has_value());
}

void TestStableHash()
{
    CASE("stableHash：空串 0，非空恒非 0 且稳定");
    CHECK_EQ(stableHash(""), 0u);
    CHECK(stableHash("gedit") != 0);
    CHECK_EQ(stableHash("gedit"), stableHash("gedit"));
    CHECK(stableHash("gedit") != stableHash("kate"));
    // 与 Swift FNV-1a 同算法：'a' 的标准 FNV-1a 32 值
    CHECK_EQ(stableHash("a"), 0xE40C292Cu);
}

/// 一帧 72 字节的浮层元数据（kind 之外的字段随便填），尾段由调用方追加。
Bytes overlay72(uint32_t kind)
{
    Bytes p;
    for (uint32_t v : {kind, 3u, 200u, 40u, 0x5u, 4u, 10u, 20u, 10u, 20u, 0u, 0u, 0u, 0u, 200u,
                       40u, 0u, 0x3F800000u /* 1.0f */}) {
        append(p, le32(v));
    }
    return p;
}

void TestOverlayDragTail()
{
    CASE("overlay_drag_tail_parsed_only_for_panel_kinds");
    // 工具栏：count + count × (x y w h)，偏移按 Rust `encode_overlay_frame` 写死（72 字节之后）。
    Bytes tb = overlay72(OVERLAY_KIND_TOOLBAR);
    append(tb, le32(2));
    for (uint32_t v : {1u, 2u, 30u, 40u, uint32_t(-5), 6u, 7u, 8u}) {
        append(tb, le32(v));
    }
    auto t = decodeOverlayFrame(tb);
    CHECK(t.has_value());
    CHECK_EQ(t->drag.size(), size_t(2));
    CHECK_EQ(t->drag[0].x, 1);
    CHECK_EQ(t->drag[0].y, 2);
    CHECK_EQ(t->drag[0].w, 30);
    CHECK_EQ(t->drag[0].h, 40);
    CHECK_EQ(t->drag[1].x, -5);
    CHECK_EQ(t->drag[1].h, 8);
    // 尾段截断：count 说有两块、数据只够一块半 → 整帧拒收（不拿半截拖动区去命中）。
    Bytes cut(tb.begin(), tb.end() - 8);
    CHECK(!decodeOverlayFrame(cut).has_value());

    // 软键盘零个拖动区：只多一个 count。
    Bytes skb = overlay72(OVERLAY_KIND_SOFTKEYBOARD);
    append(skb, le32(0));
    auto s = decodeOverlayFrame(skb);
    CHECK(s.has_value() && s->drag.empty());

    // 其它 kind：72 字节之后的东西一概不认（服务端不编码；将来真多了别的字段也不该被当成拖动区）。
    Bytes st = overlay72(OVERLAY_KIND_STATUS);
    append(st, le32(1));
    for (uint32_t v : {0u, 0u, 10u, 10u}) {
        append(st, le32(v));
    }
    auto o = decodeOverlayFrame(st);
    CHECK(o.has_value() && o->drag.empty());
    Bytes menu = overlay72(OVERLAY_KIND_MENU);
    append(menu, le32(7)); // 菜单帧后面跟着一个不成形的「count」：照样收、不读
    CHECK(decodeOverlayFrame(menu).has_value());

    CASE("CMD_PANEL_POINTER：20 字节，kind event button x y（x/y 有符号）");
    Bytes pp = encodePanelPointer(OVERLAY_KIND_SOFTKEYBOARD, PANEL_POINTER_RELEASE, 1, -5, 40);
    CHECK_EQ(pp.size(), HEADER_SIZE + 20);
    CHECK_EQ(u16At(pp, 2), uint16_t(CMD_PANEL_POINTER));
    CHECK_EQ(u32At(pp, 4), 20u);
    CHECK_EQ(u32At(pp, 8), OVERLAY_KIND_SOFTKEYBOARD);
    CHECK_EQ(u32At(pp, 12), PANEL_POINTER_RELEASE);
    CHECK_EQ(u32At(pp, 16), 1u);
    CHECK_EQ(int32_t(u32At(pp, 20)), -5);
    CHECK_EQ(int32_t(u32At(pp, 24)), 40);
}

} // namespace

int main()
{
    TestHeader();
    TestKeyEvent();
    TestFocusGained();
    TestFocusLostAndToken();
    TestSmallUpstream();
    TestDecodeCommit();
    TestDecodeComposition();
    TestDecodeDeferred();
    TestDecodeMisc();
    TestDecodeRender();
    TestOverlayDragTail();
    TestStableHash();
    TEST_MAIN_END();
}
