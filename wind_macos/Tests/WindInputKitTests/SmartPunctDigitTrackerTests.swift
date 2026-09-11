import XCTest
@testable import WindInputKit

/// 数字后智能标点的备用 prevChar 通路单测。
///
/// 语义对齐 Windows `_DigitCharFromVk` / `_TrackCommittedTextForSmartPunct`
/// （见 wind_tsf/include/KeyEventSink.h）：记「已落进文档、光标紧邻的那个字符」，
/// 是 ASCII 数字则记、否则清零。
final class SmartPunctDigitTrackerTests: XCTestCase {

    // MARK: - 按键侧记录点

    func testPassthroughMainDigit_Recorded() {
        let t = SmartPunctDigitTracker()
        t.noteKeyPassthrough(vk: 0x33, modifiers: 0)   // '3'
        XCTAssertEqual(t.prevChar, 0x33)
    }

    func testPassthroughNumpadDigit_Recorded() {
        let t = SmartPunctDigitTracker()
        t.noteKeyPassthrough(vk: 0x63, modifiers: 0)   // VK_NUMPAD3
        XCTAssertEqual(t.prevChar, 0x33, "小键盘数字须折算成 ASCII '3'")
    }

    func testPassthroughShiftDigit_NotRecorded() {
        let t = SmartPunctDigitTracker()
        t.noteKeyPassthrough(vk: 0x33, modifiers: 0x0001)  // Shift+3 = '#'
        XCTAssertEqual(t.prevChar, 0)
    }

    func testPassthroughNonDigit_Clears() {
        let t = SmartPunctDigitTracker()
        t.noteKeyPassthrough(vk: 0x33, modifiers: 0)
        t.noteKeyPassthrough(vk: 0x41, modifiers: 0)   // 'A'
        XCTAssertEqual(t.prevChar, 0)
    }

    /// 用户要的语义：删掉数字后再打标点，应回落中文标点。
    /// 退格透传即「非数字键」，天然清零 —— 不为它写特判。
    func testPassthroughBackspace_Clears() {
        let t = SmartPunctDigitTracker()
        t.noteKeyPassthrough(vk: 0x33, modifiers: 0)
        t.noteKeyPassthrough(vk: 0x08, modifiers: 0)   // VK_BACK
        XCTAssertEqual(t.prevChar, 0)
    }

    /// ⌘/⌃/⌥ + 数字**不进文档**（⌘2 切标签页、⌃3 切桌面…）。IMKit 把这类键也交给
    /// `handle`，服务端未命中热键时照样回 PassThrough，于是它会走到记录点——记了就是
    /// 一个幻影数字，随后打标点错出半角。Windows 侧没这条守卫是因为 TSF 的
    /// `OnTestKeyDown` 在记录点之前就滤掉了这类键，IMKit 不滤。
    func testPassthroughCommandDigit_NotRecorded() {
        let t = SmartPunctDigitTracker()
        t.noteKeyPassthrough(vk: 0x32, modifiers: 0x0008)   // ⌘2
        XCTAssertEqual(t.prevChar, 0)
        t.noteKeyPassthrough(vk: 0x33, modifiers: 0x0002)   // ⌃3
        XCTAssertEqual(t.prevChar, 0)
        t.noteKeyPassthrough(vk: 0x62, modifiers: 0x0004)   // ⌥ + 小键盘2
        XCTAssertEqual(t.prevChar, 0)
    }

    // MARK: - 上屏侧记录点

    func testCommittedTextEndingInDigit_Recorded() {
        let t = SmartPunctDigitTracker()
        t.noteCommittedText("2024")
        XCTAssertEqual(t.prevChar, 0x34)
    }

    func testCommittedTextEndingInHan_Clears() {
        let t = SmartPunctDigitTracker()
        t.noteKeyPassthrough(vk: 0x33, modifiers: 0)
        t.noteCommittedText("你好")
        XCTAssertEqual(t.prevChar, 0)
    }

    /// 全角数字刻意不记：服务端只认 ASCII 0x30-0x39，记了也不命中，徒增误判面。
    func testCommittedFullWidthDigit_Clears() {
        let t = SmartPunctDigitTracker()
        t.noteKeyPassthrough(vk: 0x33, modifiers: 0)
        t.noteCommittedText("３")
        XCTAssertEqual(t.prevChar, 0)
    }

    /// 空文本没有东西写进文档，光标前是什么并没有改变 → 不动状态。
    func testCommittedEmptyText_KeepsState() {
        let t = SmartPunctDigitTracker()
        t.noteKeyPassthrough(vk: 0x33, modifiers: 0)
        t.noteCommittedText("")
        XCTAssertEqual(t.prevChar, 0x33)
    }

    func testReset_Clears() {
        let t = SmartPunctDigitTracker()
        t.noteKeyPassthrough(vk: 0x33, modifiers: 0)
        t.reset()
        XCTAssertEqual(t.prevChar, 0)
    }

    // MARK: - 与 router 的联动

    func testRouterCommitText_UpdatesTracker() {
        let r = BridgeResponseRouter()
        let mock = BridgeResponseRouterTests.MockClient()
        r.digitTracker.noteKeyPassthrough(vk: 0x33, modifiers: 0)
        r.applyCommitText(BinaryCodec.CommitTextPayload(flags: 0, text: "好", newComposition: ""),
                          client: mock)
        XCTAssertEqual(r.digitTracker.prevChar, 0, "上屏汉字后不该再继承数字状态")
    }

    /// 智能符号 press1 走 HoldComposition：符号只进 marked text、不 insertText。
    /// 不记账的话 tracker 残留数字 → press2 的 prev_char 比对失配，连按替换静默失效。
    func testRouterHoldComposition_ClearsTracker() {
        let r = BridgeResponseRouter()
        let mock = BridgeResponseRouterTests.MockClient()
        r.digitTracker.noteKeyPassthrough(vk: 0x33, modifiers: 0)
        // HoldComposition 载荷: timeoutMs u32 + textLen u32 + UTF-8 文本。
        var d = Data(count: 8)
        d.writeUInt32LE(0, at: 0)                       // 0 = 不自动落定 (测试里不起表)
        d.writeUInt32LE(UInt32("，".utf8.count), at: 4)
        d.append(contentsOf: "，".utf8)
        _ = r.apply(Frame(cmd: DownstreamCmd.holdComposition, isAsync: false, payload: d), to: mock)
        XCTAssertEqual(r.digitTracker.prevChar, 0)
    }

    /// 组字中光标前是编码末位，不再是先前那个数字。
    func testRouterUpdateComposition_ClearsTracker() {
        let r = BridgeResponseRouter()
        let mock = BridgeResponseRouterTests.MockClient()
        r.digitTracker.noteKeyPassthrough(vk: 0x33, modifiers: 0)
        r.applyUpdateComposition(BinaryCodec.UpdateCompositionPayload(caretPos: 2, text: "wo"),
                                 client: mock)
        XCTAssertEqual(r.digitTracker.prevChar, 0)
    }

    /// 智能跳过：合成右方向键跨过已补全的右标点，光标前字符变成 `）`，但这条路不经
    /// `insertText`。不清账的话 `（3）` 之后打 `。` 会拿着陈旧的 `3` 错出半角。
    func testRouterMoveCursor_ClearsTracker() {
        let r = BridgeResponseRouter()
        let mock = BridgeResponseRouterTests.MockClient()
        r.moveHostCursor = { _ in }
        r.digitTracker.noteKeyPassthrough(vk: 0x33, modifiers: 0)
        var d = Data(count: 4)
        d.writeUInt32LE(1, at: 0)   // direction=1 右移
        _ = r.apply(Frame(cmd: DownstreamCmd.moveCursor, isAsync: false, payload: d), to: mock)
        XCTAssertEqual(r.digitTracker.prevChar, 0)
    }

    /// `ReplaceBackward{text: ""}` 是**纯删除**（命令直通车 `ime.undo_commit`）：文档确实
    /// 变了，不能按「空文本＝没写东西」放过。
    func testRouterReplaceBackwardEmptyText_ClearsTracker() {
        let r = BridgeResponseRouter()
        let mock = BridgeResponseRouterTests.MockClient()
        r.digitTracker.noteCommittedText("第3")
        XCTAssertEqual(r.digitTracker.prevChar, 0x33)
        var d = Data(count: 8)
        d.writeUInt32LE(2, at: 0)   // count=2
        d.writeUInt32LE(0, at: 4)   // textLen=0
        _ = r.apply(Frame(cmd: DownstreamCmd.replaceBackward, isAsync: false, payload: d), to: mock)
        XCTAssertEqual(r.digitTracker.prevChar, 0)
    }

    /// 组字中把光标移到编码串**中间**：串尾那个字符不再是光标前字符。码表方案可以把
    /// `0-9` 配成码元（`a-z0-9` 打 `Win10`），所以这不是纯理论。拿不准就清零。
    func testRouterMarkedTextCaretNotAtEnd_ClearsTracker() {
        let r = BridgeResponseRouter()
        let mock = BridgeResponseRouterTests.MockClient()
        r.digitTracker.noteKeyPassthrough(vk: 0x33, modifiers: 0)
        // caretPos=1，串是 "10"：光标在 '1' 之后、'0' 之前，串尾的 '0' 不是光标前字符。
        r.applyUpdateComposition(BinaryCodec.UpdateCompositionPayload(caretPos: 1, text: "10"),
                                 client: mock)
        XCTAssertEqual(r.digitTracker.prevChar, 0)
    }

    func testRouterCommitText_DigitTail_Recorded() {
        let r = BridgeResponseRouter()
        let mock = BridgeResponseRouterTests.MockClient()
        r.applyCommitText(BinaryCodec.CommitTextPayload(flags: 0, text: "第7", newComposition: ""),
                          client: mock)
        XCTAssertEqual(r.digitTracker.prevChar, 0x37)
    }
}
