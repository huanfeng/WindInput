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
        // caret_pos 已是 UTF-16 偏移, 原样落到 NSRange.location, 中间不换算。
        XCTAssertEqual(mock.setMarkedCalls[0].selectionRange.location, 2)
        XCTAssertEqual(r.composition.text, text)
    }

    func testApply_UpdateComposition_CJK_CaretMapping() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        let text = "你好"   // 均在 BMP 内, 2 个 utf16 unit
        var payload = Data(count: 4)
        payload.writeUInt32LE(1, at: 0)
        payload.append(contentsOf: text.utf8)
        let frame = Frame(cmd: DownstreamCmd.updateComposition, isAsync: false, payload: payload)

        _ = r.apply(frame, to: mock)
        XCTAssertEqual(mock.setMarkedCalls[0].selectionRange.location, 1)
    }

    /// `selectionRange.length` 必须恒为 0。
    ///
    /// IMKit 语境里这个 range 表达的是**「活动分句」**(日文分节转换要高亮正在转换的那一
    /// 节), 而我们要表达的是**插入点**。宿主 (`NSTextInputClient`) 见 `length > 0` 会把
    /// 整段当选中分句、把光标画到段首 —— 这正是 2026-09-02 那个「光标总在组合最前面」的
    /// 形态 (当时是 IMKit 替我们写成了 `{0, 全长}`, 见 `MarkedTextAttributes`)。
    /// 本仓自己更不能主动这么传。
    func testApply_UpdateComposition_SelectionLengthIsAlwaysZero() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        for (caret, text) in [(0, "s"), (2, "sf"), (3, "sfg"), (2, "你好")] {
            var payload = Data(count: 4)
            payload.writeUInt32LE(UInt32(caret), at: 0)
            payload.append(contentsOf: text.utf8)
            _ = r.apply(Frame(cmd: DownstreamCmd.updateComposition, isAsync: false,
                              payload: payload), to: mock)
        }
        for call in mock.setMarkedCalls {
            XCTAssertEqual(call.selectionRange.length, 0,
                           "marked=\(call.text): length 非 0 会被宿主当成选中分句")
        }
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

    /// issue #64: 宿主快捷键 (⌘C/⌃C…) 触发的清组合 —— 组合要清, 但按键必须交还宿主,
    /// 否则网页版 WPS/GitHub 里组字期间复制粘贴失灵。
    func testApply_ClearComposition_HostShortcut_ReturnsFalseButStillClears() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        r.applyUpdateComposition(.init(caretPos: 0, text: "ni"), client: mock)
        XCTAssertFalse(r.composition.isEmpty)

        let frame = Frame(cmd: DownstreamCmd.clearComposition, isAsync: false, payload: Data())
        XCTAssertFalse(r.apply(frame, to: mock, hostShortcut: true), "快捷键组合不得吃键")
        XCTAssertTrue(r.composition.isEmpty, "组合仍须清干净")
        XCTAssertEqual(mock.setMarkedCalls.last?.text, "")
    }

    // MARK: - ClearCompositionThenPassThrough

    /// 联想态回车/退格透传: 组合要清干净, 键要交还宿主 —— 与 `clearComposition` 的区别是
    /// **不看 hostShortcut**, 服务端已经显式声明了要交还。
    func testApply_ClearThenPassThrough_ClearsAndReturnsFalse() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        // 联想态挂的是占位组合, 用同样的方式先设上。
        r.applyUpdateComposition(.init(caretPos: 0, text: " "), client: mock)
        XCTAssertFalse(r.composition.isEmpty)

        let frame = Frame(cmd: DownstreamCmd.clearThenPassThrough, isAsync: false, payload: Data())
        XCTAssertFalse(r.apply(frame, to: mock), "键必须交还宿主 (回车要能换行/发送)")
        XCTAssertTrue(r.composition.isEmpty, "占位组合必须收干净, 否则悬在输入框里")
        XCTAssertEqual(mock.setMarkedCalls.last?.text, "")
    }

    /// 普通按键路径 (hostShortcut = false) 同样交还 —— 判据不该落在快捷键与否上。
    func testApply_ClearThenPassThrough_IgnoresHostShortcutFlag() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        r.applyUpdateComposition(.init(caretPos: 0, text: " "), client: mock)
        let frame = Frame(cmd: DownstreamCmd.clearThenPassThrough, isAsync: false, payload: Data())
        XCTAssertFalse(r.apply(frame, to: mock, hostShortcut: true), "两种取值都得交还")
        XCTAssertTrue(r.composition.isEmpty)
    }

    /// 同一帧在**非**快捷键路径 (Esc 取消) 下仍是消费, 不受上一条影响。
    func testApply_ClearComposition_NotShortcut_StillConsumes() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        r.applyUpdateComposition(.init(caretPos: 0, text: "ni"), client: mock)
        let frame = Frame(cmd: DownstreamCmd.clearComposition, isAsync: false, payload: Data())
        XCTAssertTrue(r.apply(frame, to: mock, hostShortcut: false))
    }

    /// 命中热键的组合走 Consumed/StatusUpdate 一路, hostShortcut 不该把它们降级。
    func testApply_Consumed_HostShortcut_StillConsumes() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        let frame = Frame(cmd: DownstreamCmd.consumed, isAsync: false, payload: Data())
        XCTAssertTrue(r.apply(frame, to: mock, hostShortcut: true))
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

    // MARK: - 延迟组合三兄弟 (0x010A/0x010B/0x010C)

    /// timeoutMs + commitLen + holdLen + commit + hold
    private func deferredPayload(timeout: UInt32, commit: String, hold: String) -> Data {
        var d = Data(count: 12)
        d.writeUInt32LE(timeout, at: 0)
        d.writeUInt32LE(UInt32(commit.utf8.count), at: 4)
        d.writeUInt32LE(UInt32(hold.utf8.count), at: 8)
        d.append(contentsOf: commit.utf8)
        d.append(contentsOf: hold.utf8)
        return d
    }

    /// 顶码 direct_commit 走 commitThenDefer: 必须真上屏, 且余码开成新组合。
    /// 回归钉子 —— 此前落在 router 的 default 分支, 按键被消费但一个字都不出。
    func testApply_CommitThenDefer_CommitsAndOpensDeferredComposition() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        let payload = deferredPayload(timeout: 300, commit: "好", hold: "vv")

        let consumed = r.apply(Frame(cmd: DownstreamCmd.commitThenDefer,
                                     isAsync: false, payload: payload), to: mock)

        XCTAssertTrue(consumed)
        XCTAssertEqual(mock.insertCalls.count, 1)
        XCTAssertEqual(mock.insertCalls[0].text, "好")
        XCTAssertEqual(mock.setMarkedCalls.map { $0.text }, ["vv"])
        XCTAssertEqual(r.composition.text, "vv")
    }

    /// 余码为空时只上屏, 不留空组合。
    func testApply_CommitThenDefer_EmptyDeferred_ClearsComposition() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        let payload = deferredPayload(timeout: 0, commit: "好", hold: "")

        _ = r.apply(Frame(cmd: DownstreamCmd.commitThenDefer, isAsync: false, payload: payload),
                    to: mock)

        XCTAssertEqual(mock.insertCalls.map { $0.text }, ["好"])
        XCTAssertTrue(mock.setMarkedCalls.isEmpty)
        XCTAssertTrue(r.composition.isEmpty)
    }

    /// 智能符号 HoldComposition press1: 待定标点作为组合预览, 不上屏。
    func testApply_HoldComposition_ShowsMarkedTextOnly() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        var d = Data(count: 8)
        d.writeUInt32LE(500, at: 0)
        d.writeUInt32LE(UInt32("，".utf8.count), at: 4)
        d.append(contentsOf: "，".utf8)

        let consumed = r.apply(Frame(cmd: DownstreamCmd.holdComposition,
                                     isAsync: false, payload: d), to: mock)

        XCTAssertTrue(consumed)
        XCTAssertTrue(mock.insertCalls.isEmpty)
        XCTAssertEqual(mock.setMarkedCalls.map { $0.text }, ["，"])
        XCTAssertEqual(r.composition.text, "，")
    }

    /// press2 的 CommitTextReplacingHeld 带 replacingHeld 位: 先清 held 预览再插入,
    /// 否则待定标点与替换结果会同时留在宿主里。
    func testApply_CommitTextReplacingHeld_ClearsMarkedBeforeInsert() {
        let r = BridgeResponseRouter()
        let mock = MockClient()

        // press1: hold "，"
        var hold = Data(count: 8)
        hold.writeUInt32LE(500, at: 0)
        hold.writeUInt32LE(UInt32("，".utf8.count), at: 4)
        hold.append(contentsOf: "，".utf8)
        _ = r.apply(Frame(cmd: DownstreamCmd.holdComposition, isAsync: false, payload: hold),
                    to: mock)

        // press2: 替换为 ","
        let text = ","
        var c = Data(count: 12)
        c.writeUInt32LE(BinaryCodec.commitFlagReplacingHeld, at: 0)
        c.writeUInt32LE(UInt32(text.utf8.count), at: 4)
        c.writeUInt32LE(0, at: 8)
        c.append(contentsOf: text.utf8)
        _ = r.apply(Frame(cmd: DownstreamCmd.commitText, isAsync: false, payload: c), to: mock)

        XCTAssertEqual(mock.setMarkedCalls.map { $0.text }, ["，", ""])
        XCTAssertEqual(mock.insertCalls.map { $0.text }, [","])
        XCTAssertTrue(r.composition.isEmpty)
    }

    // MARK: - 待定标点的收口 (HoldComposition → 前缀)

    private func holdFrame(_ text: String, timeoutMs: UInt32 = 500) -> Frame {
        var d = Data(count: 8)
        d.writeUInt32LE(timeoutMs, at: 0)
        d.writeUInt32LE(UInt32(text.utf8.count), at: 4)
        d.append(contentsOf: text.utf8)
        return Frame(cmd: DownstreamCmd.holdComposition, isAsync: false, payload: d)
    }

    /// UpdateComposition 载荷: caretPos u32 + 裸 UTF-8 文本 (无长度前缀)。
    private func updateCompFrame(_ text: String) -> Frame {
        var d = Data(count: 4)
        d.writeUInt32LE(UInt32(text.count), at: 0)
        d.append(contentsOf: text.utf8)
        return Frame(cmd: DownstreamCmd.updateComposition, isAsync: false, payload: d)
    }

    private func commitFrame(_ text: String, flags: UInt32 = 0) -> Frame {
        var c = Data(count: 12)
        c.writeUInt32LE(flags, at: 0)
        c.writeUInt32LE(UInt32(text.utf8.count), at: 4)
        c.writeUInt32LE(0, at: 8)
        c.append(contentsOf: text.utf8)
        return Frame(cmd: DownstreamCmd.commitText, isAsync: false, payload: c)
    }

    /// **回归**: 待定标点后接着输入, 标点被新组合覆盖而凭空消失。
    ///
    /// 服务端在下发 UpdateComposition 时已同步清掉自己的 held_text 并按上屏记过账
    /// (coordinator.rs::handle_key_event_policed), 它假定客户端此刻已把符号收口 ——
    /// Windows 侧确实调了 AbsorbHeldIntoPrefix。这边一度什么都没做。
    func testHeldSymbol_SurvivesFollowingComposition() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        _ = r.apply(holdFrame("，"), to: mock)
        _ = r.apply(updateCompFrame("n"), to: mock)

        XCTAssertEqual(mock.setMarkedCalls.map { $0.text }, ["，", "，n"],
                       "标点须与新组合显示在同一段 marked text 里, 而不是被覆盖")
        // 光标落在符号之后, 不能停在符号里面。
        XCTAssertEqual(mock.setMarkedCalls.last?.selectionRange.location,
                       ("，n" as NSString).length)
    }

    /// 组合最终上屏时, 定格的标点必须随之一起 insertText —— 一次收口, 不是两次。
    /// (Windows 走 `full = _pendingCommitPrefix + text`, 刻意避开 commit+重开组合:
    ///  那个模式在 WPS/微信下被误读成替换, 符号会被新输入顶掉。)
    func testHeldSymbol_IsIncludedInFinalCommit() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        _ = r.apply(holdFrame("，"), to: mock)
        _ = r.apply(updateCompFrame("ni"), to: mock)
        _ = r.apply(commitFrame("你"), to: mock)

        XCTAssertEqual(mock.insertCalls.map { $0.text }, ["，你"])
        XCTAssertTrue(r.composition.isEmpty)
    }

    /// 按键交回系统前必须先把待定标点真上屏: 之后没有任何一帧会再管它,
    /// 而服务端已经按上屏记了账。对位 Windows PassThrough 分支的 FlushHoldCompositionIfActive。
    func testHeldSymbol_FlushedOnPassThrough() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        _ = r.apply(holdFrame("，"), to: mock)

        let consumed = r.apply(Frame(cmd: DownstreamCmd.passThrough, isAsync: false, payload: Data()),
                               to: mock)

        XCTAssertFalse(consumed, "PassThrough 语义不变")
        XCTAssertEqual(mock.setMarkedCalls.map { $0.text }, ["，", ""])
        XCTAssertEqual(mock.insertCalls.map { $0.text }, ["，"])
    }

    /// 失焦 / 主动结束组合时**转为提交而非丢弃** —— 切窗口时符号应直接上屏, 与标准输入
    /// 流程一致 (Windows EndComposition 同此)。丢了就是用户凭空少一个字。
    func testHeldSymbol_CommittedOnClearComposition() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        _ = r.apply(holdFrame("，"), to: mock)
        _ = r.apply(Frame(cmd: DownstreamCmd.clearComposition, isAsync: false, payload: Data()),
                    to: mock)

        XCTAssertEqual(mock.insertCalls.map { $0.text }, ["，"])
        XCTAssertTrue(r.composition.isEmpty)
    }

    /// 普通组合(无待定标点)被清掉时不该凭空插入任何东西。
    func testClearComposition_WithoutHeld_InsertsNothing() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        _ = r.apply(updateCompFrame("ni"), to: mock)
        _ = r.apply(Frame(cmd: DownstreamCmd.clearComposition, isAsync: false, payload: Data()),
                    to: mock)

        XCTAssertTrue(mock.insertCalls.isEmpty)
    }

    /// press2 (replacingHeld) 要的是**替换**: 待定符号丢弃, 不许并进前缀跟着一起上屏。
    func testReplacingHeld_DiscardsHeldInsteadOfPrefixing() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        _ = r.apply(holdFrame("，"), to: mock)
        _ = r.apply(commitFrame(",", flags: BinaryCodec.commitFlagReplacingHeld), to: mock)

        XCTAssertEqual(mock.insertCalls.map { $0.text }, [","], "不能出现「，,」")
    }

    /// 连打两个待定标点: 前一个定格进前缀, 两个都要保住。
    func testTwoHeldSymbols_BothSurvive() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        _ = r.apply(holdFrame("，"), to: mock)
        _ = r.apply(holdFrame("。"), to: mock)

        XCTAssertEqual(mock.setMarkedCalls.map { $0.text }, ["，", "，。"])

        _ = r.apply(Frame(cmd: DownstreamCmd.passThrough, isAsync: false, payload: Data()), to: mock)
        XCTAssertEqual(mock.insertCalls.map { $0.text }, ["，。"])
    }

    /// 转一会儿 runloop 让 Timer 有机会烧。
    private func spinRunLoop(_ seconds: TimeInterval) {
        RunLoop.current.run(until: Date().addingTimeInterval(seconds))
    }

    /// 待定标点到点自动定稿：内容不变，只是从 marked text 变成正文。
    ///
    /// 不加这个计时器，用户打完一个「，」就会看到它一直带着下划线待在文档里，直到下一次
    /// 按键或失焦才收掉（Windows 侧靠同名计时器收；那边是 TSF 刚需，这边是观感）。
    func testHeldSymbol_AutoCommitsAfterTimeout() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        _ = r.apply(holdFrame("，", timeoutMs: 50), to: mock)
        XCTAssertTrue(mock.insertCalls.isEmpty, "到点前不该上屏")

        spinRunLoop(0.4)

        XCTAssertEqual(mock.insertCalls.map { $0.text }, ["，"], "到点应定稿为正文")
        XCTAssertTrue(r.composition.isEmpty)
    }

    /// 期间有过按键 → 那条路自己已经处置过，过期计时器**不得**再动手。
    /// 漏了这道「代」判据的话，会在新一轮输入里凭空多插一个符号。
    func testHeldSymbol_TimerDoesNotFireAfterStateMoved() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        _ = r.apply(holdFrame("，", timeoutMs: 50), to: mock)
        _ = r.apply(updateCompFrame("n"), to: mock)   // 符号定格进前缀, 停表

        spinRunLoop(0.4)

        XCTAssertTrue(mock.insertCalls.isEmpty, "组合还活着, 不该有任何上屏")
        XCTAssertEqual(mock.setMarkedCalls.last?.text, "，n", "符号仍与组合同段显示")
    }

    /// timeout=0 表示不自动落定（服务端可据配置下发 0）。
    func testHeldSymbol_ZeroTimeoutNeverAutoCommits() {
        let r = BridgeResponseRouter()
        let mock = MockClient()
        _ = r.apply(holdFrame("，", timeoutMs: 0), to: mock)

        spinRunLoop(0.3)

        XCTAssertTrue(mock.insertCalls.isEmpty)
    }
}
