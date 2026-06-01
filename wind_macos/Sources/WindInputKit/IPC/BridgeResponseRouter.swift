import Foundation

// BridgeResponseRouter — 把 Go bridge 返回的 Frame 路由到 TextInputClient 调用.
//
// 从 InputController 抽出, 因为:
//   1. 单测需要在不依赖 IMKInputController/IMKServer 的情况下驱动 (后者构造极重)
//   2. 复用方便: smoke CLI / 未来其它客户端也能用同一套 dispatch
//
// 用法:
//   let router = BridgeResponseRouter()
//   let consumed = router.apply(frame, to: mockClient)
//   XCTAssertEqual(mockClient.insertedTexts, ["你好"])
public final class BridgeResponseRouter {

    /// 当前 IME 端 composition 状态, applyXxx 内部维护.
    public private(set) var composition = CompositionState()

    public init() {}

    public func reset() {
        composition.clear()
    }

    /// 路由一个 bridge 响应帧到 client. 返回值同 IMKInputController.handle 的
    /// Bool 语义: true 表示按键已被 IME 消费, IMKit 不再传给系统; false 表示
    /// PassThrough.
    public func apply(_ frame: Frame, to client: TextInputClient?) -> Bool {
        switch frame.cmd {
        case DownstreamCmd.passThrough:
            return false

        case DownstreamCmd.consumed, DownstreamCmd.ack:
            return true

        case DownstreamCmd.commitText:
            if let p = try? BinaryCodec.decodeCommitTextPayload(frame.payload) {
                applyCommitText(p, client: client)
            }
            return true

        case DownstreamCmd.commitTextWithCursor:
            if let p = try? BinaryCodec.decodeCommitTextWithCursorPayload(frame.payload) {
                applyCommitTextWithCursor(p, client: client)
            }
            return true

        case DownstreamCmd.updateComposition:
            if let p = try? BinaryCodec.decodeUpdateCompositionPayload(frame.payload) {
                applyUpdateComposition(p, client: client)
            }
            return true

        case DownstreamCmd.clearComposition:
            applyClearComposition(client: client)
            return true

        case DownstreamCmd.keyType:
            // 命令直通车 key.type / clip.paste 文本上屏: 整段 UTF-8, 直接 insertText
            // (不经 composition, 与 commitText 一样落到当前光标处)。
            if let text = try? BinaryCodec.decodeKeyTypePayload(frame.payload), !text.isEmpty {
                let notFound = NSRange(location: NSNotFound, length: NSNotFound)
                client?.insertText(text, replacementRange: notFound)
            }
            return true

        case DownstreamCmd.moveCursor:
            // M3 实装 (智能跳过); M2.2 仅丢弃但仍消费按键
            return true

        case DownstreamCmd.deletePair:
            // M3 实装 (智能 backspace); M2.2 仅丢弃但仍消费按键
            return true

        default:
            return true   // 未知 cmd: 默认消费, 避免重复出字符
        }
    }

    // MARK: - 具体动作

    public func applyCommitText(_ p: BinaryCodec.CommitTextPayload, client: TextInputClient?) {
        let notFound = NSRange(location: NSNotFound, length: NSNotFound)
        client?.insertText(p.text, replacementRange: notFound)

        if !p.newComposition.isEmpty {
            // 内联 preedit: commit 后立即开始新一轮 marked text
            composition.text = p.newComposition
            composition.caretRune = countRunes(p.newComposition)
            applyMarkedText(text: p.newComposition,
                            caretRuneInText: composition.caretRune,
                            client: client)
        } else {
            composition.clear()
        }
    }

    public func applyCommitTextWithCursor(_ p: BinaryCodec.CommitTextWithCursorPayload,
                                          client: TextInputClient?) {
        let notFound = NSRange(location: NSNotFound, length: NSNotFound)
        client?.insertText(p.text, replacementRange: notFound)
        composition.clear()
        // cursorOffset 真实左移 M3 实装 (IMKit 没标准 API, 需 client.selectedRange).
    }

    public func applyUpdateComposition(_ p: BinaryCodec.UpdateCompositionPayload,
                                       client: TextInputClient?) {
        composition.text = p.text
        composition.caretRune = Int(p.caretPos)
        applyMarkedText(text: p.text,
                        caretRuneInText: Int(p.caretPos),
                        client: client)
    }

    public func applyClearComposition(client: TextInputClient?) {
        let notFound = NSRange(location: NSNotFound, length: NSNotFound)
        client?.setMarkedText("",
                              selectionRange: NSRange(location: 0, length: 0),
                              replacementRange: notFound)
        composition.clear()
    }

    // MARK: - Helpers

    private func applyMarkedText(text: String, caretRuneInText: Int, client: TextInputClient?) {
        guard let client = client else { return }
        let notFound = NSRange(location: NSNotFound, length: NSNotFound)
        let utf16Caret = CompositionState(text: text, caretRune: caretRuneInText).caretInUTF16()
        let selRange = NSRange(location: utf16Caret, length: 0)
        client.setMarkedText(text, selectionRange: selRange, replacementRange: notFound)
    }

    private func countRunes(_ s: String) -> Int {
        return s.count
    }
}
