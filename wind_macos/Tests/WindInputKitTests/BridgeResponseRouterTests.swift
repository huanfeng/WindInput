import XCTest
@testable import WindInputKit

/// BridgeResponseRouter 单测: 用 InMemoryTextInputClient 实现 TextInputClient 协议,
/// 记录调用历史, 验证 router 对各种 bridge 响应帧的正确路由.
final class BridgeResponseRouterTests: XCTestCase {

    // MARK: - Mock TextInputClient

    final class MockClient: TextInputClient {
        struct InsertCall: Equatable {
            let text: String
            let replacementRange: NSRange
        }
        struct SetMarkedCall: Equatable {
            let text: String
            let selectionRange: NSRange
            let replacementRange: NSRange
        }
        private(set) var insertCalls: [InsertCall] = []
        private(set) var setMarkedCalls: [SetMarkedCall] = []

        func insertText(_ text: String, replacementRange: NSRange) {
            insertCalls.append(InsertCall(text: text, replacementRange: replacementRange))
        }
        func setMarkedText(_ text: String,
                           selectionRange: NSRange,
                           replacementRange: NSRange) {
            setMarkedCalls.append(SetMarkedCall(text: text,
                                                selectionRange: selectionRange,
                                                replacementRange: replacementRange))
        }
    }

    private static let notFound = NSRange(location: NSNotFound, length: NSNotFound)

    // MARK: - 控制流

    func testApply_PassThrough_ReturnsFalse() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        let frame = Frame(cmd: DownstreamCmd.passThrough, isAsync: false, payload: Data())
        XCTAssertFalse(r.apply(frame, to: mock))
        XCTAssertTrue(mock.insertCalls.isEmpty)
        XCTAssertTrue(mock.setMarkedCalls.isEmpty)
    }

    func testApply_Consumed_ReturnsTrue() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        let frame = Frame(cmd: DownstreamCmd.consumed, isAsync: false, payload: Data())
        XCTAssertTrue(r.apply(frame, to: mock))
        XCTAssertTrue(mock.insertCalls.isEmpty)
    }

    func testApply_Ack_ReturnsTrue() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        let frame = Frame(cmd: DownstreamCmd.ack, isAsync: false, payload: Data())
        XCTAssertTrue(r.apply(frame, to: mock))
    }

    func testApply_UnknownCmd_DefaultsToConsumed() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        let frame = Frame(cmd: 0xABCD, isAsync: false, payload: Data())
        XCTAssertTrue(r.apply(frame, to: mock))
        XCTAssertTrue(mock.insertCalls.isEmpty)
    }

    // MARK: - CommitText

    func testApply_CommitText_CallsInsertText() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        // flags(0) + textLen(6) + compLen(0) + "你好"
        var payload = Data(count: 12)
        let text = "你好"
        payload.writeUInt32LE(0, at: 0)
        payload.writeUInt32LE(UInt32(text.utf8.count), at: 4)
        payload.writeUInt32LE(0, at: 8)
        payload.append(contentsOf: text.utf8)
        let frame = Frame(cmd: DownstreamCmd.commitText, isAsync: false, payload: payload)

        XCTAssertTrue(r.apply(frame, to: mock))
        XCTAssertEqual(mock.insertCalls.count, 1)
        XCTAssertEqual(mock.insertCalls[0].text, "你好")
        XCTAssertEqual(mock.insertCalls[0].replacementRange.location, NSNotFound)
        XCTAssertTrue(r.composition.isEmpty)
    }

    func testApply_CommitText_WithNewComposition_AlsoSetsMarked() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        let text = "你好"
        let comp = "hao"
        var payload = Data(count: 12)
        payload.writeUInt32LE(0, at: 0)
        payload.writeUInt32LE(UInt32(text.utf8.count), at: 4)
        payload.writeUInt32LE(UInt32(comp.utf8.count), at: 8)
        payload.append(contentsOf: text.utf8)
        payload.append(contentsOf: comp.utf8)
        let frame = Frame(cmd: DownstreamCmd.commitText, isAsync: false, payload: payload)

        XCTAssertTrue(r.apply(frame, to: mock))
        // commit + 新一轮 marked: 一次 insert + 一次 setMarked
        XCTAssertEqual(mock.insertCalls.count, 1)
        XCTAssertEqual(mock.insertCalls[0].text, "你好")
        XCTAssertEqual(mock.setMarkedCalls.count, 1)
        XCTAssertEqual(mock.setMarkedCalls[0].text, "hao")
        XCTAssertEqual(r.composition.text, "hao")
    }

    // MARK: - UpdateComposition

    func testApply_UpdateComposition_CallsSetMarkedWithCaret() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        let text = "ni'hao"
        var payload = Data(count: 4)
        payload.writeUInt32LE(2, at: 0)   // caretPos = 2
        payload.append(contentsOf: text.utf8)
        let frame = Frame(cmd: DownstreamCmd.updateComposition, isAsync: false, payload: payload)

        XCTAssertTrue(r.apply(frame, to: mock))
        XCTAssertEqual(mock.setMarkedCalls.count, 1)
        XCTAssertEqual(mock.setMarkedCalls[0].text, text)
        // text 全 ASCII, rune index 2 → utf16 index 2
        XCTAssertEqual(mock.setMarkedCalls[0].selectionRange.location, 2)
        XCTAssertEqual(r.composition.text, text)
    }

    func testApply_UpdateComposition_CJK_CaretMapping() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        let text = "你好"   // 2 rune, 2 utf16 unit (BMP)
        var payload = Data(count: 4)
        payload.writeUInt32LE(1, at: 0)
        payload.append(contentsOf: text.utf8)
        let frame = Frame(cmd: DownstreamCmd.updateComposition, isAsync: false, payload: payload)

        _ = r.apply(frame, to: mock)
        XCTAssertEqual(mock.setMarkedCalls[0].selectionRange.location, 1)
    }

    // MARK: - ClearComposition

    func testApply_ClearComposition_SetsEmptyAndResetsState() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        // 先设有 composition
        r.applyUpdateComposition(.init(caretPos: 0, text: "abc"), client: mock)
        XCTAssertFalse(r.composition.isEmpty)

        let frame = Frame(cmd: DownstreamCmd.clearComposition, isAsync: false, payload: Data())
        XCTAssertTrue(r.apply(frame, to: mock))
        XCTAssertTrue(r.composition.isEmpty)
        // 最后一次 setMarked 应该是空字符串清 preedit
        XCTAssertEqual(mock.setMarkedCalls.last?.text, "")
    }

    // MARK: - CommitTextWithCursor

    func testApply_CommitTextWithCursor_CallsInsert() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        let text = "abc"
        var payload = Data(count: 8)
        payload.writeUInt32LE(UInt32(text.utf8.count), at: 0)
        payload.writeUInt32LE(2, at: 4)
        payload.append(contentsOf: text.utf8)
        let frame = Frame(cmd: DownstreamCmd.commitTextWithCursor, isAsync: false, payload: payload)

        XCTAssertTrue(r.apply(frame, to: mock))
        XCTAssertEqual(mock.insertCalls.count, 1)
        XCTAssertEqual(mock.insertCalls[0].text, "abc")
        XCTAssertTrue(r.composition.isEmpty)
    }

    // MARK: - 状态完整生命周期 (update → update → commit)

    func testApply_FullLifecycle() {
        let r = BridgeResponseRouter()
        let mock = MockClient()

        // 1. 第一次更新 composition: 推 "n"
        var u1 = Data(count: 4)
        u1.writeUInt32LE(1, at: 0)
        u1.append(contentsOf: "n".utf8)
        _ = r.apply(Frame(cmd: DownstreamCmd.updateComposition, isAsync: false, payload: u1),
                    to: mock)

        // 2. 继续推 "ni"
        var u2 = Data(count: 4)
        u2.writeUInt32LE(2, at: 0)
        u2.append(contentsOf: "ni".utf8)
        _ = r.apply(Frame(cmd: DownstreamCmd.updateComposition, isAsync: false, payload: u2),
                    to: mock)

        // 3. commit "你"
        let commitText = "你"
        var c = Data(count: 12)
        c.writeUInt32LE(0, at: 0)
        c.writeUInt32LE(UInt32(commitText.utf8.count), at: 4)
        c.writeUInt32LE(0, at: 8)
        c.append(contentsOf: commitText.utf8)
        _ = r.apply(Frame(cmd: DownstreamCmd.commitText, isAsync: false, payload: c),
                    to: mock)

        XCTAssertEqual(mock.setMarkedCalls.count, 2)
        XCTAssertEqual(mock.setMarkedCalls.map { $0.text }, ["n", "ni"])
        XCTAssertEqual(mock.insertCalls.count, 1)
        XCTAssertEqual(mock.insertCalls[0].text, "你")
        XCTAssertTrue(r.composition.isEmpty)
    }
}
