import Foundation
import CoreGraphics

// OverlayProtocol — 光栅浮层（状态气泡 / Toast / 悬停提示 / 自绘菜单）的 wire 与纯逻辑。
//
// 服务进程把这四类窗口按主题光栅化进各自的 SHM 段，`CMD_OVERLAY_FRAME` 只带「某层有新帧 /
// 该隐藏」与落位规则；`.app` 只负责贴图、按屏幕几何落位、计时、转发鼠标。与 Linux Fcitx5
// addon 同构：本文件逐项对位 `wind_linux` 的 `placeOverlay` / `OverlayInput`，布局见 Rust
// `wind_ipc::codec::encode_overlay_frame`。
//
// **坐标单位**：与候选窗一致，x/y/alt/margin 是 wire 点（原点在主屏左上、y 向下）；
// 位图尺寸与内容盒偏移 / 尺寸是**位图像素**，换算成点要除以帧里带的 `scale`。
// 本文件只做纯几何（不碰 NSScreen），故可在 `swift test` 里测；取屏幕的部分在 App 层。

public enum OverlayKind {
    public static let tooltip: UInt32 = 1
    public static let status: UInt32 = 2
    public static let toast: UInt32 = 3
    /// 自绘菜单第 0 级；第 k 级子菜单是 `menu + k`。
    public static let menu: UInt32 = 4
    public static let menuLevels: UInt32 = 6
    /// 皮肤里拆出的穿透装饰层：不归 `OverlayHost`，挂在候选窗的子 panel 上 (§17.3)。
    public static let decor: UInt32 = 12

    public static func isMenu(_ kind: UInt32) -> Bool {
        kind >= menu && kind < menu + menuLevels
    }
}

public enum OverlayPlace {
    public static let absolute: UInt32 = 0
    public static let flip: UInt32 = 1
    public static let followCandidate: UInt32 = 2
    public static let anchor: UInt32 = 3
    /// 服务端已按宿主报来的工作区算定，宿主原样摆放、不再夹回。
    public static let exact: UInt32 = 4
}

public enum OverlayAnchor {
    public static let center: UInt32 = 1
    public static let topLeft: UInt32 = 2
    public static let topRight: UInt32 = 3
    public static let bottomLeft: UInt32 = 4
    public static let bottomRight: UInt32 = 5
    public static let topCenter: UInt32 = 6
    public static let bottomCenter: UInt32 = 7
}

/// `CMD_OVERLAY_FRAME` (0x0513) 载荷。68 字节基础布局，72 字节版在末尾多一个 `scale`（f32 位型）。
public struct OverlayFrame: Equatable {
    public var kind: UInt32 = 0
    public var seq: UInt32 = 0
    /// 位图尺寸（像素，含软阴影扩边）。隐藏帧为 0。
    public var width: UInt32 = 0
    public var height: UInt32 = 0
    public var flags: UInt32 = 0
    public var place: UInt32 = 0
    public var x: Int32 = 0
    public var y: Int32 = 0
    public var altX: Int32 = 0
    public var altY: Int32 = 0
    public var anchor: UInt32 = 0
    public var margin: Int32 = 0
    /// 内容盒在位图内的偏移与尺寸（像素）。
    public var contentX: Int32 = 0
    public var contentY: Int32 = 0
    public var contentW: UInt32 = 0
    public var contentH: UInt32 = 0
    /// 自动隐藏时长（毫秒）；0 = 常驻到下一帧或隐藏帧。计时归 `.app`。
    public var durationMs: Int32 = 0
    /// 位图像素 / wire 点；旧 68 字节帧缺省 1。
    public var scale: Float = 1

    public init() {}

    public var isVisible: Bool { (flags & 0x1) != 0 }
    public var hasSoftwareShadow: Bool { (flags & 0x4) != 0 }

    public static let baseSize = 68

    /// 解载荷；不足 68 字节返回 nil。
    public static func decode(_ p: Data) -> OverlayFrame? {
        guard p.count >= baseSize else { return nil }
        var f = OverlayFrame()
        func u(_ o: Int) -> UInt32 { p.readUInt32LE(at: o) }
        func i(_ o: Int) -> Int32 { Int32(bitPattern: p.readUInt32LE(at: o)) }
        f.kind = u(0); f.seq = u(4); f.width = u(8); f.height = u(12)
        f.flags = u(16); f.place = u(20)
        f.x = i(24); f.y = i(28); f.altX = i(32); f.altY = i(36)
        f.anchor = u(40); f.margin = i(44)
        f.contentX = i(48); f.contentY = i(52)
        f.contentW = u(56); f.contentH = u(60)
        f.durationMs = i(64)
        if p.count >= baseSize + 4 {
            let s = Float(bitPattern: u(baseSize))
            if s.isFinite && s >= 1 { f.scale = s }
        }
        return f
    }
}

/// wire 下的矩形（点，原点在主屏左上，y 向下）。
public struct WireRect: Equatable {
    public var x: CGFloat
    public var y: CGFloat
    public var w: CGFloat
    public var h: CGFloat

    public init(x: CGFloat, y: CGFloat, w: CGFloat, h: CGFloat) {
        self.x = x; self.y = y; self.w = w; self.h = h
    }

    public var right: CGFloat { x + w }
    public var bottom: CGFloat { y + h }

    public func contains(_ px: CGFloat, _ py: CGFloat) -> Bool {
        px >= x && px < right && py >= y && py < bottom
    }
}

public enum OverlayPlacer {

    /// 位图 / 内容盒换算成点后的尺寸。
    public struct Metrics: Equatable {
        public var width: CGFloat, height: CGFloat
        public var contentX: CGFloat, contentY: CGFloat
        public var contentW: CGFloat, contentH: CGFloat
    }

    public static func metrics(of f: OverlayFrame) -> Metrics {
        let s = CGFloat(max(f.scale, 1))
        let w = CGFloat(f.width) / s, h = CGFloat(f.height) / s
        let cw = f.contentW != 0 ? CGFloat(f.contentW) / s : w
        let ch = f.contentH != 0 ? CGFloat(f.contentH) / s : h
        return Metrics(width: w, height: h,
                       contentX: CGFloat(f.contentX) / s, contentY: CGFloat(f.contentY) / s,
                       contentW: cw, contentH: ch)
    }

    /// 浮层窗口（含软阴影扩边）的 wire 矩形。`wa` 是落点所在屏的工作区（wire 坐标），
    /// `shift` 仅 `followCandidate` 用：候选窗实际落点 − 建议落点。
    /// 逐项对位 `wind_linux` 的 `placeOverlay`。
    public static func place(_ f: OverlayFrame, workArea wa: WireRect,
                             shift: (dx: CGFloat, dy: CGFloat) = (0, 0)) -> WireRect {
        let m = metrics(of: f)
        if f.place == OverlayPlace.exact {
            return WireRect(x: CGFloat(f.x) - m.contentX, y: CGFloat(f.y) - m.contentY,
                            w: m.width, h: m.height)
        }
        let (cw, ch) = (m.contentW, m.contentH)
        let (left, top, right, bottom) = (wa.x, wa.y, wa.right, wa.bottom)
        var x = CGFloat(f.x), y = CGFloat(f.y)
        switch f.place {
        case OverlayPlace.followCandidate, OverlayPlace.flip:
            let follow = f.place == OverlayPlace.followCandidate
            let dx = follow ? shift.dx : 0
            let dy = follow ? shift.dy : 0
            x = CGFloat(f.x) + dx
            y = CGFloat(f.y) + dy
            if x + cw > right { x = CGFloat(f.altX) + dx }
            if y + ch > bottom {
                let alt = CGFloat(f.altY) + dy
                y = alt >= top ? alt : bottom - ch
            }
        case OverlayPlace.anchor:
            let mg = CGFloat(f.margin)
            let cx = ((left + right) / 2 - cw / 2).rounded()
            let cy = ((top + bottom) / 2 - ch / 2).rounded()
            let l = left + mg, r = right - mg - cw
            let t = top + mg, b = bottom - mg - ch
            switch f.anchor {
            case OverlayAnchor.topLeft: (x, y) = (l, t)
            case OverlayAnchor.topRight: (x, y) = (r, t)
            case OverlayAnchor.bottomLeft: (x, y) = (l, b)
            case OverlayAnchor.bottomRight: (x, y) = (r, b)
            case OverlayAnchor.topCenter: (x, y) = (cx, t)
            case OverlayAnchor.bottomCenter: (x, y) = (cx, b)
            default: (x, y) = (cx, cy)
            }
        default:
            break
        }
        // 夹回：先右/下再左/上，内容比工作区还大时保住左上角可见。
        if x + cw > right { x = right - cw }
        if x < left { x = left }
        if y + ch > bottom { y = bottom - ch }
        if y < top { y = top }
        return WireRect(x: x - m.contentX, y: y - m.contentY, w: m.width, h: m.height)
    }

    /// 拖动状态气泡：指针在 `(px, py)`（wire 点）、按下时相对窗口左上的偏移 `(grabDx, grabDy)`，
    /// 返回新的窗口矩形。规则与 `place` 末尾的夹回同一条，落点经服务端存成固定位置后
    /// 下一次按 `absolute` 摆放不会被挪动。对位 `dragOverlay`。
    public static func drag(pointer px: CGFloat, _ py: CGFloat, grab: (dx: CGFloat, dy: CGFloat),
                            frame f: OverlayFrame, workArea wa: WireRect) -> WireRect {
        var g = f
        let m = metrics(of: f)
        g.place = OverlayPlace.absolute
        // 窗口左上 = 指针 − 抓取偏移；absolute 的坐标指内容盒，加回扩边。
        let nx = px - grab.dx + m.contentX
        let ny = py - grab.dy + m.contentY
        g.x = Int32(nx.rounded())
        g.y = Int32(ny.rounded())
        return place(g, workArea: wa)
    }
}

// MARK: - 鼠标交互纯逻辑（对位 wind_linux/OverlayInput）

public enum OverlayPress: Equatable {
    case none, drag, menu, close
}

public enum OverlayInput {

    /// 指针在某层浮层上按下时做什么。`button`：1 左 / 2 中 / 3 右（与 X11 约定一致，
    /// 调用方把 AppKit 的 buttonNumber 0/1/2 映射为 1/3/2）。
    /// - 状态气泡：左键拖动，右键请求菜单；
    /// - 悬停提示：只认右键；
    /// - Toast：任意键点一下提前关掉。
    public static func pressAction(kind: UInt32, button: Int) -> OverlayPress {
        switch kind {
        case OverlayKind.status:
            return button == 1 ? .drag : (button == 3 ? .menu : .none)
        case OverlayKind.tooltip:
            return button == 3 ? .menu : .none
        case OverlayKind.toast:
            return (1...3).contains(button) ? .close : .none
        default:
            return .none
        }
    }

    /// `menu.open` 的 target（对位 Rust `menu_target`）：主菜单 −1 / 状态气泡 −2 / 悬停提示 −3。
    public static func menuTarget(kind: UInt32) -> Int {
        switch kind {
        case OverlayKind.status: return -2
        case OverlayKind.tooltip: return -3
        default: return -1
        }
    }

    public static let tipGraceMs: UInt32 = 280
    public static let tipSwitchMs: UInt32 = 150

    /// 候选悬停变化的延后（毫秒；0 = 立即报）。只有 `last` 的悬停提示正显示时才延后：
    /// 去往「无」按 grace，去往另一个候选按 switch。给「从候选行穿过空隙挪进提示」留时间。
    public static func hoverDeferMs(last: Int, raw: Int, tipShown: Bool) -> UInt32 {
        if last < 0 || !tipShown || raw == last { return 0 }
        return raw >= 0 ? tipSwitchMs : tipGraceMs
    }

    /// 指针从 (px, py) 移到 (nx, ny) 这一步是不是朝着悬停提示 `tip` 去。是的话延后的悬停变化按
    /// switch 顺延（展开态的提示停靠在窗口外侧，过去要横穿好几格）。对位 Windows
    /// `candidate_window::heading_to_tip`：沿提示所在那一侧在靠近，且偏离提示跨度不比靠近快。
    public static func headingToTip(_ px: CGFloat, _ py: CGFloat, _ nx: CGFloat, _ ny: CGFloat,
                                    tip: WireRect) -> Bool {
        let (l, t, r, b) = (tip.x, tip.y, tip.right, tip.bottom)
        let (dx, dy) = (nx - px, ny - py)
        let beside = l >= px || r <= px
        if !beside && t < py && b > py { return false }   // 已在提示上
        let toward = l >= px ? dx : r <= px ? -dx : t >= py ? dy : -dy
        let lateral = beside ? dy : dx
        let (lo, hi, pos) = beside ? (t, b, ny) : (l, r, nx)
        let away = pos > hi ? lateral : pos < lo ? -lateral : 0
        return toward > 0 && away <= toward
    }

    /// 点到提示的距离（沿提示所在那一侧的轴；在提示上为 0）。只有刷新到本次宽限的新低才顺延：
    /// 指针停着抖动时一来一回刷不出新低，不会把切换无限拖住。对位 Windows `tip_gap`。
    public static func tipGap(_ x: CGFloat, _ y: CGFloat, tip: WireRect) -> CGFloat {
        if tip.x >= x { return tip.x - x }
        if tip.right <= x { return x - tip.right }
        if tip.y >= y { return tip.y - y }
        if tip.bottom <= y { return y - tip.bottom }
        return 0
    }

    /// 「不改悬停」的哨兵。
    public static let keepHover = Int.min

    /// 离开悬停提示后宽限到期（或菜单关闭后）重新定悬停。指针回到提示上 → 不动；回到提示所属的
    /// 候选行 → 不动；在候选窗别处 → 报那里；在外面 → 报 −1。
    public static func tipRecheckHover(onTip: Bool, onCandidate: Bool, candidateHit: Int,
                                       held: Int) -> Int {
        if onTip { return keepHover }
        let target = (onCandidate && candidateHit >= 0) ? candidateHit : -1
        return target == held ? keepHover : target
    }
}

/// 悬停门控：「收到移动事件」不等于「用户动了鼠标」。气泡弹在静止的指针下面时系统一样会发
/// 进入 / 移动事件，把它当悬停，气泡就永远不会自动消失。以窗口出现那一刻的指针位置为基线，
/// 位置真变了才算。
public struct HoverGate {
    private var x = CGFloat.nan
    private var y = CGFloat.nan

    public init() {}

    /// 窗口从隐藏变为显示时调用。已显示时不要调（会抹掉真实悬停）。
    public mutating func rebase(_ px: CGFloat, _ py: CGFloat) { x = px; y = py }

    public mutating func accept(_ px: CGFloat, _ py: CGFloat) -> Bool {
        if px == x && py == y { return false }
        x = px; y = py
        return true
    }
}
