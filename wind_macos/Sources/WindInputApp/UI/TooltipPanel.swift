import Cocoa
import CoreText
import WindInputKit

// TooltipPanel — 候选悬停提示浮窗 (拼音/拆字/编码等)。
//
// Go 端经 push 通道发 CmdTooltipShow(text + 主题色), .app 据当前悬停候选的屏幕矩形
// 把本浮窗定位到候选下方 (放不下翻到上方), 文本支持 \n 多行、\t 列 (默认制表位)。
// 点击穿透 (ignoresMouseEvents), 不抢焦点, 不干扰候选框鼠标交互。
//
// 样式来自传入的 #RRGGBBAA 主题色 (Go 端从已解析主题 Tooltip 配色取); 空串退到内置
// 深色默认, 与 Win 端一致 (#3C3C3CF0 背景 / 白字)。
final class TooltipPanel: NSPanel {
    private let label = NSTextField(labelWithString: "")
    private let bgView = NSView()
    private let hPad: CGFloat = 8
    private let vPad: CGFloat = 5
    private let gap: CGFloat = 4   // 与候选项的间距

    init() {
        super.init(contentRect: NSRect(x: 0, y: 0, width: 120, height: 24),
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
        ignoresMouseEvents = true   // 点击穿透, 不干扰候选框

        bgView.wantsLayer = true
        bgView.layer?.cornerRadius = 6
        bgView.layer?.masksToBounds = true

        label.isBezeled = false
        label.isEditable = false
        label.isSelectable = false
        label.drawsBackground = false
        label.lineBreakMode = .byClipping
        label.maximumNumberOfLines = 0
        label.cell?.wraps = false
        label.cell?.isScrollable = false
        label.translatesAutoresizingMaskIntoConstraints = true

        bgView.addSubview(label)
        contentView = bgView
    }

    /// 显示 tooltip。anchorScreenRect 为悬停候选在屏幕坐标系下的矩形 (y 向上);
    /// 默认贴候选下方, 下方空间不足时翻到上方; 水平居中对齐并夹进屏幕可见区。
    /// fontPath 非空时注册该字根字体并以级联回退渲染 PUA 字根字符 (五笔拆字)。
    func show(text: String, bgHex: String, fgHex: String, fontPath: String = "", anchorScreenRect: NSRect) {
        guard !text.isEmpty else { hidePanel(); return }

        let bg = NSColor(windHex: bgHex) ?? NSColor(calibratedWhite: 0.235, alpha: 0.94)
        let fg = NSColor(windHex: fgHex) ?? .white
        bgView.layer?.backgroundColor = bg.cgColor

        let font = Self.tooltipFont(size: 13, chaiziFontPath: fontPath)
        let para = NSMutableParagraphStyle()
        para.lineSpacing = 2
        label.attributedStringValue = NSAttributedString(string: text, attributes: [
            .font: font, .foregroundColor: fg, .paragraphStyle: para,
        ])

        label.sizeToFit()
        let textSize = label.frame.size
        let w = ceil(textSize.width) + hPad * 2
        let h = ceil(textSize.height) + vPad * 2
        label.frame = NSRect(x: hPad, y: vPad, width: ceil(textSize.width), height: ceil(textSize.height))
        setContentSize(NSSize(width: w, height: h))

        guard let screen = screenForRect(anchorScreenRect) else {
            orderFrontRegardless(); return
        }
        let vf = screen.visibleFrame

        var originX = anchorScreenRect.midX - w / 2
        if originX + w > vf.maxX { originX = vf.maxX - w }
        if originX < vf.minX { originX = vf.minX }

        // 候选下方 (屏幕 y 向上, 下方 = 更小的 y)。
        var originY = anchorScreenRect.minY - gap - h
        if originY < vf.minY {
            // 下方放不下 → 翻到候选上方。
            originY = anchorScreenRect.maxY + gap
        }
        if originY + h > vf.maxY { originY = vf.maxY - h }
        if originY < vf.minY { originY = vf.minY }

        setFrameOrigin(NSPoint(x: originX, y: originY))
        orderFrontRegardless()
    }

    func hidePanel() {
        orderOut(nil)
    }

    private func screenForRect(_ r: NSRect) -> NSScreen? {
        for s in NSScreen.screens where s.frame.intersects(r) { return s }
        return NSScreen.main ?? NSScreen.screens.first
    }

    // MARK: - 字根字体级联

    /// 已处理过的字根字体路径 → 字体族名 (失败缓存空串), 避免重复注册/解析。
    private static var registeredChaizi: [String: String] = [:]

    /// tooltip 用字体: 系统字体打底; chaiziFontPath 非空时把字根字体挂为级联回退,
    /// 使 PUA 字根字符 (系统字体缺字) 落到字根字体渲染, 其余字符仍用系统字体。
    private static func tooltipFont(size: CGFloat, chaiziFontPath: String) -> NSFont {
        let base = NSFont.systemFont(ofSize: size)
        guard !chaiziFontPath.isEmpty,
              let family = registerChaiziFont(chaiziFontPath)
        else { return base }
        let fallback = CTFontDescriptorCreateWithNameAndSize(family as CFString, size)
        let desc = CTFontDescriptorCreateCopyWithAttributes(
            base.fontDescriptor as CTFontDescriptor,
            [kCTFontCascadeListAttribute as String: [fallback]] as CFDictionary)
        return CTFontCreateWithFontDescriptor(desc, size, nil) as NSFont
    }

    /// 注册字根字体文件并返回字体族名 (进程级缓存)。已注册视为成功。
    private static func registerChaiziFont(_ path: String) -> String? {
        if let cached = registeredChaizi[path] { return cached.isEmpty ? nil : cached }
        let url = URL(fileURLWithPath: path) as CFURL
        var family = ""
        if let descs = CTFontManagerCreateFontDescriptorsFromURL(url) as? [CTFontDescriptor],
           let first = descs.first,
           let name = CTFontDescriptorCopyAttribute(first, kCTFontFamilyNameAttribute) as? String {
            family = name
        }
        var err: Unmanaged<CFError>?
        if !CTFontManagerRegisterFontsForURL(url, .process, &err),
           let e = err?.takeRetainedValue(),
           CFErrorGetCode(e) != CTFontManagerError.alreadyRegistered.rawValue {
            registeredChaizi[path] = ""
            return nil
        }
        registeredChaizi[path] = family
        return family.isEmpty ? nil : family
    }
}

private extension NSColor {
    /// 解析 #RGB / #RRGGBB / #RRGGBBAA (Go theme.ColorToHex 产 #RRGGBBAA)。
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
