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
        } else {
            var p: CandidatePanel?
            var t: TooltipPanel?
            DispatchQueue.main.sync { p = CandidatePanel(); t = TooltipPanel() }
            panel = p!
            tooltip = t!
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
        panel.onUnifiedAction = { [weak self] id in
            self?.sendFrame(BinaryCodec.encodeMenuActionFrame(id: Int32(id)))
        }
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
        do {
            reader = try SharedMemoryReader(name: "/WindInput_SHM", size: 4 * 1024 * 1024)
            NSLog("CandidatePanelHost: SHM opened /WindInput_SHM")
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

    /// 空白处右键: 向 Go 请求统一菜单树 (CmdShowContextMenu → CmdMenuShow 响应)。
    /// 同步走 request 连接 (本地 socket, 快); 失败返回 nil。在主线程调用 (鼠标事件)。
    private func requestUnifiedMenu() -> [MenuItemData]? {
        lock.lock()
        if sendClient == nil {
            sendClient = try? BridgeClient(socketPath: BridgeEndpoints.requestSocket)
        }
        let c = sendClient
        lock.unlock()
        guard let c = c else { return nil }
        do {
            try c.send(BinaryCodec.encodeEmptyFrame(cmd: UpstreamCmd.showContextMenu))
            let resp = try c.readFrame()
            guard resp.cmd == DownstreamCmd.menuShow else { return nil }
            return try BinaryCodec.decodeUnifiedMenuPayload(resp.payload)
        } catch {
            NSLog("CandidatePanelHost: requestUnifiedMenu failed: \(error)")
            lock.lock(); sendClient?.close(); sendClient = nil; lock.unlock()
            return nil
        }
    }

    private func pagerKeyFrame(vk: UInt32) -> Data {
        BinaryCodec.encodeKeyEventFrame(KeyEventPayload(
            keyCode: vk, scanCode: 0, modifiers: 0, eventType: .down, eventSeq: 0, prevChar: 0))
    }

    /// 通过独立 request 连接发一帧 (CandidateSelect / Hover / 翻页键), 读掉 Ack。
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
