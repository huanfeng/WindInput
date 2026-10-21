import Cocoa
import os
import WindInputKit

// CandidatePanelHost — IMKit `.app` 内的候选框承载层 (PR-A.5 Phase 1 + M5 鼠标点选).
//
// 职责:
//   1. 启动时 try open /WindInput_SHM, 启 PushClient 订阅 bridge_push.sock
//   2. 收 CmdHostRenderFrame → snapshot SHM → CGImage → 贴 NSPanel
//   3. 收 CmdCandidateRects → 存命中矩形, 喂 panel 供鼠标 hit-test
//   4. NSPanel 鼠标点选 → 发 CmdCandidateSelect 回 Go (经独立 request 连接)
//   4b. 收 CmdOverlayFrame → 交 OverlayHost (悬停提示 / 状态气泡 / Toast / 自绘菜单, 同样是
//       服务端光栅化后经 SHM 贴图); 候选悬停与悬停提示的保持 / 宽限在这里协调
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
    /// 悬停提示 / 状态气泡 / Toast / 自绘菜单的承载层。仅主线程使用。
    private let overlays = OverlayHost()
    /// 最近一次**实际报给服务端**的候选悬停 (仅主线程)。可能晚于鼠标真实位置: 悬停提示显示着时,
    /// 候选悬停的变化会被延后上报, 给「从候选行穿过空隙挪进提示」留时间 (见 `handleRawHover`)。
    private var reportedHover = -1
    private var pendingHover: DispatchWorkItem?
    /// 待报悬停的目标 (计时中途目标可换, 计时不重排——正朝提示去时除外, 见 `notePointerMoved`)。
    private var pendingHoverTarget = -1
    /// `pendingHover` 的到期时刻。
    private var pendingHoverDue: DispatchTime = .now()
    /// 上一次指针移动的 wire 坐标, 判这一步的方向用。离开候选窗 / 撤掉待报项时清掉。
    private var lastPointerWire: (x: CGFloat, y: CGFloat)?
    /// 本次宽限里离提示最近的距离 (`tipGap`), 刷新新低才顺延。
    private var tipBest: CGFloat?
    private var pendingRecheck: DispatchWorkItem?
    /// 服务端最近一次建议的候选窗落点 (wire 左上, 仅主线程)。悬停提示按「候选窗实际落点 − 它」平移。
    private var lastSuggestedWire: (x: CGFloat, y: CGFloat)?
    private var reader: SharedMemoryReader?
    private var push: PushClient?
    private var sendClient: BridgeClient?       // 发 CmdCandidateSelect 用 (request 连接)
    private var latestRects: [CandidateHitRect] = []
    private var currentScale: CGFloat = 1
    private var reconnecting = false            // push 重连排程中, 防重复
    private let lock = NSLock()

    /// 逐帧渲染的跟踪走 debug 级: 默认不落盘、插值惰性求值, 每键不再付格式化的代价;
    /// 要看时 `log stream --level debug --predicate 'category == "render"'`。
    private static let renderLog = Logger(subsystem: "to.feng.inputmethod.WindInput", category: "render")

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
        panel.onSelect = { [weak self] index in self?.handlePanelClick(index) }
        panel.onHover = { [weak self] index in self?.handleRawHover(index) }
        panel.onPointerMoved = { [weak self] in self?.notePointerMoved() }
        panel.onContextMenu = { [weak self] target in self?.openMenu(target: target) }
        wireOverlays()
        panel.onScroll = { [weak self] delta in
            self?.sendFrame(BinaryCodec.encodeCandidateScrollFrame(delta: delta))
        }
        panel.onMoved = { [weak self] x, y in self?.reportPos(ExtKind.posCandidate, x, y) }
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

    // MARK: - 浮层 (悬停提示 / 状态气泡 / Toast / 自绘菜单)

    /// 把 `OverlayHost` 的注入点接到本类的发送通路上。
    private func wireOverlays() {
        overlays.sendExt = { [weak self] kind, obj in
            guard let body = try? JSONSerialization.data(withJSONObject: obj) else { return }
            self?.sendFrame(BinaryCodec.encodeExtFrame(kind: kind, body: body))
        }
        overlays.sendMenuPointer = { [weak self] event, button, x, y in
            self?.sendAsync(BinaryCodec.encodeMenuPointerFrame(event: event, button: button, x: x, y: y))
        }
        overlays.candidateShift = { [weak self] in self?.candidateShift() ?? (0, 0) }
        overlays.onTooltipPointerEntered = { [weak self] in
            // 指针挪进了提示: 撤掉待报的悬停变化与重定, 提示留着让用户够得着它。
            self?.pendingHover?.cancel(); self?.pendingHover = nil
            self?.pendingRecheck?.cancel(); self?.pendingRecheck = nil
        }
        overlays.onTooltipPointerExited = { [weak self] in
            self?.scheduleHoverRecheck(afterMs: OverlayInput.tipGraceMs)
        }
        overlays.onMenuClosed = { [weak self] in self?.scheduleHoverRecheck(afterMs: 0) }
        overlays.onMenuRequested = { [weak self] in self?.cancelPendingHover() }
    }

    /// 候选窗实际落点 − 服务端建议落点 (点)。候选窗被钳边 / 翻转 / 拖动过时, 悬停提示跟着挪。
    private func candidateShift() -> (dx: CGFloat, dy: CGFloat) {
        guard let sug = lastSuggestedWire, let actual = panel.wireTopLeft() else { return (0, 0) }
        return (CGFloat(actual.0) - sug.x, CGFloat(actual.1) - sug.y)
    }

    /// 撤掉待报的悬停变化与重定 (指针进了提示 / 点击 / 菜单请求时)。
    private func cancelPendingHover() {
        lastPointerWire = nil
        pendingHover?.cancel(); pendingHover = nil
        pendingRecheck?.cancel(); pendingRecheck = nil
    }

    /// 候选悬停变化 (主线程)。悬停提示显示着时, 变化被延后上报: 一离开候选就报的话, 提示当场被
    /// 收掉, 用户永远够不着它 (Windows `candidate_window::hover_move` 同一规则)。
    private func handleRawHover(_ raw: Int) {
        // 菜单开着时指针归捕获窗：候选视图补发的 mouseExited 不是用户真的移开了，别据此收提示。
        // 菜单收起时 `onMenuClosed` 会按指针真实位置重定一次。
        guard !overlays.isMenuOpen else { return }
        if raw == CandidateContentView.noHover { lastPointerWire = nil }   // 指针离开了候选窗
        let norm = raw < 0 ? -1 : raw
        // 已有待报项: 回到已报的目标 = 撤销待报; 否则只换目标, 计时不顺延 (一路擦过多个候选
        // 不该把宽限无限拖长)。对位 Linux addon 的 candidateHover。
        if pendingHover != nil {
            if norm == reportedHover {
                pendingHover?.cancel(); pendingHover = nil
            } else {
                pendingHoverTarget = raw
            }
            return
        }
        let ms = OverlayInput.hoverDeferMs(last: reportedHover, raw: raw,
                                           tipShown: overlays.isShown(OverlayKind.tooltip))
        if ms == 0 {
            reportHover(raw)
            return
        }
        pendingHoverTarget = raw
        tipBest = currentTipGap()
        schedulePendingHover(afterMs: ms)
    }

    private func currentTipGap() -> CGFloat? {
        guard let p = lastPointerWire, let tip = overlays.tooltipWireRect() else { return nil }
        return OverlayInput.tipGap(p.x, p.y, tip: tip)
    }

    private func schedulePendingHover(afterMs ms: UInt32) {
        pendingHover?.cancel()
        let item = DispatchWorkItem { [weak self] in
            guard let self = self else { return }
            self.pendingHover = nil
            self.reportHover(self.pendingHoverTarget)
        }
        pendingHover = item
        pendingHoverDue = .now() + .milliseconds(Int(ms))
        DispatchQueue.main.asyncAfter(deadline: pendingHoverDue, execute: item)
    }

    /// 指针每次移动: 有待报的悬停变化且这一步正朝提示去, 就把到期按 switch 往后顺延——展开态的
    /// 提示停靠在窗口外侧, 过去要横穿好几格; 停下来才切换。对位 Linux addon 的 candidateHover。
    private func notePointerMoved() {
        let m = NSEvent.mouseLocation
        let now = (x: m.x, y: WireGeometry.flipY(m.y, screenHeight: PanelGeometry.referenceHeight))
        let prev = lastPointerWire
        lastPointerWire = now
        guard let prev, pendingHover != nil, let tip = overlays.tooltipWireRect(),
              OverlayInput.headingToTip(prev.x, prev.y, now.x, now.y, tip: tip) else { return }
        let gap = OverlayInput.tipGap(now.x, now.y, tip: tip)
        guard let best = tipBest, gap < best else { return }
        tipBest = gap
        let ms = OverlayInput.tipSwitchMs
        guard DispatchTime.now() + .milliseconds(Int(ms)) > pendingHoverDue else { return }
        schedulePendingHover(afterMs: ms)
    }

    private func reportHover(_ index: Int) {
        reportedHover = index < 0 ? -1 : index   // 翻页器 (-1/-2) 与无悬停哨兵一律记作「无」
        sendFrame(BinaryCodec.encodeCandidateHoverFrame(index: index))
    }

    /// 离开悬停提示 (或菜单收起) 后, 宽限到期重新定悬停: 指针回到提示 / 提示所属候选行就不动,
    /// 在候选窗别处报那里, 在外面报「无」(提示随之收起)。
    private func scheduleHoverRecheck(afterMs ms: UInt32) {
        pendingRecheck?.cancel()
        let item = DispatchWorkItem { [weak self] in
            self?.pendingRecheck = nil
            self?.recheckHover()
        }
        pendingRecheck = item
        if ms == 0 {
            DispatchQueue.main.async(execute: item)
        } else {
            DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(Int(ms)), execute: item)
        }
    }

    private func recheckHover() {
        // 菜单开着不重定: 提示右键菜单挂在提示上, 此刻把悬停报成「无」会把提示收掉。
        // 菜单关闭时 `onMenuClosed` 会补做一次。
        guard !overlays.isMenuOpen else { return }
        guard reportedHover >= 0 || overlays.isShown(OverlayKind.tooltip) else { return }
        let m = NSEvent.mouseLocation
        let wy = WireGeometry.flipY(m.y, screenHeight: PanelGeometry.referenceHeight)
        let onTip = overlays.tooltipContainsWire(m.x, wy)
        let (onCandidate, hit) = panel.hover(atScreen: m)
        let r = OverlayInput.tipRecheckHover(onTip: onTip, onCandidate: onCandidate,
                                             candidateHit: hit, held: reportedHover)
        // 上行线约定: 「无悬停」是 Int32.min 哨兵; -1 / -2 是翻页器 (服务端 mac 分支)。
        if r != OverlayInput.keepHover { reportHover(r >= 0 ? r : CandidateContentView.noHover) }
    }

    /// 候选窗右键: 请求服务端在鼠标处打开自绘菜单 (候选下标 ≥ 0 → 候选菜单, -1 → 主菜单)。
    private func openMenu(target: Int) {
        cancelPendingHover()
        let m = NSEvent.mouseLocation
        let wy = WireGeometry.flipY(m.y, screenHeight: PanelGeometry.referenceHeight)
        overlays.requestMenu(target: target, at: (m.x, wy))
    }

    /// 发一帧、不等 ack (带异步标志)。菜单指针移动每秒上百次, 同步往返会拖慢主线程。
    private func sendAsync(_ frame: Data) {
        lock.lock()
        if sendClient == nil {
            sendClient = try? BridgeClient(socketPath: BridgeEndpoints.requestSocket,
                                            ioTimeoutMs: BridgeEndpoints.requestIOTimeoutMs)
        }
        let c = sendClient
        lock.unlock()
        guard let c = c else { return }
        do {
            try c.send(frame)
        } catch {
            lock.lock(); sendClient?.close(); sendClient = nil; lock.unlock()
        }
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
            // 服务断了, 没人会再推菜单的隐藏帧: 本端就地收菜单 (服务端重启后自会复位)。
            DispatchQueue.main.async {
                self.overlays.dismissMenu(reason: "push_disconnected")
                self.overlays.hideAll()
            }
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
            self?.overlays.dismissMenu(reason: "stop")
            self?.overlays.hideAll()
        }
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
        pendingHover?.cancel(); pendingHover = nil
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
                sendClient = try? BridgeClient(socketPath: BridgeEndpoints.requestSocket,
                                                ioTimeoutMs: BridgeEndpoints.requestIOTimeoutMs)
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
                if error as? IPCError == .timeout { return nil } // 服务卡住, 重试只会再等一轮
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
            sendClient = try? BridgeClient(socketPath: BridgeEndpoints.requestSocket,
                                            ioTimeoutMs: BridgeEndpoints.requestIOTimeoutMs)
        }
        let c = sendClient
        lock.unlock()
        guard let c = c else { return }
        do {
            try c.send(frame)
            _ = try c.readFrame()
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
            // 各浮层的 SHM 段同样被重建; 开着的菜单也随旧服务一并作废。
            DispatchQueue.main.async { [weak self] in
                self?.lastSuggestedWire = nil
                self?.overlays.serviceRestarted()
            }
        case DownstreamCmd.overlayFrame:
            if let f = OverlayFrame.decode(frame.payload) {
                DispatchQueue.main.async { [weak self] in self?.overlays.apply(f) }
            }
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
        case DownstreamCmd.ext:
            // 扩展信封：低频消息统一入口。未知 kind 安静忽略（版本兼容的根本）。
            guard let (kind, body) = BinaryCodec.decodeExt(frame.payload) else { break }
            switch kind {
            case ExtKind.settingsOpen:
                // body = {"args":["--page=dict", …]}，argv 已由 Rust 侧切好，这里直接用。
                let args = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any]
                let argv = (args?["args"] as? [String]) ?? []
                ModeStatusController.shared.openSettings(arguments: argv)
            case ExtKind.posCandidateQuery, ExtKind.posStatusTipQuery:
                // 服务端问「你现在在哪」(用户点了菜单里的「固定位置」)。浮窗不可见时**不答**:
                // 那种情况下没有"当前位置"可言, 沉默让服务端保留旧值, 好过报一个上次残留的
                // 坐标把配置写坏。
                let isCandidate = (kind == ExtKind.posCandidateQuery)
                DispatchQueue.main.async { [weak self] in
                    guard let self = self else { return }
                    let pos = isCandidate ? self.panel.wireTopLeft() : self.overlays.statusTipTopLeft()
                    guard let (x, y) = pos else { return }
                    self.reportPos(isCandidate ? ExtKind.posCandidate : ExtKind.posStatusTip, x, y)
                }
            default:
                NSLog("WindInput[ext] 未处理的 kind=\(kind)")
            }
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
        Self.renderLog.debug("renderFrame seq=\(p.seq, privacy: .public) vis=\(visible, privacy: .public) \(p.width, privacy: .public)x\(p.height, privacy: .public) x=\(p.x, privacy: .public) y=\(p.y, privacy: .public) scale=\(p.scale, privacy: .public) flags=0x\(String(p.flags, radix: 16), privacy: .public)")
        if !visible || p.width == 0 || p.height == 0 {
            Self.renderLog.debug("renderFrame hiding (invisible or zero-size)")
            DispatchQueue.main.async { [weak self] in
                self?.lastSuggestedWire = nil
                self?.reportedHover = -1
                self?.pendingHover?.cancel(); self?.pendingHover = nil
                self?.panel.hidePanel()
                // 悬停提示随候选一起收: 服务端 `hide_frame` 会补推它的隐藏帧, 这里先收省一拍。
                self?.overlays.hide(OverlayKind.tooltip)
            }
            return
        }
        let scale = max(1, CGFloat(p.scale))
        if reader == nil { lock.lock(); openSHMIfNeeded(); lock.unlock() }
        guard let r = reader else {
            Self.renderLog.error("renderFrame FAIL: reader is nil after openSHMIfNeeded")
            return
        }
        guard let frame = r.snapshot() else {
            Self.renderLog.error("renderFrame FAIL: snapshot() returned nil")
            return
        }
        Self.renderLog.debug("renderFrame SHM frame \(frame.width, privacy: .public)x\(frame.height, privacy: .public) flags=0x\(String(frame.flags, radix: 16), privacy: .public) seq=\(frame.sequence, privacy: .public) dataSize=\(frame.bgra.count, privacy: .public)")
        guard let img = Self.makeNSImage(from: frame, scale: scale) else {
            Self.renderLog.error("renderFrame FAIL: makeNSImage returned nil (frame \(frame.width, privacy: .public)x\(frame.height, privacy: .public) bgra=\(frame.bgra.count, privacy: .public))")
            return
        }
        let pt = NSPoint(x: CGFloat(p.x), y: CGFloat(p.y))
        let useSoftwareShadow = frame.hasSoftwareShadow
        let absolute = p.isAbsolutePos
        lock.lock(); currentScale = scale; let rects = Self.scaleRects(latestRects, by: scale); lock.unlock()
        DispatchQueue.main.async { [weak self] in
            // 软件阴影已渲染在 image 中时禁用系统窗口阴影，避免在画布边缘产生黑边。
            self?.panel.hasShadow = !useSoftwareShadow
            self?.lastSuggestedWire = (pt.x, pt.y)
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
        // 头与像素不是原子写入：读到半新半旧的头时 stride×height 会超出像素缓冲，CGImage 会越界读。
        guard f.stride >= f.width * 4, f.bgra.count >= f.stride * f.height else { return nil }
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
