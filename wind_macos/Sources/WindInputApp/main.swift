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
// 必要性: macOS Tahoe (26) 上 TIS 仅接受来自 IME 自身进程的 TISRegisterInputSource
// 调用 (校验 codesign identity 与 bundleID 是否匹配本 IME), 外部 swift CLI 调
// silently no-op. 与 rime/squirrel 的 InputSource.swift / Main.swift 路径一致.
let args = CommandLine.arguments
if args.count > 1 {
    let bundleURL = Bundle.main.bundleURL as CFURL
    // 本 .app 自身的 bundleID (release: to.feng.inputmethod.WindInput; debug: ...WindInputDebug)。
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
            // 用 "<bundleID>." 前缀精确隔离变体: "...WindInputDebug.mode" 不以 "...WindInput." 开头,
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
        // 总是 (重新) 注册并保持常驻维持注册 —— 不能因「已注册」就早退 (踩过的坑):
        //   1. 重新部署后 .app 的 cdhash 变化, 必须用新 bundle 重新 TISRegisterInputSource
        //      刷新注册, 否则系统用旧 cdhash 校验新二进制失配 → 输入法无法切换/激活。
        //   2. TIS 注册是进程级 lifecycle, 需常驻进程维持 (退出后条目会被系统回收)。install
        //      会先按路径杀掉旧守护, 此时若早退则没有进程维持注册 → 注册失效。
        // 本子命令仅由 install_app.sh 调用, 不在正常 IME 启动路径, 重注册无副作用。
        let already = enabledModeIDs()
        let st = TISRegisterInputSource(bundleURL)
        print("TISRegisterInputSource bundleURL=\(bundleURL) OSStatus=\(st) wasEnabled=\(!already.isEmpty)")
        // 全新注册 (此前未启用) 失败才视为致命; 重注册已存在的源时 OSStatus 可能为
        // paramErr 等非 0, 属正常, 仍保持常驻维持现有注册。
        if already.isEmpty && st != noErr {
            exit(2)
        }
        print("保持进程常驻维持 TIS 注册 (后台等待 SIGTERM 退出)...")
        // 进入 RunLoop 永久等待信号
        RunLoop.current.run()
        exit(0)

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

// PR-A.5 Phase 1: 启动候选框 host (订阅 bridge push + mmap SHM)。
// 失败不致命 — Go 服务可能晚启动, host 内部 lazy retry。
CandidatePanelHost.shared.start()

let app = NSApplication.shared
// accessory 策略: 不在 Dock 显示, 也不抢菜单焦点. IMKit `.app` 标准做法.
app.setActivationPolicy(.accessory)
app.run()
