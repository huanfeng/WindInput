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
    private var reader: SharedMemoryReader?
    private var push: PushClient?
    private var sendClient: BridgeClient?       // 发 CmdCandidateSelect 用 (request 连接)
    private var latestRects: [CandidateHitRect] = []
    private var currentScale: CGFloat = 1
    private let lock = NSLock()

    /// 当前焦点 InputController, push 通道 commit 路由目标。weak 避免保活已销毁的 controller。
    public weak var activeResponder: PushResponder?

    private init() {
        if Thread.isMainThread {
            panel = CandidatePanel()
        } else {
            var p: CandidatePanel?
            DispatchQueue.main.sync { p = CandidatePanel() }
            panel = p!
        }
        panel.onSelect = { [weak self] index in self?.sendCandidateSelect(index) }
    }

    public func start() {
        lock.lock(); defer { lock.unlock() }
        if push != nil { return }
        openSHMIfNeeded()

        let pc = PushClient(socketPath: BridgeEndpoints.pushSocket)
        pc.onFrame = { [weak self] frame in self?.handlePushFrame(frame) }
        pc.onError = { err in NSLog("CandidatePanelHost: push error: \(err)") }
        do {
            try pc.start()
            push = pc
            NSLog("CandidatePanelHost: push subscribed \(BridgeEndpoints.pushSocket)")
        } catch {
            NSLog("CandidatePanelHost: push start failed: \(error)")
        }
    }

    public func stop() {
        lock.lock(); defer { lock.unlock() }
        push?.stop(); push = nil
        sendClient?.close(); sendClient = nil
        reader?.closeReader(); reader = nil
        DispatchQueue.main.async { [weak self] in self?.panel.hidePanel() }
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

    private func sendCandidateSelect(_ index: Int) {
        lock.lock()
        if sendClient == nil {
            sendClient = try? BridgeClient(socketPath: BridgeEndpoints.requestSocket)
        }
        let c = sendClient
        lock.unlock()
        guard let c = c else {
            NSLog("CandidatePanelHost: no send client for candidate select")
            return
        }
        do {
            try c.send(BinaryCodec.encodeCandidateSelectFrame(index: index))
            _ = try? c.readFrame() // Go 同步返 Ack, 读掉; commit 走 push 通道异步到达
            NSLog("CandidatePanelHost: sent CmdCandidateSelect index=\(index)")
        } catch {
            NSLog("CandidatePanelHost: send select failed: \(error)")
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
            DispatchQueue.main.async { [weak self] in self?.panel.hidePanel() }
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
