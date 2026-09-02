import Cocoa
import InputMethodKit
import WindInputKit
import Carbon.HIToolbox // IsSecureEventInputEnabled (密码框/安全输入检测)
import ApplicationServices // AXUIElement (前台窗口标题, 命令直通车 title())

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

    // request 连接 I/O 超时 (毫秒): 服务卡死/重启时避免同步 readFrame 在 IMKit 主线程
    // 无限阻塞 (表现为输入法整体无响应)。正常 UDS roundtrip <1ms; 超时后 catch →
    // reconnect, 下一键用新连接自愈。push 连接不设此超时 (见 BridgeClient.ioTimeoutMs)。
    private static let requestIOTimeoutMs = 2000

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
    // Go 触发模式切换 (如 lshift 切中英)。macOS 修饰键走 .flagsChanged, 不是 keyDown。
    private var pendingModVK: UInt32?    // 当前按住、待判定的修饰键 Win VK (nil=无)
    private var pendingModSawOther = false // 修饰键按住期间是否出现过其它键 (→ 非 tap)

    // 上次随 focusGained 上报给 Go 的系统安全输入(密码框)状态。handle 每键与系统实时值
    // 比对, 翻转即补发 focusGained —— 补偿同一 IMKit client 内字段切换不触发 activateServer
    // 的盲区(见 syncSecureInputIfChanged)。
    private var lastReportedSecureInput = false

    public override init!(server: IMKServer!, delegate: Any!, client inputClient: Any!) {
        super.init(server: server, delegate: delegate, client: inputClient)

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

    /// IME 获得某 client 焦点时由系统调用。发 FocusGained 让 Go 端置 imeActivated=true,
    /// 从而驱动工具栏 reducer 显示模式指示器 (CmdModeStatus → 菜单栏)。
    public override func activateServer(_ sender: Any!) {
        super.activateServer(sender)
        currentClient = sender as? (IMKTextInput & NSObjectProtocol)
        CandidatePanelHost.shared.activeResponder = self
        // 激活即确保连上 (装完首次激活 / 重启后并发竞态时 init 那次可能没连上)。
        ensureConnected()
        sendFocusGained()
        sendFrontContext()
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
            let adapter = imkClient.map { IMKClientAdapter(imkClient: $0, controller: self) }
            router.applyClearComposition(client: adapter)
        }
        sendEmpty(UpstreamCmd.focusLost)
        super.deactivateServer(sender)
    }

    /// 发 FocusGained 帧, 携带 InputScope bitmask。读掉 ack, 失败仅 log。
    ///
    /// 密码框适配 (对齐 Win 36614ae): macOS 焦点进入密码框/NSSecureTextField 时, AppKit
    /// 会启用系统"安全输入" (IsSecureEventInputEnabled 返回真; 浏览器 <input type=password>
    /// 同样置位)。命中即把 InputScope 置上 IS_PASSWORD 位 (TSF 枚举 31), Go 端 coordinator
    /// 据此对密码框强制英文半角直通 (sensitiveFieldActive), 与 Win 端共用同一套判定逻辑。
    /// 非密码框时 mask=0, 行为与原空帧一致。
    private func sendFocusGained() {
        guard let bridge = bridge, bridge.isConnected else { return }
        // 读系统安全输入状态并记录, 供 handle 每键比对 (见 syncSecureInputIfChanged)。
        let secure = IsSecureEventInputEnabled()
        lastReportedSecureInput = secure
        let mask: UInt64 = secure ? Self.inputScopePasswordBit : 0
        let bundleID = currentClient?.bundleIdentifier() ?? ""
        do {
            try bridge.send(BinaryCodec.encodeFocusGainedFrame(
                clientToken: Self.clientToken(pid: hostPid(for: bundleID), client: currentClient),
                inputScopeMask: mask,
                bundleID: bundleID))
            _ = try bridge.readFrame()
        } catch {
            NSLog("WindInput[sendFocusGained] io error: \(error)")
            reconnect()
        }
    }

    /// 宿主 app 的标识, 供服务端做「焦点是否跨应用切入」判定 (clientToken 高 32 位)。
    ///
    /// **一律取 bundleID 的稳定散列, 不取真实 pid。**
    ///
    /// 曾经的做法是「前台 app 的 bundleID 对得上就用它的真 pid, 否则退化为散列」。问题在于
    /// 同一个 app 会因此拿到两个不同的键 —— 输入法与宿主是两个进程, Spotlight 覆盖层、
    /// 非前台面板等场景下 `frontmostApplication` 不是持有文本框的那个 app, 于是同一个
    /// 微信一会儿记在 pid 上、一会儿记在散列上, 表现为「按应用记忆的中英状态和 compat 规则
    /// 时不时自己重置」。
    ///
    /// 真 pid 在 macOS 上也换不来任何东西: 服务端拿 pid 只做两件事, 一是当 `pid_names`
    /// 的键 (散列同样满足「同 app 恒等、异 app 相异」), 二是 `process_name(pid)` 反查进程名
    /// —— 那个函数在非 Windows 恒返回空串, 宿主名改由 `.app` 随焦点事件直接送 bundleID。
    /// 散列还有个附带好处: 宿主重启后仍相等, 对「按应用记忆」而言反而更合用。
    private func hostPid(for bundleID: String) -> UInt32 {
        return Self.stableHash(bundleID)
    }

    /// FNV-1a 32 位散列, 结果恒非 0 (服务端以 pid==0 表示「未知宿主」并跳过按应用逻辑)。
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
    /// **窗口标题与发送一律挪出主线程。** 取标题走 AX, 那是到目标 app 的**同步跨进程调用**,
    /// 而 `activateServer` 的调用时机恰恰是那个 app 刚被切到前台、正忙着重绘的时候 —— 实测
    /// (sample 输入法进程 6 秒 / 切 6 次应用) 主线程 3467/4643 个采样点全部堵在这一个
    /// `AXUIElementCopyAttributeValue` 上, 平均每次切换阻塞约 0.5 秒, 且阻塞期间 AX 的
    /// mach_msg 会转 runloop, 下一次 activateServer 直接重入叠上来。IMKit 在主线程派发按键,
    /// 于是表现为「切换应用后刚开始输入非常卡」。
    ///
    /// 这份快照只有命令直通车读 (`coordinator.rs::front_ctx_snapshot`), 是个用户偶尔主动触发
    /// 的功能 —— 为它同步阻塞每一次焦点切换完全不成比例。异步晚到几十毫秒没有任何影响。
    private func sendFrontContext() {
        let app = currentClient?.bundleIdentifier() ?? ""
        let pid = NSWorkspace.shared.frontmostApplication?.processIdentifier ?? 0
        // 选中文本只能在主线程取 (IMKTextInput 的约定), 但**不在 activateServer 里取**:
        // `selectedRange()` 同样是跨进程调用, 实测占了另外 1191/4643 个采样点。改排到下一轮
        // 主线程事件, 激活流程即刻返回; 那时 IMKit 已完成激活、目标 app 也缓过来了。
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
    /// **不能复用 `bridge`**: 那条连接由主线程发按键帧并同步读响应, 从别的线程往里插一帧
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
    /// **只在 `frontCtxQueue` 上调用**: 这是到目标 app 的同步跨进程调用, 目标忙时会一直等。
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

    /// IS_PASSWORD 位 (TSF InputScope 枚举 31) 的 bitmask, 与 Go coordinator 的
    /// inputScopePassword 常量对齐。
    private static let inputScopePasswordBit: UInt64 = UInt64(1) << 31

    /// 每键检测系统安全输入(密码框)状态是否相对上次上报发生翻转, 变化则补发一帧
    /// focusGained 同步 Go 端 sensitiveFieldActive。
    ///
    /// 为何需要: macOS IMKit 的 activateServer 是「输入法 ↔ 某 client」级别的激活, 而浏览器
    /// 整个网页是单个 client —— 网页内普通框 ↔ 密码框的字段切换**不**触发 activateServer,
    /// 只在激活时检测会漏掉(表现: 第二次进密码框不再抑制)。IsSecureEventInputEnabled 是
    /// 全局实时状态, 随密码框聚焦翻转, 故在每个 keyDown 前比对补偿。轻量系统调用, 每键一次。
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

    /// 发一个修饰键 VK 的 KeyEvent (eventType=up) 给服务, 触发模式切换; 应用其响应。
    /// 用 key-up: 协调器的 toggle 键(Shift/Ctrl 单击切中英)仅在 EVENT_KEY_UP 分支处理
    /// (对齐 TSF「仅 keyUp 转发 toggle 键」约定); 发 .down 会落不进该分支 → 不切换。
    private func sendModifierTap(_ vk: UInt32, sender: Any!) {
        guard let bridge = bridge, bridge.isConnected else { return }
        // 模式切换 (Shift/Ctrl tap) 通常无 composition, 先刷新 caret 让状态气泡锚到当前
        // 插入点 (否则会显示在上一次组字的旧位置)。
        sendCaretUpdateIfAvailable(client: sender as? IMKTextInput)
        keySeq &+= 1
        let frame = BinaryCodec.encodeKeyEventFrame(KeyEventPayload(
            keyCode: vk, scanCode: 0, modifiers: 0, eventType: .up, eventSeq: keySeq, prevChar: 0))
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
        // IMK 输入源菜单必须 target=nil 走 IMK doCommandBySelector 路由（菜单在 IMK 独立上下文
        // 渲染，直接 AppKit target-action 投递不到本进程对象 → 整菜单失效）。而 IMK 对【嵌套子菜单】
        // 叶子回传的是子菜单首项 tag（点五笔切英文/点微软切默认）。故拍平为顶层带「父·」前缀项，
        // 各项 tag 即真实菜单 id，IMK 顶层项回传可靠——这是 IMK 输入源菜单的固有约束（无法保留子菜单）。
        // 候选框右键/菜单栏指示器走 inProcess 直接投递，仍用原始嵌套树，不受影响。
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
    /// CmdMenuAction, 由 Go 端 handleUnifiedMenuAction 派发, 与其它两处菜单同一路径。
    @objc public func imkMenuCommand(_ sender: Any!) {
        guard let info = sender as? NSDictionary,
              let item = info[kIMKCommandMenuItemName as Any] as? NSMenuItem else { return }
        CandidatePanelHost.shared.sendMenuAction(item.tag)
    }

    /// 服务不可达时的兜底菜单: 仅「设置…」(直接拉起设置应用, 不依赖 Go)。避免空菜单。
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

        // 修饰键变化 (Shift/Ctrl 等): 做 tap 检测, 不消费事件本身。
        if event.type == .flagsChanged {
            handleFlagsChanged(event, client: sender)
            return false
        }
        guard event.type == .keyDown else { return false }
        // 任意真实按键出现 → 取消当前修饰键 tap 判定 (Shift+X 不算 tap)。
        pendingModSawOther = true
        guard ensureConnected(), let bridge = bridge else {
            NSLog("WindInput[handle] bridge not connected (重连失败), pass through")
            return false
        }

        // 密码框实时跟随: 在发本键前同步系统安全输入状态(同一 client 内字段切换补偿),
        // 让 Go 处理本键时 sensitiveFieldActive 已最新(密码框→英文直通 / 普通框→恢复中文)。
        // 同连接串行: 补发的 focusGained 必先于本 keyEvent 被 Go 处理, 顺序正确。
        syncSecureInputIfChanged()

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

        // 宿主快捷键组合 (⌘/⌃/⌥ + 键): 照常问服务 (热键表在那边), 但服务回「清组合」时
        // 这一键仍要交还宿主, 否则 ⌘C/⌘V 在组字期间被吞 —— 见 KeyHandler.isHostShortcut。
        let hostShortcut = KeyHandler.isHostShortcut(event.modifierFlags)

        do {
            return try sendAndApply(frame, on: bridge, sender: sender, hostShortcut: hostShortcut)
        } catch {
            // 服务重启/卡死后这条连接已死 (write→EPIPE 或 read→EOF/超时)。重连到新服务
            // 并**用新连接重试当前键一次**, 让服务重启后第一个键就自愈, 不丢字、不需手动
            // 重启前端。重连或重试仍失败 (如服务尚在重启窗口未就绪) 才透传, 下一键再试。
            NSLog("WindInput[handle] bridge io error: \(error), 重连后重试本键")
            reconnect()
            // 注意: 上方 guard 把属性 bridge 遮蔽为非可选局部量, 这里须取重连后的 self.bridge。
            guard let fresh = self.bridge else { return false }
            do {
                return try sendAndApply(frame, on: fresh, sender: sender, hostShortcut: hostShortcut)
            } catch {
                NSLog("WindInput[handle] 重连后重试仍失败: \(error)")
                return false
            }
        }
    }

    /// 在指定连接上发一帧、读响应并应用; composition 非空时上报 caret。
    /// 抽出供 handle 的「首发 + 重连重试」两条路径共用。
    private func sendAndApply(_ frame: Data, on bridge: BridgeClient, sender: Any?,
                              hostShortcut: Bool = false) throws -> Bool {
        try bridge.send(frame)
        let resp = try bridge.readFrame()
        let consumed = applyResponse(resp, sender: sender, hostShortcut: hostShortcut)
        // M2.2-E: composition 启动/更新后, 上报当前 caret 屏幕位置给 Go,
        // 让候选框/Toast/光标跟随有正确锚点. 仅在 marked text 非空时发。
        if !composition.isEmpty {
            sendCaretUpdateIfAvailable(client: sender as? IMKTextInput)
        }
        return consumed
    }

    // MARK: - Caret update (M2.2-E)

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

        // 参照屏必须与浮窗落位那边同源（`PanelGeometry.referenceScreen`，带菜单栏的主屏）。
        // 这里曾用 `NSScreen.main`：两个方向都用它时误差恰好抵消，一旦落位那边改用主屏，
        // 两屏高度不同就直接变成候选窗相对光标的垂直偏移——在副屏比主屏高的机器上尤其明显。
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

    /// 把 Go 返回的 bridge 帧路由到 IMKTextInput 协议方法. 委托给 BridgeResponseRouter
    /// (在 WindInputKit 里, 不依赖 IMKit, 便于 swift test 用 mock 驱动).
    internal func applyResponse(_ frame: Frame, sender: Any?, hostShortcut: Bool = false) -> Bool {
        let imkClient = sender as? IMKTextInput
        let adapter = imkClient.map { IMKClientAdapter(imkClient: $0, controller: self) }
        return router.apply(frame, to: adapter, hostShortcut: hostShortcut)
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
        /// `selectedRange` 覆写成 `{0, 全长}` —— 我们给的 `{caret, 0}` 就此丢失,
        /// 宿主把组合内光标画在**最前面**。完整的现象、判据与影响面见
        /// `MarkedTextAttributes` 的文件头注释。
        ///
        /// 真机判据 (改完必须复验): 用 `WINDUI_IME=1` 跑 wind-ui-rust 示例, 打 `sf`,
        /// 日志应为 `selected={2,0} → caret=2 sel=None`; 若仍是 `selected={0,2}`,
        /// 说明属性没被 IMKit 认可, 回头查 `markedClauseSegment` 是否真进了字典。
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
            // `markForStyle:` 返回 [AnyHashable: Any], **逐项**转 key 而不是整体 `as?`:
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
    /// 必要性 (实测): IME 的 InputController 在 Go 服务 socket 就绪前被创建 (装完首次激活,
    /// 尤其重启后 IME 随登录自启与服务 LaunchAgent RunAtLoad 并发) 时, init() 那次连接会
    /// 失败 → bridge=nil → 此后该实例所有按键直通英文且不重试 (得切走再切回让 IMKit 新建
    /// 实例才会重连)。这里在 activate/handle 入口懒重连让同一实例自愈, 免去手动切换。
    /// 已连时是廉价 no-op, 不影响正常路径。
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
