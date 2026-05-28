import Cocoa
import WindInputKit

// ModeStatusController — 菜单栏输入模式指示器 (NSStatusItem)。
//
// 接收 Go 端经 push 通道发来的 CmdModeStatus (中英模式/全半角/标点/方案标签),
// 在屏幕右上角菜单栏显示当前状态; 点击弹出菜单展示完整状态 (带勾选)。
//
// 可见性由 Go 端 toolbar reducer 驱动: IME 激活且应显示时 visible=true, 失活/失焦
// 时 visible=false → 隐藏菜单栏项。整个 .app 进程一个指示器 (单例)。
//
// 当前菜单为只读状态展示; 点击切换模式 (回发上行命令) 留待后续。
public final class ModeStatusController {
    public static let shared = ModeStatusController()

    private var statusItem: NSStatusItem?
    private var latest: ModeStatusPayload?

    private init() {}

    /// 供候选框空白处右键复用的主菜单 (当前模式状态只读 + 设置入口)。
    public func mainMenu() -> NSMenu {
        let p = latest ?? ModeStatusPayload(
            chineseMode: true, fullWidth: false, chinesePunct: true,
            capsLock: false, visible: true, effectiveMode: 0, modeLabel: "")
        return buildMenu(p)
    }

    /// 应用一帧模式状态 (线程安全, 内部切回主线程操作 AppKit)。
    public func apply(_ p: ModeStatusPayload) {
        if Thread.isMainThread {
            applyMain(p)
        } else {
            DispatchQueue.main.async { [weak self] in self?.applyMain(p) }
        }
    }

    private func applyMain(_ p: ModeStatusPayload) {
        latest = p
        guard p.visible else {
            statusItem?.isVisible = false
            return
        }
        let item = ensureItem()
        item.isVisible = true
        item.button?.title = title(for: p)
        item.menu = buildMenu(p)
    }

    private func ensureItem() -> NSStatusItem {
        if let it = statusItem { return it }
        let it = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        it.button?.font = NSFont.systemFont(ofSize: 14)
        statusItem = it
        return it
    }

    /// 菜单栏紧凑标题: 模式字 + 标点字。
    ///   中文: 方案标签 (拼/五/双/混; 空则 "中"); 英文: "英"。
    ///   标点: 中文标点 "。" / 英文标点 "."。
    private func title(for p: ModeStatusPayload) -> String {
        let mode = p.chineseMode ? (p.modeLabel.isEmpty ? "中" : p.modeLabel) : "英"
        let punct = p.chinesePunct ? "。" : "."
        return mode + punct
    }

    /// 下拉菜单: 当前状态只读展示 (带勾选) + 设置入口。
    private func buildMenu(_ p: ModeStatusPayload) -> NSMenu {
        let menu = NSMenu()
        let header = NSMenuItem(title: "清风输入法", action: nil, keyEquivalent: "")
        header.isEnabled = false
        menu.addItem(header)
        menu.addItem(.separator())
        addState(menu, "中文模式", on: p.chineseMode)
        addState(menu, "英文模式", on: !p.chineseMode)
        menu.addItem(.separator())
        addState(menu, "中文标点", on: p.chinesePunct)
        addState(menu, "英文标点", on: !p.chinesePunct)
        menu.addItem(.separator())
        addState(menu, "全角", on: p.fullWidth)
        addState(menu, "半角", on: !p.fullWidth)
        menu.addItem(.separator())
        let settings = NSMenuItem(title: "设置…", action: #selector(openSettings), keyEquivalent: ",")
        settings.target = self
        menu.addItem(settings)
        return menu
    }

    private func addState(_ menu: NSMenu, _ title: String, on: Bool) {
        let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        item.state = on ? .on : .off
        item.isEnabled = false
        menu.addItem(item)
    }

    /// 打开设置应用 (wind_setting.app, Wails)。经 LaunchServices 按 bundleID 查找并启动,
    /// 已在运行则激活已有窗口 (macOS .app 天然单实例)。
    @objc private func openSettings() {
        let bundleID = "com.wails.wind_setting"
        let ws = NSWorkspace.shared
        if let url = ws.urlForApplication(withBundleIdentifier: bundleID) {
            ws.openApplication(at: url, configuration: NSWorkspace.OpenConfiguration())
        } else {
            // LaunchServices 尚未登记时的兜底: open -b 触发一次注册+启动。
            let p = Process()
            p.launchPath = "/usr/bin/open"
            p.arguments = ["-b", bundleID]
            try? p.run()
        }
    }
}
