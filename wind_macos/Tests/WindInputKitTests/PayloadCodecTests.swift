import XCTest
@testable import WindInputKit

/// 测试 BinaryCodec 新增的 downstream payload decode 方法 (M2.2-C 实装).
final class PayloadCodecTests: XCTestCase {

    // MARK: - CmdCommitText (0x0101)

    func testDecodeCommitText_PlainAscii() throws {
        // 构 payload: flags(0) + textLen(5) + compLen(0) + "hello"
        var buf = Data(count: 12)
        buf.writeUInt32LE(0, at: 0)
        buf.writeUInt32LE(5, at: 4)
        buf.writeUInt32LE(0, at: 8)
        buf.append(contentsOf: "hello".utf8)

        let p = try BinaryCodec.decodeCommitTextPayload(buf)
        XCTAssertEqual(p.text, "hello")
        XCTAssertEqual(p.newComposition, "")
        XCTAssertFalse(p.modeChanged)
        XCTAssertFalse(p.hasNewComposition)
        XCTAssertFalse(p.chineseMode)
    }

    func testDecodeCommitText_UTF8WithFlagsAndComposition() throws {
        let text = "你好"   // 6 utf-8 bytes
        let comp = "world"  // 5 utf-8 bytes
        let flags: UInt32 = 0x0001 | 0x0002 | 0x0004   // 三个 flag 全开

        var buf = Data(count: 12)
        buf.writeUInt32LE(flags, at: 0)
        buf.writeUInt32LE(UInt32(text.utf8.count), at: 4)
        buf.writeUInt32LE(UInt32(comp.utf8.count), at: 8)
        buf.append(contentsOf: text.utf8)
        buf.append(contentsOf: comp.utf8)

        let p = try BinaryCodec.decodeCommitTextPayload(buf)
        XCTAssertEqual(p.text, text)
        XCTAssertEqual(p.newComposition, comp)
        XCTAssertTrue(p.modeChanged)
        XCTAssertTrue(p.hasNewComposition)
        XCTAssertTrue(p.chineseMode)
    }

    func testDecodeCommitText_TooShort() {
        let buf = Data([0x00, 0x00])
        XCTAssertThrowsError(try BinaryCodec.decodeCommitTextPayload(buf)) { error in
            if case .payloadTooShort = error as? IPCError {} else { XCTFail("wrong: \(error)") }
        }
    }

    // MARK: - CmdUpdateComposition (0x0102)

    func testDecodeUpdateComposition_Roundtrip() throws {
        let text = "ni'hao"
        var buf = Data(count: 4)
        buf.writeUInt32LE(3, at: 0)   // caretPos = 3
        buf.append(contentsOf: text.utf8)

        let p = try BinaryCodec.decodeUpdateCompositionPayload(buf)
        XCTAssertEqual(p.caretPos, 3)
        XCTAssertEqual(p.text, text)
    }

    func testDecodeUpdateComposition_EmptyText() throws {
        var buf = Data(count: 4)
        buf.writeUInt32LE(0, at: 0)
        let p = try BinaryCodec.decodeUpdateCompositionPayload(buf)
        XCTAssertEqual(p.caretPos, 0)
        XCTAssertEqual(p.text, "")
    }

    // MARK: - CmdCommitTextWithCursor (0x0106)

    func testDecodeCommitTextWithCursor_Roundtrip() throws {
        let text = "abc"
        var buf = Data(count: 8)
        buf.writeUInt32LE(UInt32(text.utf8.count), at: 0)
        buf.writeUInt32LE(2, at: 4)    // cursorOffset = 2
        buf.append(contentsOf: text.utf8)

        let p = try BinaryCodec.decodeCommitTextWithCursorPayload(buf)
        XCTAssertEqual(p.text, "abc")
        XCTAssertEqual(p.cursorOffset, 2)
    }

    // MARK: - CmdMoveCursor (0x0107)

    func testDecodeMoveCursor_DirectionRight() throws {
        var buf = Data(count: 4)
        buf.writeUInt32LE(1, at: 0)
        let p = try BinaryCodec.decodeMoveCursorPayload(buf)
        XCTAssertEqual(p.direction, 1)
    }

    // MARK: - CmdStatePush (0x0206 push)

    func testDecodeStatePush_AllFlags() throws {
        let label = "中"   // 3 utf-8 bytes
        var buf = Data(count: 12)
        // 0x0001 ChineseMode | 0x0008 ToolbarVisible | 0x0020 CapsLock
        buf.writeUInt32LE(0x0001 | 0x0008 | 0x0020, at: 0)
        buf.writeUInt32LE(0, at: 4)
        buf.writeUInt32LE(0, at: 8)
        buf.append(contentsOf: label.utf8)

        let p = try BinaryCodec.decodeStatePushPayload(buf)
        XCTAssertEqual(p.iconLabel, "中")
        XCTAssertTrue(p.chineseMode)
        XCTAssertTrue(p.toolbarVisible)
        XCTAssertTrue(p.capsLock)
        XCTAssertFalse(p.fullWidth)
        XCTAssertFalse(p.chinesePunct)
    }

    // MARK: - CompositionState rune ↔ utf16

    func testCompositionState_CaretMapping_ASCII() {
        let s = CompositionState(text: "abc", caretRune: 2)
        XCTAssertEqual(s.caretInUTF16(), 2)
        XCTAssertEqual(s.utf16Length, 3)
    }

    func testCompositionState_CaretMapping_CJK() {
        // "你好" 每个字 = 1 rune = 1 UTF-16 unit (在 BMP 内)
        let s = CompositionState(text: "你好", caretRune: 1)
        XCTAssertEqual(s.caretInUTF16(), 1)
        XCTAssertEqual(s.utf16Length, 2)
    }

    func testCompositionState_CaretMapping_EmojiSurrogatePair() {
        // 😀 (U+1F600) = 1 rune = 2 UTF-16 unit
        let s = CompositionState(text: "😀a", caretRune: 1)   // 光标在 emoji 后
        XCTAssertEqual(s.caretInUTF16(), 2)   // emoji 占 2 unit
        XCTAssertEqual(s.utf16Length, 3)      // emoji 2 + a 1 = 3
    }

    func testCompositionState_Clear() {
        var s = CompositionState(text: "ni", caretRune: 2)
        s.clear()
        XCTAssertTrue(s.isEmpty)
        XCTAssertEqual(s.caretRune, 0)
    }
}
