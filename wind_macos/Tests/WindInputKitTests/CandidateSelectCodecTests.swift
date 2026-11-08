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
}
