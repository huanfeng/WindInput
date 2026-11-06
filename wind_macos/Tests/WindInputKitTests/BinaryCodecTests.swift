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

    func testTokenFrame_ImeActivated() throws {
        // macOS 激活路径补发 IME_ACTIVATED：payload 就是 8 字节 clientToken。
        let token: UInt64 = (UInt64(0x1234) << 32) | 7
        let f = BinaryCodec.encodeTokenFrame(
            cmd: UpstreamCmd.imeActivated, clientToken: token, async: true)
        XCTAssertEqual(f.count, WireProtocol.headerSize + 8)
        let (cmd, len, isAsync) = try BinaryCodec.decodeHeader(f)
        XCTAssertEqual(cmd, UpstreamCmd.imeActivated)
        XCTAssertEqual(len, 8)
        XCTAssertTrue(isAsync)
        let payload = f.subdata(in: WireProtocol.headerSize ..< f.count)
        XCTAssertEqual(payload.readUInt64LE(at: 0), token)
    }

    func testFocusGainedFrame_InputScopeMask() throws {
        // 密码框: IS_PASSWORD 位 (bit31)。载荷与 Win 端同构 (39B) + bundleID 段。
        let mask: UInt64 = UInt64(1) << 31
        let token: UInt64 = (UInt64(4321) << 32) | 7
        let f = BinaryCodec.encodeFocusGainedFrame(
            clientToken: token, inputScopeMask: mask, bundleID: "com.apple.TextEdit")
        let (cmd, len, _) = try BinaryCodec.decodeHeader(f)
        XCTAssertEqual(cmd, UpstreamCmd.focusGained)
        XCTAssertEqual(len, UInt32(43 + "com.apple.TextEdit".utf8.count))
        let payload = f.subdata(in: WireProtocol.headerSize ..< f.count)
        // caret 段全 0: 服务端 apply_focus_caret 见 height==0 即返回, 不污染坐标缓存。
        for i in 0..<20 { XCTAssertEqual(payload[payload.startIndex + i], 0) }
        XCTAssertEqual(payload.readUInt64LE(at: 20), token)
        XCTAssertEqual(payload.readUInt64LE(at: 28), mask)
        XCTAssertEqual(payload[payload.startIndex + 38], 0) // caretSource = UNKNOWN
        let n = Int(payload.readUInt32LE(at: 39))
        XCTAssertEqual(
            String(data: payload.subdata(in: (payload.startIndex + 43)..<(payload.startIndex + 43 + n)),
                   encoding: .utf8),
            "com.apple.TextEdit")
    }

    func testFocusGainedFrame_MeetsRustMinimumLength() throws {
        // 回归: 曾只发 12 字节, 而 Rust FocusGainedPayload::from_bytes 下限是 36 —— 解码恒
        // 失败, FOCUS_GAINED 重型段 (按应用初始模式 / compat 规则 / 密码框抑制) 从未执行。
        let f = BinaryCodec.encodeFocusGainedFrame(
            clientToken: 0, inputScopeMask: 0, bundleID: "")
        let (_, len, _) = try BinaryCodec.decodeHeader(f)
        XCTAssertGreaterThanOrEqual(len, 39)
        let payload = f.subdata(in: WireProtocol.headerSize ..< f.count)
        XCTAssertEqual(payload.readUInt32LE(at: 39), 0) // bundleID 为空也要有长度字段
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

    /// 滚轮帧：负 delta 必须按 i32 位模式落盘，不能被 UInt32 转换截成正数——
    /// 服务端靠符号区分上滚/下滚，符号丢了就只会往一个方向走。
    func testCandidateScrollFrame_NegativeDeltaKeepsSign() throws {
        let f = BinaryCodec.encodeCandidateScrollFrame(delta: -240)
        let (cmd, len, _) = try BinaryCodec.decodeHeader(f)
        XCTAssertEqual(cmd, UpstreamCmd.candidateScroll)
        XCTAssertEqual(len, 4)
        let payload = f.subdata(in: WireProtocol.headerSize ..< f.count)
        XCTAssertEqual(Int32(bitPattern: payload.readUInt32LE(at: 0)), -240)
    }

    // MARK: - 扩展信封 (0x0E01)

    func testExtEnvelope_Roundtrip() throws {
        let body = Data(#"{"args":["--page=dict","--schema=wubi86"]}"#.utf8)
        let f = BinaryCodec.encodeExtFrame(kind: ExtKind.settingsOpen, body: body)
        let (cmd, _, _) = try BinaryCodec.decodeHeader(f)
        XCTAssertEqual(cmd, UpstreamCmd.ext)
        let payload = f.subdata(in: WireProtocol.headerSize ..< f.count)
        let got = BinaryCodec.decodeExt(payload)
        XCTAssertEqual(got?.kind, ExtKind.settingsOpen)
        XCTAssertEqual(got?.body, body)
    }

    func testExtEnvelope_EmptyBody() throws {
        let f = BinaryCodec.encodeExtFrame(kind: "diag.hud", body: Data())
        let payload = f.subdata(in: WireProtocol.headerSize ..< f.count)
        XCTAssertEqual(BinaryCodec.decodeExt(payload)?.kind, "diag.hud")
        XCTAssertEqual(BinaryCodec.decodeExt(payload)?.body.count, 0)
    }

    func testExtEnvelope_TruncatedReturnsNil() {
        // 截断的信封须解成 nil 交由调用方忽略, 不能拿半截 kind 去分发。
        var bad = Data(count: 4)
        bad.writeUInt32LE(99, at: 0)   // kindLen 超出实际字节
        bad.append(contentsOf: Array("abc".utf8))
        XCTAssertNil(BinaryCodec.decodeExt(bad))
        XCTAssertNil(BinaryCodec.decodeExt(Data()))
    }

    /// Rust 侧 settings_argv 已把 argv 切好, Swift 只做 JSON 取值 —— 回归:
    /// 此前是 Swift 自己按空格+双引号切词, 等于让另一门语言去猜 Rust 的引号规则。
    func testExtEnvelope_SettingsArgsAreStructured() throws {
        let body = Data(#"{"args":["--page=add-word","--text=\u4f60 \u597d"]}"#.utf8)
        let f = BinaryCodec.encodeExtFrame(kind: ExtKind.settingsOpen, body: body)
        let payload = f.subdata(in: WireProtocol.headerSize ..< f.count)
        let got = try XCTUnwrap(BinaryCodec.decodeExt(payload))
        let obj = try JSONSerialization.jsonObject(with: got.body) as? [String: Any]
        let argv = try XCTUnwrap(obj?["args"] as? [String])
        XCTAssertEqual(argv.count, 2, "含空格的值必须仍是一个 argv")
        XCTAssertEqual(argv[1], "--text=\u{4f60} \u{597d}")
    }
}
