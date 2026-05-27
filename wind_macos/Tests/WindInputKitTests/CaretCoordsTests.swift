import XCTest
import CoreGraphics
@testable import WindInputKit

/// CaretCoords + BinaryCodec.encodeCaretUpdateFrame 单测.
final class CaretCoordsTests: XCTestCase {

    // MARK: - 坐标转换

    func testCaretRectToWire_SingleMonitor_BottomLeftToTopLeft() {
        // 屏幕高度 1000. caret rect bottom-left 原点 = (100, 800), size 18×18.
        // 顶部 Y (wire) = 1000 - (800 + 18) = 182
        let rect = CGRect(x: 100, y: 800, width: 18, height: 18)
        let (x, y, h) = CaretCoords.caretRectToWire(rect, screenHeight: 1000)
        XCTAssertEqual(x, 100)
        XCTAssertEqual(y, 182)
        XCTAssertEqual(h, 18)
    }

    func testCaretRectToWire_AtBottomOfScreen() {
        // caret 在屏幕底部, bottom-left Y = 0
        let rect = CGRect(x: 0, y: 0, width: 10, height: 20)
        let (_, y, h) = CaretCoords.caretRectToWire(rect, screenHeight: 1000)
        XCTAssertEqual(y, 980)   // 1000 - 20
        XCTAssertEqual(h, 20)
    }

    func testCaretRectToWire_AtTopOfScreen() {
        // caret 紧贴屏幕顶部, bottom-left Y = screenHeight - height
        let rect = CGRect(x: 0, y: 980, width: 10, height: 20)
        let (_, y, _) = CaretCoords.caretRectToWire(rect, screenHeight: 1000)
        XCTAssertEqual(y, 0)
    }

    func testCaretRectToWire_FractionalRounding() {
        let rect = CGRect(x: 100.7, y: 800.3, width: 18, height: 18.4)
        let (x, _, h) = CaretCoords.caretRectToWire(rect, screenHeight: 1000)
        XCTAssertEqual(x, 101)   // 100.7 rounded
        XCTAssertEqual(h, 18)    // 18.4 rounded
    }

    // MARK: - Frame 字节布局

    func testEncodeCaretUpdateFrame_12BytePayload() {
        let frame = BinaryCodec.encodeCaretUpdateFrame(x: 100, y: 200, height: 18)
        // header (8) + payload (12) = 20 字节
        XCTAssertEqual(frame.count, 20)

        // 解 header
        let (cmd, length, isAsync) = try! BinaryCodec.decodeHeader(frame)
        XCTAssertEqual(cmd, UpstreamCmd.caretUpdate)
        XCTAssertEqual(length, 12)
        XCTAssertFalse(isAsync)

        // 解 payload
        let payload = frame.subdata(in: WireProtocol.headerSize..<frame.count)
        XCTAssertEqual(Int32(bitPattern: payload.readUInt32LE(at: 0)), 100)
        XCTAssertEqual(Int32(bitPattern: payload.readUInt32LE(at: 4)), 200)
        XCTAssertEqual(Int32(bitPattern: payload.readUInt32LE(at: 8)), 18)
    }

    func testEncodeCaretUpdateFrame_20ByteWithCompositionStart() {
        let frame = BinaryCodec.encodeCaretUpdateFrame(x: 100, y: 200, height: 18,
                                                       compositionStartX: 50, compositionStartY: 200)
        XCTAssertEqual(frame.count, 8 + 20)

        let (cmd, length, _) = try! BinaryCodec.decodeHeader(frame)
        XCTAssertEqual(cmd, UpstreamCmd.caretUpdate)
        XCTAssertEqual(length, 20)

        let payload = frame.subdata(in: WireProtocol.headerSize..<frame.count)
        XCTAssertEqual(Int32(bitPattern: payload.readUInt32LE(at: 0)),  100)
        XCTAssertEqual(Int32(bitPattern: payload.readUInt32LE(at: 4)),  200)
        XCTAssertEqual(Int32(bitPattern: payload.readUInt32LE(at: 8)),  18)
        XCTAssertEqual(Int32(bitPattern: payload.readUInt32LE(at: 12)), 50)
        XCTAssertEqual(Int32(bitPattern: payload.readUInt32LE(at: 16)), 200)
    }

    func testEncodeCaretUpdateFrame_NegativeCoordinates() {
        // 副屏可能 Y 为负, 协议用 int32 (LE) 编, 这里验证负数也能正确 roundtrip.
        let frame = BinaryCodec.encodeCaretUpdateFrame(x: -50, y: -100, height: 18)
        let payload = frame.subdata(in: WireProtocol.headerSize..<frame.count)
        XCTAssertEqual(Int32(bitPattern: payload.readUInt32LE(at: 0)), -50)
        XCTAssertEqual(Int32(bitPattern: payload.readUInt32LE(at: 4)), -100)
    }
}
