import XCTest
@testable import WindInputKit

final class SwitchInGateTests: XCTestCase {
    func testNoSwitchNoActivation() {
        // 只切文本框 / 切应用（没有输入源切换通知）：不补发。
        var g = SwitchInGate()
        XCTAssertFalse(g.consume())
    }

    func testSwitchInIsConsumedOnce() {
        // 从别家切回本输入法：第一次激活补发，同应用内再切框不再补发。
        var g = SwitchInGate()
        g.selected(isSelf: true)
        XCTAssertTrue(g.consume())
        XCTAssertFalse(g.consume())
    }

    func testSwitchAwayCancelsPending() {
        // 切入后没等到激活就又切走：标记作废，不把旧的切入带到之后。
        var g = SwitchInGate()
        g.selected(isSelf: true)
        g.selected(isSelf: false)
        XCTAssertFalse(g.consume())
    }

    func testSwitchInAgainAfterConsume() {
        var g = SwitchInGate()
        g.selected(isSelf: true)
        XCTAssertTrue(g.consume())
        g.selected(isSelf: false)
        g.selected(isSelf: true)
        XCTAssertTrue(g.consume())
    }

    func testRepeatedNotificationForSameSelectionFiresOnce() {
        // 系统一次切换连发两条通知（实测间隔 7ms）：只补发一次。
        var g = SwitchInGate()
        g.selected(isSelf: false)
        g.selected(isSelf: true)
        XCTAssertTrue(g.consume())
        g.selected(isSelf: true)
        XCTAssertFalse(g.consume())
    }
}
