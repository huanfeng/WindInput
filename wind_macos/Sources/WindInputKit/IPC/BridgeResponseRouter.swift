import Foundation

// BridgeResponseRouter — 把服务端返回的 Frame 路由到 TextInputClient 调用。
// 从 InputController 抽出，单测可以不依赖构造极重的 IMKInputController/IMKServer 来驱动。
public final class BridgeResponseRouter {

    /// 当前 IME 端 composition 状态, applyXxx 内部维护.
    public private(set) var composition = CompositionState()

    /// host 文本光标移动意图。智能配对（插入 `（）` 后回到中间、右标点跳过）在 IMKit 无标准 API
    /// 移动宿主光标，只能由 app 层合成方向键；kit 不依赖 CGEvent，故以闭包上抛。nil 时不移动光标。
    public enum CursorMove: Equatable {
        case left(Int)
        case right(Int)
    }

    /// app 层注入: 执行 host 光标移动。见 CursorMove。
    public var moveHostCursor: ((CursorMove) -> Void)?

    /// 智能符号 HoldComposition 挂着的待定标点（作为组合预览显示，语义是待提交）。
    ///
    /// 服务端下发 PassThrough / UpdateComposition 时会清掉自己的 held_text，假定客户端已把这个
    /// 符号收口（对位 Windows 的 FlushHoldCompositionIfActive / AbsorbHeldIntoPrefix）。不收口的话
    /// UpdateComposition 会直接覆盖 marked 的符号，而服务端已按上屏记过账。
    private var heldSymbol: String?

    /// 已「定格」但尚未真正 insertText 的前缀（对位 Windows 的 `_pendingCommitPrefix`）。
    ///
    /// 新组合到来时把符号并进同一段 marked text、最终由一次 CommitText 收口，而不是 commit 后重开
    /// 组合：后者在部分宿主下会被误读成「替换」，符号被新输入顶掉。
    private var pendingCommitPrefix: String = ""

    /// 待定标点的自动落定计时器。
    ///
    /// 只做一件事：到点把已是 marked text 的符号定稿成正文（去掉下划线），否则「，」会一直带着
    /// 下划线待到下一次按键或失焦。press2 能否替换由服务端的 smart_symbol_timeout 决定，这边到点
    /// 不改变可替换性，两者不打架。
    private var holdTimer: Timer?
    /// 每次状态变化自增；计时器只在"代"没变时才动手，避免过期回调误伤新一轮输入。
    private var holdGeneration: UInt64 = 0

    /// 数字后智能标点的 prevChar 记账（macOS 无主路径，全靠它），见 [`SmartPunctDigitTracker`]。
    /// 上屏侧的喂食统一收口在 `insertCommitted`，按键侧由 `InputController.passThroughToHost` 喂。
    public let digitTracker = SmartPunctDigitTracker()
    /// `seqChar` 记账（中英间补空格 / 英文后智能标点）。与 `digitTracker` 分开的理由见
    /// [`InputSequenceTracker`]：组字更新不动它，只在真写进文档时更新。
    public let seqTracker = InputSequenceTracker()

    public init() {}

    /// 真写进文档的唯一出口：`insertText` + 记账。
    ///
    /// router 里任何上屏都必须走这里，绕过去就漏一条记录点（某条路径打出的数字后标点仍出中文）。
    /// `client == nil` 时没写进文档，也就不记账，否则凭空捏造一个光标前字符。
    private func insertCommitted(_ text: String,
                                 replacementRange: NSRange,
                                 client: TextInputClient?) {
        guard let client = client else { return }
        client.insertText(text, replacementRange: replacementRange)
        digitTracker.noteCommittedText(text)
        seqTracker.noteCommittedText(text)
    }

    public func reset() {
        composition.clear()
        digitTracker.reset()
        seqTracker.reset()
        heldSymbol = nil
        pendingCommitPrefix = ""
        cancelHoldTimer()
    }

    /// 停表并作废在途回调。任何改动 heldSymbol / pendingCommitPrefix 的路径都要调它，
    /// 否则旧计时器会对着新一轮输入的状态动手。
    private func cancelHoldTimer() {
        holdTimer?.invalidate()
        holdTimer = nil
        holdGeneration &+= 1
    }

    /// 把待定标点定格进 `pendingCommitPrefix`, 不上屏、不动文档。
    /// 定格后不可再被 press2 替换 —— 语义上已承诺提交, 与服务端状态机一致。
    private func absorbHeldIntoPrefix() {
        guard let held = heldSymbol else { return }
        pendingCommitPrefix += held
        heldSymbol = nil
        cancelHoldTimer()
    }

    /// 取出「本次上屏该带上的前缀」(定格前缀 + 尚未定格的待定标点) 并清空状态。
    private func takePendingPrefix() -> String {
        absorbHeldIntoPrefix()   // 内含 cancelHoldTimer
        defer { pendingCommitPrefix = "" }
        return pendingCommitPrefix
    }

    /// 路由一个 bridge 响应帧到 client。返回值同 IMKInputController.handle：true 表示按键已被
    /// IME 消费，false 表示 PassThrough。
    ///
    /// `hostShortcut`：触发本帧的按键是宿主快捷键组合（⌘/⌃/⌥ + 键）。此时 ClearComposition 只表示
    /// 清组合，键仍须交给宿主执行复制/粘贴。Windows 靠 OnTestKeyDown 根本不转发这类键；macOS 的
    /// IMKit 把每个 keyDown 都给我们，判据只能落在这里，否则组字中按 ⌘C 会被吞。
    public func apply(_ frame: Frame, to client: TextInputClient?, hostShortcut: Bool = false) -> Bool {
        switch frame.cmd {
        case DownstreamCmd.passThrough:
            // 按键要交回系统，待定标点必须先真上屏，否则挂在 marked text 里没人管
            // （服务端已按上屏记账）。对位 Windows 的 FlushHoldCompositionIfActive。
            flushPendingPrefix(client: client)
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
            // 快捷键组合触发的清组合: 组合已清, 但按键交还宿主 (见方法头 hostShortcut)。
            return !hostShortcut

        case DownstreamCmd.clearThenPassThrough:
            // 联想态回车/退格透传：组合收掉、键交还宿主。与上一条不同，这是服务端显式要求交还。
            applyClearComposition(client: client)
            return false

        case DownstreamCmd.keyType:
            // 命令直通车 key.type / clip.paste 文本上屏：整段直接 insertText（不经 composition）。
            if let text = try? BinaryCodec.decodeKeyTypePayload(frame.payload), !text.isEmpty {
                let notFound = NSRange(location: NSNotFound, length: NSNotFound)
                insertCommitted(text, replacementRange: notFound, client: client)
            }
            return true

        case DownstreamCmd.moveCursor:
            // 智能跳过：右标点与栈顶匹配时跳过已自动补全的右标点。经 moveHostCursor 合成方向键，
            // 未注入则仅消费按键。
            if let p = try? BinaryCodec.decodeMoveCursorPayload(frame.payload), p.direction == 1 {
                moveHostCursor?(.right(1))
                // 光标跨过了右标点，这条路不经 insertText，须在此更新记账；光标前必是标点，清零即准确。
                digitTracker.reset()
                seqTracker.noteCursorCrossedSymbol()
            }
            return true

        case DownstreamCmd.deletePair:
            // 预留：coordinator 当前不生成此响应，收到仅消费按键。
            return true

        case DownstreamCmd.replaceBackward:
            // 智能符号：删除光标前 count 个字符并插入替换文本。用 selectedRange 定位，
            // insertText(replacementRange:) 一步原子替换，免去合成退格的时序问题。
            // 取不到光标时只插入不删除，保证不误删。
            if let p = try? BinaryCodec.decodeReplaceBackwardPayload(frame.payload), let client = client {
                let count = Int(p.count)
                // `text` 为空 = 纯删除（ime.undo_commit）。文档确实变了，不能沿用
                // noteCommittedText「空串不动状态」的规则，否则被删的数字会残留在记账里。
                if p.text.isEmpty && count > 0 {
                    digitTracker.reset()
                    seqTracker.reset()
                }
                let sel = client.selectedRange()
                if count > 0, sel.location != NSNotFound, sel.location >= count {
                    let range = NSRange(location: sel.location - count, length: count)
                    insertCommitted(p.text, replacementRange: range, client: client)
                } else {
                    let notFound = NSRange(location: NSNotFound, length: NSNotFound)
                    insertCommitted(p.text, replacementRange: notFound, client: client)
                }
            }
            return true

        case DownstreamCmd.holdComposition:
            // 智能符号 HoldComposition 方案 press1: 把待定标点作为组合预览显示。
            if let p = try? BinaryCodec.decodeHoldCompositionPayload(frame.payload) {
                // 前一个待定标点先定格 (连打两个不同符号: `，` 后紧跟 `。`)。内含停表。
                absorbHeldIntoPrefix()
                heldSymbol = p.holdText
                setCompositionRaw(pendingCommitPrefix + p.holdText, client: client)
                armHoldTimer(ms: p.timeoutMs, client: client)
            }
            return true

        case DownstreamCmd.commitAndHold, DownstreamCmd.commitThenDefer:
            // 先真上屏 commitText，再把余码/待定文本开成新组合。commitThenDefer 是码表顶码的
            // 正常通路；Windows「延迟到 keyup 才开组合」是 TSF 的规避手段，IMKit 立即开即可。
            if let p = try? BinaryCodec.decodeCommitAndHoldPayload(frame.payload) {
                applyCommitText(
                    BinaryCodec.CommitTextPayload(
                        flags: p.holdText.isEmpty ? 0 : BinaryCodec.commitFlagHasNewComposition,
                        text: p.commitText,
                        newComposition: p.holdText),
                    client: client)
                // commitAndHold 的 holdText 是待定标点，须登记成 held，否则下一次 UpdateComposition
                // 会覆盖它；commitThenDefer 的 holdText 是顶码余码，是普通组合，不登记。
                if frame.cmd == DownstreamCmd.commitAndHold, !p.holdText.isEmpty {
                    heldSymbol = p.holdText
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
        // hold 预览态活跃时，本次提交必须交代待定符号的去向（insertText 会换掉显示它的 marked
        // text），规则对齐 Windows CTextService::CommitText：replacingHeld（press2）丢弃，其余并入
        // 前缀一起上屏。默认追加是刻意的：hold 期间能触发提交的路径很多，安全一侧做默认，
        // 新增路径自动正确。
        if p.flags & BinaryCodec.commitFlagReplacingHeld != 0 {
            heldSymbol = nil
        } else {
            absorbHeldIntoPrefix()
        }
        let prefix = pendingCommitPrefix
        pendingCommitPrefix = ""
        // replacingHeld：宿主对「marked 未清就 insertText」处理不一，先显式清一次再插，免得待定
        // 标点与替换结果同时留下。
        if p.flags & BinaryCodec.commitFlagReplacingHeld != 0, !composition.text.isEmpty {
            client?.setMarkedText("",
                                  selectionRange: NSRange(location: 0, length: 0),
                                  replacementRange: notFound)
            composition.clear()
        }
        insertCommitted(prefix + p.text, replacementRange: notFound, client: client)

        if !p.newComposition.isEmpty {
            // 内联 preedit: commit 后立即开始新一轮 marked text
            composition.text = p.newComposition
            composition.caretUTF16 = utf16Len(p.newComposition)
            applyMarkedText(text: p.newComposition,
                            caretUTF16InText: composition.caretUTF16,
                            client: client)
        } else {
            composition.clear()
        }
    }

    public func applyCommitTextWithCursor(_ p: BinaryCodec.CommitTextWithCursorPayload,
                                          client: TextInputClient?) {
        let notFound = NSRange(location: NSNotFound, length: NSNotFound)
        // 同 applyCommitText: 待定符号并入前缀一起上屏, 否则智能配对插入时它会被吃掉。
        insertCommitted(takePendingPrefix() + p.text, replacementRange: notFound, client: client)
        composition.clear()
        // 自动配对插入 `（）` 后，cursorOffset 是从末尾向左的字符数，经 moveHostCursor 合成左方向键
        // 把光标退回配对中间；未注入则光标停在右侧。
        if p.cursorOffset > 0 {
            moveHostCursor?(.left(Int(p.cursorOffset)))
        }
    }

    public func applyUpdateComposition(_ p: BinaryCodec.UpdateCompositionPayload,
                                       client: TextInputClient?) {
        // 待定标点定格进前缀，与新组合显示在同一段 marked text（对位 Windows 的
        // AbsorbHeldIntoPrefix），光标随之右移前缀长度。
        absorbHeldIntoPrefix()
        let text = pendingCommitPrefix + p.text
        // 前缀长度须与 `p.caretPos` 同为 UTF-16 单位，用 `.count` 在前缀含非 BMP 字符时会偏。
        let caret = pendingCommitPrefix.utf16.count + Int(p.caretPos)
        composition.text = text
        composition.caretUTF16 = caret
        applyMarkedText(text: text, caretUTF16InText: caret, client: client)
    }

    /// 直接摆一段 marked text，不叠加待定前缀。供 HoldComposition 用：那时符号本身就是组合内容。
    private func setCompositionRaw(_ text: String, client: TextInputClient?) {
        composition.text = text
        composition.caretUTF16 = utf16Len(text)
        applyMarkedText(text: text, caretUTF16InText: composition.caretUTF16, client: client)
    }

    /// 结束组合。待定标点转为提交而非丢弃：失焦/切窗口时符号应直接上屏（同 Windows
    /// EndComposition），服务端也已按上屏记过账。
    public func applyClearComposition(client: TextInputClient?) {
        let prefix = takePendingPrefix()
        let notFound = NSRange(location: NSNotFound, length: NSNotFound)
        client?.setMarkedText("",
                              selectionRange: NSRange(location: 0, length: 0),
                              replacementRange: notFound)
        composition.clear()
        if !prefix.isEmpty {
            insertCommitted(prefix, replacementRange: notFound, client: client)
        }
    }

    // MARK: - Helpers

    /// 把待定前缀真上屏并清掉 marked text。用于按键交回系统、组合被主动结束这两种没有后续
    /// CommitText 收口的情况（对位 Windows 的 OnHoldTimerExpired 与 EndComposition）。
    private func flushPendingPrefix(client: TextInputClient?) {
        let prefix = takePendingPrefix()
        guard !prefix.isEmpty else { return }
        let notFound = NSRange(location: NSNotFound, length: NSNotFound)
        if !composition.text.isEmpty {
            client?.setMarkedText("",
                                  selectionRange: NSRange(location: 0, length: 0),
                                  replacementRange: notFound)
            composition.clear()
        }
        insertCommitted(prefix, replacementRange: notFound, client: client)
    }

    /// 到点把待定标点定稿成正文（内容不变，只收掉 marked 的下划线）。
    ///
    /// `client` 弱持有：计时器活得比一次按键长，强持有会吊住宿主的输入上下文。
    private func armHoldTimer(ms: UInt32, client: TextInputClient?) {
        cancelHoldTimer()
        guard ms > 0 else { return }   // 0 = 不自动落定
        let gen = holdGeneration
        weak var weakClient = client
        holdTimer = Timer.scheduledTimer(withTimeInterval: Double(ms) / 1000.0,
                                         repeats: false) { [weak self] _ in
            guard let self = self else { return }
            // 「代」变了说明这中间已经有过按键/提交/清除，那条路自己处置过了。
            guard self.holdGeneration == gen, self.heldSymbol != nil else { return }
            self.flushPendingPrefix(client: weakClient)
        }
    }

    /// 摆一段 marked text, 光标落在 `caretUTF16InText`。
    ///
    /// `length` 恒为 0，不要改成整串选中：IMKit 里 selectionRange 表达的是活动分句，宿主见
    /// `length > 0` 会把整段当选中分句、光标画在段首。配套的另一半在 IMKClientAdapter.setMarkedText
    /// （必须传带分句属性的 NSAttributedString）。
    private func applyMarkedText(text: String, caretUTF16InText: Int, client: TextInputClient?) {
        guard let client = client else { return }
        let notFound = NSRange(location: NSNotFound, length: NSNotFound)
        let caret = CompositionState(text: text, caretUTF16: caretUTF16InText).caretInUTF16()
        let selRange = NSRange(location: caret, length: 0)
        client.setMarkedText(text, selectionRange: selRange, replacementRange: notFound)
        // marked text 也摆在光标前，同样要记账：否则组字中仍报着之前的 '3'，智能符号 press1 走
        // HoldComposition 时 press2 的 prev_char 比对会失配，连按替换静默失效。
        // 只在光标确实在串尾时取串尾；光标在编码中间（码表可把 0-9 配成码元）就清零。
        if caretUTF16InText >= utf16Len(text) {
            digitTracker.noteCommittedText(text)
        } else {
            digitTracker.reset()
        }
    }

    /// 「光标在串尾」的取值。单位须与服务端 `caret_pos` 一致 (UTF-16 单元), 见
    /// `CompositionState.caretUTF16`。
    private func utf16Len(_ s: String) -> Int {
        return s.utf16.count
    }
}
