import Cocoa
import Carbon
import InputMethodKit

// WindInput macOS IMKit 入口.
//
// 系统通过 Info.plist 中 InputMethodConnectionName 定位本 .app, 启动它,
// 然后通过 InputMethodServerControllerClass 指定的类创建 IMKInputController 实例.
//
// top-level code 自动运行在 main actor, 因此可以直接同步调用 NSApp/IMKServer API,
// 无需 @MainActor 注解与 unsafe 闭包包装.

// MARK: - 子命令分发 (--register-input-source / --enable-input-source / --select-input-source)
//
// macOS Tahoe (26) 上 TIS 只接受 IME 自身进程发起的 TISRegisterInputSource (校验 codesign
// identity 与 bundleID), 外部 CLI 调用会静默无效. 做法同 rime/squirrel.
let args = CommandLine.arguments
if args.count > 1 {
    let bundleURL = Bundle.main.bundleURL as CFURL
    // 本 .app 自身的 bundleID (release: to.feng.inputmethod.WindInput; dev: ...WindInputDev)。
    // 变体共存的关键: 只认本 bundle 自己的 mode, 不把另一变体的 mode 误判为已注册。
    let selfBundleID = Bundle.main.bundleIdentifier ?? "to.feng.inputmethod.WindInput"
    let selfModeID = "\(selfBundleID).mode"

    func enabledModeIDs() -> Set<String> {
        guard let arr = TISCreateInputSourceList(nil, true)?.takeRetainedValue() as? [TISInputSource] else {
            return []
        }
        var found = Set<String>()
        for src in arr {
            guard let p = TISGetInputSourceProperty(src, kTISPropertyInputSourceID) else { continue }
            let id = Unmanaged<CFString>.fromOpaque(p).takeUnretainedValue() as String
            // 用 "<bundleID>." 前缀精确隔离变体: "...WindInputDev.mode" 不以 "...WindInput." 开头,
            // 反之亦然, 避免两变体互相把对方的 mode 当成自己已启用。
            if id.hasPrefix("\(selfBundleID).") {
                let ep = TISGetInputSourceProperty(src, kTISPropertyInputSourceIsEnabled)
                let isEnabled = ep.map { CFBooleanGetValue(Unmanaged<CFBoolean>.fromOpaque($0).takeUnretainedValue()) } ?? false
                if isEnabled { found.insert(id) }
            }
        }
        return found
    }

    func findInputSource(id targetID: String) -> TISInputSource? {
        guard let arr = TISCreateInputSourceList(nil, true)?.takeRetainedValue() as? [TISInputSource] else {
            return nil
        }
        for src in arr {
            guard let p = TISGetInputSourceProperty(src, kTISPropertyInputSourceID) else { continue }
            let id = Unmanaged<CFString>.fromOpaque(p).takeUnretainedValue() as String
            if id == targetID { return src }
        }
        return nil
    }

    switch args[1] {
    case "--register-input-source", "--install":
        // 总是重新注册, 不因「已注册」早退: 重新部署后 cdhash 变化, 不刷新注册系统会校验失配。
        // 注册完立即退出, 不能常驻 (同 Squirrel): 同 bundle 有进程在跑时 imklaunchagent 不再拉起
        // 控制器, 而本进程不建 IMKServer, 之后就打不了字。注册是持久写入, 进程退出不影响。
        let already = enabledModeIDs()
        let st = TISRegisterInputSource(bundleURL)
        print("TISRegisterInputSource bundleURL=\(bundleURL) OSStatus=\(st) wasEnabled=\(!already.isEmpty)")
        // 全新注册 (此前未启用) 失败才视为致命; 重注册已存在的源时 OSStatus 可能为
        // paramErr 等非 0, 属正常。
        exit(already.isEmpty && st != noErr ? 2 : 0)

    case "--enable-input-source":
        let modeID = args.count > 2 ? args[2] : selfModeID
        guard let src = findInputSource(id: modeID) else {
            print("Mode \(modeID) 未在 TIS 中找到 (先跑 --register-input-source)")
            exit(3)
        }
        let st = TISEnableInputSource(src)
        print("TISEnableInputSource \(modeID) OSStatus=\(st)")
        exit(st == noErr ? 0 : 2)

    case "--select-input-source":
        let modeID = args.count > 2 ? args[2] : selfModeID
        guard let src = findInputSource(id: modeID) else {
            print("Mode \(modeID) 未找到")
            exit(3)
        }
        let st = TISSelectInputSource(src)
        print("TISSelectInputSource \(modeID) OSStatus=\(st)")
        exit(st == noErr ? 0 : 2)

    case "--help", "-h":
        print("""
        WindInput.app 子命令:
          (no args)                   作为 IME 服务跑 (系统 imklaunchagent 拉起的默认路径)
          --register-input-source     调 TISRegisterInputSource 把本 .app 注册到 TIS
          --enable-input-source [id]  调 TISEnableInputSource 启用 mode (默认 to.feng.inputmethod.WindInput)
          --select-input-source [id]  调 TISSelectInputSource 选中 mode
        """)
        exit(0)

    default:
        // 未识别参数: 当成正常 IME 启动 (兼容 imklaunchagent 偶尔附加未知参数的情况)
        break
    }
}

// MARK: - 正常 IME 启动路径

// 与 Info.plist 中 InputMethodConnectionName 必须一致, 且必须等于
// "$(CFBundleIdentifier)_Connection" (现代 macOS sandbox IME 强制约定).
guard let bundleID = Bundle.main.bundleIdentifier else {
    fatalError("WindInputApp: Info.plist 缺 CFBundleIdentifier")
}
let connectionName = "\(bundleID)_Connection"

NSLog("WindInputApp boot bundleID=\(bundleID) connection=\(connectionName)")

// IMKServer 必须在 NSApp.run 前创建并持有引用; ARC 不能释放它,
// 所以放到顶层 (top-level let) 让进程生命周期持有.
let imkServer = IMKServer(name: connectionName, bundleIdentifier: bundleID)
NSLog("WindInputApp IMKServer ready name=\(connectionName) instance=\(String(describing: imkServer))")

// 辅助功能授权自检, 只记一行日志、不弹框。按键合成 (KeySynthesizer) 与智能配对的宿主光标回退
// 需要它, 未授权时静默失效。自签证书下 TCC 把授权钉在当次构建的 cdhash 上, 重新部署后系统设置
// 里开关仍亮着、实际已失效; 排查时先看这一行。
let axTrusted = AXIsProcessTrusted()
NSLog("WindInputApp 辅助功能授权: \(axTrusted ? "已获得" : "未获得（按键合成/配对光标回退将静默失效；重新部署后需重新授权）")")

// NSApplication 必须先于任何 NSWindow/NSPanel 创建: 否则 AppKit 建出的窗口拿不到 windowNumber(=0)、
// 不进 NSApp.windows, 之后 orderFrontRegardless() 静默失效而 isVisible 仍读 true (候选框永不显示且无报错)。
// 下面的 CandidatePanelHost 初始化就会构造 CandidatePanel, 不要把这两行挪到它后面。
let app = NSApplication.shared
// accessory 策略: 不在 Dock 显示, 也不抢菜单焦点. IMKit `.app` 标准做法.
app.setActivationPolicy(.accessory)

// 启动候选框 host (订阅 bridge push + mmap SHM)。失败不致命: 服务可能晚启动, host 内部 lazy retry。
CandidatePanelHost.shared.start()

// 监听输入源切换：只在真正切入本输入法时补发 IME_ACTIVATED（见 InputSourceMonitor）。
InputSourceMonitor.shared.start()

app.run()
