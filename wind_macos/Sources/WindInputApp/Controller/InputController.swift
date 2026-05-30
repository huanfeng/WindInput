import Cocoa
import InputMethodKit
import WindInputKit

// InputController — IMKit 为每个文本框/会话实例化一个本类对象 (PR-1 设计 方案 A).
//
// M2.2-C/D 实装范围:
//   - init 时连 bridge.sock (BridgeClient.connect, 失败仅 log 不抛)
//   - handle(_:client:) 把 NSEvent 翻译成 KeyEvent 帧, 同步发送, 等响应
//   - applyResponse 路由 Go 返回的 cmd, 真调 IMKTextInput 协议方法:
//       * CmdCommitText (0x0101)        → client.insertText
//       * CmdUpdateComposition (0x0102) → client.setMarkedText
//       * CmdClearComposition (0x0103)  → setMarkedText("") + 状态清零
//       * CmdCommitTextWithCursor (0x0106) → insertText + 光标偏移
//       * CmdConsumed / CmdPassThrough / CmdAck → 控制流路由
//   - CompositionState 跟踪本端最新 marked text + caret
//
// Commit 触发键路径 (M2.2-D, 与 Win 端 barrier 设计不同):
//   Win TSF DLL 用 CmdCommitRequest (0x0104) 异步 barrier 解决 TSF race condition
//   (用户在 IME 处理中快速按 commit 键导致 commit 文本与下一键错位).
//   darwin IMKit handle 是同步的, 没有 race, **不需要 barrier 机制**, server_darwin.go
//   dispatch 也没处理 CmdCommitRequest. 所以 darwin 上 Space/Enter/数字 1-9 选词
//   直接走 CmdKeyEvent: Go HandleKeyEvent 识别 VK_SPACE/VK_RETURN/0x31-0x39 时
//   直接返 CmdCommitText, 由 applyResponse 调 insertText. KeyHandler 已覆盖这些键
//   的翻译 (NSEvent.keyCode 0x12-0x19 / 0x1D → VK 0x30-0x39, 0x24 → VK_RETURN,
//   0x31 → VK_SPACE, 0x35 → VK_ESCAPE).
//
// 线程模型: IMKit 在主线程调用 handle, BridgeClient 阻塞 socket I/O.
//   UDS roundtrip < 1ms, 用户感知不到. 未来改 async + barrier seq.
@objc(WindInputController)
public class InputController: IMKInputController {

    private var bridge: BridgeClient?
    private var keySeq: UInt16 = 0
    private let router = BridgeResponseRouter()
    private var composition: CompositionState { router.composition }
    // 当前焦点 IMKit client, 供鼠标选词 push commit 路由 (见 applyPushResponse)。
    private weak var currentClient: (IMKTextInput & NSObjectProtocol)?

    // 修饰键单击 (tap) 检测: 按下某修饰键 → 抬起且其间无其它键 = tap, 发对应 VK 给
    // Go 触发模式切换 (如 lshift 切中英)。macOS 修饰键走 .flagsChanged, 不是 keyDown。
    private var pendingModVK: UInt32?    // 当前按住、待判定的修饰键 Win VK (nil=无)
    private var pendingModSawOther = false // 修饰键按住期间是否出现过其它键 (→ 非 tap)

    public override init!(server: IMKServer!, delegate: Any!, client inputClient: Any!) {
        super.init(server: server, delegate: delegate, client: inputClient)

        let path = BridgeEndpoints.requestSocket
        do {
            bridge = try BridgeClient(socketPath: path)
            NSLog("WindInput[InputController] bridge connected path=\(path)")
        } catch {
            NSLog("WindInput[InputController] bridge connect FAILED path=\(path) err=\(error)")
            bridge = nil
        }
    }

    deinit {
        bridge?.close()
    }

    // MARK: - IMKit 生命周期 (激活/失活)

    /// IME 获得某 client 焦点时由系统调用。发 FocusGained 让 Go 端置 imeActivated=true,
    /// 从而驱动工具栏 reducer 显示模式指示器 (CmdModeStatus → 菜单栏)。
    public override func activateServer(_ sender: Any!) {
        super.activateServer(sender)
        currentClient = sender as? (IMKTextInput & NSObjectProtocol)
        CandidatePanelHost.shared.activeResponder = self
        sendEmpty(UpstreamCmd.focusGained)
    }

    /// IME 失去焦点 (切到别的输入法/应用) 时由系统调用。发 FocusLost 让 Go 端
    /// 置 imeActivated=false, reducer 隐藏指示器。
    public override func deactivateServer(_ sender: Any!) {
        // 失焦即清干净: 若仍有嵌入编码 (marked text) 未提交, 主动抹掉残留并清本端
        // composition 状态。否则切到别的文本框时旧 marked text 会残留 (macOS 不会
        // 像 Win TSF 那样自动收回), 且与 Go 端不一致 (HandleFocusLost 对普通焦点切换
        // 已 clearState 清空 inputBuffer)。两端一致后, 切回该文本框是全新一轮输入。
        // 必须在 super.deactivateServer 之前做: 此时 sender client 仍可接收 setMarkedText。
        if !composition.isEmpty {
            let imkClient = sender as? IMKTextInput
            let adapter = imkClient.map { IMKClientAdapter(imkClient: $0) }
            router.applyClearComposition(client: adapter)
        }
        sendEmpty(UpstreamCmd.focusLost)
        super.deactivateServer(sender)
    }

    /// 发一个无 payload 的上行帧 (focusGained/focusLost 等), 读掉 ack。失败仅 log。
    private func sendEmpty(_ cmd: UInt16) {
        guard let bridge = bridge, bridge.isConnected else { return }
        do {
            try bridge.send(BinaryCodec.encodeEmptyFrame(cmd: cmd))
            _ = try bridge.readFrame()
        } catch {
            NSLog("WindInput[sendEmpty] cmd=\(cmd) io error: \(error)")
            reconnect()
        }
    }

    // MARK: - 修饰键 tap (Shift/Ctrl 单击切换模式)

    /// 处理 .flagsChanged: 修饰键按下记录待判定; 抬起且其间无其它键 = tap, 发 VK 给 Go。
    private func handleFlagsChanged(_ event: NSEvent, client sender: Any!) {
        guard let (vk, mask) = Self.modifierInfo(forKeyCode: event.keyCode) else {
            pendingModVK = nil
            return
        }
        let pressed = (event.modifierFlags.rawValue & mask) != 0
        if pressed {
            pendingModVK = vk
            pendingModSawOther = false
        } else {
            if pendingModVK == vk && !pendingModSawOther {
                sendModifierTap(vk, sender: sender)
            }
            pendingModVK = nil
        }
    }

    /// mac keyCode → (Win VK, NSEvent.ModifierFlags 掩码)。仅可作模式切换的修饰键。
    private static func modifierInfo(forKeyCode kc: UInt16) -> (UInt32, UInt)? {
        switch kc {
        case 56: return (0xA0, NSEvent.ModifierFlags.shift.rawValue)   // 左 Shift → VK_LSHIFT
        case 60: return (0xA1, NSEvent.ModifierFlags.shift.rawValue)   // 右 Shift → VK_RSHIFT
        case 59: return (0xA2, NSEvent.ModifierFlags.control.rawValue) // 左 Ctrl → VK_LCONTROL
        case 62: return (0xA3, NSEvent.ModifierFlags.control.rawValue) // 右 Ctrl → VK_RCONTROL
        default: return nil
        }
    }

    /// 发一个修饰键 VK 的 KeyEvent (eventType=down) 给 Go, 触发模式切换; 应用其响应。
    private func sendModifierTap(_ vk: UInt32, sender: Any!) {
        guard let bridge = bridge, bridge.isConnected else { return }
        // 模式切换 (Shift/Ctrl tap) 通常无 composition, 先刷新 caret 让状态气泡锚到当前
        // 插入点 (否则会显示在上一次组字的旧位置)。
        sendCaretUpdateIfAvailable(client: sender as? IMKTextInput)
        keySeq &+= 1
        let frame = BinaryCodec.encodeKeyEventFrame(KeyEventPayload(
            keyCode: vk, scanCode: 0, modifiers: 0, eventType: .down, eventSeq: keySeq, prevChar: 0))
        do {
            try bridge.send(frame)
            let resp = try bridge.readFrame()
            _ = applyResponse(resp, sender: sender)
        } catch {
            NSLog("WindInput[modTap] vk=\(vk) io error: \(error)")
            reconnect()
        }
    }

    // MARK: - IMKit hook

    /// 告诉 IMKit 本输入法要接收哪些事件。默认只有 keyDown; 必须显式加 flagsChanged
    /// 才能收到修饰键 (Shift/Ctrl) 变化, 做单击切换检测。
    public override func recognizedEvents(_ sender: Any!) -> Int {
        return Int(NSEvent.EventTypeMask.keyDown.rawValue | NSEvent.EventTypeMask.flagsChanged.rawValue)
    }

    public override func handle(_ event: NSEvent!, client sender: Any!) -> Bool {
        guard let event = event else { return false }

        // 记录当前焦点 client + 把自己登记为 active responder, 让鼠标选词的
        // push 通道 commit (CandidatePanelHost 收到) 能路由回这个文本框。
        currentClient = sender as? (IMKTextInput & NSObjectProtocol)
        CandidatePanelHost.shared.activeResponder = self

        // 修饰键变化 (Shift/Ctrl 等): 做 tap 检测, 不消费事件本身。
        if event.type == .flagsChanged {
            handleFlagsChanged(event, client: sender)
            return false
        }
        guard event.type == .keyDown else { return false }
        // 任意真实按键出现 → 取消当前修饰键 tap 判定 (Shift+X 不算 tap)。
        pendingModSawOther = true
        guard let bridge = bridge, bridge.isConnected else {
            NSLog("WindInput[handle] bridge not connected, pass through")
            return false
        }

        keySeq &+= 1
        guard let frame = KeyHandler.encodeKeyEvent(event, seq: keySeq) else {
            return false
        }

        // 无 composition 时本端 caret 可能是上一次组字的旧位置 (换行/移动光标后未更新)。
        // 处理本键前先刷新一次, 让 Go 的状态气泡/首帧候选锚到当前真实插入点。
        // 组字中 caret 由下方 (composition 非空) 分支持续更新, 无需在此重复。
        if composition.isEmpty {
            sendCaretUpdateIfAvailable(client: sender as? IMKTextInput)
        }

        do {
            try bridge.send(frame)
            let resp = try bridge.readFrame()
            let consumed = applyResponse(resp, sender: sender)

            // M2.2-E: composition 启动/更新后, 上报当前 caret 屏幕位置给 Go,
            // 让候选框/Toast/光标跟随有正确锚点. 仅在 marked text 非空时发,
            // 避免无 composition 时浪费带宽.
            if !composition.isEmpty {
                sendCaretUpdateIfAvailable(client: sender as? IMKTextInput)
            }
            return consumed
        } catch {
            NSLog("WindInput[handle] bridge io error: \(error)")
            reconnect()
            return false
        }
    }

    // MARK: - Caret update (M2.2-E)

    /// 从 IMKTextInput 拿 caret 屏幕坐标, 转换为 wire top-left 坐标后发 CmdCaretUpdate.
    /// 不抛错, 失败仅 log.
    internal func sendCaretUpdateIfAvailable(client: IMKTextInput?) {
        guard let client = client, let bridge = bridge, bridge.isConnected else { return }

        // IMKTextInput.attributes(forCharacterIndex:lineHeightRectangle:) 把 caret
        // 所在那一行的矩形写到 lineHeightRectangle (out 参数).
        // 注: 返回值是 attribute dict (NSColor / font 等), 这里我们只关心 rect.
        var rect = NSRect.zero
        _ = client.attributes(forCharacterIndex: 0, lineHeightRectangle: &rect)
        guard rect.size.height > 0 else { return }

        let screen = NSScreen.main ?? NSScreen.screens.first
        let screenHeight = screen?.frame.height ?? 0
        guard screenHeight > 0 else { return }

        let (x, y, h) = CaretCoords.caretRectToWire(rect, screenHeight: screenHeight)
        let frame = BinaryCodec.encodeCaretUpdateFrame(x: x, y: y, height: h)
        do {
            try bridge.send(frame)
            _ = try bridge.readFrame()   // Go server_darwin.go 一律返 ack, 必须读掉避免堆积
        } catch {
            NSLog("WindInput[caretUpdate] send/read error: \(error)")
        }
    }

    // MARK: - Response routing

    /// 把 Go 返回的 bridge 帧路由到 IMKTextInput 协议方法. 委托给 BridgeResponseRouter
    /// (在 WindInputKit 里, 不依赖 IMKit, 便于 swift test 用 mock 驱动).
    internal func applyResponse(_ frame: Frame, sender: Any?) -> Bool {
        let imkClient = sender as? IMKTextInput
        let adapter = imkClient.map { IMKClientAdapter(imkClient: $0) }
        return router.apply(frame, to: adapter)
    }

    /// 应用 push 通道帧 (鼠标选词的 commit/composition 异步到达, 非 KeyEvent 同步响应)。
    /// 路由到当前焦点 client。在主线程调用 (CandidatePanelHost 已 dispatch)。
    public func applyPushResponse(_ frame: Frame) {
        guard let client = currentClient else {
            NSLog("WindInput[applyPushResponse] no current client, drop cmd=\(frame.cmd)")
            return
        }
        _ = router.apply(frame, to: IMKClientAdapter(imkClient: client))
        if !composition.isEmpty {
            sendCaretUpdateIfAvailable(client: client)
        }
    }

    // MARK: - IMKit Adapter (把 IMKTextInput 桥接到 TextInputClient)

    /// IMKTextInput → TextInputClient 的适配器, 让 BridgeResponseRouter (在
    /// WindInputKit 不依赖 IMKit 的子库里) 也能调到 IMKit 真客户端.
    private final class IMKClientAdapter: TextInputClient {
        let imkClient: IMKTextInput
        init(imkClient: IMKTextInput) { self.imkClient = imkClient }

        func insertText(_ text: String, replacementRange: NSRange) {
            imkClient.insertText(text, replacementRange: replacementRange)
        }
        func setMarkedText(_ text: String, selectionRange: NSRange, replacementRange: NSRange) {
            imkClient.setMarkedText(text, selectionRange: selectionRange, replacementRange: replacementRange)
        }
    }

    // MARK: - Reconnect

    private func reconnect() {
        bridge?.close()
        bridge = nil
        do {
            bridge = try BridgeClient(socketPath: BridgeEndpoints.requestSocket)
            NSLog("WindInput[reconnect] bridge reconnected")
        } catch {
            NSLog("WindInput[reconnect] still down: \(error)")
        }
    }
}

// PushResponder: 让 CandidatePanelHost 能把 push 通道 commit 路由到此 controller。
extension InputController: PushResponder {}
