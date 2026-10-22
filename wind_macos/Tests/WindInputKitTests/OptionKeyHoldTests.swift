import XCTest
@testable import WindInputKit

final class OptionKeyHoldTests: XCTestCase {

    /// 按住 ⌥ 期间按过 Shift 又先松开: 松开 ⌥ 时仍带 Shift (⌥+Shift 不算单击)。
    func testStickyModifierSurvivesRelease() {
        var h = OptionKeyHold()
        h.down(forwarded: true)
        h.note(modifiers: 0x0004 | 0x0001)
        h.note(modifiers: 0x0004)
        XCTAssertEqual(h.stickyMods, 0x0001)
    }

    func testOptionBitIsNotOther() {
        var h = OptionKeyHold()
        h.down(forwarded: true)
        h.note(modifiers: 0x0004 | 0x0008)
        XCTAssertEqual(h.stickyMods, 0x0008, "Command 记 Win 位, ⌥ 位不算")
    }

    func testModifiersBeforeDownIgnoredAndUpResets() {
        var h = OptionKeyHold()
        h.note(modifiers: 0x0002)
        h.down(forwarded: true)
        XCTAssertEqual(h.stickyMods, 0)
        h.note(modifiers: 0x0002)
        h.up()
        XCTAssertEqual(h.stickyMods, 0)
        XCTAssertFalse(h.shouldForwardUp(composing: false))
    }

    /// 按下转发过, 期间上屏关了组字: 松开照样转发 (按住翻转要复原)。
    func testUpForwardedAfterForwardedDown() {
        var h = OptionKeyHold()
        h.down(forwarded: true)
        XCTAssertTrue(h.shouldForwardUp(composing: false))

        var idle = OptionKeyHold()
        idle.down(forwarded: false)
        XCTAssertFalse(idle.shouldForwardUp(composing: false))
        XCTAssertTrue(idle.shouldForwardUp(composing: true))
    }
}
