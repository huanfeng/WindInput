import Cocoa
import InputMethodKit
import WindInputKit
import Carbon.HIToolbox // IsSecureEventInputEnabled (密码框/安全输入检测)
import ApplicationServices // AXUIElement (前台窗口标题, 命令直通车 title())
import os

// InputController — IMKit 为每个文本框/会话实例化一个本类对象。
//
// handle(_:client:) 把 NSEvent 译成 KeyEvent 帧同步发给服务、读响应，交 BridgeResponseRouter 路由成
// IMKTextInput 调用（insertText / setMarkedText 等）。IMKit 的 handle 是同步的，没有 TSF 那种提交键
// 竞态，所以 Space / Enter / 数字选词也直接走 KeyEvent，不需要 Windows 的 CmdCommitRequest barrier。
//
// 线程模型：IMKit 在主线程调用 handle，BridgeClient 阻塞 socket I/O（UDS 往返 <1ms）。
@objc(WindInputController)
public class InputController: IMKInputController {

    // 超时后 catch → reconnect, 下一键用新连接自愈。
    private static let requestIOTimeoutMs = BridgeEndpoints.requestIOTimeoutMs

    private var bridge: BridgeClient?
    private var keySeq: UInt16 = 0
    private let router = BridgeResponseRouter()
    // 系统输入菜单 (点击菜单栏输入源图标弹出) 的统一菜单构建器。须持有 (虽 IMK 模式
    // target=nil, 但每次 menu() 重建依赖此实例存活)。
    private let imkMenuBuilder = UnifiedMenuBuilder()
    private var composition: CompositionState { router.composition }
    // 当前焦点 IMKit client, 供鼠标选词 push commit 路由 (见 applyPushResponse)。
    private weak var currentClient: (IMKTextInput & NSObjectProtocol)?

    // 修饰键单击 (tap) 检测: 按下某修饰键 → 抬起且其间无其它键 = tap, 发对应 VK 给
    // 服务端触发模式切换 (如 lshift 切中英)。macOS 修饰键走 .flagsChanged, 不是 keyDown。
    private var pendingModVK: UInt32?    // 当前按住、待判定的修饰键 Win VK (nil=无)
    private var pendingModSawOther = false // 修饰键按住期间是否出现过其它键 (→ 非 tap)
    // 组字中 ⌥ 转发 (注释态切换) 的这次按住记账, 见 OptionKeyHold。
    private var optionHold = OptionKeyHold()
    // 候选显示时 Ctrl 如实上报的这次按住记账, 见 CtrlRawReport。
    private var ctrlRaw = CtrlRawReport()

    // 上次随 focusGained 上报给服务端的系统安全输入(密码框)状态。handle 每键与系统实时值
    // 比对, 翻转即补发 focusGained —— 补偿同一 IMKit client 内字段切换不触发 activateServer
    // 的盲区(见 syncSecureInputIfChanged)。
    private var lastReportedSecureInput = false

    /// 最近一次被 activateServer 的控制器。跨宿主切换时系统先激活新 client、后失活旧 client,
    /// 而 FocusLost 帧不带宿主标识: 旧控制器晚到的失活若照发, 会清掉新宿主刚上报的状态
    /// (如授权框的密码位)。失活时据此判断是否已被取代。
    private static weak var lastActivated: InputController?

    /// 登录以来左 / 右 / 其他鼠标键按下的总次数（触控板轻点计入左键）。
    private static func mouseDownCount() -> UInt32 {
        let src = CGEventSourceStateID.combinedSessionState
        return CGEventSource.counterForEventType(src, eventType: .leftMouseDown)
            &+ CGEventSource.counterForEventType(src, eventType: .rightMouseDown)
            &+ CGEventSource.counterForEventType(src, eventType: .otherMouseDown)
    }

    public override init!(server: IMKServer!, delegate: Any!, client inputClient: Any!) {
        super.init(server: server, delegate: delegate, client: inputClient)
        // 鼠标挪光标识别（补空格 / 英文后标点的「上一笔」作废）：只比鼠标按下次数，见
        // `InputSequenceTracker.mouseDownCount`。不需要辅助功能权限。
        router.seqTracker.mouseDownCount = InputController.mouseDownCount

        // 智能配对的宿主光标移动: kit 层 router 把意图上抛, 这里用 CGEvent 合成方向键。
        // 主线程 async 执行, 确保排在本轮 insertText (宿主已处理) 之后再发方向键。
        // 需辅助功能授权 (同命令直通车按键合成); 未授权则静默不动 (降级为不回退光标)。
        router.moveHostCursor = { move in
            DispatchQueue.main.async {
                let (key, count): (String, Int)
                switch move {
                case .left(let n): (key, count) = ("left", n)
                case .right(let n): (key, count) = ("right", n)
                }
                let combo = KeyComboPayload(key: key, modifiers: [])
                for _ in 0..<max(0, min(count, 64)) {
                    KeySynthesizer.tap(combo)
                }
            }
        }

        let path = BridgeEndpoints.requestSocket
        do {
            bridge = try BridgeClient(socketPath: path, ioTimeoutMs: Self.requestIOTimeoutMs)
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

    /// IME 获得某 client 焦点时由系统调用: 补发旧控制器欠的 FocusLost, 再报 FocusGained
    /// 与前台上下文。
    public override func activateServer(_ sender: Any!) {
        super.activateServer(sender)
        currentClient = sender as? (IMKTextInput & NSObjectProtocol)
        CandidatePanelHost.shared.activeResponder = self
        // 数字后智能标点的记账只对「这一个文本框里我们自己打出去的东西」有效，换了焦点
        // 就全部作废 —— 不清的话，在 A 框敲完 "3" 切到 B 框打标点会错出半角。
        router.digitTracker.reset()
        router.seqTracker.reset()
        // 激活即确保连上 (装完首次激活 / 重启后并发竞态时 init 那次可能没连上)。
        ensureConnected()
        // 旧控制器还没失活 (跨宿主切换系统先激活新 client): 它欠的 FocusLost 在这里先补上,
        // 保住「Lost 先于 Gained」—— 服务端只在失焦时清输入缓冲/候选/菜单态, 不补的话组字中
        // 切应用, 旧宿主里的编码会带进新宿主。它晚到的那次失活随后按过期丢弃, 见 performDeactivation。
        if let prev = Self.lastActivated, prev !== self { sendEmpty(UpstreamCmd.focusLost) }
        Self.lastActivated = self
        sendFocusGained()
        // 只在「刚切入本输入法」时补发 IME_ACTIVATED（Windows 同口径）。activateServer 切应用 /
        // 切文本框也会触发，那些情形补发会把用户手切的中英等状态拉回 input.default。
        let switchedIn = InputSourceMonitor.shared.consumeSwitchIn()
        switchInLog.info("activateServer switchedIn=\(switchedIn, privacy: .public)")
        if switchedIn { sendImeActivated() }
        sendFrontContext()
    }

    /// 输入源切换通知晚于 `activateServer` 到达时的补救：已有活动控制器就当场补发。
    /// 没有活动控制器（通知先到）则什么都不做，标记留给随后的 `activateServer` 消费。
    static func inputSourceSwitchedIn() {
        guard let active = lastActivated, InputSourceMonitor.shared.consumeSwitchIn() else { return }
        switchInLog.info("notification after activateServer, send IME_ACTIVATED now")
        active.sendImeActivated()
    }

    /// IME 失去焦点 (切到别的输入法/应用) 时由系统调用, 发 FocusLost。
    public override func deactivateServer(_ sender: Any!) {
        // 失焦即抹掉未提交的 marked text 并清本端组合状态: macOS 不会像 TSF 那样自动收回,
        // 服务端失焦也会清缓冲, 两端一致。必须在 super.deactivateServer 之前做, 此时 sender
        // 仍可接收 setMarkedText。
        performDeactivation(sender)
        super.deactivateServer(sender)
    }

    private func performDeactivation(_ sender: Any?) {
        if !composition.isEmpty {
            let imkClient = sender as? IMKTextInput
            let adapter = imkClient.map { IMKClientAdapter(imkClient: $0, controller: self) }
            router.applyClearComposition(client: adapter)
        }
        // 已有别的控制器在本控制器之后激活: 它激活时已替本控制器补过 FocusLost, 服务端此刻的
        // 焦点属于它, 这条失活已过期, 不报。
        if let newer = Self.lastActivated, newer !== self { return }
        Self.lastActivated = nil
        sendEmpty(UpstreamCmd.focusLost)
    }

    /// 发 FocusGained 帧, 携带 InputScope bitmask。读掉 ack, 失败仅 log。
    ///
    /// 密码框: 焦点进入密码框 / NSSecureTextField (浏览器密码框同样) 时系统启用「安全输入」
    /// (IsSecureEventInputEnabled 为真), 命中即置 IS_PASSWORD 位 (TSF 枚举 31), 服务端据此
    /// 强制英文直通, 与 Windows 同一套判定。
    private func sendFocusGained() {
        guard let bridge = bridge, bridge.isConnected else { return }
        // 读系统安全输入状态并记录, 供 handle 每键比对 (见 syncSecureInputIfChanged)。
        let secure = IsSecureEventInputEnabled()
        lastReportedSecureInput = secure
        let mask: UInt64 = secure ? Self.inputScopePasswordBit : 0
        let bundleID = currentClient?.bundleIdentifier() ?? ""
        do {
            try bridge.send(BinaryCodec.encodeFocusGainedFrame(
                clientToken: currentClientToken(),
                inputScopeMask: mask,
                bundleID: bundleID))
            _ = try bridge.readFrame()
        } catch {
            NSLog("WindInput[sendFocusGained] io error: \(error)")
            reconnect()
        }
    }

    /// 当前焦点 client 的 token；FocusGained 与 IME_ACTIVATED 必须使用同一个。
    private func currentClientToken() -> UInt64 {
        let bundleID = currentClient?.bundleIdentifier() ?? ""
        return Self.clientToken(pid: hostPid(for: bundleID), client: currentClient)
    }

    /// 发 IME_ACTIVATED。服务端把它当异步命令（不写 ack），故这里只 send、不 readFrame，
    /// 否则会把后续按键的响应读错位。
    private func sendImeActivated() {
        guard let bridge = bridge, bridge.isConnected else { return }
        do {
            try bridge.send(BinaryCodec.encodeTokenFrame(
                cmd: UpstreamCmd.imeActivated,
                clientToken: currentClientToken(),
                async: true))
        } catch {
            NSLog("WindInput[sendImeActivated] io error: \(error)")
            reconnect()
        }
    }

    /// 宿主 app 的标识, 供服务端做「焦点是否跨应用切入」判定 (clientToken 高 32 位)。
    ///
    /// 一律取 bundleID 的稳定散列, 不取真实 pid: `frontmostApplication` 未必是持有文本框的
    /// app (Spotlight、非前台面板), 混用 pid 与散列会让同一 app 得到两个键, 按应用记忆时不时
    /// 重置。服务端在 macOS 上只拿它当键 (进程名由 bundleID 另送), 散列跨宿主重启也不变。
    private func hostPid(for bundleID: String) -> UInt32 {
        return Self.stableHash(bundleID)
    }

    /// FNV-1a 32 位散列; 非空串结果恒非 0, 空串返回 0 (服务端以 pid==0 表示「未知宿主」并跳过
    /// 按应用逻辑)。
    private static func stableHash(_ s: String) -> UInt32 {
        if s.isEmpty { return 0 }
        var h: UInt32 = 2_166_136_261
        for b in s.utf8 {
            h = (h ^ UInt32(b)) &* 16_777_619
        }
        return h == 0 ? 1 : h
    }

    /// clientToken = pid(高 32) | client 实例标识(低 32)。
    /// 低位取 IMKit client 的对象身份: 同一文本框重复聚焦得同一 token, 换文本框则不同 ——
    /// 对齐 Windows 端「pid | docMgrId」的语义 (服务端据此区分同应用内的焦点跳转)。
    private static func clientToken(pid: UInt32, client: (IMKTextInput & NSObjectProtocol)?) -> UInt64 {
        let low: UInt32 = client.map {
            UInt32(truncatingIfNeeded: UInt(bitPattern: ObjectIdentifier($0).hashValue))
        } ?? 0
        return (UInt64(pid) << 32) | UInt64(low)
    }

    /// 发前台上下文帧: client app bundle id / 聚焦窗口标题 / 选中文本, 供命令直通车
    /// app()/title()/sel() 取值。聚焦时快照 —— app()/title() 稳定; sel() 反映聚焦时选区
    /// (best-effort, 部分 app 不支持取选中文本, 选区随后变化不会刷新)。
    ///
    /// 窗口标题与发送一律挪出主线程: 取标题走 AX, 是到目标 app 的同步跨进程调用, 而激活时
    /// 那个 app 正忙着重绘, 每次切换能把主线程堵约 0.5 秒 (还会经 runloop 重入下一次激活),
    /// 表现为切换应用后刚开始输入很卡。快照只有命令直通车读, 异步晚到几十毫秒无妨。
    private func sendFrontContext() {
        let app = currentClient?.bundleIdentifier() ?? ""
        let pid = NSWorkspace.shared.frontmostApplication?.processIdentifier ?? 0
        // 选中文本只能在主线程取 (IMKTextInput 的约定), 但不在 activateServer 里取:
        // `selectedRange()` 同样是跨进程调用。排到下一轮主线程事件, 激活流程即刻返回。
        DispatchQueue.main.async { [weak self] in
            let sel = self?.selectedClientText() ?? ""
            Self.frontCtxQueue.async {
                let title = pid > 0 ? Self.frontmostWindowTitle(pid: pid) : ""
                Self.sendFrontContextFrame(app: app, title: title, sel: sel)
            }
        }
    }

    /// 前台上下文的专用串行队列 + 专用连接。
    ///
    /// 不能复用 `bridge`: 那条连接由主线程发按键帧并同步读响应, 从别的线程往里插一帧
    /// 会与按键帧交错、把两边的读写配对错开。专用连接只被本队列碰, 天然串行。
    private static let frontCtxQueue = DispatchQueue(label: "to.feng.windinput.frontctx")
    /// 仅在 `frontCtxQueue` 上访问。
    private static var frontCtxBridge: BridgeClient?

    private static func sendFrontContextFrame(app: String, title: String, sel: String) {
        if frontCtxBridge == nil {
            frontCtxBridge = try? BridgeClient(socketPath: BridgeEndpoints.requestSocket,
                                               ioTimeoutMs: requestIOTimeoutMs)
        }
        guard let c = frontCtxBridge else { return }
        do {
            try c.send(BinaryCodec.encodeFrontContextFrame(app: app, title: title, sel: sel))
            _ = try c.readFrame()
        } catch {
            // 连接陈旧 (服务重启) → 丢弃, 下次聚焦时重建。不重发: 上下文是快照, 丢一次无害。
            NSLog("WindInput[sendFrontContext] io error: \(error)")
            frontCtxBridge?.close()
            frontCtxBridge = nil
        }
    }

    /// 取当前 client 选中文本 (IMKit selectedRange + attributedSubstring); 无选区/不支持返回空。
    private func selectedClientText() -> String {
        guard let client = currentClient else { return "" }
        let range = client.selectedRange()
        guard range.length > 0, range.location != NSNotFound else { return "" }
        if let attr = client.attributedSubstring(from: range) {
            return attr.string
        }
        return ""
    }

    /// 取指定进程聚焦窗口的标题 (AX)。需辅助功能授权 (本 IME 已为合成按键/移动光标申请);
    /// 未授权/取不到返回空, 不弹授权框。
    ///
    /// 只在 `frontCtxQueue` 上调用: 这是到目标 app 的同步跨进程调用, 目标忙时会一直等。
    private static func frontmostWindowTitle(pid: pid_t) -> String {
        let axApp = AXUIElementCreateApplication(pid)
        // 默认超时是 6 秒。窗口标题是「取不到就算了」的锦上添花, 没有任何理由等那么久 ——
        // 挂起/无响应的目标 app 会把本队列连同后续每一次聚焦快照一起拖住。
        AXUIElementSetMessagingTimeout(axApp, axMessagingTimeout)
        var winRef: CFTypeRef?
        guard AXUIElementCopyAttributeValue(axApp, kAXFocusedWindowAttribute as CFString, &winRef) == .success,
              let win = winRef else { return "" }
        // swiftlint:disable:next force_cast
        let window = win as! AXUIElement
        AXUIElementSetMessagingTimeout(window, axMessagingTimeout)
        var titleRef: CFTypeRef?
        guard AXUIElementCopyAttributeValue(window, kAXTitleAttribute as CFString, &titleRef) == .success
        else { return "" }
        return (titleRef as? String) ?? ""
    }

    /// AX 跨进程调用的超时上限 (秒)。
    private static let axMessagingTimeout: Float = 0.5

    /// IS_PASSWORD 位 (TSF InputScope 枚举 31) 的 bitmask, 与服务端协调器的判定对齐。
    private static let inputScopePasswordBit: UInt64 = UInt64(1) << 31

    /// 每键检测系统安全输入(密码框)状态是否相对上次上报翻转, 变了就补发一帧 focusGained。
    /// activateServer 是 client 级的, 浏览器整个网页是一个 client, 网页内切到密码框不触发它;
    /// IsSecureEventInputEnabled 是全局实时状态, 每键比对一次 (轻量系统调用)。
    private func syncSecureInputIfChanged() {
        if IsSecureEventInputEnabled() != lastReportedSecureInput {
            sendFocusGained()
        }
    }

    /// 发一个无 payload 的上行帧 (focusLost/toggleMode 等), 读掉 ack。失败仅 log。
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

    /// 处理 .flagsChanged: 修饰键按下记录待判定; 抬起且其间无其它键 = tap, 发 VK 给服务端。
    /// ⌥ 另走 handleOptionChanged。返回是否吃掉事件。
    private func handleFlagsChanged(_ event: NSEvent, client sender: Any!) -> Bool {
        optionHold.note(modifiers: KeyHandler.toModifiers(event.modifierFlags))
        guard let (vk, mask) = Self.modifierInfo(forKeyCode: event.keyCode) else {
            pendingModVK = nil
            ctrlRaw.noteOther() // Command / Fn 等: Ctrl+⌘ 不算单击
            return false
        }
        let pressed = (event.modifierFlags.rawValue & mask) != 0
        let isCtrl = vk == 0xA2 || vk == 0xA3
        if pressed && !isCtrl { ctrlRaw.noteOther() }
        if vk == 0xA4 || vk == 0xA5 {
            // ⌥ 不做单击切换模式; 按住期间同样作废 Shift/Ctrl 的单击判定。
            pendingModVK = nil
            return handleOptionChanged(vk, pressed: pressed, flags: event.modifierFlags, sender: sender)
        }
        if isCtrl && pressed {
            let composing = OptionKeyHold.inputSession(
                hasComposition: !composition.isEmpty,
                candidatesVisible: CandidatePanelHost.shared.candidatesVisible)
            if ctrlRaw.down(vk: vk, composing: composing) {
                pendingModVK = nil // 这次按住走如实上报, 不再判单击 (一次按住只走一条路)
                sendCtrlRaw(vk, .modDown, flags: event.modifierFlags, notTap: false, sender: sender)
                return false
            }
        }
        if isCtrl && !pressed, let notTap = ctrlRaw.up(vk: vk) {
            pendingModVK = nil
            sendCtrlRaw(vk, .modUp, flags: event.modifierFlags, notTap: notTap, sender: sender)
            return false
        }
        if pressed {
            pendingModVK = vk
            pendingModSawOther = false
        } else {
            if pendingModVK == vk && !pendingModSawOther {
                sendModifierTap(vk, sender: sender)
            }
            pendingModVK = nil
        }
        return false
    }

    /// 候选显示时 Ctrl 的按下 / 松开如实上报 (单击由服务端判, 见 CtrlRawReport)。
    /// 应用回包 (单击可能切中英 / 选词 / 上屏编码), 但 Ctrl 本身永不吃。
    private func sendCtrlRaw(_ vk: UInt32, _ type: KeyEventType, flags: NSEvent.ModifierFlags,
                             notTap: Bool, sender: Any!) {
        guard ensureConnected(), let bridge = bridge else { return }
        if type == .modUp {
            sendCaretUpdateIfAvailable(client: sender as? IMKTextInput) // 同 sendModifierTap
        }
        keySeq &+= 1
        let frame = BinaryCodec.encodeKeyEventFrame(KeyEventPayload(
            keyCode: vk, scanCode: 0, modifiers: KeyHandler.toModifiers(flags) | 0x0002,
            eventType: type,
            toggles: CtrlRawReport.toggles(capsLock: flags.contains(.capsLock), notTap: notTap),
            eventSeq: keySeq, prevChar: router.digitTracker.prevChar,
            seqChar: router.seqTracker.seqChar))
        do {
            try bridge.send(frame)
            let resp = try bridge.readFrame()
            _ = applyResponse(resp, sender: sender)
        } catch {
            NSLog("WindInput[ctrlRaw] vk=\(vk) io error: \(error)")
            reconnect()
        }
    }

    /// 组字中 ⌥ 的按下 / 松开转发给服务切换注释态, 回包 Consumed 才吃。
    /// 修饰位带上这次按住期间按过的 Shift/Ctrl/Command (协调器 `handle_comment_alt`)。
    private func handleOptionChanged(_ vk: UInt32, pressed: Bool, flags: NSEvent.ModifierFlags,
                                     sender: Any!) -> Bool {
        let composing = OptionKeyHold.inputSession(
            hasComposition: !composition.isEmpty,
            candidatesVisible: CandidatePanelHost.shared.candidatesVisible)
        let forward: Bool
        if pressed {
            forward = composing
            optionHold.down(forwarded: forward)
        } else {
            forward = optionHold.shouldForwardUp(composing: composing)
        }
        let mods = KeyHandler.toModifiers(flags) | optionHold.stickyMods
        if !pressed { optionHold.up() }
        guard forward, ensureConnected(), let bridge = bridge else { return false }
        keySeq &+= 1
        let frame = BinaryCodec.encodeKeyEventFrame(KeyEventPayload(
            keyCode: vk, scanCode: 0, modifiers: mods, eventType: pressed ? .down : .up,
            eventSeq: keySeq, prevChar: router.digitTracker.prevChar,
            seqChar: router.seqTracker.seqChar))
        do {
            try bridge.send(frame)
            let resp = try bridge.readFrame()
            return applyResponse(resp, sender: sender)
        } catch {
            NSLog("WindInput[option] vk=\(vk) io error: \(error)")
            reconnect()
            return false
        }
    }

    /// mac keyCode → (Win VK, NSEvent.ModifierFlags 掩码)。可作模式切换的修饰键, 外加 ⌥ (注释态切换)。
    private static func modifierInfo(forKeyCode kc: UInt16) -> (UInt32, UInt)? {
        switch kc {
        case 56: return (0xA0, NSEvent.ModifierFlags.shift.rawValue)   // 左 Shift → VK_LSHIFT
        case 60: return (0xA1, NSEvent.ModifierFlags.shift.rawValue)   // 右 Shift → VK_RSHIFT
        // Ctrl 用设备相关位分左右 (NX_DEVICELCTLKEYMASK / NX_DEVICERCTLKEYMASK): 笼统的 .control
        // 在另一侧还按着时松开这一侧仍为真, 判不出松开。
        case 59: return (0xA2, 0x0001) // 左 Ctrl → VK_LCONTROL
        case 62: return (0xA3, 0x2000) // 右 Ctrl → VK_RCONTROL
        case 58: return (0xA4, NSEvent.ModifierFlags.option.rawValue)  // 左 ⌥ → VK_LMENU
        case 61: return (0xA5, NSEvent.ModifierFlags.option.rawValue)  // 右 ⌥ → VK_RMENU
        default: return nil
        }
    }

    /// 发一个修饰键 VK 的 KeyEvent (eventType=up) 给服务, 触发模式切换; 应用其响应。
    /// 用 key-up: 协调器的 toggle 键(Shift/Ctrl 单击切中英)仅在 EVENT_KEY_UP 分支处理
    /// (对齐 TSF「仅 keyUp 转发 toggle 键」约定); 发 .down 会落不进该分支 → 不切换。
    private func sendModifierTap(_ vk: UInt32, sender: Any!) {
        guard let bridge = bridge, bridge.isConnected else { return }
        // 模式切换 (Shift/Ctrl tap) 通常无 composition, 先刷新 caret 让状态气泡锚到当前
        // 插入点 (否则会显示在上一次组字的旧位置)。
        sendCaretUpdateIfAvailable(client: sender as? IMKTextInput)
        keySeq &+= 1
        // prevChar 如实带上: 修饰键 tap 不走标点通路, 但日志里恒为 0 正是「客户端没接
        // prevChar」的故障特征, 带上免得排查时误判。
        let frame = BinaryCodec.encodeKeyEventFrame(KeyEventPayload(
            keyCode: vk, scanCode: 0, modifiers: 0, eventType: .up, eventSeq: keySeq,
            prevChar: router.digitTracker.prevChar,
            seqChar: router.seqTracker.seqChar))
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
    /// 才能收到修饰键 (Shift/Ctrl) 变化, 做单击切换检测; keyUp 只认服务端吃掉过按下的功能键
    /// (见 eatenFnKeyCode), 其余一律交还宿主。
    public override func recognizedEvents(_ sender: Any!) -> Int {
        return Int(NSEvent.EventTypeMask.keyDown.rawValue | NSEvent.EventTypeMask.flagsChanged.rawValue
            | NSEvent.EventTypeMask.keyUp.rawValue)
    }

    /// 服务端吃掉过按下的功能键 (macOS 只映射了 F1): 它的松开也报给服务端 —— 注释切换键选
    /// 功能键 + 按住档时, 服务端靠松开收起详细态。按下吃了, 松开同样吃掉。
    private var eatenFnKeyCode: UInt16?

    private func noteEatenFnKey(_ event: NSEvent, consumed: Bool, hostShortcut: Bool) {
        let vk = KeyHandler.toWindowsVK(event.keyCode)
        if consumed && !hostShortcut && (0x70...0x7B).contains(vk) {
            eatenFnKeyCode = event.keyCode
        }
    }

    private func handleKeyUp(_ event: NSEvent, client sender: Any!) -> Bool {
        guard let code = eatenFnKeyCode, code == event.keyCode else { return false }
        eatenFnKeyCode = nil
        guard let bridge = bridge else { return true }
        keySeq &+= 1
        if let frame = KeyHandler.encodeKeyEvent(event, seq: keySeq) {
            _ = try? sendAndApply(frame, on: bridge, sender: sender)
        }
        return true
    }

    // MARK: - 系统输入菜单 (点击菜单栏输入源图标弹出)

    /// IMKit 在「文本输入菜单」需要绘制时调用 (每次打开都会问一次, 故可动态反映当前状态)。
    /// 返回的菜单项会被系统追加到输入源列表下方 —— 这是标准 Mac 输入法的菜单接入方式
    /// (Rime/Squirrel、搜狗等同此), 复用与候选框右键、菜单栏指示器完全一致的统一菜单树。
    ///
    /// 派发: 系统输入菜单由 IMK 在另一上下文绘制, 选中项经 doCommandBySelector 回到本进程,
    /// 故菜单项 target=nil + action=imkMenuCommand:, 菜单 id 经 NSMenuItem.tag 回传。
    public override func menu() -> NSMenu! {
        guard let items = CandidatePanelHost.shared.unifiedMenuItems(), !items.isEmpty else {
            return imkFallbackMenu()
        }
        // IMK 对嵌套子菜单的叶子回传的是子菜单首项的 tag, 故拍平为顶层带「父·」前缀的项, 各项 tag
        // 即真实菜单 id。候选框右键 / 菜单栏指示器走进程内投递, 仍用原始嵌套树。
        return imkMenuBuilder.build(flattenForIMK(items), dispatch: .imkCommand(action: #selector(imkMenuCommand(_:))))
    }

    /// 把统一菜单树拍平供 IMK 输入源菜单用：子菜单父项的子项提升到顶层，标签加「父·」前缀；
    /// 分隔线原样保留。递归处理多级子菜单。
    private func flattenForIMK(_ items: [MenuItemData], prefix: String = "") -> [MenuItemData] {
        var out: [MenuItemData] = []
        for it in items {
            if !it.children.isEmpty {
                let childPrefix = prefix.isEmpty ? it.label : "\(prefix)·\(it.label)"
                out.append(contentsOf: flattenForIMK(it.children, prefix: childPrefix))
            } else if it.separator {
                out.append(it)
            } else {
                let label = prefix.isEmpty ? it.label : "\(prefix)·\(it.label)"
                out.append(MenuItemData(id: it.id, label: label, separator: false,
                                        checked: it.checked, disabled: it.disabled, children: []))
            }
        }
        return out
    }

    /// 统一菜单项被选中: IMK 经 doCommandBySelector 调用本方法, sender 是 infoDictionary
    /// (含 kIMKCommandMenuItemName = 被点的 NSMenuItem)。读其 tag (统一菜单 id) 回发
    /// CmdMenuAction 由服务端派发, 与其它两处菜单同一路径。
    @objc public func imkMenuCommand(_ sender: Any!) {
        guard let info = sender as? NSDictionary,
              let item = info[kIMKCommandMenuItemName as Any] as? NSMenuItem else { return }
        CandidatePanelHost.shared.sendMenuAction(item.tag)
    }

    /// 服务不可达时的兜底菜单: 仅「设置…」(直接拉起设置应用, 不依赖服务)。避免空菜单。
    private func imkFallbackMenu() -> NSMenu {
        let menu = NSMenu()
        menu.autoenablesItems = false
        let item = NSMenuItem(title: "设置…", action: #selector(imkOpenSettings(_:)), keyEquivalent: "")
        item.target = nil
        menu.addItem(item)
        return menu
    }

    @objc public func imkOpenSettings(_ sender: Any!) {
        ModeStatusController.shared.openSettings(arguments: [])
    }

    public override func handle(_ event: NSEvent!, client sender: Any!) -> Bool {
        guard let event = event else { return false }

        // 记录当前焦点 client + 把自己登记为 active responder, 让鼠标选词的
        // push 通道 commit (CandidatePanelHost 收到) 能路由回这个文本框。
        currentClient = sender as? (IMKTextInput & NSObjectProtocol)
        CandidatePanelHost.shared.activeResponder = self

        // 修饰键变化 (Shift/Ctrl 等): 做 tap 检测, 不消费事件本身; 组字中的 ⌥ 看回包。
        if event.type == .flagsChanged {
            return handleFlagsChanged(event, client: sender)
        }
        if event.type == .keyUp {
            return handleKeyUp(event, client: sender)
        }
        guard event.type == .keyDown else { return false }
        // 任意真实按键出现 → 取消当前修饰键 tap 判定 (Shift+X 不算 tap)。
        pendingModSawOther = true
        ctrlRaw.noteOther()
        guard ensureConnected(), let bridge = bridge else {
            NSLog("WindInput[handle] bridge not connected (重连失败), pass through")
            return passThroughToHost(event)
        }

        // 发本键前同步系统安全输入状态, 让服务端处理本键时密码框判定已最新; 同连接串行,
        // 补发的 focusGained 必先于本键被处理。
        syncSecureInputIfChanged()

        keySeq &+= 1
        // prevChar 取自本端记账 (macOS 无现读文档通路), 供服务端判「数字后智能标点」。
        // 必须在处理本键之前取: 本键的响应回来后 tracker 就被更新成新的光标前字符了。
        guard let frame = KeyHandler.encodeKeyEvent(event, seq: keySeq,
                                                    prevChar: router.digitTracker.prevChar,
                                                    seqChar: router.seqTracker.seqChar) else {
            // VK 未映射 → 这一键 IMKit 自行透传给宿主, 同样要记账。
            return passThroughToHost(event)
        }

        // 无 composition 时本端 caret 可能是上一次组字的旧位置 (换行/移动光标后未更新)。
        // 处理本键前先刷新一次, 让状态气泡/首帧候选锚到当前真实插入点。
        // 组字中 caret 由下方 (composition 非空) 分支持续更新, 无需在此重复。
        if composition.isEmpty {
            sendCaretUpdateIfAvailable(client: sender as? IMKTextInput)
        }

        // 宿主快捷键组合 (⌘/⌃/⌥ + 键): 照常问服务 (热键表在那边), 但服务回「清组合」时
        // 这一键仍要交还宿主, 否则 ⌘C/⌘V 在组字期间被吞 —— 见 KeyHandler.isHostShortcut。
        let hostShortcut = KeyHandler.isHostShortcut(event.modifierFlags)

        do {
            let consumed = try sendAndApply(frame, on: bridge, sender: sender, hostShortcut: hostShortcut)
            noteEatenFnKey(event, consumed: consumed, hostShortcut: hostShortcut)
            return consumed ? true : passThroughToHost(event)
        } catch {
            // 服务重启后这条连接已死 (EPIPE / EOF): 重连并用新连接重试当前键一次, 第一个键
            // 就自愈、不丢字。重连或重试仍失败才透传, 下一键再试。
            NSLog("WindInput[handle] bridge io error: \(error), 重连后重试本键")
            reconnect()
            // 超时 = 服务卡住而非连接陈旧, 重试只会再等一轮。
            if error as? IPCError == .timeout { return passThroughToHost(event) }
            // 注意: 上方 guard 把属性 bridge 遮蔽为非可选局部量, 这里须取重连后的 self.bridge。
            guard let fresh = self.bridge else { return passThroughToHost(event) }
            do {
                let consumed = try sendAndApply(frame, on: fresh, sender: sender, hostShortcut: hostShortcut)
                noteEatenFnKey(event, consumed: consumed, hostShortcut: hostShortcut)
                return consumed ? true : passThroughToHost(event)
            } catch {
                NSLog("WindInput[handle] 重连后重试仍失败: \(error)")
                return passThroughToHost(event)
            }
        }
    }

    /// 本键最终交回宿主: 记账后返回 false (handle 的「未消费」)。
    ///
    /// 宿主拿这一键改文档, 我们看不见结果, 只能按「这一键产出什么」推断光标前字符
    /// (见 [`SmartPunctDigitTracker`])。每一条透传出口都要走这里, 漏一条就是某种情形下
    /// 数字后的标点仍出中文。
    ///
    /// 上屏侧 (经引擎写进文档的文本) 由 `BridgeResponseRouter.insertCommitted` 记, 两侧成对。
    @discardableResult
    private func passThroughToHost(_ event: NSEvent) -> Bool {
        router.digitTracker.noteKeyPassthrough(
            vk: KeyHandler.toWindowsVK(event.keyCode),
            modifiers: KeyHandler.toModifiers(event.modifierFlags))
        router.seqTracker.noteKeyPassthrough(
            vk: KeyHandler.toWindowsVK(event.keyCode),
            modifiers: KeyHandler.toModifiers(event.modifierFlags))
        return false
    }

    /// 在指定连接上发一帧、读响应并应用; composition 非空时上报 caret。
    /// 抽出供 handle 的「首发 + 重连重试」两条路径共用。
    private func sendAndApply(_ frame: Data, on bridge: BridgeClient, sender: Any?,
                              hostShortcut: Bool = false) throws -> Bool {
        try bridge.send(frame)
        let resp = try bridge.readFrame()
        let consumed = applyResponse(resp, sender: sender, hostShortcut: hostShortcut)
        // composition 启动/更新后上报当前 caret 屏幕位置, 让候选框/Toast/光标跟随有正确锚点。
        // 仅在 marked text 非空时发。
        if !composition.isEmpty {
            sendCaretUpdateIfAvailable(client: sender as? IMKTextInput)
        }
        return consumed
    }

    // MARK: - Caret update

    /// 从 IMKTextInput 拿 caret 屏幕坐标, 转换为 wire top-left 坐标后发 CmdCaretUpdate.
    /// 不抛错, 失败仅 log.
    internal func sendCaretUpdateIfAvailable(client: IMKTextInput?) {
        guard let client = client, let bridge = bridge, bridge.isConnected else { return }

        // 优先取真实光标所在行 rect（markedRange 组字中 / selectedRange 非组字）。
        var rect = NSRect.zero
        let markedRange = client.markedRange()
        let loc: Int
        if markedRange.location != NSNotFound {
            loc = markedRange.location
        } else {
            let sel = client.selectedRange()
            loc = sel.location != NSNotFound ? sel.location : 0
        }
        _ = client.attributes(forCharacterIndex: loc, lineHeightRectangle: &rect)

        // 备用 1: 部分 app 仅支持 index=0 查询（如某些 NSTextField 实现）。
        if rect.size.height == 0 && loc != 0 {
            _ = client.attributes(forCharacterIndex: 0, lineHeightRectangle: &rect)
        }
        // 备用 2: attributes API 完全不可用时，用鼠标光标位置近似 caret 位置。
        // 比固定 (0,0) 好：至少候选框不会卡在屏幕左上角。
        if rect.size.height == 0 {
            let mouse = NSEvent.mouseLocation  // Cocoa bottom-left 屏幕坐标
            rect = NSRect(x: mouse.x, y: mouse.y, width: 1, height: 16)
        }

        // 参照屏必须与浮窗落位那边同源（`PanelGeometry.referenceScreen`，带菜单栏的主屏），
        // 否则两屏高度不同时会变成候选窗相对光标的垂直偏移。
        let screenHeight = PanelGeometry.referenceHeight
        guard screenHeight > 0 else { return }

        let (x, y, h) = CaretCoords.caretRectToWire(rect, screenHeight: screenHeight)
        let frame = BinaryCodec.encodeCaretUpdateFrame(x: x, y: y, height: h)
        do {
            try bridge.send(frame)
            _ = try bridge.readFrame()   // 服务端一律返 ack，必须读掉避免堆积
        } catch {
            NSLog("WindInput[caretUpdate] send/read error: \(error)")
        }
    }

    // MARK: - Response routing

    /// 把服务端返回的 bridge 帧路由到 IMKTextInput 协议方法. 委托给 BridgeResponseRouter
    /// (在 WindInputKit 里, 不依赖 IMKit, 便于 swift test 用 mock 驱动).
    internal func applyResponse(_ frame: Frame, sender: Any?, hostShortcut: Bool = false) -> Bool {
        let imkClient = sender as? IMKTextInput
        let adapter = imkClient.map { IMKClientAdapter(imkClient: $0, controller: self) }
        return router.apply(frame, to: adapter, hostShortcut: hostShortcut)
    }

    /// 见 `PushResponder.invalidateDigitTracking`。合成按键改了文档、我们不知道改成什么,
    /// 作废记账回落默认行为 (标点按中文出), 好过拿着陈旧的数字错出半角。
    public func invalidateDigitTracking() {
        router.digitTracker.reset()
        router.seqTracker.reset()
    }

    /// 应用 push 通道帧 (鼠标选词的 commit/composition 异步到达, 非 KeyEvent 同步响应)。
    /// 路由到当前焦点 client。在主线程调用 (CandidatePanelHost 已 dispatch)。
    public func applyPushResponse(_ frame: Frame) {
        guard let client = currentClient else {
            NSLog("WindInput[applyPushResponse] no current client, drop cmd=\(frame.cmd)")
            return
        }
        _ = router.apply(frame, to: IMKClientAdapter(imkClient: client, controller: self))
        if !composition.isEmpty {
            sendCaretUpdateIfAvailable(client: client)
        }
    }

    // MARK: - IMKit Adapter (把 IMKTextInput 桥接到 TextInputClient)

    /// IMKTextInput → TextInputClient 的适配器, 让 BridgeResponseRouter (在
    /// WindInputKit 不依赖 IMKit 的子库里) 也能调到 IMKit 真客户端.
    private final class IMKClientAdapter: TextInputClient {
        let imkClient: IMKTextInput
        /// 弱持: adapter 是每次响应现造的短命对象, 强持会与 controller 成环。
        /// 仅用于取 `markForStyle:atRange:`; 取不到时 `MarkedTextAttributes` 自行兜底。
        private weak var controller: IMKInputController?

        init(imkClient: IMKTextInput, controller: IMKInputController?) {
            self.imkClient = imkClient
            self.controller = controller
        }

        func insertText(_ text: String, replacementRange: NSRange) {
            imkClient.insertText(text, replacementRange: replacementRange)
        }

        /// # 必须传 NSAttributedString, 不能传裸 String
        ///
        /// 传裸 String 时 IMKit 会替我们合成默认分句 (整串) 并把转发给宿主的
        /// `selectedRange` 覆写成 `{0, 全长}`, 宿主把组合内光标画在最前面。现象、真机判据
        /// (改完须复验) 与影响面见 `MarkedTextAttributes` 的文件头注释。
        func setMarkedText(_ text: String, selectionRange: NSRange, replacementRange: NSRange) {
            imkClient.setMarkedText(Self.markedText(text, controller: controller),
                                    selectionRange: selectionRange,
                                    replacementRange: replacementRange)
        }

        /// 组合串 → 带分句属性的 NSAttributedString。
        ///
        /// 样式取 `kTSMHiliteRawText`(未转换文本: 细下划线、不反白) 而非
        /// `kTSMHiliteSelectedRawText`(整段反白) —— 后者是鼠须管那种「整串高亮」的观感,
        /// 我们要的是系统拼音那种「下划线 + 光标跟随」。
        private static func markedText(_ text: String,
                                       controller: IMKInputController?) -> NSAttributedString {
            let full = NSRange(location: 0, length: (text as NSString).length)
            // 空串 = 清除组合。此时无分句可言, 属性反而可能让宿主多走一遍无谓的重排。
            guard full.length > 0 else { return NSAttributedString(string: text) }
            // `markForStyle:` 返回 [AnyHashable: Any], 逐项转 key 而不是整体 `as?`:
            // 整体转换失败时会静默得到 nil, 于是丢掉系统按主题算好的样式、只剩兜底,
            // 而这种退化在界面上看不出来。逐项转则至少保住能转的那些。
            var base: [NSAttributedString.Key: Any] = [:]
            if let raw = controller?.mark(forStyle: Int(kTSMHiliteRawText), at: full) {
                for (key, value) in raw {
                    guard let name = key as? String else { continue }
                    base[NSAttributedString.Key(name)] = value
                }
            }
            return NSAttributedString(string: text,
                                      attributes: MarkedTextAttributes.ensureClauseSegment(base))
        }

        func selectedRange() -> NSRange {
            imkClient.selectedRange()
        }
    }

    // MARK: - Reconnect

    /// 确保 bridge 已连接; 未连/断开则尝试 (重)连, 返回是否已连。
    ///
    /// 控制器可能在服务 socket 就绪前创建 (装完首次激活、登录时与服务 LaunchAgent 并发),
    /// init() 那次连接失败后该实例就一直直通英文; 在 activate/handle 入口懒重连让同一实例
    /// 自愈。已连时是廉价 no-op。
    @discardableResult
    private func ensureConnected() -> Bool {
        if let b = bridge, b.isConnected { return true }
        bridge?.close()
        do {
            bridge = try BridgeClient(socketPath: BridgeEndpoints.requestSocket, ioTimeoutMs: Self.requestIOTimeoutMs)
            NSLog("WindInput[ensureConnected] bridge (重)连成功")
            return true
        } catch {
            bridge = nil
            return false
        }
    }

    private func reconnect() {
        bridge?.close()
        bridge = nil
        do {
            bridge = try BridgeClient(socketPath: BridgeEndpoints.requestSocket, ioTimeoutMs: Self.requestIOTimeoutMs)
            NSLog("WindInput[reconnect] bridge reconnected")
        } catch {
            NSLog("WindInput[reconnect] still down: \(error)")
        }
    }
}

// PushResponder: 让 CandidatePanelHost 能把 push 通道 commit 路由到此 controller。
extension InputController: PushResponder {}
