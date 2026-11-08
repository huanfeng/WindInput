import XCTest
@testable import WindInputKit

/// `seqChar` 记账单测。取值规则对齐 Windows `wind_tsf/include/InputSequenceChar.h`
/// （那边有 `tests/input_sequence_char_test.cpp` 钉同一张表）。
final class InputSequenceTrackerTests: XCTestCase {

    func testPassthroughKeysReportClassRepresentative() {
        XCTAssertEqual(InputSequenceTracker.charFromVk(vk: 0x48, modifiers: 0), 0x61)       // H → 'a'
        XCTAssertEqual(InputSequenceTracker.charFromVk(vk: 0x37, modifiers: 0), 0x37)       // '7'
        XCTAssertEqual(InputSequenceTracker.charFromVk(vk: 0x63, modifiers: 0), 0x33)       // 小键盘 3
        XCTAssertEqual(InputSequenceTracker.charFromVk(vk: 0x31, modifiers: 0x0001), 0x3F)  // Shift+1 = '!'
        XCTAssertEqual(InputSequenceTracker.charFromVk(vk: 0x20, modifiers: 0), 0x20)       // 空格
        XCTAssertEqual(InputSequenceTracker.charFromVk(vk: 0xBE, modifiers: 0), 0x3F)       // .
    }

    func testEditingKeysAndShortcutsAreUnknown() {
        let u = InputSequenceTracker.unknown
        XCTAssertEqual(InputSequenceTracker.charFromVk(vk: 0x08, modifiers: 0), u)          // Backspace
        XCTAssertEqual(InputSequenceTracker.charFromVk(vk: 0x25, modifiers: 0), u)          // ←
        XCTAssertEqual(InputSequenceTracker.charFromVk(vk: 0x56, modifiers: 0x0008), u)     // ⌘V
    }

    func testCommittedTextTail() {
        let t = InputSequenceTracker()
        t.noteCommittedText("你好")
        XCTAssertEqual(t.seqChar, 0x597D)
        t.noteCommittedText("𠀀")   // U+20000：取高代理
        XCTAssertEqual(t.seqChar, 0xD840)
        t.noteCommittedText("")      // 空文本不动状态
        XCTAssertEqual(t.seqChar, 0xD840)
        t.reset()
        XCTAssertEqual(t.seqChar, InputSequenceTracker.unknown)
    }

    /// 记下之后点过鼠标：报「不知道」；下一次记账起按新计数重新算。
    func testMouseClickAfterRecordReportsUnknown() {
        let t = InputSequenceTracker()
        var clicks: UInt32 = 5
        t.mouseDownCount = { clicks }
        t.noteCommittedText("你")
        XCTAssertEqual(t.seqChar, 0x4F60)
        clicks += 1                       // 用户点了别处
        XCTAssertEqual(t.seqChar, InputSequenceTracker.unknown)
        t.noteKeyPassthrough(vk: 0x33, modifiers: 0)
        XCTAssertEqual(t.seqChar, 0x33, "点击之后的记账照常")
        // 点选候选：点击先于上屏，上屏记账时已计入，不误判。
        clicks += 1
        t.noteCommittedText("好")
        XCTAssertEqual(t.seqChar, 0x597D)
    }

    /// 组字更新不动 seqChar：Esc 取消组字后光标前仍是组字前那个字符。
    func testRouterUpdateComposition_KeepsSeqChar() {
        let r = BridgeResponseRouter()
        let mock = BridgeResponseRouterTests.MockClient()
        r.applyCommitText(BinaryCodec.CommitTextPayload(flags: 0, text: "好", newComposition: ""),
                          client: mock)
        r.applyUpdateComposition(BinaryCodec.UpdateCompositionPayload(caretPos: 2, text: "wo"),
                                 client: mock)
        XCTAssertEqual(r.seqTracker.seqChar, 0x597D)
    }
}
