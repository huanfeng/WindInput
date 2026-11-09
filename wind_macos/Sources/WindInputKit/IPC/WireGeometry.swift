import Foundation
import CoreGraphics

// WireGeometry — 浮窗落位的 wire ↔ Cocoa 坐标换算。
//
// 与 [CaretCoords] 同一件事的另一半：那边管「caret 矩形 → wire」(只有一个方向)，
// 这边管**窗口矩形**的双向换算。分开是因为参照点不同——caret 传的是光标左上角，窗口
// 传的是内容左上角，且窗口这边要能反着算回去。
//
// 两套坐标系的差别只有一处但很致命：
//   - wire (服务进程 / Windows 侧)：原点在**主屏左上角**，y 轴**向下**。
//   - Cocoa 全局坐标：原点在**主屏左下角**，y 轴**向上**。
//
// 以前只有「服务端算好位置 → 摆窗」一个方向，换算散在各 panel 的 show() 里。拖动上报
// 引入了**反方向**换算，两个方向一旦用了不同的参照屏或差一个窗口高度，每轮「拖动 → 落盘
// → 重新显示」就会累计一次偏移，窗口逐次漂走。故收口到这里，并用 round-trip 测试钉死。
//
// **参照屏必须是带菜单栏的主屏 (`NSScreen.screens.first`)，不是 `NSScreen.main`。**
// `NSScreen.main` 是「当前 key window 所在的屏」，多显示器下会随用户操作变，而 wire 原点
// 钉死在主屏左上角。两者在单屏下恰好相同，所以这个错误在单屏机器上永远显不出来。
public enum WireGeometry {

    /// wire y（自主屏顶边向下）↔ Cocoa y（自主屏底边向上）。同一公式自逆。
    public static func flipY(_ y: CGFloat, screenHeight: CGFloat) -> CGFloat {
        screenHeight - y
    }

    /// 窗口矩形 → wire 下的**左上角**（即候选窗/气泡落盘用的 custom_x/custom_y）。
    public static func wireTopLeft(of frame: CGRect, screenHeight: CGFloat) -> (x: Int32, y: Int32) {
        (Int32(frame.origin.x.rounded()),
         Int32(flipY(frame.maxY, screenHeight: screenHeight).rounded()))
    }

    /// wire 左上角 + 窗口尺寸 → Cocoa 的 `setFrameOrigin` 入参（左**下**角）。
    /// `wireTopLeft(of:screenHeight:)` 的逆。
    public static func cocoaOrigin(wireX: CGFloat, wireY: CGFloat, size: CGSize,
                                   screenHeight: CGFloat) -> CGPoint {
        CGPoint(x: wireX, y: flipY(wireY, screenHeight: screenHeight) - size.height)
    }

    /// 跟随光标的候选窗落位：wire 左上（光标底端下方）+ 窗口尺寸 → Cocoa 左下角。
    ///
    /// `visibleFrame` 必须是**光标所在屏**的可见区（GH#179：曾用主屏，副屏上的候选窗被一把
    /// 钳回主屏右边缘）。超出左右边回拉；下方放不下翻到光标上方，翻了仍越界再兜底夹进可见区。
    ///
    /// - `above`：服务端算好的「翻到上方时」的窗口 wire 左上（候选帧 aboveX/aboveY，按 Windows 上翻
    ///   规则：皮肤 `caret_above` 对齐点、上方避让、偏移远离光标）。给了就用它；没给（状态气泡、旧帧）
    ///   退回估算：窗口底边放在锚点上方 `aboveGap` 处——候选窗让出整行光标（18），状态气泡只留一线（2）。
    /// - `box`：判翻转与夹边用的框，相对窗口左上的 wire 矩形（y 向下；候选帧 box*：内容框，皮肤
    ///   `clamp = "all"` 时连装饰外扩）。皮肤的透明外扩撞到屏幕边不算越界，否则内容框会被推离光标、
    ///   且装饰会让候选窗提前翻上去。缺省整个窗口。
    public static func followCaretOrigin(wireX: CGFloat, wireY: CGFloat, size: CGSize,
                                         screenHeight: CGFloat, visibleFrame vf: CGRect,
                                         aboveGap: CGFloat = 18,
                                         above: CGPoint? = nil, box: CGRect? = nil) -> CGPoint {
        let b = box.flatMap { $0.width > 0 && $0.height > 0 ? $0 : nil }
            ?? CGRect(origin: .zero, size: size)
        // 框在窗口内的 Cocoa 偏移（距窗口左下）。
        let dx = b.minX
        let dy = size.height - b.maxY
        var x = wireX
        var y = flipY(wireY, screenHeight: screenHeight) - size.height
        if y + dy < vf.minY {
            if let a = above {
                x = a.x
                y = flipY(a.y, screenHeight: screenHeight) - size.height
            } else {
                y = flipY(wireY, screenHeight: screenHeight) + aboveGap
            }
        }
        if x + dx + b.width > vf.maxX { x = vf.maxX - b.width - dx }
        if x + dx < vf.minX { x = vf.minX - dx }
        if y + dy + b.height > vf.maxY { y = vf.maxY - b.height - dy }
        if y + dy < vf.minY { y = vf.minY - dy }
        return CGPoint(x: x, y: y)
    }

    /// 把窗口矩形钳进可见区 `visibleFrame`（避开菜单栏/Dock）；返回钳后的左下角。
    /// 内容比屏幕还大时保证**左上角**可见（先按右/下回拉，再按左/上兜底）。
    public static func clamp(origin: CGPoint, size: CGSize, visibleFrame vf: CGRect) -> CGPoint {
        var x = origin.x
        var y = origin.y
        if x + size.width > vf.maxX { x = vf.maxX - size.width }
        if x < vf.minX { x = vf.minX }
        if y + size.height > vf.maxY { y = vf.maxY - size.height }
        if y < vf.minY { y = vf.minY }
        return CGPoint(x: x, y: y)
    }
}
