import Cocoa
import WindInputKit

// CandidatePanel — IMKit `.app` 候选框浮窗 (PR-A.5 Phase 1 + M5 鼠标点选).
//
// 设计要点 (与 rime/squirrel SquirrelPanel.m 对齐):
//   - styleMask: [.borderless, .nonactivatingPanel] — 无标题栏, 不抢键盘焦点
//   - level = .popUpMenu — 浮在普通窗上, 不抢全屏窗
//   - collectionBehavior 含 .canJoinAllSpaces — 跟随用户切 Space
//   - isOpaque=false + backgroundColor=clear — 让候选框 RGBA alpha 走起
//   - hidesOnDeactivate=false — 切到别的 .app 时不消失 (IME 全局)
//
// 鼠标点选: contentView 自绘 bitmap + 持候选命中矩形 (panel-local, top-left),
//   mouseDown 命中 → onSelect(pageLocalIndex)。nonactivating panel 仍收 mouseDown,
//   acceptsFirstMouse=true 让首次点击 (panel 非 key 时) 也生效。

/// 自绘候选框 bitmap + 处理鼠标命中的内容视图。
final class CandidateContentView: NSView {
    private var image: NSImage?
    private var hitRects: [CandidateHitRect] = []
    /// 无悬停哨兵：须区别于翻页器 hover(-1 上页 / -2 下页)，故不能复用 -1。
    static let noHover = Int(Int32.min)
    private var lastHover: Int = CandidateContentView.noHover
    var onSelect: ((Int) -> Void)?
    var onHover: ((Int) -> Void)?
    /// 指针每次移动（不论命中变没变），悬停宽限据此判方向。
    var onPointerMoved: (() -> Void)?
    /// 右键：候选上 → 该候选页内下标（≥0）；空白 / 翻页区 → -1（主菜单）。菜单由服务端自绘，
    /// 这里只报「在哪个目标上右键」，见 `OverlayHost.requestMenu`。
    var onContextMenu: ((Int) -> Void)?
    var onScroll: ((Int32) -> Void)?              // 滚轮 (delta, WHEEL_DELTA 倍数, 正=上滚)
    var onDragMoved: (() -> Void)?                // 拖动进行中 (每次位移后)
    var onDragEnded: (() -> Void)?                // 空白区拖动松手 (整窗已移到新位置)
    /// 拖动起点：光标屏幕位置 + 窗口起始左下角。nil = 未在拖动。
    /// 记锚点差值而不是每次跟随光标当前点，是为了不把「按下点在窗口内的偏移」丢掉。
    private var dragAnchor: NSPoint?
    private var dragOrigin: NSPoint?
    /// 本次按下是否已越过位移阈值。**没越过就不算拖动**：空白区(编码栏/内边距)的一次普通
    /// 点击否则会被当成拖动完成——跟随光标模式下窗口被就地冻结一整轮组合，固定位置模式下
    /// 每次点击还白搭一次 IPC 往返 + 一次配置写盘。
    private var dragMoved = false
    /// 阈值取 3pt：小于手指/鼠标在点击瞬间的自然抖动即可，不必更大。
    private static let dragThreshold: CGFloat = 3
    /// 按下时命中的候选/翻页键。选词等到**松开**且仍落在同一项上才发, 见 `mouseUp`。
    private var pressedIndex: Int?
    /// 本帧皮肤区域 (逻辑点)：按下落在不拖动区 (block 装饰) 时不起拖，见 `CandidateSkin.blocksDrag`。
    var skin = CandidateSkin()
    override var isFlipped: Bool { true } // top-left 原点, 与 wire/rects 坐标系一致

    func update(image: NSImage, rects: [CandidateHitRect]) {
        self.image = image
        self.hitRects = rects
        needsDisplay = true
    }

    /// 仅更新命中矩形 (rects 帧晚于 render 帧到达时用)。
    func setRects(_ rects: [CandidateHitRect]) {
        self.hitRects = rects
    }

    /// 屏幕点是否落在本视图内、以及命中的候选页内下标（翻页键 / 空白 → -1）。
    /// 离开悬停提示后重定悬停用：指针此刻在不在候选窗上、在哪一项上。
    func hover(atScreen p: NSPoint, in window: NSWindow) -> (inside: Bool, index: Int) {
        let winPt = window.convertFromScreen(NSRect(origin: p, size: .zero)).origin
        let v = convert(winPt, from: nil)
        guard bounds.contains(v) else { return (false, -1) }
        for r in hitRects where r.contains(px: v.x, py: v.y) {
            return (true, Int(r.index) >= 0 ? Int(r.index) : -1)
        }
        return (true, -1)
    }

    override func draw(_ dirtyRect: NSRect) {
        // isFlipped=true 时 NSImage.draw(in:) 能正确补偿坐标系翻转，保证 BGRA 帧方向正确。
        // 不可用 CGContext.draw(cg, in:)：在翻转坐标系下 CGContext 不做翻转补偿，图像会倒置。
        image?.draw(in: bounds)
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach { removeTrackingArea($0) }
        addTrackingArea(NSTrackingArea(rect: .zero,
                                       options: [.activeAlways, .mouseMoved, .mouseEnteredAndExited, .inVisibleRect],
                                       owner: self, userInfo: nil))
    }

    /// 返回页内 index 候选在屏幕坐标系下的矩形 (供 tooltip 定位)。
    /// hitRects 是 flipped view-local 逻辑坐标 (top-left), 经 view→window→screen 转换。
    func screenRect(forIndex index: Int, in window: NSWindow) -> NSRect? {
        guard let r = hitRects.first(where: { Int($0.index) == index }) else { return nil }
        let viewRect = NSRect(x: CGFloat(r.x), y: CGFloat(r.y), width: CGFloat(r.w), height: CGFloat(r.h))
        let winRect = convert(viewRect, to: nil)
        return window.convertToScreen(winRect)
    }

    /// 命中候选 (index>=0) / 翻页按钮 (index<0) / 空白 (nil)。
    private func hitIndex(_ event: NSEvent) -> Int? {
        let p = convert(event.locationInWindow, from: nil) // isFlipped, top-left
        for r in hitRects where r.contains(px: p.x, py: p.y) { return Int(r.index) }
        return nil
    }

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func mouseDown(with event: NSEvent) {
        // 上一次点击的残留先清掉: 按下期间窗口被收起时, 那次的 mouseUp 不一定送得到。
        pressedIndex = nil
        dragAnchor = nil
        dragOrigin = nil
        dragMoved = false
        if let idx = hitIndex(event) { pressedIndex = idx; return }
        let p = convert(event.locationInWindow, from: nil)
        if skin.blocksDrag(px: p.x, py: p.y) { return }
        // 空白区（编码栏/内边距，非候选非翻页键）→ 起拖，整窗跟随光标。与 Windows 侧
        // WM_LBUTTONDOWN 的同一分支同构（那边用 SetCapture 保证移出窗口后仍收到消息；
        // AppKit 在 mouseDown 之后会把整条拖动序列都发给本视图，无需额外捕获）。
        guard let win = window else { return }
        dragAnchor = NSEvent.mouseLocation
        dragOrigin = win.frame.origin
        dragMoved = false
    }

    override func mouseDragged(with event: NSEvent) {
        guard let anchor = dragAnchor, let origin = dragOrigin, let win = window else { return }
        let now = NSEvent.mouseLocation
        let (dx, dy) = (now.x - anchor.x, now.y - anchor.y)
        if !dragMoved {
            guard abs(dx) >= Self.dragThreshold || abs(dy) >= Self.dragThreshold else { return }
            dragMoved = true
        }
        win.setFrameOrigin(NSPoint(x: origin.x + dx, y: origin.y + dy))
        // 拖动**进行中**就要落 dragPin：候选内容若在此期间刷新 (翻页/继续输入)，show() 会
        // 按服务端算的位置重摆，把窗口从手底下拽回光标处。对齐 Windows 的 WM_MOUSEMOVE 分支。
        onDragMoved?()
    }

    /// 选词在松开时发, 不在按下时 (与 Squirrel 一致)。
    ///
    /// 按下就选的后果 (macOS 27 实测): 上屏 + 收起候选窗在松开前就做完了, 约 50~130ms 后
    /// 窗口又被摆回屏幕 —— 本进程没有任何 order 调用, 只见 WindowServer 把它从 none 改回
    /// visible, 成了停在原地、点了没反应的残影, 直到下一次收起才消失。键盘上屏没有这一下。
    /// 是系统处理这次点击的哪一步把它摆回来的没有查明 (拦掉推迟到松开的窗口排序无效);
    /// 改成整个点击结束再选之后, 收起后的窗口不再回来。
    override func mouseUp(with event: NSEvent) {
        defer { dragAnchor = nil; dragOrigin = nil; dragMoved = false }
        if let idx = pressedIndex {
            pressedIndex = nil
            // 按下后移出该项再松开 = 放弃 (按钮的通行语义)。
            if hitIndex(event) == idx { onSelect?(idx) }
            return
        }
        guard dragAnchor != nil, dragMoved else { return }
        onDragEnded?()
    }

    override func rightMouseDown(with event: NSEvent) {
        // 候选 (index>=0): 候选右键菜单; 空白/翻页区: 主菜单 (方案/主题/简繁/设置…)。
        // 两者都由服务端自绘（与 Windows / Linux 同一份），这里只报目标。
        if let idx = hitIndex(event), idx >= 0 {
            onContextMenu?(idx)
        } else {
            onContextMenu?(-1)
        }
    }

    override func mouseMoved(with event: NSEvent) {
        // 命中即上报：候选(index≥0) / 翻页器(-1 上页 / -2 下页) 均高亮；空白→无悬停哨兵。
        // 先报移动再报命中：宽限起算时（onHover 里）要拿到的是这一次的指针位置。
        onPointerMoved?()
        let report = hitIndex(event) ?? CandidateContentView.noHover
        if report != lastHover { lastHover = report; onHover?(report) }
    }

    /// 滚轮 → 上报 delta，服务端解释成「上下键调整高亮项」(到页边界翻到相邻页)。
    ///
    /// **必须攒够一格再发**：触控板的一次轻扫会来几十个 `scrollingDeltaY` 极小的事件
    /// (`hasPreciseScrollingDeltas`)，逐个上报等于让高亮一口气飞过整页。鼠标滚轮一格
    /// 通常是 ±1 行，直接够阈值。
    ///
    /// wire 单位沿用 Win32 的 `WHEEL_DELTA`(120)、正=上滚：那是既有约定，服务端只有一份实现。
    override func scrollWheel(with event: NSEvent) {
        let dy = event.scrollingDeltaY
        guard dy != 0 else { return }
        if event.phase == .began || event.momentumPhase == .began {
            scrollAccum = 0   // 新一次滑动重新起算，别把上次的余量算进来
        }
        scrollAccum += dy
        let step = event.hasPreciseScrollingDeltas ? Self.preciseStep : 1
        let notches = (scrollAccum / step).rounded(.towardZero)
        guard notches != 0 else { return }
        scrollAccum -= notches * step
        // macOS 的 scrollingDeltaY 正值 = 内容向下走 = 视觉上向**上**滚，与 wire 的
        // 「正=上滚」同向，直接乘 120。natural scrolling 关闭时系统已替我们翻好符号。
        onScroll?(Int32(notches) * 120)
    }

    /// 触控板的累积量，攒够 `preciseStep` 记一格。
    private var scrollAccum: CGFloat = 0
    /// 触控板一格的阈值 (点)。取 10：一次自然轻扫约移动 2~4 项，快扫更多，慢扫能逐项微调。
    private static let preciseStep: CGFloat = 10

    override func mouseExited(with event: NSEvent) {
        let none = CandidateContentView.noHover
        if lastHover != none { lastHover = none; onHover?(none) }
    }
}

final class CandidatePanel: NSPanel {
    private let content = CandidateContentView()

    /// 鼠标点击命中候选时回调 (pageLocalIndex; <0 = 翻页按钮 -1=上 -2=下)。
    var onSelect: ((Int) -> Void)? {
        get { content.onSelect }
        set { content.onSelect = newValue }
    }
    /// 鼠标悬停候选变化时回调 (pageLocalIndex; -1=离开)。
    var onHover: ((Int) -> Void)? {
        get { content.onHover }
        set { content.onHover = newValue }
    }
    var onPointerMoved: (() -> Void)? {
        get { content.onPointerMoved }
        set { content.onPointerMoved = newValue }
    }
    /// 右键回调：候选下标（≥0）或 -1（空白 / 翻页区 → 主菜单）。
    var onContextMenu: ((Int) -> Void)? {
        get { content.onContextMenu }
        set { content.onContextMenu = newValue }
    }
    /// 滚轮回调 (delta, WHEEL_DELTA 倍数, 正=上滚)。服务端解释成高亮上下移。
    var onScroll: ((Int32) -> Void)? {
        get { content.onScroll }
        set { content.onScroll = newValue }
    }
    /// 拖动落定回调, 参数为 wire 坐标 (top-left) 下的窗口左上角。
    /// 服务端据当前定位方式决定落不落盘: 固定位置=重新摆放并写配置; 跟随光标=只是临时挪开。
    var onMoved: ((Int32, Int32) -> Void)?

    /// 本次组合内用户拖动后的落点，**wire 左上角**。
    ///
    /// 非 nil 时 `show()` 用它而不是服务端算出的位置 —— 对齐 Windows 的 `drag_pin`: 拖过之后
    /// 窗口就钉在那儿, 候选内容刷新 (翻页/继续输入) 不会把它拽回光标处。`hidePanel()` 清除,
    /// 即一次组合结束就恢复跟随光标。固定位置模式下服务端本来就一直发同一个坐标, 两者一致。
    ///
    /// 存**左上角**而非 Cocoa 的左下角是必须的（Windows 的 `drag_pin` 同样存内容左上）：
    /// Cocoa 原点是左下角，直接拿它 `setFrameOrigin` 等于钉住**底边**，组合中窗口高度一变
    /// （preedit 出现、竖排行数变化），用户看着的上沿就往上爬。
    private var dragPin: (x: CGFloat, y: CGFloat)?

    /// 拆出的穿透装饰层 (设计 theme-skin-animation.md §17.3)：同原点同尺寸的子 panel，不接鼠标、
    /// 不投影，作为子窗口随候选窗拖动与显隐。点击穿过它与候选窗的透明处落到下面的应用。
    private lazy var decorPanel: NSPanel = {
        let p = NSPanel(contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel],
                        backing: .buffered, defer: true)
        p.isOpaque = false
        p.backgroundColor = .clear
        p.hasShadow = false
        p.ignoresMouseEvents = true
        p.level = self.level
        p.collectionBehavior = self.collectionBehavior
        p.hidesOnDeactivate = false
        let v = NSImageView()
        v.imageScaling = .scaleAxesIndependently
        p.contentView = v
        return p
    }()
    /// 下一次 `show` 要贴的穿透装饰层 (nil = 本帧没有)。服务端先推它、再推候选帧。
    private var decorImage: NSImage?
    private var decorOrder: NSWindow.OrderingMode?

    init() {
        super.init(contentRect: NSRect(x: 0, y: 0, width: 200, height: 60),
                   styleMask: [.borderless, .nonactivatingPanel],
                   backing: .buffered,
                   defer: false)
        self.isOpaque = false
        self.backgroundColor = .clear
        self.hasShadow = true
        // ⚠️ 不要再设 `isFloatingPanel = true`：它的 setter 会把 level 改回 floating(3)，
        // 写在这行后面等于把 `.popUpMenu` 冲掉。3 只压得住普通窗口（layer 0），压不住
        // Spotlight / 「应用程序」列表搜索（layer 23）、Alfred 这类高层宿主，候选窗画了却
        // 被整个盖住（A2-53 / GH#158，26.5.1 实测）。Tooltip / StatusBubble / Toast 同理。
        self.level = .popUpMenu
        self.collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle]
        self.hidesOnDeactivate = false
        self.becomesKeyOnlyIfNeeded = true
        self.contentView = content
        content.onDragMoved = { [weak self] in self?.pinCurrentPosition() }
        content.onDragEnded = { [weak self] in self?.finishDrag() }
    }

    /// 以窗口**当前真实位置**落 dragPin（避免按位移累加产生误差）。
    @discardableResult
    private func pinCurrentPosition() -> (x: Int32, y: Int32)? {
        guard let p = PanelGeometry.wireTopLeft(of: frame) else { return nil }
        dragPin = (CGFloat(p.x), CGFloat(p.y))
        return p
    }

    /// 拖动松手: 落定并把 wire 坐标回报服务端。
    private func finishDrag() {
        guard let p = pinCurrentPosition() else { return }
        onMoved?(p.x, p.y)
    }

    /// 显示候选框: image=BGRA→CGImage 包裹, atScreenPoint=wire top-left, rects=命中矩形,
    /// absolute=该点是用户固定位置的绝对坐标 (见 HostRenderFramePayload.isAbsolutePos)。
    ///
    /// 落位三级优先: dragPin (本次组合内已拖过) > absolute (固定位置) > 跟随光标。
    ///
    /// `anim` = 皮肤动画换帧 (内容与落位都没变)：已在屏上且尺寸未变时照原位贴，不重新落位。
    func show(image: NSImage, atScreenPoint p: NSPoint, rects: [CandidateHitRect],
              skin: CandidateSkin = CandidateSkin(), absolute: Bool = false, anim: Bool = false) {
        let inPlace = anim && isVisible && frame.size == image.size
        content.frame = NSRect(origin: .zero, size: image.size)
        content.update(image: image, rects: rects)
        content.skin = skin
        self.setContentSize(image.size)

        guard let screen = PanelGeometry.referenceScreen else {
            self.orderFrontRegardless()
            return
        }
        let size = image.size

        if inPlace {
            // 换帧：原位不动。
        } else if let pin = dragPin {
            // 拖过就钉住: 内容刷新 (翻页/继续输入) 不把窗口拽回光标处。按左上角重新换算 ——
            // 高度变化时上沿不动; 并夹进可见区, 内容变宽变高后原落点可能已越界。
            if let origin = PanelGeometry.cocoaOrigin(wireX: pin.x, wireY: pin.y, size: size) {
                self.setFrameOrigin(origin)
            }
        } else if absolute {
            // 固定位置: 服务端已算好绝对坐标, 照搬 + 边界钳制, **不做上下翻转** ——
            // 窗口本来就不跟光标走, 翻转只会让靠近屏幕底边的固定点被莫名弹到顶上。
            if let origin = PanelGeometry.cocoaOrigin(wireX: p.x, wireY: p.y, size: size) {
                self.setFrameOrigin(origin)
            }
        } else {
            // wire 点 = 光标底端，按它所在的屏钳边界（GH#179：用主屏钳会把副屏上的候选窗拽回主屏）。
            // 往上挪 4pt 回到光标行内（服务端在光标底端下留了 2pt 间隙）：上下叠放的两块屏，
            // 光标贴在上屏底边时，wire 点会越过分界线落到下屏。
            let caretScreen = PanelGeometry.screen(wireX: p.x, wireY: p.y - 4) ?? screen
            let origin = WireGeometry.followCaretOrigin(
                wireX: p.x, wireY: p.y, size: size,
                screenHeight: screen.frame.height, visibleFrame: caretScreen.visibleFrame)
            self.setFrameOrigin(origin)
        }
        self.orderFrontRegardless()
        syncDecor(size: size, below: skin.splitDecorBelow)
        // 系统原生窗口阴影按内容 alpha 形状计算且会缓存；内容/尺寸变化后必须 invalidate，
        // 否则阴影残留旧形状或退化为矩形（候选窗为透明背景 + 圆角位图，需据新形状重算）。
        if self.hasShadow && !inPlace { self.invalidateShadow() }
    }

    /// 存下一帧的穿透装饰层像素 (nil = 没有)，随后的 `show` 贴上。
    func setDecor(image: NSImage?) {
        decorImage = image
    }

    /// 让穿透装饰子 panel 与候选窗同原点同尺寸，按 `below` 排在候选窗下面或上面。
    private func syncDecor(size: NSSize, below: Bool) {
        guard let img = decorImage, img.size == size else {
            detachDecor()
            return
        }
        let dp = decorPanel
        (dp.contentView as? NSImageView)?.image = img
        dp.setFrame(NSRect(origin: frame.origin, size: size), display: true)
        let order: NSWindow.OrderingMode = below ? .below : .above
        if dp.parent !== self || decorOrder != order {
            if dp.parent != nil { removeChildWindow(dp) }
            addChildWindow(dp, ordered: order)
            decorOrder = order
        }
    }

    private func detachDecor() {
        guard decorOrder != nil else { return }
        removeChildWindow(decorPanel)
        decorPanel.orderOut(nil)
        decorOrder = nil
    }

    /// 更新命中矩形 (CmdCandidateRects 帧晚于 render 帧到达)。
    func updateRects(_ rects: [CandidateHitRect]) {
        content.setRects(rects)
    }

    /// 候选窗当前左上角的 wire 坐标; 不可见时返回 nil (没有"当前位置"可言)。
    /// 应答服务端的 `pos.candidate.query` 用, 与拖动回报共用同一换算。
    func wireTopLeft() -> (Int32, Int32)? {
        guard isVisible, let p = PanelGeometry.wireTopLeft(of: frame) else { return nil }
        return (p.x, p.y)
    }

    /// 页内 index 候选的屏幕矩形 (供 tooltip 定位); 不可见或找不到返回 nil。
    func candidateScreenRect(index: Int) -> NSRect? {
        guard isVisible else { return nil }
        return content.screenRect(forIndex: index, in: self)
    }

    /// 屏幕点在候选窗上的悬停命中，见 `CandidateContentView.hover(atScreen:in:)`。
    func hover(atScreen p: NSPoint) -> (inside: Bool, index: Int) {
        guard isVisible else { return (false, -1) }
        return content.hover(atScreen: p, in: self)
    }

    func hidePanel() {
        // 组合结束 → 解除拖动冻结, 下次显示重新跟随光标 (固定位置模式则继续由服务端发绝对
        // 坐标)。对齐 Windows 侧 hide() 里的 reset_drag()。
        dragPin = nil
        detachDecor()
        self.orderOut(nil)
    }
}
