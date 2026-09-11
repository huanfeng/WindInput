import Cocoa
import WindInputKit

// CandidatePanelHost — IMKit `.app` 内的候选框承载层 (PR-A.5 Phase 1 + M5 鼠标点选).
//
// 职责:
//   1. 启动时 try open /WindInput_SHM, 启 PushClient 订阅 bridge_push.sock
//   2. 收 CmdHostRenderFrame → snapshot SHM → CGImage → 贴 NSPanel
//   3. 收 CmdCandidateRects → 存命中矩形, 喂 panel 供鼠标 hit-test
//   4. NSPanel 鼠标点选 → 发 CmdCandidateSelect 回 Go (经独立 request 连接)
//   5. 收 push 通道的 commit/composition (鼠标选词结果走 push) → 路由到当前
//      active InputController, 由其 insertText/setMarkedText 上屏
//
// 单例: 整个 .app 进程一个 panel + SHM reader + PushClient + send 连接。

/// active InputController 实现此协议, 让 panel host 把 push 通道的 commit/composition
/// 应用到当前焦点文本框 (鼠标选词的 commit 不是 KeyEvent 同步响应, 走 push)。
public protocol PushResponder: AnyObject {
    func applyPushResponse(_ frame: Frame)

    /// 作废「光标前一字符」的记账（数字后智能标点用，见 `SmartPunctDigitTracker`）。
    ///
    /// 供**绕过 router 直接改文档**的路径调用：合成按键往宿主打字符/退格，文档变了而
    /// 我们的记账一无所知。这类路径拿不到「改成了什么」，只能作废，回落默认行为。
    func invalidateDigitTracking()
}

public final class CandidatePanelHost {
    public static let shared = CandidatePanelHost()

    private let panel: CandidatePanel
    private let tooltip: TooltipPanel
    private let statusBubble: StatusBubblePanel
    private let toast: ToastPanel
    private var lastHoverIndex = -1   // 仅主线程访问 (onHover/tooltipShow 都切主线程)
    private var reader: SharedMemoryReader?
    private var push: PushClient?
    private var sendClient: BridgeClient?       // 发 CmdCandidateSelect 用 (request 连接)
    private var latestRects: [CandidateHitRect] = []
    private var currentScale: CGFloat = 1
    private var reconnecting = false            // push 重连排程中, 防重复
    private let lock = NSLock()

    /// 当前焦点 InputController, push 通道 commit 路由目标。weak 避免保活已销毁的 controller。
    public weak var activeResponder: PushResponder?

    private init() {
        if Thread.isMainThread {
            panel = CandidatePanel()
            tooltip = TooltipPanel()
            statusBubble = StatusBubblePanel()
            toast = ToastPanel()
        } else {
            var p: CandidatePanel?
            var t: TooltipPanel?
            var s: StatusBubblePanel?
            var to: ToastPanel?
            DispatchQueue.main.sync { p = CandidatePanel(); t = TooltipPanel(); s = StatusBubblePanel(); to = ToastPanel() }
            panel = p!
            tooltip = t!
            statusBubble = s!
            toast = to!
        }
        panel.onSelect = { [weak self] index in self?.handlePanelClick(index) }
        panel.onHover = { [weak self] index in
            guard let self = self else { return }
            self.lastHoverIndex = index               // 主线程 (mouseMoved)
            if index < 0 { self.tooltip.hidePanel() }  // 离开候选立即收起, 文本到达前先隐
            self.sendFrame(BinaryCodec.encodeCandidateHoverFrame(index: index))
        }
        panel.onContextAction = { [weak self] index, action in
            self?.sendFrame(BinaryCodec.encodeCandidateContextMenuFrame(index: index, action: action))
        }
        panel.onScroll = { [weak self] delta in
            self?.sendFrame(BinaryCodec.encodeCandidateScrollFrame(delta: delta))
        }
        panel.onMoved = { [weak self] x, y in self?.reportPos(ExtKind.posCandidate, x, y) }
        statusBubble.onMoved = { [weak self] x, y in self?.reportPos(ExtKind.posStatusTip, x, y) }
        panel.unifiedMenuProvider = { [weak self] in self?.requestUnifiedMenu() }
        panel.onUnifiedAction = { [weak self] id in self?.sendMenuAction(id) }
        // 菜单栏状态菜单复用候选框空白处右键的同一统一菜单树与回发路径, 保证两处一致。
        ModeStatusController.shared.unifiedMenuProvider = { [weak self] in self?.requestUnifiedMenu() }
        ModeStatusController.shared.onUnifiedAction = { [weak self] id in self?.sendMenuAction(id) }
    }

    // MARK: - 统一菜单复用接口 (供系统输入菜单 InputController.menu 调用)

    /// 同步取统一菜单树 (与候选框空白处右键、菜单栏指示器菜单同一 IPC 请求路径)。
    /// 主线程调用 (菜单将要绘制时), 失败返回 nil。
    public func unifiedMenuItems() -> [MenuItemData]? {
        // IMK 输入源菜单：请求【精简】树(无子菜单，IMK 无法可靠处理嵌套)。
        return requestUnifiedMenu(simplified: true)
    }

    /// 上报浮窗拖动落点 (wire top-left)。服务端据当前定位方式决定落不落盘。
    ///
    /// 走扩展信封而非专用码位: 拖动是低频动作 (用户偶尔摆一次窗口), JSON 的解析开销与
    /// 加一个协议常量的三处同步成本相比不值一提。见 ext_kind 的两档划分。
    private func reportPos(_ kind: String, _ x: Int32, _ y: Int32) {
        // 显式转 Int：JSONSerialization 只认 NSNumber，Int32 虽能桥接但没必要赌桥接细节。
        guard let body = try? JSONSerialization.data(withJSONObject: ["x": Int(x), "y": Int(y)])
        else { return }
        sendFrame(BinaryCodec.encodeExtFrame(kind: kind, body: body))
    }

    /// 截自家浮窗存盘 + 复制剪贴板, 结果回上行。主线程调用 (要渲染视图层级)。
    ///
    /// `echo` 里除 `items` 外的字段**原样回传**: 文案所需的上下文 (模式、数量、目录、
    /// 候选是否已进剪贴板) 由服务端放进来又拿回去, 两边都不必为一次往返留状态。
    ///
    /// 未知 target **不静默丢弃**: 回一条失败, 免得服务端那边少一条结果却不知为何。
    ///
    /// **右键菜单不在可截之列**: macOS 上那是原生 NSMenu, 截它要走
    /// `CGWindowListCreateImage` + 屏幕录制授权 (见 `PanelCapture`)。只截我们自己画的。
    private func capturePanels(_ items: [[String: Any]], echo: [String: Any]) {
        var results: [[String: Any]] = []
        for item in items {
            guard let target = item["target"] as? String,
                  let path = item["path"] as? String else { continue }
            var r: [String: Any] = ["target": target, "path": path]
            let panel: NSPanel? = switch target {
            case "status_tip": statusBubble
            case "tooltip": tooltip
            case "toast": toast
            default: nil
            }
            if let panel = panel {
                do {
                    r["clipboard"] = try PanelCapture.snapshot(panel, toPath: path)
                    r["ok"] = true
                } catch {
                    r["ok"] = false
                    r["reason"] = (error as? LocalizedError)?.errorDescription
                        ?? error.localizedDescription
                }
            } else {
                r["ok"] = false
                r["reason"] = "unknown_target"
            }
            results.append(r)
        }
        var body = echo
        body.removeValue(forKey: "items")
        body["results"] = results
        guard let data = try? JSONSerialization.data(withJSONObject: body) else { return }
        sendFrame(BinaryCodec.encodeExtFrame(kind: ExtKind.shotResult, body: data))
    }

    /// 回发统一菜单项点击 (CmdMenuAction)。三处菜单共用同一发送路径。
    public func sendMenuAction(_ id: Int) {
        sendFrame(BinaryCodec.encodeMenuActionFrame(id: Int32(id)))
    }

    public func start() {
        attemptPushConnect()
    }

    /// 连接 push 通道 + 开 SHM。连不上 (服务尚未起 socket) 或后续断开 (服务重启)
    /// 都会经 `scheduleReconnect` 定时重试, 直到连上。
    /// 登录时系统拉起 IME `.app` 可能早于 LaunchAgent 起服务建 socket, 必须重试。
    private func attemptPushConnect() {
        lock.lock()
        if push != nil { lock.unlock(); return }
        openSHMIfNeeded()
        let pc = PushClient(socketPath: BridgeEndpoints.pushSocket)
        pc.onFrame = { [weak self] frame in self?.handlePushFrame(frame) }
        pc.onError = { [weak self] err in
            guard let self = self else { return }
            self.lock.lock()
            if self.push === pc { self.push = nil }
            self.lock.unlock()
            NSLog("CandidatePanelHost: push error: \(err) — 安排重连")
            self.scheduleReconnect()
        }
        var connected = false
        do {
            try pc.start()
            push = pc
            connected = true
        } catch {
            push = nil
        }
        lock.unlock()

        if connected {
            NSLog("CandidatePanelHost: push subscribed \(BridgeEndpoints.pushSocket)")
        } else {
            NSLog("CandidatePanelHost: push start failed (服务未就绪?) — 安排重连")
            scheduleReconnect()
        }
    }

    /// 1s 后重试 push 连接 (幂等, 同时只排一个)。重连前丢弃旧 SHM mmap:
    /// 服务重启会 shm_unlink 重建段, 旧映射会读到失效内存, 必须重开。
    private func scheduleReconnect() {
        lock.lock()
        if reconnecting || push != nil { lock.unlock(); return }
        reconnecting = true
        reader?.closeReader(); reader = nil
        lock.unlock()

        DispatchQueue.global(qos: .utility).asyncAfter(deadline: .now() + 1.0) { [weak self] in
            guard let self = self else { return }
            self.lock.lock()
            self.reconnecting = false
            let alreadyUp = self.push != nil
            self.lock.unlock()
            if alreadyUp { return }
            self.attemptPushConnect()
        }
    }

    public func stop() {
        lock.lock(); defer { lock.unlock() }
        push?.stop(); push = nil
        sendClient?.close(); sendClient = nil
        reader?.closeReader(); reader = nil
        DispatchQueue.main.async { [weak self] in
            self?.panel.hidePanel()
            self?.tooltip.hidePanel()
            self?.statusBubble.hidePanel()
            self?.toast.hidePanel()
        }
    }

    /// 显示候选悬停 tooltip: 定位到当前悬停候选的屏幕矩形 (主线程调用)。
    /// 文本异步到达, 期间用户可能已移开 → lastHoverIndex<0 或取不到矩形则不显示。
    private func showTooltip(_ p: TooltipPayload) {
        let idx = lastHoverIndex
        guard idx >= 0, let rect = panel.candidateScreenRect(index: idx) else {
            tooltip.hidePanel()
            return
        }
        tooltip.show(text: p.text, bgHex: p.bgColor, fgHex: p.fgColor,
                     fontPath: p.fontPath, anchorScreenRect: rect)
    }

    private func openSHMIfNeeded() {
        if reader != nil { return }
        // SHM 名按变体后缀与 Rust wind_bridge::endpoint::shm_name 对齐
        // (release: /WindInput_SHM; dev: /WindInput_SHMDev)。否则两变体抢同一段,
        // 开机后候选框渲染坏掉; 名字与服务端不一致则整个 dev 变体拿不到帧。
        // 注: 服务端内部传的是管道后缀 "_dev", 由 endpoint.rs 的 variant_suffix 映射成 "Dev"。
        let shmName = "/WindInput_SHM\(BridgeEndpoints.variantSuffix)"
        do {
            reader = try SharedMemoryReader(name: shmName, size: 4 * 1024 * 1024)
            NSLog("CandidatePanelHost: SHM opened \(shmName)")
        } catch {
            NSLog("CandidatePanelHost: SHM open deferred (\(error))")
        }
    }

    // MARK: - 鼠标点选 → 发 CmdCandidateSelect

    /// 面板点击: index>=0 选词; index==-1 翻上页; index==-2 翻下页 (合成 pgup/pgdn 键)。
    private func handlePanelClick(_ index: Int) {
        if index >= 0 {
            sendFrame(BinaryCodec.encodeCandidateSelectFrame(index: index))
        } else if index == -1 {
            sendFrame(pagerKeyFrame(vk: 0x21)) // VK_PRIOR (Page Up)
        } else if index == -2 {
            sendFrame(pagerKeyFrame(vk: 0x22)) // VK_NEXT (Page Down)
        }
    }

    /// 在持久 request 连接上做一次「发一帧 + 读一帧响应」, 连接陈旧时重建并重试一次。
    /// **仅供幂等 (只读) 请求复用** —— 重试会重发同一帧, 非幂等操作 (选词/翻页/菜单动作)
    /// 不可用本助手, 以免「服务端已处理但 ack 丢失」时被重发双重执行。
    ///
    /// 必要性: sendClient 是长连接, 服务侧可能因空闲回收/重启把它关掉, 而本端半开连接
    /// 察觉不到 —— 首次 send 看似成功但 readFrame 立即 EOF。单次尝试会让「切换应用后首次
    /// 打开菜单」偶发取不到菜单树 (退回兜底/只读)。重试一次 (重建连接) 即可对用户透明自愈。
    /// 两次都失败 (服务真的不可达) 返回 nil。主线程调用 (鼠标/菜单事件, 主线程串行化)。
    private func requestResponseIdempotent(_ frame: Data) -> Frame? {
        for attempt in 0..<2 {
            lock.lock()
            if sendClient == nil {
                sendClient = try? BridgeClient(socketPath: BridgeEndpoints.requestSocket)
            }
            let c = sendClient
            lock.unlock()
            guard let c = c else { return nil } // 连不上服务 (socket 不存在), 重试无意义
            do {
                try c.send(frame)
                return try c.readFrame()
            } catch {
                NSLog("CandidatePanelHost: requestResponse attempt \(attempt) failed: \(error)")
                lock.lock(); sendClient?.close(); sendClient = nil; lock.unlock()
                // 陈旧连接已丢弃; 下一轮 attempt 重建后重试。
            }
        }
        return nil
    }

    /// 空白处右键 / 菜单栏指示器 / 系统输入菜单: 向 Go 请求统一菜单树
    /// (CmdShowContextMenu → CmdMenuShow 响应)。只读查询, 连接陈旧自动重试一次; 失败返回 nil。
    private func requestUnifiedMenu(simplified: Bool = false) -> [MenuItemData]? {
        // simplified=true 仅 IMK 输入源菜单用(精简树)；候选框右键/菜单栏指示器用完整树(默认 false)。
        guard let resp = requestResponseIdempotent(BinaryCodec.encodeShowContextMenuFrame(simplified: simplified)),
              resp.cmd == DownstreamCmd.menuShow else { return nil }
        return try? BinaryCodec.decodeUnifiedMenuPayload(resp.payload)
    }

    private func pagerKeyFrame(vk: UInt32) -> Data {
        BinaryCodec.encodeKeyEventFrame(KeyEventPayload(
            keyCode: vk, scanCode: 0, modifiers: 0, eventType: .down, eventSeq: 0, prevChar: 0))
    }

    /// 通过持久 request 连接发一帧 (CandidateSelect / Hover / 翻页键 / 菜单动作), 读掉 Ack。
    /// 非幂等, 单次尝试: 连接陈旧时丢弃并重置 (下次自愈), 不重发以免双重执行。
    /// 候选更新/commit 走 push 通道异步到达。
    private func sendFrame(_ frame: Data) {
        lock.lock()
        if sendClient == nil {
            sendClient = try? BridgeClient(socketPath: BridgeEndpoints.requestSocket)
        }
        let c = sendClient
        lock.unlock()
        guard let c = c else { return }
        do {
            try c.send(frame)
            _ = try? c.readFrame()
        } catch {
            NSLog("CandidatePanelHost: sendFrame failed: \(error)")
            lock.lock(); sendClient?.close(); sendClient = nil; lock.unlock()
        }
    }

    // MARK: - Push 路由

    private func handlePushFrame(_ frame: Frame) {
        switch frame.cmd {
        case DownstreamCmd.serviceReady:
            // 服务（重）连：服务进程重启会 shm_unlink + 重建 SHM 段（新 inode），本进程旧 mmap
            // 变成孤立旧段 → 候选窗卡在旧帧不刷新。收到 SERVICE_READY 即强制重开 SHM reader，
            // 下一帧按名重开拿到新段，无需重装/重启 .app。
            lock.lock(); reader?.closeReader(); reader = nil; lock.unlock()
        case DownstreamCmd.hostRenderFrame:
            guard let p = try? BinaryCodec.decodeHostRenderFramePayload(frame.payload) else { return }
            applyHostRenderFrame(p)
        case DownstreamCmd.candidateRects:
            if let rects = try? BinaryCodec.decodeCandidateRectsPayload(frame.payload) {
                lock.lock(); latestRects = rects; let s = currentScale; lock.unlock()
                let logical = Self.scaleRects(rects, by: s)
                DispatchQueue.main.async { [weak self] in self?.panel.updateRects(logical) }
            }
        case DownstreamCmd.modeStatus:
            if let st = try? BinaryCodec.decodeModeStatusPayload(frame.payload) {
                ModeStatusController.shared.apply(st)
            }
        case DownstreamCmd.candidateMenuFlags:
            if let flags = try? BinaryCodec.decodeCandidateMenuFlagsPayload(frame.payload) {
                DispatchQueue.main.async { [weak self] in self?.panel.updateMenuFlags(flags) }
            }
        case DownstreamCmd.ext:
            // 扩展信封：低频消息统一入口。未知 kind 安静忽略（版本兼容的根本）。
            guard let (kind, body) = BinaryCodec.decodeExt(frame.payload) else { break }
            switch kind {
            case ExtKind.settingsOpen:
                // body = {"args":["--page=dict", …]}，argv 已由 Rust 侧切好，这里直接用。
                let args = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any]
                let argv = (args?["args"] as? [String]) ?? []
                ModeStatusController.shared.openSettings(arguments: argv)
            case ExtKind.shotPanel:
                // 服务端请我们截自家的浮窗（像素不在它那边）。存盘 + 复制剪贴板都在这里做，
                // 文件名是它给的；结果回上行, 由它统一弹 Toast——两平台文案因此保持一致。
                guard let o = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any],
                      let items = o["items"] as? [[String: Any]] else {
                    NSLog("WindInput[ext] shot.panel 载荷无法解析")
                    break
                }
                DispatchQueue.main.async { [weak self] in self?.capturePanels(items, echo: o) }
            case ExtKind.posCandidateQuery, ExtKind.posStatusTipQuery:
                // 服务端问「你现在在哪」(用户点了菜单里的「固定位置」)。浮窗不可见时**不答**:
                // 那种情况下没有"当前位置"可言, 沉默让服务端保留旧值, 好过报一个上次残留的
                // 坐标把配置写坏。
                let isCandidate = (kind == ExtKind.posCandidateQuery)
                DispatchQueue.main.async { [weak self] in
                    guard let self = self else { return }
                    let pos = isCandidate ? self.panel.wireTopLeft() : self.statusBubble.wireTopLeft()
                    guard let (x, y) = pos else { return }
                    self.reportPos(isCandidate ? ExtKind.posCandidate : ExtKind.posStatusTip, x, y)
                }
            default:
                NSLog("WindInput[ext] 未处理的 kind=\(kind)")
            }
        case DownstreamCmd.tooltipShow:
            if let p = try? BinaryCodec.decodeTooltipPayload(frame.payload) {
                DispatchQueue.main.async { [weak self] in self?.showTooltip(p) }
            }
        case DownstreamCmd.tooltipHide:
            DispatchQueue.main.async { [weak self] in self?.tooltip.hidePanel() }
        case DownstreamCmd.statusShow:
            if let p = try? BinaryCodec.decodeStatusBubblePayload(frame.payload) {
                DispatchQueue.main.async { [weak self] in
                    self?.statusBubble.show(text: p.text, bgHex: p.bgColor, fgHex: p.fgColor,
                                            wireX: p.x, wireY: p.y, durationMs: p.durationMs)
                }
            }
        case DownstreamCmd.statusHide:
            DispatchQueue.main.async { [weak self] in self?.statusBubble.hidePanel() }
        case DownstreamCmd.toastShow:
            if let p = try? BinaryCodec.decodeToastPayload(frame.payload) {
                DispatchQueue.main.async { [weak self] in self?.toast.show(p) }
            }
        case DownstreamCmd.toastHide:
            DispatchQueue.main.async { [weak self] in self?.toast.hidePanel() }
        // 合成按键四路: 往宿主打任意键 (含数字、退格), 文档被改而 router 完全不经手 ——
        // 「光标前一字符」的记账够不到这里, 只能作废。用户自定义宏才触发, 量级小于按键/
        // 上屏两条主路, 但漏了就是「打完宏再打标点出错形」这种查无可查的偶发。
        case DownstreamCmd.keyTap:
            if let p = try? BinaryCodec.decodeKeyComboPayload(frame.payload) {
                let responder = activeResponder
                DispatchQueue.main.async { responder?.invalidateDigitTracking(); KeySynthesizer.tap(p) }
            }
        case DownstreamCmd.keyHold:
            if let p = try? BinaryCodec.decodeKeyComboPayload(frame.payload) {
                let responder = activeResponder
                DispatchQueue.main.async { responder?.invalidateDigitTracking(); KeySynthesizer.hold(p) }
            }
        case DownstreamCmd.keyRelease:
            if let p = try? BinaryCodec.decodeKeyComboPayload(frame.payload) {
                let responder = activeResponder
                DispatchQueue.main.async { responder?.invalidateDigitTracking(); KeySynthesizer.release(p) }
            }
        case DownstreamCmd.keySeq:
            if let p = try? BinaryCodec.decodeKeySeqPayload(frame.payload) {
                let responder = activeResponder
                DispatchQueue.main.async { responder?.invalidateDigitTracking(); KeySynthesizer.sequence(p.combos) }
            }
        case DownstreamCmd.commitText, DownstreamCmd.updateComposition, DownstreamCmd.clearComposition,
             DownstreamCmd.keyType:
            // 鼠标选词的 commit / composition 及命令直通车 key.type / clip.paste 文本上屏
            // 经 push 通道异步到达, 路由到当前焦点 controller (其 router 调 client.insertText)。
            let responder = activeResponder
            DispatchQueue.main.async { responder?.applyPushResponse(frame) }
        default:
            break
        }
    }

    private func applyHostRenderFrame(_ p: HostRenderFramePayload) {
        let visible = (p.flags & 0x1) != 0
        NSLog("CPH.renderFrame seq=\(p.seq) vis=\(visible) \(p.width)x\(p.height) x=\(p.x) y=\(p.y) scale=\(p.scale) flags=0x\(String(p.flags, radix:16))")
        if !visible || p.width == 0 || p.height == 0 {
            NSLog("CPH.renderFrame hiding (invisible or zero-size)")
            DispatchQueue.main.async { [weak self] in
                self?.lastHoverIndex = -1
                self?.panel.hidePanel()
                self?.tooltip.hidePanel()
            }
            return
        }
        let scale = max(1, CGFloat(p.scale))
        if reader == nil { lock.lock(); openSHMIfNeeded(); lock.unlock() }
        NSLog("CPH.renderFrame reader=\(reader != nil) scale=\(scale)")
        guard let r = reader else {
            NSLog("CPH.renderFrame FAIL: reader is nil after openSHMIfNeeded")
            return
        }
        guard let frame = r.snapshot() else {
            NSLog("CPH.renderFrame FAIL: snapshot() returned nil")
            return
        }
        NSLog("CPH.renderFrame SHM frame \(frame.width)x\(frame.height) flags=0x\(String(frame.flags, radix:16)) seq=\(frame.sequence) dataSize=\(frame.bgra.count)")
        guard let img = Self.makeNSImage(from: frame, scale: scale) else {
            NSLog("CPH.renderFrame FAIL: makeNSImage returned nil (frame \(frame.width)x\(frame.height) bgra=\(frame.bgra.count))")
            return
        }
        let pt = NSPoint(x: CGFloat(p.x), y: CGFloat(p.y))
        NSLog("CPH.renderFrame showing panel at (\(p.x),\(p.y)) imgSize=\(img.size)")
        let useSoftwareShadow = frame.hasSoftwareShadow
        let absolute = p.isAbsolutePos
        lock.lock(); currentScale = scale; let rects = Self.scaleRects(latestRects, by: scale); lock.unlock()
        DispatchQueue.main.async { [weak self] in
            // 软件阴影已渲染在 image 中时禁用系统窗口阴影，避免在画布边缘产生黑边。
            self?.panel.hasShadow = !useSoftwareShadow
            self?.panel.show(image: img, atScreenPoint: pt, rects: rects, absolute: absolute)
        }
    }

    /// 把 device-px 命中矩形除以 scale → logical 点 (与 NSView 坐标系一致)。
    static func scaleRects(_ rects: [CandidateHitRect], by scale: CGFloat) -> [CandidateHitRect] {
        if scale == 1 { return rects }
        let s = Int32(scale)
        return rects.map { CandidateHitRect(index: $0.index, x: $0.x / s, y: $0.y / s, w: $0.w / s, h: $0.h / s) }
    }

    /// BGRA device 像素 → NSImage, size 设为 logical (像素/scale)。Retina 上高分辨率
    /// 位图贴 logical 框 = 1 device px : 1 image px, 清晰。
    static func makeNSImage(from f: SharedFrame, scale: CGFloat) -> NSImage? {
        guard let provider = CGDataProvider(data: f.bgra as CFData) else { return nil }
        let bitmapInfo: CGBitmapInfo = [
            CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedFirst.rawValue),
            CGBitmapInfo.byteOrder32Little,
        ]
        let cs = CGColorSpaceCreateDeviceRGB()
        guard let cg = CGImage(
            width: f.width, height: f.height,
            bitsPerComponent: 8, bitsPerPixel: 32,
            bytesPerRow: f.stride,
            space: cs, bitmapInfo: bitmapInfo,
            provider: provider, decode: nil,
            shouldInterpolate: false, intent: .defaultIntent
        ) else { return nil }
        let logical = NSSize(width: CGFloat(f.width) / scale, height: CGFloat(f.height) / scale)
        // NSImage(cgImage:size:) 不会把 cgImage 注册为高分辨率 representation，AppKit 把它当
        // 1x 图放大到 logical 尺寸 → Retina 下模糊。改用 NSBitmapImageRep 显式声明逻辑尺寸，
        // AppKit 即可以 1 device-px : 1 image-px 的精度直接绘制，保持清晰。
        let rep = NSBitmapImageRep(cgImage: cg)
        rep.size = logical
        let img = NSImage(size: logical)
        img.addRepresentation(rep)
        return img
    }
}
