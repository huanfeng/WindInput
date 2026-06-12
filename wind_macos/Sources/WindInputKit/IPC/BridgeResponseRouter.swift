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

    /// host 文本光标移动意图. 由 app 层用 CGEvent 合成方向键实现 —— 智能配对
    /// (插入 `（）` 后回退到中间、输入右标点时跳过) 在 IMKit 无标准 API 移动宿主
    /// 光标, 只能合成方向键。kit 不直接依赖 CGEvent/Accessibility (保持可在 swift
    /// test 无 IMKit 环境驱动), 故以闭包把副作用上抛 app 层。nil 时静默降级 (不移动
    /// 光标, 退化为旧行为)。
    public enum CursorMove: Equatable {
        case left(Int)
        case right(Int)
    }

    /// app 层注入: 执行 host 光标移动。见 CursorMove。
    public var moveHostCursor: ((CursorMove) -> Void)?

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
            // 智能跳过: 输入右标点时栈顶匹配 → 跳过已自动补全的右标点。direction=1 右移。
            // 经合成方向键实现 (moveHostCursor); 未注入则降级为仅消费按键 (旧行为)。
            if let p = try? BinaryCodec.decodeMoveCursorPayload(frame.payload), p.direction == 1 {
                moveHostCursor?(.right(1))
            }
            return true

        case DownstreamCmd.deletePair:
            // 预留: coordinator 当前未生成此响应 (Windows/macOS 均未实装成对删除)。
            // 收到则仅消费按键, 待将来需要时经 moveHostCursor + 删除键合成。
            return true

        case DownstreamCmd.replaceBackward:
            // 智能符号: 删除光标前 count 个字符并插入替换文本。用 IMKit selectedRange
            // 定位光标, insertText(replacementRange:) 一步原子替换 (无需合成退格, 规避
            // 时序与修饰键问题)。无法取得光标时降级为仅插入 (不删除), 保证不误删。
            if let p = try? BinaryCodec.decodeReplaceBackwardPayload(frame.payload), let client = client {
                let count = Int(p.count)
                let sel = client.selectedRange()
                if count > 0, sel.location != NSNotFound, sel.location >= count {
                    let range = NSRange(location: sel.location - count, length: count)
                    client.insertText(p.text, replacementRange: range)
                } else {
                    let notFound = NSRange(location: NSNotFound, length: NSNotFound)
                    client.insertText(p.text, replacementRange: notFound)
                }
            }
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
        // 自动配对插入 `（）` 后, cursorOffset 是从文本末尾向左偏移的字符数 (通常 1),
        // 把光标退回到配对中间。IMKit 无移动宿主光标的标准 API → 经 moveHostCursor
        // 合成左方向键; 未注入则降级为不回退 (光标停在配对右侧, 旧行为)。
        if p.cursorOffset > 0 {
            moveHostCursor?(.left(Int(p.cursorOffset)))
        }
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
