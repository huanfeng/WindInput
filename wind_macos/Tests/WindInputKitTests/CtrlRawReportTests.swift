import XCTest
@testable import WindInputKit

final class CtrlRawReportTests: XCTestCase {

    func testComposingPressReportsDownAndCleanUp() {
        var r = CtrlRawReport()
        XCTAssertTrue(r.down(vk: 0xA2, composing: true))
        XCTAssertEqual(r.up(vk: 0xA2), false, "干净松开: 报 modUp, 不带 notTap")
        XCTAssertNil(r.vk)
    }

    /// 配对: 按下时不在组字 → 整次走旧路 (松开不报 modUp)。
    func testIdlePressStaysLegacy() {
        var r = CtrlRawReport()
        XCTAssertFalse(r.down(vk: 0xA2, composing: false))
        XCTAssertNil(r.up(vk: 0xA2))
    }

    /// 配对: 按下时在组字, 松开时已上屏关窗也照报。
    func testUpReportedEvenAfterWindowClosed() {
        var r = CtrlRawReport()
        _ = r.down(vk: 0xA3, composing: true)
        XCTAssertNotNil(r.up(vk: 0xA3))
    }

    func testOtherKeyMarksNotTap() {
        var r = CtrlRawReport()
        _ = r.down(vk: 0xA2, composing: true)
        r.noteOther() // Ctrl+C
        XCTAssertEqual(r.up(vk: 0xA2), true)
    }

    func testOtherSideIsNotOurs() {
        var r = CtrlRawReport()
        _ = r.down(vk: 0xA2, composing: true)
        XCTAssertNil(r.up(vk: 0xA3), "另一侧的松开不算")
        XCTAssertFalse(r.down(vk: 0xA3, composing: false))
        XCTAssertEqual(r.up(vk: 0xA2), true, "另一侧按下弄脏这次按住")
    }

    func testNoteOtherWithoutPressIsNoop() {
        var r = CtrlRawReport()
        r.noteOther()
        XCTAssertFalse(r.dirty)
    }

    func testNotTapBitMatchesProtocol() {
        XCTAssertEqual(CtrlRawReport.notTapBit, 0x10)
        XCTAssertEqual(KeyEventType.modDown.rawValue, 2)
        XCTAssertEqual(KeyEventType.modUp.rawValue, 3)
    }
}
