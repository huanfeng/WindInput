import Cocoa
import WindInputKit

// ModeStatusController — 菜单栏输入模式指示器 (NSStatusItem)。
//
// 接收 Go 端经 push 通道发来的 CmdModeStatus (中英模式/全半角/标点/方案标签),
// 在屏幕右上角菜单栏显示当前状态 (紧凑标题)。
//
// 下拉菜单与候选框空白处右键菜单保持一致: 打开时经 unifiedMenuProvider 同步向 Go
// 拉取统一菜单树 (输入方案 / 检索范围 / 全角 / 中文标点 / 简入繁出 / 主题 / 设置…),
// 用共享的 UnifiedMenuBuilder 构建为可点击的原生菜单, 点击经 onUnifiedAction 回发
// CmdMenuAction。服务尚未就绪 (拉取失败) 时回退到只读状态展示 + 设置入口。
//
// 可见性由 Go 端 toolbar reducer 驱动: IME 激活且应显示时 visible=true, 失活/失焦
// 时 visible=false → 隐藏菜单栏项。整个 .app 进程一个指示器 (单例)。
public final class ModeStatusController: NSObject, NSMenuDelegate {
    public static let shared = ModeStatusController()

    /// 统一菜单树提供者 (同步向 Go 请求, 失败返回 nil)。由 CandidatePanelHost 注入,
    /// 与候选框空白处右键菜单复用同一 IPC 请求路径。
    public var unifiedMenuProvider: (() -> [MenuItemData]?)?
    /// 统一菜单项点击回调 (menu item id → CmdMenuAction)。由 CandidatePanelHost 注入。
    public var onUnifiedAction: ((Int) -> Void)?

    private var statusItem: NSStatusItem?
    private var latest: ModeStatusPayload?
    private let menuBuilder = UnifiedMenuBuilder() // 须持有: 作为统一菜单叶子项 target

    private override init() { super.init() }

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
        // 菜单内容在打开时经 menuNeedsUpdate 动态构建 (拉取最新统一菜单树), 此处无需重建。
    }

    private func ensureItem() -> NSStatusItem {
        if let it = statusItem { return it }
        let it = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        it.button?.font = NSFont.systemFont(ofSize: 14)
        let menu = NSMenu()
        menu.delegate = self // 打开时经 menuNeedsUpdate 动态填充
        it.menu = menu
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

    // MARK: - NSMenuDelegate

    /// 菜单将要打开: 清空并重建。优先用 Go 下发的统一菜单树 (与候选框右键菜单一致);
    /// 拉取失败 (服务未就绪) 时回退到只读状态展示。
    public func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        let header = NSMenuItem(title: "清风输入法", action: nil, keyEquivalent: "")
        header.isEnabled = false
        menu.addItem(header)
        menu.addItem(.separator())

        if let items = unifiedMenuProvider?(), !items.isEmpty {
            menuBuilder.populate(menu, with: items, dispatch: .inProcess { [weak self] id in self?.onUnifiedAction?(id) })
        } else {
            appendFallbackState(menu)
        }
    }

    /// 兜底只读菜单 (服务不可达时): 当前状态展示 (带勾选) + 设置入口。
    private func appendFallbackState(_ menu: NSMenu) {
        let p = latest ?? ModeStatusPayload(
            chineseMode: true, fullWidth: false, chinesePunct: true,
            capsLock: false, visible: true, effectiveMode: 0, modeLabel: "")
        addState(menu, "中文模式", on: p.chineseMode)
        addState(menu, "英文模式", on: !p.chineseMode)
        menu.addItem(.separator())
        addState(menu, "中文标点", on: p.chinesePunct)
        addState(menu, "英文标点", on: !p.chinesePunct)
        menu.addItem(.separator())
        addState(menu, "全角", on: p.fullWidth)
        addState(menu, "半角", on: !p.fullWidth)
        menu.addItem(.separator())
        let settings = NSMenuItem(title: "设置…", action: #selector(openSettingsMenuAction), keyEquivalent: ",")
        settings.target = self
        menu.addItem(settings)
    }

    private func addState(_ menu: NSMenu, _ title: String, on: Bool) {
        let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        item.state = on ? .on : .off
        item.isEnabled = false
        menu.addItem(item)
    }

    /// 打开设置应用 (wind_setting.app, Wails)。经 LaunchServices 按 bundleID 查找并启动,
    /// 已在运行则激活已有窗口 (macOS .app 天然单实例)。
    @objc private func openSettingsMenuAction() { openSettings(page: "") }

    /// 打开设置应用并可选跳转到指定页 (page 非空时传 --page=<page>)。
    /// 经 LaunchServices 按 bundleID 启动/激活已有实例。线程安全 (切主线程)。
    public func openSettings(page: String) {
        if !Thread.isMainThread {
            DispatchQueue.main.async { [weak self] in self?.openSettings(page: page) }
            return
        }
        let bundleID = "com.wails.wind_setting"
        let ws = NSWorkspace.shared
        if let url = ws.urlForApplication(withBundleIdentifier: bundleID) {
            let cfg = NSWorkspace.OpenConfiguration()
            if !page.isEmpty { cfg.arguments = ["--page=\(page)"] }
            ws.openApplication(at: url, configuration: cfg)
        } else {
            // LaunchServices 尚未登记时的兜底: open -b 触发一次注册+启动。
            let p = Process()
            p.launchPath = "/usr/bin/open"
            p.arguments = page.isEmpty ? ["-b", bundleID] : ["-b", bundleID, "--args", "--page=\(page)"]
            try? p.run()
        }
    }
}
