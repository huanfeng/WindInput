import Cocoa
import WindInputKit

// WindInputDemo — 自包含 IME 测试台 (绕开 IMKit/TIS 环境墙)。
//
// 完整链路 (与真 IMKit `.app` 等价, 只是 host 是本 demo 自己的 NSTextView):
//   键盘输入 → IMETextView.keyDown → bridge.sock 发 KeyEvent → 同步响应
//     (UPDATE_COMP→setMarkedText / COMMIT→insertText) 应用到 NSTextView
//   候选框 → push 通道 CmdHostRenderFrame (SHM blit) + CmdCandidateRects → NSPanel
//   鼠标点候选 → 发 CmdCandidateSelect → push commit → 路由回 NSTextView 上屏
//
// 用法: 先起 wind_input 服务 (pinyin schema), 再 swift run wind-input-demo,
//   在窗口文本框里打拼音 → 看候选框 → 数字/空格 或 鼠标点击 选词上屏。

// MARK: - TextInputClient → NSTextView 适配

/// ⚠️ 本 demo **测不出 marked text 的宿主侧缺陷**: 它直接调 NSTextView, 中间没有 IMKit。
/// 真机上组合串要经 `IMKTextInput.setMarkedText` 转发, IMKit 会在那一层对 selectionRange
/// 动手 (见 `MarkedTextAttributes` 的文件头注释) —— 本路径完全绕开。组合区光标 / 分句
/// 相关的问题一律以真机 + 宿主侧诊断为准, 别拿 demo 正常当证据。
final class TextViewClient: TextInputClient {
    weak var tv: NSTextView?
    init(_ tv: NSTextView) { self.tv = tv }

    func insertText(_ text: String, replacementRange: NSRange) {
        tv?.insertText(text, replacementRange: replacementRange)
    }
    func setMarkedText(_ text: String, selectionRange: NSRange, replacementRange: NSRange) {
        guard let tv = tv else { return }
        if text.isEmpty {
            tv.unmarkText()
        } else {
            tv.setMarkedText(text, selectedRange: selectionRange, replacementRange: replacementRange)
        }
    }
}

// MARK: - 拦截 keyDown 的 NSTextView

final class IMETextView: NSTextView {
    var onKey: ((NSEvent) -> Bool)?
    override func keyDown(with event: NSEvent) {
        if onKey?(event) == true { return }   // IME 消费, 不走默认插入
        super.keyDown(with: event)
    }
}

// MARK: - 候选框 (rects + 鼠标点选)

final class DemoPanelView: NSView {
    private var image: NSImage?
    private var rects: [CandidateHitRect] = []
    private var lastHover = -1
    var onSelect: ((Int) -> Void)?
    var onHover: ((Int) -> Void)?
    override var isFlipped: Bool { true }
    func update(_ img: NSImage, _ r: [CandidateHitRect]) { image = img; rects = r; needsDisplay = true }
    func setRects(_ r: [CandidateHitRect]) { rects = r }
    override func draw(_ dirtyRect: NSRect) { image?.draw(in: bounds) }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach { removeTrackingArea($0) }
        addTrackingArea(NSTrackingArea(rect: .zero,
            options: [.activeAlways, .mouseMoved, .mouseEnteredAndExited, .inVisibleRect],
            owner: self, userInfo: nil))
    }
    private func hit(_ e: NSEvent) -> Int? {
        let p = convert(e.locationInWindow, from: nil)
        for r in rects where r.contains(px: p.x, py: p.y) { return Int(r.index) }
        return nil
    }
    override func mouseDown(with event: NSEvent) { if let i = hit(event) { onSelect?(i) } }
    override func mouseMoved(with event: NSEvent) {
        let idx = hit(event) ?? -1
        let rep = idx >= 0 ? idx : -1
        if rep != lastHover { lastHover = rep; onHover?(rep) }
    }
    override func mouseExited(with event: NSEvent) {
        if lastHover != -1 { lastHover = -1; onHover?(-1) }
    }
}

final class DemoCandidatePanel: NSPanel {
    private let view = DemoPanelView()
    var onSelect: ((Int) -> Void)? {
        get { view.onSelect } set { view.onSelect = newValue }
    }
    var onHover: ((Int) -> Void)? {
        get { view.onHover } set { view.onHover = newValue }
    }
    init() {
        super.init(contentRect: NSRect(x: 0, y: 0, width: 200, height: 60),
                   styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        isOpaque = false; backgroundColor = .clear; hasShadow = true
        level = .popUpMenu; isFloatingPanel = true
        collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle]
        hidesOnDeactivate = false; becomesKeyOnlyIfNeeded = true
        contentView = view
    }
    func show(_ img: NSImage, at p: NSPoint, rects: [CandidateHitRect]) {
        view.frame = NSRect(origin: .zero, size: img.size)
        view.update(img, rects)
        setContentSize(img.size)
        guard let screen = NSScreen.main ?? NSScreen.screens.first else { orderFrontRegardless(); return }
        setFrameOrigin(NSPoint(x: p.x, y: screen.frame.height - p.y - img.size.height))
        orderFrontRegardless()
    }
    func updateRects(_ r: [CandidateHitRect]) { view.setRects(r) }
    func hidePanel() { orderOut(nil) }
}

// MARK: - IME 测试台

final class IMEHarness: NSObject {
    let textView: IMETextView
    private let client: TextViewClient
    private let router = BridgeResponseRouter()
    private let panel = DemoCandidatePanel()
    private var bridge: BridgeClient?
    private var push: PushClient?
    private var reader: SharedMemoryReader?
    private var latestRects: [CandidateHitRect] = []
    private var currentScale: CGFloat = 1
    private var keySeq: UInt16 = 0
    private let lock = NSLock()

    init(textView: IMETextView) {
        self.textView = textView
        self.client = TextViewClient(textView)
        super.init()
        textView.onKey = { [weak self] ev in self?.handleKey(ev) ?? false }
        panel.onSelect = { [weak self] idx in self?.handleClick(idx) }
        panel.onHover = { [weak self] idx in self?.send(BinaryCodec.encodeCandidateHoverFrame(index: idx)) }
    }

    func start() {
        do {
            bridge = try BridgeClient(socketPath: BridgeEndpoints.requestSocket)
            // 激活 IME: FocusGained + IMEActivated (payload = pid u32, 取 0)
            var pid = Data(count: 4)
            _ = try? bridge?.send(makeFrame(UpstreamCmd.focusGained, pid)); _ = try? bridge?.readFrame()
            _ = try? bridge?.send(makeFrame(UpstreamCmd.imeActivated, pid)); _ = try? bridge?.readFrame()
            _ = pid
            NSLog("Demo: bridge connected + IME activated")
        } catch {
            NSLog("Demo: bridge connect failed: \(error) (先起 wind_input 服务)")
        }
        let pc = PushClient(socketPath: BridgeEndpoints.pushSocket)
        pc.onFrame = { [weak self] f in self?.handlePush(f) }
        try? pc.start()
        push = pc
    }

    private func makeFrame(_ cmd: UInt16, _ payload: Data) -> Data {
        var out = BinaryCodec.encodeHeader(cmd: cmd, payloadLen: UInt32(payload.count))
        out.append(payload); return out
    }

    // MARK: 键盘 → bridge 同步

    private func handleKey(_ event: NSEvent) -> Bool {
        guard let bridge = bridge, bridge.isConnected else { return false }
        keySeq &+= 1
        guard let frame = KeyHandler.encodeKeyEvent(event, seq: keySeq) else { return false }
        do {
            try bridge.send(frame)
            let resp = try bridge.readFrame()
            let consumed = router.apply(resp, to: client)
            sendCaretUpdate()
            return consumed
        } catch {
            NSLog("Demo: key io error \(error)"); return false
        }
    }

    /// 把 NSTextView 光标屏幕坐标上报 Go, 让候选框贴在光标下。
    private func sendCaretUpdate() {
        guard let bridge = bridge, bridge.isConnected else { return }
        let sel = textView.selectedRange()
        var rect = textView.firstRect(forCharacterRange: sel, actualRange: nil)
        if rect.size.height <= 0 { rect.size.height = 18 }
        guard let screen = NSScreen.main ?? NSScreen.screens.first else { return }
        let (x, y, h) = CaretCoords.caretRectToWire(rect, screenHeight: screen.frame.height)
        _ = try? bridge.send(BinaryCodec.encodeCaretUpdateFrame(x: x, y: y, height: h))
        _ = try? bridge.readFrame()
    }

    // MARK: 鼠标点选 → CmdCandidateSelect

    // 点击: index>=0 选词; -1 翻上页; -2 翻下页。
    private func handleClick(_ index: Int) {
        if index >= 0 {
            send(BinaryCodec.encodeCandidateSelectFrame(index: index))
        } else if index == -1 {
            send(BinaryCodec.encodeKeyEventFrame(KeyEventPayload(keyCode: 0x21))) // PgUp
        } else if index == -2 {
            send(BinaryCodec.encodeKeyEventFrame(KeyEventPayload(keyCode: 0x22))) // PgDn
        }
    }

    private func send(_ frame: Data) {
        guard let bridge = bridge, bridge.isConnected else { return }
        _ = try? bridge.send(frame)
        _ = try? bridge.readFrame()    // Ack; 候选更新/commit 走 push
    }

    // MARK: push 通道

    private func handlePush(_ frame: Frame) {
        switch frame.cmd {
        case DownstreamCmd.hostRenderFrame:
            guard let p = try? BinaryCodec.decodeHostRenderFramePayload(frame.payload) else { return }
            showFrame(p)
        case DownstreamCmd.candidateRects:
            if let r = try? BinaryCodec.decodeCandidateRectsPayload(frame.payload) {
                lock.lock(); latestRects = r; let s = currentScale; lock.unlock()
                let logical = Self.scaleRects(r, by: s)
                DispatchQueue.main.async { [weak self] in self?.panel.updateRects(logical) }
            }
        case DownstreamCmd.commitText, DownstreamCmd.updateComposition, DownstreamCmd.clearComposition:
            // 鼠标选词 commit 走 push, 路由回 NSTextView
            DispatchQueue.main.async { [weak self] in
                guard let self = self else { return }
                _ = self.router.apply(frame, to: self.client)
            }
        default: break
        }
    }

    private func showFrame(_ p: HostRenderFramePayload) {
        if (p.flags & 0x1) == 0 || p.width == 0 {
            DispatchQueue.main.async { [weak self] in self?.panel.hidePanel() }; return
        }
        let scale = max(1, CGFloat(p.scale))
        if reader == nil { reader = try? SharedMemoryReader(name: "/WindInput_SHM", size: 4 * 1024 * 1024) }
        guard let f = reader?.snapshot(), let img = Self.makeImage(f, scale: scale) else { return }
        lock.lock(); currentScale = scale; let rects = Self.scaleRects(latestRects, by: scale); lock.unlock()
        DispatchQueue.main.async { [weak self] in
            self?.panel.show(img, at: NSPoint(x: CGFloat(p.x), y: CGFloat(p.y)), rects: rects)
        }
    }

    static func scaleRects(_ rects: [CandidateHitRect], by scale: CGFloat) -> [CandidateHitRect] {
        if scale == 1 { return rects }
        let s = Int32(scale)
        return rects.map { CandidateHitRect(index: $0.index, x: $0.x / s, y: $0.y / s, w: $0.w / s, h: $0.h / s) }
    }

    static func makeImage(_ f: SharedFrame, scale: CGFloat) -> NSImage? {
        guard let provider = CGDataProvider(data: f.bgra as CFData) else { return nil }
        let info: CGBitmapInfo = [CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedFirst.rawValue), .byteOrder32Little]
        guard let cg = CGImage(width: f.width, height: f.height, bitsPerComponent: 8, bitsPerPixel: 32,
                               bytesPerRow: f.stride, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: info,
                               provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)
        else { return nil }
        return NSImage(cgImage: cg, size: NSSize(width: CGFloat(f.width) / scale, height: CGFloat(f.height) / scale))
    }
}

// MARK: - 窗口 + 启动

final class AppDelegate: NSObject, NSApplicationDelegate {
    var window: NSWindow!
    var harness: IMEHarness!

    func applicationDidFinishLaunching(_ note: Notification) {
        let frame = NSRect(x: 0, y: 0, width: 560, height: 320)
        window = NSWindow(contentRect: frame,
                          styleMask: [.titled, .closable, .resizable],
                          backing: .buffered, defer: false)
        window.title = "WindInput Demo — 打拼音 → 选词 (键盘/鼠标)"
        window.center()

        let scroll = NSScrollView(frame: frame)
        scroll.hasVerticalScroller = true
        let tv = IMETextView(frame: frame)
        tv.font = NSFont.systemFont(ofSize: 24)
        tv.isRichText = false
        tv.isAutomaticQuoteSubstitutionEnabled = false
        scroll.documentView = tv
        window.contentView = scroll

        harness = IMEHarness(textView: tv)
        harness.start()

        window.makeKeyAndOrderFront(nil)
        window.makeFirstResponder(tv)
        NSApp.activate(ignoringOtherApps: true)
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.regular)
let delegate = AppDelegate()
app.delegate = delegate

let menubar = NSMenu()
let appItem = NSMenuItem(); menubar.addItem(appItem)
let appMenu = NSMenu()
appMenu.addItem(withTitle: "Quit", action: #selector(NSApp.terminate(_:)), keyEquivalent: "q")
appItem.submenu = appMenu
app.mainMenu = menubar

app.run()
