import Cocoa
import WindInputKit

// ModeStatusController — 菜单栏输入模式指示器 (NSStatusItem)。
//
// 接收服务端经 push 通道发来的 CmdModeStatus, 在菜单栏显示紧凑状态标题。
//
// 下拉菜单与候选框空白处右键菜单一致: 打开时经 unifiedMenuProvider 同步拉取统一菜单树, 用共享的
// UnifiedMenuBuilder 构建原生菜单, 点击经 onUnifiedAction 回发 CmdMenuAction; 拉取失败时回退到
// 只读状态展示 + 设置入口。可见性由服务端 toolbar reducer 驱动, 整个进程一个指示器 (单例)。
public final class ModeStatusController: NSObject, NSMenuDelegate {
    public static let shared = ModeStatusController()

    /// 统一菜单树提供者 (同步向服务端请求, 失败返回 nil)。由 CandidatePanelHost 注入,
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

    /// 菜单将要打开: 清空并重建。优先用服务端下发的统一菜单树 (与候选框右键菜单一致);
    /// 拉取失败 (服务未就绪) 时回退到只读状态展示。
    public func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        // 菜单头显示本变体名 (release: 清风输入法; debug: 清风输入法开发版)。取自 Info.plist
        // CFBundleDisplayName (app.sh 按变体注入), 让两套指示器一眼可辨。
        let appName = (Bundle.main.infoDictionary?["CFBundleDisplayName"] as? String)
            ?? (Bundle.main.infoDictionary?["CFBundleName"] as? String) ?? "清风输入法"
        let header = NSMenuItem(title: appName, action: nil, keyEquivalent: "")
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

    /// 打开设置应用 (wind_setting.app, Rust + windui)。经 LaunchServices 按 bundleID 查找并启动,
    /// 已在运行则激活已有窗口 (无参数时; 带深链参数的路径见 openSettings 注释)。
    @objc private func openSettingsMenuAction() { openSettings(arguments: []) }

    /// 打开设置应用, arguments 为原样直通的命令行参数 (深链见
    /// `BinaryCodec.decodeOpenSettingsArguments`)。空数组 = 默认页。
    /// 经 LaunchServices 按 bundleID 启动/激活已有实例。线程安全 (切主线程)。
    ///
    /// 已在运行时 LaunchServices 不重传 arguments, 只激活窗口; 深链要靠 windui 的单实例转发
    /// (二次实例把 argv 发给首实例), 故带参数时显式要求新实例 (它把 argv 转给首实例后自行退出)。
    /// 无参数 (托盘「设置…」) 仍走普通激活, 否则每点一次就多开一个窗口。
    public func openSettings(arguments: [String]) {
        if !Thread.isMainThread {
            DispatchQueue.main.async { [weak self] in self?.openSettings(arguments: arguments) }
            return
        }
        // 设置应用按本 IME 变体启动: dev IME → dev 设置应用 (连 WindInputDev 服务 rpc.sock),
        // release IME → 正式设置应用, 实现配置分离 (各编辑各自服务的配置)。
        // 判据用 bundleID 后缀 "Dev": dev IME bundleID = to.feng.inputmethod.WindInputDev,
        // 对应 dev 设置 app bundleID = com.wails.wind_setting_debug (见 scripts/mac/dev.sh build_setting)。
        let isDev = Bundle.main.bundleIdentifier?.hasSuffix("Dev") ?? false
        let bundleID = isDev ? "com.wails.wind_setting_debug" : "com.wails.wind_setting"
        let ws = NSWorkspace.shared
        if let url = ws.urlForApplication(withBundleIdentifier: bundleID) {
            let cfg = NSWorkspace.OpenConfiguration()
            if !arguments.isEmpty {
                cfg.arguments = arguments
                cfg.createsNewApplicationInstance = true
            }
            ws.openApplication(at: url, configuration: cfg)
        } else {
            // LaunchServices 尚未登记时的兜底: open -b 触发一次注册+启动。
            // 带参数时同样要 -n 强起新实例, 理由同上 (不加则已运行时 argv 被丢弃)。
            let p = Process()
            p.launchPath = "/usr/bin/open"
            p.arguments = arguments.isEmpty
                ? ["-b", bundleID]
                : ["-n", "-b", bundleID, "--args"] + arguments
            try? p.run()
        }
    }
}
