import XCTest
@testable import WindInputKit

final class WireGeometryTests: XCTestCase {

    /// 1080p 主屏。
    private let H: CGFloat = 1080
    private let vf = CGRect(x: 0, y: 0, width: 1920, height: 1055) // 顶部 25pt 菜单栏

    func testFlipYIsSelfInverse() {
        for y in [CGFloat(0), 1, 540, 1079, 1080] {
            XCTAssertEqual(WireGeometry.flipY(WireGeometry.flipY(y, screenHeight: H),
                                              screenHeight: H), y)
        }
    }

    /// wire 顶边 0 = 屏幕最上方；wire y 增大 = 往下走。
    func testWireTopLeftMapsScreenTopToZero() {
        let size = CGSize(width: 200, height: 60)
        // 窗口顶边贴屏幕顶 → Cocoa 左下角 y = 1080 - 60。
        let atTop = CGRect(origin: CGPoint(x: 30, y: H - size.height), size: size)
        let p = WireGeometry.wireTopLeft(of: atTop, screenHeight: H)
        XCTAssertEqual(p.x, 30)
        XCTAssertEqual(p.y, 0)
    }

    /// **拖动不漂移的根据**：`wireTopLeft` 与 `cocoaOrigin` 必须严格互逆。
    ///
    /// 回归意义：拖动落点经 wire 上报 → 落盘 → 下一帧再发回来摆窗，若两个方向差一个窗口
    /// 高度或用了不同参照，每一轮都会多偏一次，候选窗逐次往上/下爬。
    func testWireAndCocoaRoundTrip() {
        let size = CGSize(width: 240, height: 72)
        for (wx, wy) in [(CGFloat(0), CGFloat(0)), (100, 200), (1680, 1008), (-40, 13)] {
            let origin = WireGeometry.cocoaOrigin(wireX: wx, wireY: wy, size: size, screenHeight: H)
            let back = WireGeometry.wireTopLeft(of: CGRect(origin: origin, size: size),
                                                screenHeight: H)
            XCTAssertEqual(CGFloat(back.x), wx, "x 不互逆 @(\(wx),\(wy))")
            XCTAssertEqual(CGFloat(back.y), wy, "y 不互逆 @(\(wx),\(wy))")
        }
    }

    func testClampPullsBackFromRightAndBottom() {
        let size = CGSize(width: 200, height: 60)
        let out = WireGeometry.clamp(origin: CGPoint(x: 1900, y: -30), size: size, visibleFrame: vf)
        XCTAssertEqual(out.x, vf.maxX - size.width)
        XCTAssertEqual(out.y, vf.minY)
    }

    /// 越过顶边（菜单栏下沿）时回拉，避免窗口钻到菜单栏后面。
    func testClampRespectsTopOfVisibleFrame() {
        let size = CGSize(width: 200, height: 60)
        let out = WireGeometry.clamp(origin: CGPoint(x: 10, y: 1040), size: size, visibleFrame: vf)
        XCTAssertEqual(out.y, vf.maxY - size.height)
    }

    /// 窗口比屏幕还大时保证**左上角**可见：先按右/下回拉会把左上角推出屏幕，
    /// 必须再按左/上兜一次。
    func testClampKeepsTopLeftVisibleWhenOversized() {
        let huge = CGSize(width: 3000, height: 2000)
        let out = WireGeometry.clamp(origin: CGPoint(x: 500, y: 500), size: huge, visibleFrame: vf)
        XCTAssertEqual(out.x, vf.minX)
        XCTAssertEqual(out.y + huge.height, vf.minY + huge.height, "左上角须落在可见区内")
    }

    // MARK: - 跟随光标落位 (GH#179)

    /// 主屏右侧接一块 2560×1440 副屏（Cocoa 下 x 从 1920 起、底边对齐主屏底边）。
    private let sideVF = CGRect(x: 1920, y: 0, width: 2560, height: 1415)

    /// **GH#179 的回归**：副屏上的光标，按副屏可见区钳位，候选窗留在副屏、就在光标下方。
    /// 以前拿主屏可见区钳，x 一律被拉到 `1920 - 宽`，候选窗飞回主屏右边缘。
    func testFollowCaretStaysOnSecondaryScreen() {
        let size = CGSize(width: 300, height: 60)
        let out = WireGeometry.followCaretOrigin(wireX: 2500, wireY: 400, size: size,
                                                 screenHeight: H, visibleFrame: sideVF)
        XCTAssertEqual(out.x, 2500)
        XCTAssertEqual(out.y, WireGeometry.flipY(400, screenHeight: H) - size.height)
    }

    func testFollowCaretPullsBackAtSecondaryRightEdge() {
        let size = CGSize(width: 300, height: 60)
        let out = WireGeometry.followCaretOrigin(wireX: 4400, wireY: 400, size: size,
                                                 screenHeight: H, visibleFrame: sideVF)
        XCTAssertEqual(out.x, sideVF.maxX - size.width)
    }

    /// 光标贴屏幕底边、下方放不下 → 翻到光标上方（估算光标高 18pt）。
    func testFollowCaretFlipsAboveNearBottom() {
        let size = CGSize(width: 300, height: 60)
        let out = WireGeometry.followCaretOrigin(wireX: 100, wireY: 1060, size: size,
                                                 screenHeight: H, visibleFrame: vf)
        XCTAssertEqual(out.y, WireGeometry.flipY(1060, screenHeight: H) + 18)
    }

    /// 状态气泡共用同一落位，只是翻上去时离锚点 2pt 而不是让出整行光标。
    func testFollowCaretAboveGapIsConfigurable() {
        let size = CGSize(width: 120, height: 34)
        let out = WireGeometry.followCaretOrigin(wireX: 100, wireY: 1060, size: size,
                                                 screenHeight: H, visibleFrame: vf, aboveGap: 2)
        XCTAssertEqual(out.y, WireGeometry.flipY(1060, screenHeight: H) + 2)
    }

    // MARK: - 皮肤：按内容框翻转 / 落位 / 夹边

    /// 皮肤窗口 300×100，内容框在窗内 (20, 30, 260, 50)：装饰向上外扩 30、向下 20、左右各 20。
    private let skinSize = CGSize(width: 300, height: 100)
    private let skinBox = CGRect(x: 20, y: 30, width: 260, height: 50)

    /// 窗口 wire 左上 → 内容框的 wire 上沿 / 下沿（由 Cocoa 左下角反算）。
    private func contentWireTopBottom(_ o: CGPoint) -> (top: CGFloat, bottom: CGFloat) {
        let windowTop = WireGeometry.flipY(o.y + skinSize.height, screenHeight: H)
        return (windowTop + skinBox.minY, windowTop + skinBox.maxY)
    }

    /// 下方放得下：照服务端的点摆，内容框上沿 = 光标底端 + 2，与无皮肤时一致。
    func testSkinBelowKeepsContentUnderCaret() {
        // 光标底端 600 → 内容框上沿 602 → 窗口左上 572。
        let out = WireGeometry.followCaretOrigin(wireX: 480, wireY: 572, size: skinSize,
                                                 screenHeight: H, visibleFrame: vf,
                                                 above: CGPoint(x: 480, y: 498), box: skinBox)
        XCTAssertEqual(out.x, 480)
        XCTAssertEqual(contentWireTopBottom(out).top, 602)
    }

    /// 翻不翻按内容框判：整窗下沿 (1090) 出了屏底，内容框下沿 (1070) 没出——不翻，只是外扩伸出屏外。
    func testSkinFlipDecidedByContentBox() {
        let out = WireGeometry.followCaretOrigin(wireX: 480, wireY: 990, size: skinSize,
                                                 screenHeight: H, visibleFrame: vf,
                                                 above: CGPoint(x: 480, y: 916), box: skinBox)
        XCTAssertEqual(contentWireTopBottom(out).top, 1020, "仍在光标下方")
        XCTAssertEqual(out.y, WireGeometry.flipY(990, screenHeight: H) - skinSize.height)
    }

    /// 翻到上方：用服务端给的上方点，内容框下沿 = 光标顶端 − 2（与无皮肤、与 Windows 同规则）。
    /// 旧写法把窗口底边放在 wire 点上方 18pt，内容框会离光标「外扩 + 18」那么远。
    func testSkinFlipAboveUsesServerAbovePoint() {
        // 光标底端 1058、高 20（顶端 1038）；内容框上沿 1060 → 窗口左上 1030，内容框下沿 1110 出屏 → 翻。
        // 上方：内容框下沿 1036 → 上沿 986 → 窗口左上 956。
        let out = WireGeometry.followCaretOrigin(wireX: 480, wireY: 1030, size: skinSize,
                                                 screenHeight: H, visibleFrame: vf,
                                                 above: CGPoint(x: 470, y: 956), box: skinBox)
        XCTAssertEqual(out.x, 470, "上方对齐点的横坐标可以不同于下方")
        XCTAssertEqual(contentWireTopBottom(out).bottom, 1036)
    }

    /// 夹边按内容框：透明外扩可以伸出屏外，内容框贴边。
    func testSkinClampUsesContentBox() {
        let out = WireGeometry.followCaretOrigin(wireX: 1650, wireY: 572, size: skinSize,
                                                 screenHeight: H, visibleFrame: vf,
                                                 above: CGPoint(x: 1650, y: 498), box: skinBox)
        XCTAssertEqual(out.x + skinBox.maxX, vf.maxX)
        // 屏顶：内容框上沿贴可见区上沿（菜单栏下），上方的外扩伸进菜单栏。
        let top = WireGeometry.followCaretOrigin(wireX: 480, wireY: -20, size: skinSize,
                                                 screenHeight: H, visibleFrame: vf,
                                                 above: CGPoint(x: 480, y: -100), box: skinBox)
        XCTAssertEqual(contentWireTopBottom(top).top, H - vf.maxY)
    }
}
