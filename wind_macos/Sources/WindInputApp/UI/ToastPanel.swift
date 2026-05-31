import Cocoa
import WindInputKit

// ToastPanel — 屏幕级 Toast 通知浮窗 (如「词库加载完成」)。
//
// 与 Win 端 ToastWindow 对齐: 暗背景圆角卡片 + 左侧 accent 条 (按级别配色) + 标题 + 正文。
// 区别于锚 caret 的瞬态状态气泡 (StatusBubblePanel): Toast 落在工作区右下角 (info/success)
// 或居中 (warn/error), 与输入位置无关, 用于异步操作结果反馈。
//
// Go 端 (forwarder) 据主题合成配色 + accent + 位置 + 时长, 经 push CmdToastShow 下发;
// 本浮窗只负责渲染、落位与计时。点击穿透, 不抢焦点。
final class ToastPanel: NSPanel {
    private let bgView = NSView()
    private let accentBar = NSView()
    private let titleLabel = NSTextField(labelWithString: "")
    private let messageLabel = NSTextField(labelWithString: "")
    private var hideTimer: Timer?

    // 布局常量 (逻辑点, 镜像 Win toast_renderer)。
    private let padding: CGFloat = 12
    private let titleGap: CGFloat = 6
    private let cornerRadius: CGFloat = 6
    private let accentBarWidth: CGFloat = 4
    private let accentBarInset: CGFloat = 5
    private let screenMargin: CGFloat = 16
    private let defaultMaxWidth: CGFloat = 360
    private var textLeft: CGFloat { accentBarInset + accentBarWidth + 8 }

    init() {
        super.init(contentRect: NSRect(x: 0, y: 0, width: 200, height: 60),
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
        ignoresMouseEvents = true // 点击穿透

        bgView.wantsLayer = true
        bgView.layer?.cornerRadius = cornerRadius
        bgView.layer?.masksToBounds = true

        accentBar.wantsLayer = true
        accentBar.layer?.cornerRadius = accentBarWidth / 2

        for l in [titleLabel, messageLabel] {
            l.isBezeled = false
            l.isEditable = false
            l.isSelectable = false
            l.drawsBackground = false
            l.translatesAutoresizingMaskIntoConstraints = true
            l.lineBreakMode = .byWordWrapping
            l.usesSingleLineMode = false
            l.maximumNumberOfLines = 0
            l.cell?.wraps = true
        }

        bgView.addSubview(accentBar)
        bgView.addSubview(titleLabel)
        bgView.addSubview(messageLabel)
        contentView = bgView
    }

    /// 显示 Toast。durationMs: 0=默认 5000ms, >0 自动隐藏毫秒数, <0 不自动隐藏。
    func show(_ p: ToastPayload) {
        guard !(p.title.isEmpty && p.message.isEmpty) else { hidePanel(); return }
        hideTimer?.invalidate()

        let bg = NSColor(windHex: p.bgColor) ?? NSColor(calibratedWhite: 0.17, alpha: 1)
        let fg = NSColor(windHex: p.fgColor) ?? .white
        let accent = NSColor(windHex: p.accentColor) ?? NSColor(calibratedRed: 0.26, green: 0.65, blue: 0.96, alpha: 1)
        bgView.layer?.backgroundColor = bg.cgColor
        accentBar.layer?.backgroundColor = accent.cgColor

        // 内容最大宽: maxWidth(逻辑点) 优先, 0 用默认; 再夹到主屏工作区的一半内。
        let screen = NSScreen.main ?? NSScreen.screens.first
        let vf = screen?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1280, height: 800)
        var maxPanelW = p.maxWidth > 0 ? CGFloat(p.maxWidth) : defaultMaxWidth
        maxPanelW = min(maxPanelW, vf.width * 0.5)
        let contentMaxW = max(80, maxPanelW - textLeft - padding)

        let titleFont = NSFont.systemFont(ofSize: 15, weight: .semibold)
        let bodyFont = NSFont.systemFont(ofSize: 13)

        let hasTitle = !p.title.isEmpty
        let hasBody = !p.message.isEmpty

        var titleSize = NSSize.zero
        if hasTitle {
            titleLabel.attributedStringValue = NSAttributedString(string: p.title, attributes: [
                .font: titleFont, .foregroundColor: accent,
            ])
            titleLabel.preferredMaxLayoutWidth = contentMaxW
            titleSize = titleLabel.sizeThatFits(NSSize(width: contentMaxW, height: .greatestFiniteMagnitude))
        }
        titleLabel.isHidden = !hasTitle

        var bodySize = NSSize.zero
        if hasBody {
            messageLabel.attributedStringValue = NSAttributedString(string: p.message, attributes: [
                .font: bodyFont, .foregroundColor: fg,
            ])
            messageLabel.preferredMaxLayoutWidth = contentMaxW
            bodySize = messageLabel.sizeThatFits(NSSize(width: contentMaxW, height: .greatestFiniteMagnitude))
        }
        messageLabel.isHidden = !hasBody

        let contentW = max(titleSize.width, bodySize.width)
        var panelW = contentW + textLeft + padding
        panelW = max(160, min(panelW, maxPanelW))
        var panelH = padding * 2 + titleSize.height + bodySize.height
        if hasTitle && hasBody { panelH += titleGap }

        setContentSize(NSSize(width: panelW, height: panelH))

        // 子视图布局 (bgView 非翻转, 原点左下; 自上而下排标题→正文)。
        accentBar.frame = NSRect(x: accentBarInset, y: accentBarInset,
                                 width: accentBarWidth, height: panelH - accentBarInset * 2)
        var cursorY = panelH - padding // 顶部内边距下沿
        if hasTitle {
            cursorY -= titleSize.height
            titleLabel.frame = NSRect(x: textLeft, y: cursorY, width: contentW, height: titleSize.height)
            if hasBody { cursorY -= titleGap }
        }
        if hasBody {
            cursorY -= bodySize.height
            messageLabel.frame = NSRect(x: textLeft, y: cursorY, width: contentW, height: bodySize.height)
        }

        // 落位: bottom_right=工作区右下角, 其它(含 center)=工作区居中。
        let origin: NSPoint
        if p.position == "bottom_right" {
            origin = NSPoint(x: vf.maxX - panelW - screenMargin, y: vf.minY + screenMargin)
        } else {
            origin = NSPoint(x: vf.midX - panelW / 2, y: vf.midY - panelH / 2)
        }
        setFrameOrigin(origin)
        orderFrontRegardless()

        if p.durationMs >= 0 {
            let ms = p.durationMs == 0 ? 5000 : Int(p.durationMs)
            hideTimer = Timer.scheduledTimer(withTimeInterval: Double(ms) / 1000.0,
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
