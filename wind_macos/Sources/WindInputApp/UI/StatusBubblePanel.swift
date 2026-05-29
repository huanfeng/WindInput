import Cocoa
import WindInputKit

// StatusBubblePanel — 模式切换状态提示气泡 (中英/标点/全半角)。
//
// 与 Win 端 StatusWindow 对齐: 模式切换时在 caret 附近弹出一个短文气泡 (如 "中 ，"),
// temp 模式到点自动消失, always 模式常驻。区别于菜单栏 NSStatusItem (常驻显示当前
// 方案/模式), 这是输入位置旁的瞬态反馈。
//
// Go 端 (forwarder) 据 config 合成文本 + 主题色 + 位置 + 时长, 经 push CmdStatusShow
// 下发; 本浮窗只负责渲染与定位。点击穿透, 不抢焦点。
final class StatusBubblePanel: NSPanel {
    private let label = NSTextField(labelWithString: "")
    private let bgView = NSView()
    private let hPad: CGFloat = 8
    private let vPad: CGFloat = 4
    private var hideTimer: Timer?

    init() {
        super.init(contentRect: NSRect(x: 0, y: 0, width: 60, height: 24),
                   styleMask: [.borderless, .nonactivatingPanel],
                   backing: .buffered,
                   defer: false)
        isOpaque = false
        backgroundColor = .clear
        hasShadow = true
        level = .popUpMenu
        isFloatingPanel = true
        collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle]
        hidesOnDeactivate = false
        ignoresMouseEvents = true   // 点击穿透

        bgView.wantsLayer = true
        bgView.layer?.cornerRadius = 6
        bgView.layer?.masksToBounds = true

        label.isBezeled = false
        label.isEditable = false
        label.isSelectable = false
        label.drawsBackground = false
        label.alignment = .center
        label.lineBreakMode = .byClipping
        label.translatesAutoresizingMaskIntoConstraints = true

        bgView.addSubview(label)
        contentView = bgView
    }

    /// 显示气泡。x/y 为 caret 屏幕坐标 (wire top-left, y 向下); durationMs>0 时到点自动隐藏。
    func show(text: String, bgHex: String, fgHex: String, wireX: Int32, wireY: Int32, durationMs: Int32) {
        guard !text.isEmpty else { hidePanel(); return }
        hideTimer?.invalidate()

        let bg = NSColor(windHex: bgHex) ?? NSColor(calibratedWhite: 0.235, alpha: 0.9)
        let fg = NSColor(windHex: fgHex) ?? .white
        bgView.layer?.backgroundColor = bg.cgColor

        let font = NSFont.systemFont(ofSize: 16)
        label.attributedStringValue = NSAttributedString(string: text, attributes: [
            .font: font, .foregroundColor: fg,
        ])
        label.sizeToFit()
        let textSize = label.frame.size
        let w = ceil(textSize.width) + hPad * 2
        let h = ceil(textSize.height) + vPad * 2
        label.frame = NSRect(x: hPad, y: vPad, width: ceil(textSize.width), height: ceil(textSize.height))
        setContentSize(NSSize(width: w, height: h))

        guard let screen = screenForWirePoint(x: CGFloat(wireX), y: CGFloat(wireY)) else {
            orderFrontRegardless(); return
        }
        let vf = screen.visibleFrame
        // wire top-left → Cocoa bottom-left: caret 点的 Cocoa y。气泡顶端贴 caret 点下方。
        // wireY 已是 caret 底部下方的锚点 (Go forwarder 加了 caretHeight+gap), 与候选窗口
        // 同位置。气泡顶边贴该锚点 (originY 为底边, 故 -h)。
        let caretLine = screen.frame.height - CGFloat(wireY)
        var originX = CGFloat(wireX)
        var originY = caretLine - h

        if originX + w > vf.maxX { originX = vf.maxX - w }
        if originX < vf.minX { originX = vf.minX }
        if originY < vf.minY { originY = caretLine + 2 } // 下方放不下 → 翻到锚点上方
        if originY + h > vf.maxY { originY = vf.maxY - h }
        if originY < vf.minY { originY = vf.minY }

        setFrameOrigin(NSPoint(x: originX, y: originY))
        orderFrontRegardless()

        if durationMs > 0 {
            hideTimer = Timer.scheduledTimer(withTimeInterval: Double(durationMs) / 1000.0,
                                             repeats: false) { [weak self] _ in
                self?.orderOut(nil)
            }
        }
    }

    func hidePanel() {
        hideTimer?.invalidate()
        hideTimer = nil
        orderOut(nil)
    }

    private func screenForWirePoint(x: CGFloat, y: CGFloat) -> NSScreen? {
        // wire 点在主屏坐标系 (top-left), 简化用主屏判定; 多屏精确归属由 clamp 兜底。
        return NSScreen.main ?? NSScreen.screens.first
    }
}

private extension NSColor {
    /// 解析 #RGB / #RRGGBB / #RRGGBBAA。
    convenience init?(windHex: String) {
        var s = windHex.trimmingCharacters(in: .whitespaces)
        guard s.hasPrefix("#") else { return nil }
        s.removeFirst()
        guard let v = UInt64(s, radix: 16) else { return nil }
        let r, g, b, a: CGFloat
        switch s.count {
        case 6:
            r = CGFloat((v >> 16) & 0xFF) / 255
            g = CGFloat((v >> 8) & 0xFF) / 255
            b = CGFloat(v & 0xFF) / 255
            a = 1
        case 8:
            r = CGFloat((v >> 24) & 0xFF) / 255
            g = CGFloat((v >> 16) & 0xFF) / 255
            b = CGFloat((v >> 8) & 0xFF) / 255
            a = CGFloat(v & 0xFF) / 255
        default:
            return nil
        }
        self.init(srgbRed: r, green: g, blue: b, alpha: a)
    }
}
