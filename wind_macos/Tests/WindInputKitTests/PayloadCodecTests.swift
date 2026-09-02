import XCTest
import AppKit   // NSAttributedString.Key.markedClauseSegment / .underlineStyle
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

    // MARK: - CompositionState: caret 恒为 UTF-16 单元, 两端同单位不换算

    func testCompositionState_CaretMapping_ASCII() {
        let s = CompositionState(text: "abc", caretUTF16: 2)
        XCTAssertEqual(s.caretInUTF16(), 2)
        XCTAssertEqual(s.utf16Length, 3)
    }

    func testCompositionState_CaretMapping_CJK() {
        // "你好" 每个字在 BMP 内 = 1 UTF-16 unit
        let s = CompositionState(text: "你好", caretUTF16: 1)
        XCTAssertEqual(s.caretInUTF16(), 1)
        XCTAssertEqual(s.utf16Length, 2)
    }

    /// ★ 这条是旧实现真正错的地方 —— 也是「用中文怎么测都测不出来」的那一格。
    ///
    /// 组合区前缀含扩展 B 区汉字 (生僻字候选上屏后作为已转换前缀) 且光标**不在串尾**时:
    /// 服务端按 UTF-16 给 2 (「𠮷」占 2 个单元), 旧实现把这个 2 当成「前 2 个**字符**」
    /// 再折算, 得出 3 —— 光标凭空右移一格。光标在串尾时被 min 钳住恰好蒙对, 所以它一直没
    /// 暴露。
    func testCompositionState_CaretIsUTF16NotCharacterIndex() {
        let s = CompositionState(text: "𠮷zh", caretUTF16: 2)   // 𠮷 = U+20BB7, 占 2 个单元
        XCTAssertEqual(s.utf16Length, 4)
        XCTAssertEqual(s.caretInUTF16(), 2,
                       "caret 已是 UTF-16 偏移, 不得再按字符数折算 (那样会得到 3)")
    }

    /// 越界一律退到**串尾**而非 0: 组合期的编辑点绝大多数时候就在末尾, 退到 0 会让光标
    /// 停在刚打出的字母之前, 比没有光标更让人困惑。
    func testCompositionState_CaretClampsToTailNotZero() {
        XCTAssertEqual(CompositionState(text: "ab", caretUTF16: 99).caretInUTF16(), 2)
        XCTAssertEqual(CompositionState(text: "ab", caretUTF16: -1).caretInUTF16(), 0)
    }

    func testCompositionState_Clear() {
        var s = CompositionState(text: "ni", caretUTF16: 2)
        s.clear()
        XCTAssertTrue(s.isEmpty)
        XCTAssertEqual(s.caretUTF16, 0)
    }

    // MARK: - MarkedTextAttributes (组合串属性: selectionRange 能否活着到宿主)

    /// 少了 `markedClauseSegment`, IMKit 会替我们合成默认分句并把 selectionRange 覆写成
    /// `{0, 全长}`, 宿主于是把组合内光标画在最前面。这是真机才看得出的缺陷, 故在此守门。
    func testMarkedTextAttributes_AlwaysDeclaresClauseSegment() {
        XCTAssertTrue(MarkedTextAttributes.declaresClauseSegment(
            MarkedTextAttributes.ensureClauseSegment()),
            "controller 拿不到时也必须自带分句声明")
        XCTAssertTrue(MarkedTextAttributes.declaresClauseSegment(
            MarkedTextAttributes.ensureClauseSegment([.underlineStyle: 9])),
            "markForStyle 返回的字典若不含分句声明, 必须补上")
    }

    /// 兜底不得覆盖 `markForStyle:` 已给出的取值 —— 那是系统按当前主题算的。
    func testMarkedTextAttributes_PreservesBaseValues() {
        let attrs = MarkedTextAttributes.ensureClauseSegment([
            .markedClauseSegment: 3,
            .underlineStyle: 9,
        ])
        XCTAssertEqual(attrs[.markedClauseSegment] as? Int, 3)
        XCTAssertEqual(attrs[.underlineStyle] as? Int, 9)
    }
}
