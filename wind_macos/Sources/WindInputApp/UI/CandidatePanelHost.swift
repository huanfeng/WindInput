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
        return requestUnifiedMenu()
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
        // SHM 名按变体后缀与 Go host_render_darwin.darwinSHMName 对齐 (release: /WindInput_SHM;
        // debug: /WindInput_SHM_debug)。否则两变体抢同一段, 开机后候选框渲染坏掉。
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
    private func requestUnifiedMenu() -> [MenuItemData]? {
        guard let resp = requestResponseIdempotent(BinaryCodec.encodeEmptyFrame(cmd: UpstreamCmd.showContextMenu)),
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
        case DownstreamCmd.openSettings:
            let page = String(data: frame.payload, encoding: .utf8) ?? ""
            ModeStatusController.shared.openSettings(page: page)
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
        case DownstreamCmd.commitText, DownstreamCmd.updateComposition, DownstreamCmd.clearComposition:
            // 鼠标选词的 commit / composition 经 push 通道异步到达, 路由到当前焦点 controller。
            let responder = activeResponder
            DispatchQueue.main.async { responder?.applyPushResponse(frame) }
        default:
            break
        }
    }

    private func applyHostRenderFrame(_ p: HostRenderFramePayload) {
        let visible = (p.flags & 0x1) != 0
        if !visible || p.width == 0 || p.height == 0 {
            DispatchQueue.main.async { [weak self] in
                self?.lastHoverIndex = -1
                self?.panel.hidePanel()
                self?.tooltip.hidePanel()
            }
            return
        }
        let scale = max(1, CGFloat(p.scale))
        if reader == nil { lock.lock(); openSHMIfNeeded(); lock.unlock() }
        guard let r = reader, let frame = r.snapshot() else { return }
        guard let img = Self.makeNSImage(from: frame, scale: scale) else { return }
        let pt = NSPoint(x: CGFloat(p.x), y: CGFloat(p.y))
        lock.lock(); currentScale = scale; let rects = Self.scaleRects(latestRects, by: scale); lock.unlock()
        DispatchQueue.main.async { [weak self] in
            self?.panel.show(image: img, atScreenPoint: pt, rects: rects)
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
        return NSImage(cgImage: cg, size: logical)
    }
}
