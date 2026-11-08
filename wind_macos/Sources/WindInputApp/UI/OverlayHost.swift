import Cocoa
import os
import WindInputKit

// OverlayHost — 光栅浮层（悬停提示 / 状态气泡 / Toast / 自绘菜单各级）的承载层。
//
// 服务进程把这几类窗口按主题光栅化进各自的 SHM 段，`CMD_OVERLAY_FRAME` 通知这里；本类
// 读像素、按屏幕几何落位、管自动隐藏计时、把鼠标事件转成服务端认得的上行消息。与 Linux
// Fcitx5 addon 同构，行为逐项对位 `wind_linux`（落位见 `OverlayPlacer`，交互见 `OverlayInput`）。
//
// 线程：**只在主线程使用**（AppKit 窗口、Timer）。`CandidatePanelHost` 收到 push 帧后
// `DispatchQueue.main.async` 再调进来，同一队列保序——悬停提示的 `followCandidate` 落位依赖
// 「候选窗帧先于提示帧生效」。

final class OverlayHost: OverlayPanelDelegate {

    // MARK: 注入（由 CandidatePanelHost 接线）

    /// 上行扩展信封（kind, JSON 对象）。
    var sendExt: ((String, [String: Any]) -> Void)?
    /// 上行菜单指针事件（event, button, wireX, wireY）。
    var sendMenuPointer: ((UInt32, UInt32, Int32, Int32) -> Void)?
    /// 候选窗实际落点 − 服务端建议落点（点）。候选窗被钳边 / 翻转 / 拖动过时，悬停提示跟着挪。
    var candidateShift: (() -> (dx: CGFloat, dy: CGFloat))?
    /// 指针进入 / 离开悬停提示（候选窗据此保持 / 宽限悬停）。
    var onTooltipPointerEntered: (() -> Void)?
    var onTooltipPointerExited: (() -> Void)?
    /// 自绘菜单整个收起（悬停提示据此重定悬停）。
    var onMenuClosed: (() -> Void)?
    /// 本端刚请求打开自绘菜单（候选窗据此撤掉待报的悬停变化与重定）。
    var onMenuRequested: (() -> Void)?

    private static let log = Logger(subsystem: "to.feng.inputmethod.WindInput", category: "overlay")

    private var panels: [UInt32: OverlayPanel] = [:]
    private var readers: [UInt32: SharedMemoryReader] = [:]

    // MARK: 自动隐藏与保持

    private var hideTimers: [UInt32: Timer] = [:]
    private var durations: [UInt32: Int32] = [:]
    /// 指针正悬停在该层上（状态气泡 / Toast；经 `HoverGate` 过滤静止指针）。
    private var hovering: Set<UInt32> = []
    /// 右键后到菜单出现之间的短暂保持（服务端要一个往返才会推菜单帧），上限 2 秒。
    private var menuHoldUntil: [UInt32: Date] = [:]
    private var gates: [UInt32: HoverGate] = [:]

    // MARK: 状态气泡拖动

    private var dragGrab: (dx: CGFloat, dy: CGFloat)?
    private var dragStart: (x: CGFloat, y: CGFloat)?
    /// 指针此刻在悬停提示上（进入时置位，离开 / 提示被收时清，保证 exited 只回调一次）。
    private var pointerOnTooltip = false
    private var dragMoved = false
    private static let dragThreshold: CGFloat = 3

    // MARK: 菜单

    /// 每块屏一个捕获窗（见 `MenuCatcherPanel`）。
    private var menuCatchers: [MenuCatcherPanel] = []
    /// 菜单指针移动的合并：每个 runloop 轮次最多发一条最新位置，免得主线程被阻塞写拖慢。
    private var pendingMotion: (x: Int32, y: Int32)?
    private var menuLevels: Set<UInt32> = []
    private var menuIdleTimer: Timer?
    private var menuCapTimer: Timer?
    /// 请求打开菜单后等第 0 级帧的计时：帧迟迟不来（解码失败 / 没开成），就地收口并通知服务端复位。
    private var menuWaitTimer: Timer?
    private var screenObserver: NSObjectProtocol?
    /// 请求菜单后最多等这么久，帧还没到就当没开成。服务端只要一个往返。
    private static let menuWait: TimeInterval = 2
    /// 菜单空闲（既没有按下、服务端也没推新帧）超过这么久自收。对位 Linux addon 的 60s。
    private static let menuIdle: TimeInterval = 60
    /// 从打开算起的上限，不续。对位 Linux addon 的 120s。
    private static let menuCap: TimeInterval = 120

    var isMenuOpen: Bool { !menuLevels.isEmpty }

    // MARK: - 查询

    func isShown(_ kind: UInt32) -> Bool {
        panels[kind]?.isVisible ?? false
    }

    /// 显示中的悬停提示的 wire 矩形；没显示为 nil。
    func tooltipWireRect() -> WireRect? {
        guard let p = panels[OverlayKind.tooltip], p.isVisible else { return nil }
        return p.wireRect
    }

    /// 指针（wire 坐标）是否落在悬停提示上。
    func tooltipContainsWire(_ x: CGFloat, _ y: CGFloat) -> Bool {
        panels[OverlayKind.tooltip]?.containsWire(x, y) ?? false
    }

    /// 状态气泡内容盒左上的 wire 坐标；不可见返回 nil。应答 `pos.status_tip.query`。
    func statusTipTopLeft() -> (x: Int32, y: Int32)? {
        panels[OverlayKind.status]?.contentTopLeftWire()
    }

    // MARK: - 帧入口

    /// 处理一帧浮层通知。主线程。
    func apply(_ f: OverlayFrame) {
        let kind = f.kind
        guard kind >= OverlayKind.tooltip, kind < OverlayKind.menu + OverlayKind.menuLevels else { return }
        if !f.isVisible || f.width == 0 || f.height == 0 {
            hide(kind)
            return
        }
        guard PanelGeometry.referenceScreen != nil, let image = loadImage(f) else {
            // 菜单第 0 级贴不出来：屏上没有菜单，服务端却已标记 menu_open 并在吞方向键 / 回车。
            // 它只在自己认为菜单不可见时才自愈，这里不报的话要等 Esc 或超时才能恢复。
            if kind == OverlayKind.menu, !isMenuOpen {
                sendExt?(ExtKind.menuDismiss, ["reason": "present_failed"])
            }
            return
        }

        let panel = panelFor(kind)
        let wasVisible = panel.isVisible
        let rect: WireRect
        if kind == OverlayKind.status, dragMoved, let cur = panel.wireRect {
            // 拖动进行中服务端刷新了气泡内容：只换像素，别把窗口从手底下拽回去。
            rect = cur
        } else {
            var shift: (dx: CGFloat, dy: CGFloat) = (0, 0)
            if f.place == OverlayPlace.followCandidate, let s = candidateShift?() { shift = s }
            rect = OverlayPlacer.place(f, workArea: workArea(for: f), shift: shift)
        }
        panel.present(frame: f, image: image, wireRect: rect)

        if OverlayKind.isMenu(kind) {
            menuLevelShown(kind)
        } else {
            if !wasVisible { gates[kind] = Self.newGate() }
            durations[kind] = f.durationMs
            armHide(kind)
        }
    }

    /// 服务重启：旧 SHM 段被 unlink 重建，旧映射读到失效内存，须全部重开；菜单与气泡一并收掉。
    func serviceRestarted() {
        for r in readers.values { r.closeReader() }
        readers.removeAll()
        if isMenuOpen { dismissMenu(reason: "service_restart") }
        hideAll()
    }

    func hideAll() {
        for k in Array(panels.keys) { hide(k) }
    }

    func hide(_ kind: UInt32) {
        hideTimers[kind]?.invalidate()
        hideTimers[kind] = nil
        durations[kind] = nil
        hovering.remove(kind)
        gates[kind] = nil
        if kind == OverlayKind.status { endDrag() }
        panels[kind]?.hidePanel()
        if OverlayKind.isMenu(kind) {
            menuLevelHidden(kind)
        } else if kind == OverlayKind.tooltip, pointerOnTooltip {
            // 提示被服务端收掉时指针还在它上面：AppKit 不会再补发 mouseExited，这里替它收口。
            pointerOnTooltip = false
            onTooltipPointerExited?()
        }
    }

    // MARK: - SHM 与图像

    private func panelFor(_ kind: UInt32) -> OverlayPanel {
        if let p = panels[kind] { return p }
        let p = OverlayPanel(kind: kind)
        p.panelDelegate = self
        panels[kind] = p
        return p
    }

    static func shmName(for kind: UInt32) -> String {
        let tail: String
        switch kind {
        case OverlayKind.tooltip: tail = "_TIP"
        case OverlayKind.status: tail = "_STS"
        case OverlayKind.toast: tail = "_TST"
        case OverlayKind.decor: tail = "_DEC"
        default: tail = "_MN\(kind - OverlayKind.menu)"
        }
        // 与 Rust `wind_bridge::endpoint::overlay_shm_name` 对齐：候选窗那段 + 层后缀。
        return "/WindInput_SHM\(BridgeEndpoints.variantSuffix)\(tail)"
    }

    private func reader(for kind: UInt32) -> SharedMemoryReader? {
        if let r = readers[kind] { return r }
        do {
            let r = try SharedMemoryReader(name: Self.shmName(for: kind), size: 4 * 1024 * 1024)
            readers[kind] = r
            return r
        } catch {
            Self.log.error("overlay SHM 打开失败 kind=\(kind, privacy: .public): \(String(describing: error), privacy: .public)")
            return nil
        }
    }

    private func loadImage(_ f: OverlayFrame) -> NSImage? {
        guard let r = reader(for: f.kind), let snap = r.snapshot() else { return nil }
        // 取到的若是更新的一帧（尺寸与本帧通知不符），放弃这次：随后会有它自己的通知，
        // 用旧元数据摆新像素只会让落位错一拍。
        guard snap.width == Int(f.width), snap.height == Int(f.height) else { return nil }
        return CandidatePanelHost.makeNSImage(from: snap, scale: CGFloat(max(f.scale, 1)))
    }

    // MARK: - 落位

    /// 本帧落点所在屏的可见区（避开菜单栏 / Dock），换成 wire 坐标。
    private func workArea(for f: OverlayFrame) -> WireRect {
        let ref = PanelGeometry.referenceHeight
        let screen: NSScreen?
        if f.place == OverlayPlace.anchor {
            // 锚点类（状态气泡 / Toast）落在焦点所在屏：鼠标所在屏优先。
            screen = PanelGeometry.anchorScreen(wireX: CGFloat(f.x), wireY: CGFloat(f.y))
        } else {
            screen = PanelGeometry.screen(wireX: CGFloat(f.x), wireY: CGFloat(f.y))
        }
        return Self.wireWorkArea(of: screen ?? PanelGeometry.referenceScreen, referenceHeight: ref)
    }

    static func wireWorkArea(of screen: NSScreen?, referenceHeight ref: CGFloat) -> WireRect {
        let vf = screen?.visibleFrame ?? .zero
        return WireRect(x: vf.minX, y: ref - vf.maxY, w: vf.width, h: vf.height)
    }

    // MARK: - 自动隐藏

    private static func newGate() -> HoverGate {
        var g = HoverGate()
        let m = NSEvent.mouseLocation
        g.rebase(m.x, WireGeometry.flipY(m.y, screenHeight: PanelGeometry.referenceHeight))
        return g
    }

    /// 悬停 / 拖动 / 菜单开着时不自动消失，结束后重新计时（对位 Windows `should_stay_visible`）。
    private func isHeld(_ kind: UInt32) -> Bool {
        if hovering.contains(kind) { return true }
        if kind == OverlayKind.status, dragGrab != nil { return true }
        // 悬停提示 / 状态气泡自己的右键菜单开着时也保持。
        if kind == OverlayKind.status || kind == OverlayKind.tooltip {
            if isMenuOpen { return true }
            if let t = menuHoldUntil[kind], t > Date() { return true }
        }
        return false
    }

    private func armHide(_ kind: UInt32) {
        hideTimers[kind]?.invalidate()
        hideTimers[kind] = nil
        guard let ms = durations[kind], ms > 0, !isHeld(kind) else { return }
        hideTimers[kind] = Timer.scheduledTimer(withTimeInterval: Double(ms) / 1000.0, repeats: false) {
            [weak self] _ in
            guard let self = self else { return }
            self.hideTimers[kind] = nil
            // 到期那刻若被按住 / 悬停，不收；等结束后 `armHide` 重新计时。
            if !self.isHeld(kind) { self.hide(kind) }
        }
    }

    // MARK: - OverlayPanelDelegate

    func overlayMouseEntered(_ panel: OverlayPanel) {
        if panel.kind == OverlayKind.tooltip {
            pointerOnTooltip = true
            onTooltipPointerEntered?()
        }
        // 状态气泡 / Toast 的悬停以「真的动过」为准，见 overlayMouseMoved。
    }

    func overlayMouseExited(_ panel: OverlayPanel) {
        let kind = panel.kind
        if kind == OverlayKind.tooltip, pointerOnTooltip {
            pointerOnTooltip = false
            onTooltipPointerExited?()
        }
        if hovering.remove(kind) != nil { armHide(kind) }
    }

    func overlayMouseMoved(_ panel: OverlayPanel, wireX: CGFloat, wireY: CGFloat) {
        let kind = panel.kind
        guard kind == OverlayKind.status || kind == OverlayKind.toast else { return }
        var g = gates[kind] ?? Self.newGate()
        let real = g.accept(wireX, wireY)
        gates[kind] = g
        if real, hovering.insert(kind).inserted {
            hideTimers[kind]?.invalidate()
            hideTimers[kind] = nil
        }
    }

    func overlayMouseDown(_ panel: OverlayPanel, button: Int, wireX: CGFloat, wireY: CGFloat,
                          localX: CGFloat, localY: CGFloat) {
        let kind = panel.kind
        switch OverlayInput.pressAction(kind: kind, button: button) {
        case .close:
            hide(kind)
        case .drag:
            guard let r = panel.wireRect else { return }
            dragGrab = (wireX - r.x, wireY - r.y)
            dragStart = (wireX, wireY)
            dragMoved = false
        case .menu:
            var extra: [String: Any] = [:]
            if kind == OverlayKind.tooltip {
                extra["lx"] = Int(localX.rounded())
                extra["ly"] = Int(localY.rounded())
            }
            menuHoldUntil[kind] = Date().addingTimeInterval(2)
            hideTimers[kind]?.invalidate()
            hideTimers[kind] = nil
            requestMenu(target: OverlayInput.menuTarget(kind: kind), at: (wireX, wireY), extra: extra)
        case .none:
            break
        }
    }

    func overlayMouseDragged(_ panel: OverlayPanel, wireX: CGFloat, wireY: CGFloat) {
        guard panel.kind == OverlayKind.status, let grab = dragGrab, let start = dragStart,
              let f = panel.frameInfo else { return }
        if !dragMoved {
            // 阈值内的抖动不算拖动：一次单纯的点击否则会被当成拖动完成，白搭一次落盘。
            guard abs(wireX - start.x) >= Self.dragThreshold
                    || abs(wireY - start.y) >= Self.dragThreshold else { return }
            dragMoved = true
            hideTimers[OverlayKind.status]?.invalidate()
            hideTimers[OverlayKind.status] = nil
        }
        let wa = Self.wireWorkArea(
            of: PanelGeometry.screen(wireX: wireX, wireY: wireY), referenceHeight: PanelGeometry.referenceHeight)
        let r = OverlayPlacer.drag(pointer: wireX, wireY, grab: grab, frame: f, workArea: wa)
        panel.move(to: r)
    }

    func overlayMouseUp(_ panel: OverlayPanel, wireX: CGFloat, wireY: CGFloat) {
        guard panel.kind == OverlayKind.status else { return }
        let moved = dragMoved
        endDrag()
        if moved, let p = panel.contentTopLeftWire() {
            // 松手报落点；服务端据当前定位方式决定落不落盘（固定位置才写配置）。
            sendExt?(ExtKind.posStatusTip, ["x": Int(p.x), "y": Int(p.y)])
        }
        armHide(OverlayKind.status)
    }

    private func endDrag() {
        dragGrab = nil
        dragStart = nil
        dragMoved = false
    }

    // MARK: - 菜单请求

    /// 请求服务端打开自绘菜单。`at` 是菜单左上角的锚点（wire 点）；工作区取锚点所在屏的可见区。
    func requestMenu(target: Int, at p: (CGFloat, CGFloat), extra: [String: Any] = [:]) {
        let wa = Self.wireWorkArea(
            of: PanelGeometry.screen(wireX: p.0, wireY: p.1), referenceHeight: PanelGeometry.referenceHeight)
        var body: [String: Any] = [
            "target": target,
            "x": Int(p.0.rounded()),
            "y": Int(p.1.rounded()),
            "work": [Int(wa.x.rounded()), Int(wa.y.rounded()),
                     Int(wa.right.rounded()), Int(wa.bottom.rounded())],
        ]
        for (k, v) in extra { body[k] = v }
        onMenuRequested?()
        sendExt?(ExtKind.menuOpen, body)
        armMenuWait()
    }

    // MARK: - 菜单生命周期

    private func menuLevelShown(_ kind: UInt32) {
        let first = menuLevels.isEmpty
        menuLevels.insert(kind)
        if first {
            menuWaitTimer?.invalidate(); menuWaitTimer = nil
            // 菜单开着时屏幕布局变了（插拔 / 分辨率 / 缩放）：捕获窗没盖到新屏，参照屏高度也变了，
            // 之后上报的指针坐标与已摆好的菜单不再同一参照——直接收掉最稳。
            screenObserver = NotificationCenter.default.addObserver(
                forName: NSApplication.didChangeScreenParametersNotification, object: nil, queue: .main
            ) { [weak self] _ in self?.dismissMenu(reason: "screen_change") }
            // 每块屏一个捕获窗；屏幕布局可能在两次菜单之间变过，所以每次都按当前屏幕重建。
            menuCatchers.forEach { $0.uncover() }
            menuCatchers = NSScreen.screens.map { screen in
                let c = MenuCatcherPanel()
                c.onPointer = { [weak self] event, button, x, y in
                    self?.menuPointer(event, button, x, y)
                }
                c.cover(screen)
                return c
            }
            // 菜单一开，悬停提示 / 状态气泡的自动隐藏暂停。
            hideTimers[OverlayKind.tooltip]?.invalidate(); hideTimers[OverlayKind.tooltip] = nil
            hideTimers[OverlayKind.status]?.invalidate(); hideTimers[OverlayKind.status] = nil
            menuCapTimer?.invalidate()
            menuCapTimer = Timer.scheduledTimer(withTimeInterval: Self.menuCap, repeats: false) {
                [weak self] _ in self?.dismissMenu(reason: "grab_cap")
            }
        }
        // 服务端推来帧 = 服务端还活着、菜单还在被操作（键盘导航每一步都会重绘），续空闲计时。
        // 抓取上限（menuCap）不续。
        touchMenu()
    }

    private func menuLevelHidden(_ kind: UInt32) {
        guard menuLevels.remove(kind) != nil, menuLevels.isEmpty else { return }
        closeMenuUI()
        onMenuClosed?()
    }

    /// 收菜单的公共尾巴：撤捕获窗与计时，按指针真实位置重算状态气泡 / Toast 的悬停。
    private func closeMenuUI() {
        menuCatchers.forEach { $0.uncover() }
        menuCatchers.removeAll()
        menuIdleTimer?.invalidate(); menuIdleTimer = nil
        menuCapTimer?.invalidate(); menuCapTimer = nil
        menuWaitTimer?.invalidate(); menuWaitTimer = nil
        if let o = screenObserver { NotificationCenter.default.removeObserver(o); screenObserver = nil }
        pendingMotion = nil
        menuHoldUntil.removeAll()
        // 菜单开着期间气泡可能没收到 mouseExited：以指针此刻的位置为准重设悬停，
        // 否则 `hovering` 残留会让气泡永远不再自动消失。
        let m = NSEvent.mouseLocation
        let (wx, wy) = (m.x, WireGeometry.flipY(m.y, screenHeight: PanelGeometry.referenceHeight))
        for k in [OverlayKind.status, OverlayKind.toast] where isShown(k) {
            if panels[k]?.containsWire(wx, wy) ?? false {
                hovering.insert(k)
            } else {
                hovering.remove(k)
            }
            gates[k] = Self.newGate()
            armHide(k)
        }
    }

    /// 请求菜单后起的等待计时（见 `menuWaitTimer`）。到期仍没有菜单：撤掉「右键后保持」、
    /// 让气泡 / 提示恢复自动隐藏，并通知服务端复位 `menu_open`（它此刻可能已置位并在吞键）。
    private func armMenuWait() {
        menuWaitTimer?.invalidate()
        menuWaitTimer = Timer.scheduledTimer(withTimeInterval: Self.menuWait, repeats: false) {
            [weak self] _ in
            guard let self = self else { return }
            self.menuWaitTimer = nil
            guard !self.isMenuOpen else { return }
            self.menuHoldUntil.removeAll()
            self.armHide(OverlayKind.status)
            self.armHide(OverlayKind.tooltip)
            self.sendExt?(ExtKind.menuDismiss, ["reason": "no_frame"])
        }
    }

    /// 记一次菜单操作：空闲超时从这里重新起算。按下与服务端每推一帧（含悬停高亮、键盘导航的重绘）
    /// 都算，所以光晃鼠标也会续；兜底的是不续的抓取上限（`menuCap`）。
    private func touchMenu() {
        menuIdleTimer?.invalidate()
        menuIdleTimer = Timer.scheduledTimer(withTimeInterval: Self.menuIdle, repeats: false) {
            [weak self] _ in self?.dismissMenu(reason: "idle")
        }
    }

    private func menuPointer(_ event: UInt32, _ button: UInt32, _ x: CGFloat, _ y: CGFloat) {
        let (px, py) = (Int32(x.rounded()), Int32(y.rounded()))
        if event == 2 {
            // 按下：先把已攒的移动发出去（保序），再发按下。
            touchMenu()
            flushMotion()
            sendMenuPointer?(event, button, px, py)
            return
        }
        // 移动：每个 runloop 轮次只发最新一条。
        let scheduled = pendingMotion != nil
        pendingMotion = (px, py)
        if !scheduled {
            DispatchQueue.main.async { [weak self] in self?.flushMotion() }
        }
    }

    private func flushMotion() {
        guard let m = pendingMotion else { return }
        pendingMotion = nil
        sendMenuPointer?(1, 0, m.x, m.y)
    }

    /// 本端自己把菜单收掉（超时 / 服务重启 / 断线…），并告诉服务端复位 `menu_open`。
    /// 服务端复位后会推各级隐藏帧，那时 `menuLevelHidden` 收尾；这里先就地收，免得等不到。
    func dismissMenu(reason: String) {
        guard isMenuOpen else { return }
        for k in menuLevels { panels[k]?.hidePanel() }
        menuLevels.removeAll()
        closeMenuUI()
        sendExt?(ExtKind.menuDismiss, ["reason": reason])
        onMenuClosed?()
    }
}
