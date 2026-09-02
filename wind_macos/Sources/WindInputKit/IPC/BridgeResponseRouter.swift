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

    /// 智能符号 HoldComposition 挂着的待定标点 (作为组合预览显示, 语义 = **待提交**)。
    ///
    /// 服务端在下发 PassThrough / UpdateComposition 时会同步清掉自己的 `held_text`
    /// (`coordinator.rs::handle_key_event_policed`), 因为它假定客户端此刻已经把这个符号
    /// 收口了 —— Windows 侧确实在这两个分支各调了 `FlushHoldCompositionIfActive` /
    /// `AbsorbHeldIntoPrefix`。macOS 侧一度什么都没做, 于是:
    ///   输入 `，` 再输入 `n` → UpdateComposition{"n"} 直接 setMarkedText("n") 覆盖掉
    ///   marked 的 `，`, **符号凭空消失**, 而服务端已经按上屏记过账。
    private var heldSymbol: String?

    /// 已「定格」但尚未真正 insertText 的前缀 (对位 Windows 的 `_pendingCommitPrefix`)。
    ///
    /// 新组合到来时**不是** commit 旧符号 + 重开组合, 而是把符号并进同一段 marked text,
    /// 最终由一次 CommitText 收口。Windows 那边试过 commit+重开, 结果在 WPS/微信下被
    /// 误读成「替换」, 符号被新输入顶掉 —— 同一个坑没必要在 macOS 再踩一遍。
    private var pendingCommitPrefix: String = ""

    /// 待定标点的自动落定计时器 + 它对应的"这一代"状态。
    ///
    /// Windows 侧有同名计时器，但那是 TSF 的刚需（组合必须「吃了再吐」）；IMKit 没有那个
    /// 约束，故这里**只做一件事**：到点把已是 marked text 的符号**定稿成正文**。文字内容
    /// 前后完全一样，变的只是它还带不带 marked 的下划线——不加这个计时器，用户打完一个
    /// 「，」就会看到它一直带着下划线待在文档里，直到下一次按键或失焦才收掉。
    ///
    /// 语义上的时限判定不在这里：press2 能否替换由**服务端**的 `smart_symbol_timeout` 说了算
    /// （见 `handle_punct.rs` 的 `arm.at.elapsed()` 守卫）。这边到点不改变任何可替换性，
    /// 只收下划线，故两者不会打架。
    private var holdTimer: Timer?
    /// 每次状态变化自增；计时器只在"代"没变时才动手，避免过期回调误伤新一轮输入。
    private var holdGeneration: UInt64 = 0

    public init() {}

    public func reset() {
        composition.clear()
        heldSymbol = nil
        pendingCommitPrefix = ""
        cancelHoldTimer()
    }

    /// 停表并作废在途回调。**任何**改动 heldSymbol / pendingCommitPrefix 的路径都要调它，
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

    /// 路由一个 bridge 响应帧到 client. 返回值同 IMKInputController.handle 的
    /// Bool 语义: true 表示按键已被 IME 消费, IMKit 不再传给系统; false 表示
    /// PassThrough.
    ///
    /// `hostShortcut`: 触发本帧的按键是**宿主快捷键组合** (⌘/⌃/⌥ + 键)。此时
    /// `ClearComposition` 只表示「把组合清掉」, **不表示这个键归输入法** —— 宿主仍须
    /// 收到它去执行复制/粘贴。Windows 靠 TSF `OnTestKeyDown` 根本不转发这类键来保证
    /// 这点; macOS 没有那层前置闸门 (IMKit 把每个 keyDown 都给我们), 判据只能落在这里。
    /// 不这么做的现象: 组字过程中按 ⌘C/⌃C, 组合清了、键也被吞, 网页里复制粘贴失灵
    /// (issue #64)。命中热键的组合走 Consumed/StatusUpdate/InsertText, 不受影响。
    public func apply(_ frame: Frame, to client: TextInputClient?, hostShortcut: Bool = false) -> Bool {
        switch frame.cmd {
        case DownstreamCmd.passThrough:
            // 按键要交回系统 → 待定标点必须**先真上屏**, 否则它挂在 marked text 里没人再管:
            // 服务端已经清掉 held_text 并按上屏记了账。对位 Windows PassThrough 分支的
            // FlushHoldCompositionIfActive。
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
            // 联想态回车/退格透传: 组合要收掉, 键要交还宿主 —— 两件事都做。
            // 与上一条的区别是**服务端显式声明**要交还, 与是不是快捷键组合无关。
            applyClearComposition(client: client)
            return false

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
            // 先真上屏 commitText, 再把余码/待定文本开成新组合。
            // commitThenDefer 是码表顶码 direct_commit 的正常通路 (非 Windows 专有),
            // 早先落在 default 分支 → 顶码上屏的字被吞。Win 侧的「延迟到触发键 keyup
            // 才开组合」是 TSF 组合边界的规避手段, IMKit 无此约束, 立即开即可。
            if let p = try? BinaryCodec.decodeCommitAndHoldPayload(frame.payload) {
                applyCommitText(
                    BinaryCodec.CommitTextPayload(
                        flags: p.holdText.isEmpty ? 0 : BinaryCodec.commitFlagHasNewComposition,
                        text: p.commitText,
                        newComposition: p.holdText),
                    client: client)
                // commitAndHold 的 holdText 是**待定标点**(智能符号 press1 撞上活跃编码：
                // 先顶屏候选再挂标点)，须登记成 held，否则下一次 UpdateComposition 照样把它
                // 覆盖掉——与直接 HoldComposition 那条路是同一个坑。
                // commitThenDefer 的 holdText 是码表顶码后的**余码**，是普通组合，不登记。
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
        // hold 预览态活跃时, 本次提交必须交代那个待定符号的去向 —— 下面的 insertText 会把
        // marked text 换掉, 而 marked 里此刻显示的正是它, 不处置就被静默覆盖。规则逐字对齐
        // Windows `CTextService::CommitText`:
        //   replacingHeld=true (press2): 本就是要拿英文符号换掉它 → 丢弃。
        //   replacingHeld=false (其余一切): 并入前缀, 与本次文本一起上屏 (追加语义)。
        // 默认取追加是刻意的: hold 期间能触发提交的路径远不止一处 (全角空格/数字、临时英文、
        // 各独占模式出字…), 把安全的一侧设为默认, 新增路径自动正确。Windows 那边曾默认丢弃,
        // 表现为全角下「。」+空格 → 符号消失、只剩全角空格。
        if p.flags & BinaryCodec.commitFlagReplacingHeld != 0 {
            heldSymbol = nil
        } else {
            absorbHeldIntoPrefix()
        }
        let prefix = pendingCommitPrefix
        pendingCommitPrefix = ""
        // replacingHeld: 这次上屏是在替换先前 HoldComposition 挂着的组合预览 (智能符号
        // press2)。宿主对「marked 未清就 insertText」的处理不统一, 先显式清一次再插,
        // 免得待定标点与替换结果同时留在文本里。仅在本端确实还挂着 marked 时才清。
        if p.flags & BinaryCodec.commitFlagReplacingHeld != 0, !composition.text.isEmpty {
            client?.setMarkedText("",
                                  selectionRange: NSRange(location: 0, length: 0),
                                  replacementRange: notFound)
            composition.clear()
        }
        client?.insertText(prefix + p.text, replacementRange: notFound)

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
        client?.insertText(takePendingPrefix() + p.text, replacementRange: notFound)
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
        // 待定标点定格进前缀, 与新组合内容显示在**同一段** marked text 里 (对位 Windows
        // UpdateComposition 分支的 AbsorbHeldIntoPrefix)。光标位置随之右移前缀长度,
        // 否则光标会落在符号里面。
        absorbHeldIntoPrefix()
        let text = pendingCommitPrefix + p.text
        // 前缀长度须与 `p.caretPos` 同单位 (UTF-16), 用 `.count` (字符数) 会在前缀含
        // 非 BMP 字符时偏移——中文标点都在 BMP, 故只有直通 `ime.pair` 压入的生僻符号
        // 才碰得到, 但错就是错。
        let caret = pendingCommitPrefix.utf16.count + Int(p.caretPos)
        composition.text = text
        composition.caretUTF16 = caret
        applyMarkedText(text: text, caretUTF16InText: caret, client: client)
    }

    /// 直接摆一段 marked text, **不叠加**待定前缀。供 HoldComposition 自己用:
    /// 那时符号本身就是组合内容, 走 `applyUpdateComposition` 会把它叠进前缀再显示一遍。
    private func setCompositionRaw(_ text: String, client: TextInputClient?) {
        composition.text = text
        composition.caretUTF16 = utf16Len(text)
        applyMarkedText(text: text, caretUTF16InText: composition.caretUTF16, client: client)
    }

    /// 结束组合。**待定标点转为提交而非丢弃** —— 失焦/切窗口时符号应直接上屏, 与标准输入
    /// 流程一致 (Windows EndComposition 同此), 何况服务端已按上屏记过账, 丢了就是凭空少字。
    public func applyClearComposition(client: TextInputClient?) {
        let prefix = takePendingPrefix()
        let notFound = NSRange(location: NSNotFound, length: NSNotFound)
        client?.setMarkedText("",
                              selectionRange: NSRange(location: 0, length: 0),
                              replacementRange: notFound)
        composition.clear()
        if !prefix.isEmpty {
            client?.insertText(prefix, replacementRange: notFound)
        }
    }

    // MARK: - Helpers

    /// 把待定前缀真上屏并清掉 marked text。用于「按键交回系统 / 组合被主动结束」——
    /// 这两种情况下没有后续的 CommitText 来收口前缀了。
    ///
    /// 对位 Windows 的 `OnHoldTimerExpired`(Flush 路径) 与 `EndComposition` 里那段
    /// 「主动结束组合时转为提交而非放弃, 模拟标准输入流程——切换窗口时符号应直接上屏」。
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
        client?.insertText(prefix, replacementRange: notFound)
    }

    /// 到点把待定标点定稿成正文（内容不变，只收掉 marked 的下划线）。
    ///
    /// `client` 弱持有: 计时器活得比一次按键长，强持有会把宿主的输入上下文吊住。
    /// client 已销毁 → 只清本端状态，不去动一个不存在的文本框。
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
    /// # `length` 恒为 0, 不要改成「整串选中」
    ///
    /// IMKit 语境里 `selectionRange` 表达的是**「活动分句」**(日文分节转换要高亮正在转换
    /// 的那一节), 而我们要表达的是**插入点**, 故 `length` 必须是 0 —— 宿主
    /// (`NSTextInputClient`) 见 `length > 0` 会把整段当选中分句、把光标画在段首。
    /// 与之配套的另一半在 `IMKClientAdapter.setMarkedText`: 必须传带分句属性的
    /// `NSAttributedString`, 否则本处传的值到不了宿主 (详见那边的注释)。
    private func applyMarkedText(text: String, caretUTF16InText: Int, client: TextInputClient?) {
        guard let client = client else { return }
        let notFound = NSRange(location: NSNotFound, length: NSNotFound)
        let caret = CompositionState(text: text, caretUTF16: caretUTF16InText).caretInUTF16()
        let selRange = NSRange(location: caret, length: 0)
        client.setMarkedText(text, selectionRange: selRange, replacementRange: notFound)
    }

    /// 「光标在串尾」的取值。单位须与服务端 `caret_pos` 一致 (UTF-16 单元), 见
    /// `CompositionState.caretUTF16`。
    private func utf16Len(_ s: String) -> Int {
        return s.utf16.count
    }
}
