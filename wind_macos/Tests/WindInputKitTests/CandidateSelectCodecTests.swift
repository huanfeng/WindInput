import XCTest
@testable import WindInputKit

/// CmdCandidateSelect 编码 + CmdCandidateRects 解码 (M5 鼠标点选协议).
final class CandidateSelectCodecTests: XCTestCase {

    func testEncodeCandidateSelectFrame() {
        let frame = BinaryCodec.encodeCandidateSelectFrame(index: 3)
        XCTAssertEqual(frame.count, 8 + 4)
        let (cmd, length, _) = try! BinaryCodec.decodeHeader(frame)
        XCTAssertEqual(cmd, UpstreamCmd.candidateSelect)
        XCTAssertEqual(length, 4)
        let payload = frame.subdata(in: WireProtocol.headerSize..<frame.count)
        XCTAssertEqual(payload.readUInt32LE(at: 0), 3)
    }

    func testEncodeCandidateSelectFrame_NegativeClampedToZero() {
        let frame = BinaryCodec.encodeCandidateSelectFrame(index: -5)
        let payload = frame.subdata(in: WireProtocol.headerSize..<frame.count)
        XCTAssertEqual(payload.readUInt32LE(at: 0), 0)
    }

    func testDecodeCandidateRectsPayload() throws {
        // count=2 + 2×(index,x,y,w,h)
        var buf = Data(count: 4 + 2 * 20)
        buf.writeUInt32LE(2, at: 0)
        // rect0: index=0, x=10,y=20,w=30,h=40
        buf.writeUInt32LE(0, at: 4)
        buf.writeUInt32LE(10, at: 8)
        buf.writeUInt32LE(20, at: 12)
        buf.writeUInt32LE(30, at: 16)
        buf.writeUInt32LE(40, at: 20)
        // rect1: index=1, x=50,y=20,w=30,h=40
        buf.writeUInt32LE(1, at: 24)
        buf.writeUInt32LE(50, at: 28)
        buf.writeUInt32LE(20, at: 32)
        buf.writeUInt32LE(30, at: 36)
        buf.writeUInt32LE(40, at: 40)

        let rects = try BinaryCodec.decodeCandidateRectsPayload(buf)
        XCTAssertEqual(rects.count, 2)
        XCTAssertEqual(rects[0], CandidateHitRect(index: 0, x: 10, y: 20, w: 30, h: 40))
        XCTAssertEqual(rects[1].x, 50)
        // hit-test
        XCTAssertTrue(rects[0].contains(px: 15, py: 25))
        XCTAssertFalse(rects[0].contains(px: 45, py: 25))
        XCTAssertTrue(rects[1].contains(px: 55, py: 25))
    }

    func testDecodeCandidateRectsPayload_Empty() throws {
        var buf = Data(count: 4)
        buf.writeUInt32LE(0, at: 0)
        let rects = try BinaryCodec.decodeCandidateRectsPayload(buf)
        XCTAssertEqual(rects.count, 0)
    }

    /// 偏移照 Rust `candidate_skin_layout_matches_documented_offsets` 那份写死。
    func testDecodeCandidateSkinPayload() throws {
        var buf = Data(count: 4 + 16 + 4 + 2 * 28 + 4 + 2 * 16)
        let put = { (v: Int32, at: Int) in buf.writeUInt32LE(UInt32(bitPattern: v), at: at) }
        for (o, v) in [(0, 3), (4, 10), (8, 20), (12, 300), (16, 120), (20, 2),
                       (24, 2), (28, 2), (32, 3), (36, -5), (40, -6), (44, 40), (48, 50),
                       (52, 0), (56, 1), (60, 0), (64, 1), (68, 2), (72, 3), (76, 4),
                       (80, 2), (84, 10), (88, 20), (92, 300), (96, 1)] as [(Int, Int32)] {
            put(v, o)
        }
        let skin = try BinaryCodec.decodeCandidateSkinPayload(buf)
        XCTAssertTrue(skin.isActive)
        XCTAssertEqual(skin.frame, SkinRect(x: 10, y: 20, w: 300, h: 120))
        XCTAssertEqual(skin.decor, [
            CandidateSkin.Decor(index: 2, hit: CandidateSkin.hitPass, flags: 3,
                                rect: SkinRect(x: -5, y: -6, w: 40, h: 50)),
            CandidateSkin.Decor(index: 0, hit: CandidateSkin.hitBlock, flags: 0,
                                rect: SkinRect(x: 1, y: 2, w: 3, h: 4)),
        ])
        XCTAssertTrue(skin.splitDecorBelow)
        // 截断的装饰表报错，不越界读。
        XCTAssertThrowsError(try BinaryCodec.decodeCandidateSkinPayload(buf.prefix(51)))
        // 没启用皮肤：28 字节全零。
        let none = try BinaryCodec.decodeCandidateSkinPayload(Data(count: 28))
        XCTAssertEqual(none, CandidateSkin())
        XCTAssertFalse(none.isActive)
    }

    func testSkinNoDragRegion() {
        // 窗体 (10,10,100,40)；左上外侧 block 装饰盖进窗体一角；右侧 drag 装饰；下方没拆层的 pass。
        let skin = CandidateSkin(flags: CandidateSkin.flagActive,
                                 frame: SkinRect(x: 10, y: 10, w: 100, h: 40), decor: [
            CandidateSkin.Decor(index: 0, hit: CandidateSkin.hitBlock, flags: 0,
                                rect: SkinRect(x: 0, y: 0, w: 20, h: 20)),
            CandidateSkin.Decor(index: 1, hit: CandidateSkin.hitDrag, flags: 0,
                                rect: SkinRect(x: 110, y: 10, w: 20, h: 20)),
            CandidateSkin.Decor(index: 2, hit: CandidateSkin.hitPass, flags: 0,
                                rect: SkinRect(x: 10, y: 50, w: 20, h: 10)),
            CandidateSkin.Decor(index: 3, hit: CandidateSkin.hitPass,
                                flags: CandidateSkin.decorSplit | CandidateSkin.decorUnder,
                                rect: SkinRect(x: 60, y: 50, w: 20, h: 10)),
        ])
        XCTAssertTrue(skin.blocksDrag(px: 5, py: 5))      // block 装饰、窗体外
        XCTAssertFalse(skin.blocksDrag(px: 15, py: 15))   // block 装饰盖进窗体：窗体内照常拖
        XCTAssertFalse(skin.blocksDrag(px: 120, py: 15))  // drag 装饰
        XCTAssertTrue(skin.blocksDrag(px: 15, py: 55))    // 没拆层的 pass 按 block
        XCTAssertFalse(skin.blocksDrag(px: 65, py: 55))   // 拆层的 pass 不在主窗口上
        XCTAssertFalse(skin.blocksDrag(px: 200, py: 5))   // 都不在
        XCTAssertTrue(skin.splitDecorBelow)
        var off = skin
        off.flags = 0
        XCTAssertFalse(off.blocksDrag(px: 5, py: 5))      // 没启用皮肤
        // 逻辑点：Retina 下除以 2。
        let half = skin.scaled(by: 2)
        XCTAssertEqual(half.frame, SkinRect(x: 5, y: 5, w: 50, h: 20))
        XCTAssertTrue(half.blocksDrag(px: 2, py: 2))
        XCTAssertFalse(half.blocksDrag(px: 7, py: 7))
        // 非整数倍：向外取整，装饰边缘不缩进去。
        let odd = CandidateSkin(flags: CandidateSkin.flagActive, decor: [
            CandidateSkin.Decor(index: 0, hit: CandidateSkin.hitBlock, flags: 0,
                                rect: SkinRect(x: 3, y: 3, w: 4, h: 4)),
        ]).scaled(by: 2)
        XCTAssertEqual(odd.decor[0].rect, SkinRect(x: 1, y: 1, w: 3, h: 3))
        // 1.5 倍：(10, 10, 30, 15) → [6.67, 26.67) × [6.67, 16.67) 外包。
        let frac = CandidateSkin(flags: CandidateSkin.flagActive,
                                 frame: SkinRect(x: 10, y: 10, w: 30, h: 15)).scaled(by: 1.5)
        XCTAssertEqual(frac.frame, SkinRect(x: 6, y: 6, w: 21, h: 11))
    }

    func testDecorAtPicksTopmostClickableDecor() {
        let d = { (index: UInt32, hit: UInt32, flags: UInt32, x: Int32, y: Int32, w: Int32) in
            CandidateSkin.Decor(index: index, hit: hit, flags: flags,
                                rect: SkinRect(x: x, y: y, w: w, h: w))
        }
        let skin = CandidateSkin(flags: CandidateSkin.flagActive,
                                 frame: SkinRect(x: 20, y: 20, w: 100, h: 60), decor: [
            d(7, CandidateSkin.hitBlock, CandidateSkin.decorUnder, 0, 0, 40), // 底层，伸进窗体
            d(3, CandidateSkin.hitDrag, 0, 100, 0, 40),
            d(5, CandidateSkin.hitBlock, 0, 120, 0, 30), // 压在 3 上面
            d(9, CandidateSkin.hitPass, 0, 0, 70, 30),
        ])
        XCTAssertEqual(skin.decorAt(px: 5, py: 5), 7)     // 窗体外的 under 装饰
        XCTAssertNil(skin.decorAt(px: 30, py: 30))        // 同一块落在窗体内：被窗体盖住
        XCTAssertEqual(skin.decorAt(px: 105, py: 5), 3)
        XCTAssertEqual(skin.decorAt(px: 125, py: 5), 5)   // 重叠取上层
        XCTAssertEqual(skin.decorAt(px: 130, py: 35), 3)  // 上层下方露出的一截
        XCTAssertNil(skin.decorAt(px: 150, py: 5))        // 右边界开区间
        XCTAssertNil(skin.decorAt(px: 5, py: 75))         // pass 不接鼠标
        XCTAssertNil(skin.decorAt(px: 60, py: 50))        // 窗体内空白
        var off = skin
        off.flags = 0
        XCTAssertNil(off.decorAt(px: 5, py: 5))           // 没启用皮肤
        XCTAssertEqual(ExtKind.skinDecorClick, "skin.decor_click")
    }

    func testSkinRectContainsDoesNotOverflow() {
        let r = SkinRect(x: Int32.max - 1, y: 0, w: 10, h: 1)
        XCTAssertTrue(r.contains(px: CGFloat(Int32.max), py: 0))
    }

    func testSplitDecorAboveWhenAnyIsOverFrame() {
        let d = { (flags: UInt32) in
            CandidateSkin.Decor(index: 0, hit: CandidateSkin.hitPass, flags: flags,
                                rect: SkinRect(x: 0, y: 0, w: 1, h: 1))
        }
        let split = CandidateSkin.decorSplit
        var skin = CandidateSkin(flags: CandidateSkin.flagActive, decor: [d(split | CandidateSkin.decorUnder), d(split)])
        XCTAssertFalse(skin.splitDecorBelow)
        skin.decor = [d(0)]
        XCTAssertFalse(skin.splitDecorBelow)              // 没有拆层装饰
    }

    func testAnimInPlaceNeedsMatchingSeq() {
        let anim = HostRenderFramePayload(seq: 7, x: 0, y: 0, width: 1, height: 1, flags: 0x10 | 0x1)
        XCTAssertTrue(anim.animInPlace(shmSeq: 7))
        XCTAssertFalse(anim.animInPlace(shmSeq: 8))   // SHM 已是更新的一帧：按普通帧
        let plain = HostRenderFramePayload(seq: 7, x: 0, y: 0, width: 1, height: 1, flags: 0x1)
        XCTAssertFalse(plain.animInPlace(shmSeq: 7))
    }

    /// 候选帧按长度逐段取：60 字节带内容偏移、上方点与落位框，对照 Rust `encode_host_render_frame` 的布局。
    func testHostRenderFrameDecodesTailByLength() throws {
        let vals: [Int32] = [7, -3, 20, 100, 40, 0x3, 2, 12, -5, -3, -70, 12, 6, 76, 30]
        var buf = Data(count: vals.count * 4)
        for (i, v) in vals.enumerated() { buf.writeUInt32LE(UInt32(bitPattern: v), at: i * 4) }
        let full = try BinaryCodec.decodeHostRenderFramePayload(buf)
        XCTAssertEqual(full.scale, 2)
        XCTAssertEqual(full.contentX, 12); XCTAssertEqual(full.contentY, -5)
        XCTAssertEqual(full.above, CGPoint(x: -3, y: -70))
        XCTAssertEqual(full.placeBox, CGRect(x: 12, y: 6, width: 76, height: 30))

        let mid = try BinaryCodec.decodeHostRenderFramePayload(buf.prefix(36))
        XCTAssertEqual(mid.contentY, -5)
        XCTAssertNil(mid.above); XCTAssertNil(mid.placeBox)
        let old = try BinaryCodec.decodeHostRenderFramePayload(buf.prefix(28))
        XCTAssertEqual(old.contentX, 0); XCTAssertNil(old.above)
    }
}
