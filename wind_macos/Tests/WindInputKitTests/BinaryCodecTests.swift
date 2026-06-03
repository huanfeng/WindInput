import XCTest
@testable import WindInputKit

final class BinaryCodecTests: XCTestCase {

    func testHeaderRoundtrip_KeyEvent() throws {
        let h = BinaryCodec.encodeHeader(cmd: UpstreamCmd.keyEvent, payloadLen: 18)
        XCTAssertEqual(h.count, WireProtocol.headerSize)
        let (cmd, len, isAsync) = try BinaryCodec.decodeHeader(h)
        XCTAssertEqual(cmd, UpstreamCmd.keyEvent)
        XCTAssertEqual(len, 18)
        XCTAssertFalse(isAsync)
    }

    func testHeaderAsyncFlag() throws {
        let h = BinaryCodec.encodeHeader(cmd: UpstreamCmd.modeNotify, payloadLen: 0, async: true)
        let (cmd, len, isAsync) = try BinaryCodec.decodeHeader(h)
        XCTAssertEqual(cmd, UpstreamCmd.modeNotify)
        XCTAssertEqual(len, 0)
        XCTAssertTrue(isAsync)
    }

    func testHeaderVersionMismatch() {
        // 构造一个 v2 帧 (major != 0x1)
        var h = BinaryCodec.encodeHeader(cmd: UpstreamCmd.keyEvent, payloadLen: 0)
        h[0] = 0x01
        h[1] = 0x20  // version = 0x2001
        do {
            _ = try BinaryCodec.decodeHeader(h)
            XCTFail("expected versionMismatch")
        } catch IPCError.versionMismatch(let v) {
            XCTAssertEqual(v, 0x2001)
        } catch {
            XCTFail("wrong error: \(error)")
        }
    }

    func testHeaderPayloadTooLarge() {
        var h = BinaryCodec.encodeHeader(cmd: UpstreamCmd.keyEvent, payloadLen: 0)
        // length = MaxPayloadSize + 1
        let bad: UInt32 = WireProtocol.maxPayloadSize + 1
        h.writeUInt32LE(bad, at: 4)
        do {
            _ = try BinaryCodec.decodeHeader(h)
            XCTFail("expected payloadTooLarge")
        } catch IPCError.payloadTooLarge(let n) {
            XCTAssertEqual(n, bad)
        } catch {
            XCTFail("wrong error: \(error)")
        }
    }

    func testKeyEventFrameRoundtrip() throws {
        let original = KeyEventPayload(
            keyCode: 0x41,
            scanCode: 0x1E,
            modifiers: 0x0001,
            eventType: .down,
            toggles: 0x01,
            eventSeq: 42,
            prevChar: 0x4E2D  // '中'
        )
        let frame = BinaryCodec.encodeKeyEventFrame(original)
        XCTAssertEqual(frame.count, WireProtocol.headerSize + 18)

        // 验帧头
        let header = frame.prefix(WireProtocol.headerSize)
        let (cmd, len, _) = try BinaryCodec.decodeHeader(header)
        XCTAssertEqual(cmd, UpstreamCmd.keyEvent)
        XCTAssertEqual(len, 18)

        // 验 payload
        let payload = frame.subdata(in: WireProtocol.headerSize..<frame.count)
        let decoded = try BinaryCodec.decodeKeyEventPayload(payload)
        XCTAssertEqual(decoded, original)
    }

    func testEmptyFrame_AckHeader() throws {
        let f = BinaryCodec.encodeEmptyFrame(cmd: DownstreamCmd.ack)
        XCTAssertEqual(f.count, WireProtocol.headerSize)
        let (cmd, len, _) = try BinaryCodec.decodeHeader(f)
        XCTAssertEqual(cmd, DownstreamCmd.ack)
        XCTAssertEqual(len, 0)
    }

    func testFocusGainedFrame_InputScopeMask() throws {
        // 密码框: IS_PASSWORD 位 (bit31)。布局 = pid:u32(0) + inputScopeMask:u64。
        let mask: UInt64 = UInt64(1) << 31
        let f = BinaryCodec.encodeFocusGainedFrame(inputScopeMask: mask)
        let (cmd, len, _) = try BinaryCodec.decodeHeader(f)
        XCTAssertEqual(cmd, UpstreamCmd.focusGained)
        XCTAssertEqual(len, 12)
        let payload = f.subdata(in: WireProtocol.headerSize ..< f.count)
        XCTAssertEqual(payload.readUInt32LE(at: 0), 0) // pid 占位
        let lo = UInt64(payload.readUInt32LE(at: 4))
        let hi = UInt64(payload.readUInt32LE(at: 8))
        XCTAssertEqual(lo | (hi << 32), mask)
    }

    func testFocusGainedFrame_NonSensitiveMaskZero() throws {
        let f = BinaryCodec.encodeFocusGainedFrame(inputScopeMask: 0)
        let payload = f.subdata(in: WireProtocol.headerSize ..< f.count)
        XCTAssertEqual(payload.readUInt32LE(at: 4), 0)
        XCTAssertEqual(payload.readUInt32LE(at: 8), 0)
    }

    func testDecodeKeyEventPayloadTooShort() {
        do {
            _ = try BinaryCodec.decodeKeyEventPayload(Data(repeating: 0, count: 8))
            XCTFail("expected payloadTooShort")
        } catch IPCError.payloadTooShort {
            // ok
        } catch {
            XCTFail("wrong error: \(error)")
        }
    }

    func testDecodeKeyEventPayload_16Bytes_NoPrevChar() throws {
        // Win/老 TSF 端可能只发 16 字节 (无 prevChar). codec 应回退 prevChar=0.
        var p = Data(count: 16)
        p.writeUInt32LE(0x41, at: 0)
        p.writeUInt32LE(0x1E, at: 4)
        p.writeUInt32LE(0x0001, at: 8)
        p[12] = 0
        p[13] = 0
        p.writeUInt16LE(7, at: 14)

        let decoded = try BinaryCodec.decodeKeyEventPayload(p)
        XCTAssertEqual(decoded.keyCode, 0x41)
        XCTAssertEqual(decoded.eventSeq, 7)
        XCTAssertEqual(decoded.prevChar, 0)
    }
}
