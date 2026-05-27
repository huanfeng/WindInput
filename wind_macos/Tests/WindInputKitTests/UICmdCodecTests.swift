import XCTest
@testable import WindInputKit

final class UICmdCodecTests: XCTestCase {

    // MARK: - ByteWriter / ByteReader 基础

    func testByteCodec_PrimitivesRoundtrip() throws {
        let w = ByteWriter()
        w.writeU8(0xAB)
        w.writeBool(true)
        w.writeBool(false)
        w.writeU16LE(0x1234)
        w.writeU32LE(0xDEADBEEF)
        w.writeI32LE(-42)
        w.writeU64LE(0x0102030405060708)
        w.writeF64(3.14159)
        w.writeString("你好")
        w.writeString("")

        let r = ByteReader(w.bytes())
        XCTAssertEqual(try r.readU8(), 0xAB)
        XCTAssertTrue(try r.readBool())
        XCTAssertFalse(try r.readBool())
        XCTAssertEqual(try r.readU16LE(), 0x1234)
        XCTAssertEqual(try r.readU32LE(), 0xDEADBEEF)
        XCTAssertEqual(try r.readI32LE(), -42)
        XCTAssertEqual(try r.readU64LE(), 0x0102030405060708)
        XCTAssertEqual(try r.readF64(), 3.14159, accuracy: 1e-9)
        XCTAssertEqual(try r.readString(), "你好")
        XCTAssertEqual(try r.readString(), "")
        XCTAssertTrue(r.isEOF)
    }

    func testByteReader_BufferUnderflow() {
        let r = ByteReader(Data([0x01, 0x02]))
        _ = try? r.readU8()
        _ = try? r.readU8()
        XCTAssertThrowsError(try r.readU8()) { error in
            XCTAssertEqual(error as? IPCError, IPCError.bufferUnderflow)
        }
    }

    // MARK: - Candidate roundtrip

    func testCandidate_Roundtrip_AllFlags() throws {
        let original = Candidate(
            text: "好",
            code: "h",
            comment: "(常用)",
            index: 1,
            indexLabel: "①",
            source: "codetable",
            isCommon: true,
            isPhrase: true,
            isCommand: true,
            isGroup: true,
            isGroupMember: true,
            hasShadow: true
        )
        let w = ByteWriter()
        original.encode(to: w)
        let decoded = try Candidate.decode(from: ByteReader(w.bytes()))
        XCTAssertEqual(decoded, original)
    }

    func testCandidate_Roundtrip_NoFlags() throws {
        let original = Candidate(text: "好", index: 1)
        let w = ByteWriter()
        original.encode(to: w)
        let decoded = try Candidate.decode(from: ByteReader(w.bytes()))
        XCTAssertEqual(decoded, original)
    }

    func testCandidate_FlagsBitOrder() throws {
        // 仅 isCommon: flags = 0x01
        let c = Candidate(text: "t", index: 0, isCommon: true)
        let w = ByteWriter()
        c.encode(to: w)
        let bytes = w.bytes()
        // 最后一个字节是 flags
        XCTAssertEqual(bytes.last, 0x01)
    }

    // MARK: - CandidatesShowPayload roundtrip

    func testCandidatesShowPayload_Roundtrip() throws {
        let original = CandidatesShowPayload(
            candidates: [
                Candidate(text: "你好", code: "nh", index: 1, source: "pinyin", isCommon: true),
                Candidate(text: "拟好", code: "nh", index: 2, source: "pinyin"),
                Candidate(text: "拟稿", code: "ng", index: 3, source: "pinyin", isPhrase: true),
            ],
            input: "nihao",
            cursorPos: 5,
            caretX: 100,
            caretY: 200,
            caretHeight: 18,
            page: 1,
            totalPages: 3,
            totalCandidateCount: 27,
            candidatesPerPage: 9,
            selectedIndex: 0
        )
        let w = ByteWriter()
        original.encode(to: w)
        let decoded = try CandidatesShowPayload.decode(from: ByteReader(w.bytes()))
        XCTAssertEqual(decoded, original)
    }

    func testCandidatesShowPayload_Empty() throws {
        let original = CandidatesShowPayload()
        let w = ByteWriter()
        original.encode(to: w)
        let decoded = try CandidatesShowPayload.decode(from: ByteReader(w.bytes()))
        XCTAssertEqual(decoded, original)
    }

    // MARK: - UICmdFrame (header + payload 分离)

    func testUICmdFrame_DecodeCmdCandidatesShow() throws {
        // 构帧: cmdType=0x0601 + session=42 + CandidatesShowPayload bytes
        let session: UInt64 = 42
        let payload = CandidatesShowPayload(
            candidates: [Candidate(text: "测试", index: 1)],
            input: "ce",
            selectedIndex: 0
        )

        let w = ByteWriter(reserving: 32)
        w.writeU16LE(UICmd.candidatesShow)
        w.writeU64LE(session)
        payload.encode(to: w)
        let frameBytes = w.bytes()

        let frame = try UICmdCodec.decodeCommandFrame(frameBytes)
        XCTAssertEqual(frame.cmdType, UICmd.candidatesShow)
        XCTAssertEqual(frame.session, session)
        let decodedPayload = try CandidatesShowPayload.decode(from: ByteReader(frame.payload))
        XCTAssertEqual(decodedPayload, payload)
    }

    func testUICmdFrame_TooShort() {
        let shortData = Data([0x01, 0x06])   // 只 2 字节, 缺 session
        XCTAssertThrowsError(try UICmdCodec.decodeCommandFrame(shortData)) { error in
            if case .payloadTooShort = error as? IPCError {
                // ok
            } else {
                XCTFail("wrong error: \(error)")
            }
        }
    }
}
