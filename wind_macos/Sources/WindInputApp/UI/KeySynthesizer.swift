import Cocoa
import WindInputKit

// KeySynthesizer — 命令直通车按键合成 (key.tap / key.seq / key.hold / key.release)。
//
// Go 服务进程 (LaunchAgent) 无 GUI 事件上下文, 故由 IMKit `.app` 收到 push 命令后
// 用 CGEvent 向当前聚焦应用合成键盘事件。键名 / 修饰键沿用 Go internal/keyinject
// 的规范形态 (KeyComboPayload.key / .modifiers), 在此映射到 macOS CGKeyCode /
// CGEventFlags。
//
// 注意: 向其它进程注入键盘事件需 .app 获得系统「辅助功能」(Accessibility) 授权,
// 否则事件被静默丢弃。首次未授权时弹系统授权请求并引导用户 (见 ensureTrusted)。
enum KeySynthesizer {

    // canonical 键名 → macOS 虚拟键码 (Carbon kVK_*, ANSI 布局)。
    // 键名集合与 internal/keyinject.normalizeKey 对齐。
    private static let keyCodeMap: [String: CGKeyCode] = [
        // 字母
        "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7, "c": 8, "v": 9,
        "b": 11, "q": 12, "w": 13, "e": 14, "r": 15, "y": 16, "t": 17,
        "o": 31, "u": 32, "i": 34, "p": 35, "l": 37, "j": 38, "k": 40,
        "n": 45, "m": 46,
        // 数字 (主键盘)
        "1": 18, "2": 19, "3": 20, "4": 21, "6": 22, "5": 23, "9": 25, "7": 26, "8": 28, "0": 29,
        // 标点 (canonical 名)
        "equal": 24, "minus": 27, "rbracket": 30, "lbracket": 33, "quote": 39,
        "semicolon": 41, "backslash": 42, "comma": 43, "slash": 44, "period": 47, "grave": 50,
        // 控制 / 编辑键
        "enter": 36, "tab": 48, "space": 49, "backspace": 51, "escape": 53,
        "delete": 117, "insert": 114, // mac 无 Insert, 借用 Help 槽位 (114)
        "home": 115, "end": 119, "pageup": 116, "pagedown": 121,
        "left": 123, "right": 124, "down": 125, "up": 126,
        "capslock": 57,
        // 修饰键自身 (供 key.hold("Shift") 等以键名落地时解析)
        "shift": 56, "ctrl": 59, "alt": 58, "win": 55,
        // 功能键
        "f1": 122, "f2": 120, "f3": 99, "f4": 118, "f5": 96, "f6": 97, "f7": 98, "f8": 100,
        "f9": 101, "f10": 109, "f11": 103, "f12": 111, "f13": 105, "f14": 107, "f15": 113,
        "f16": 106, "f17": 64, "f18": 79, "f19": 80, "f20": 90,
    ]

    private static func flags(for modifiers: [String]) -> CGEventFlags {
        var f: CGEventFlags = []
        for m in modifiers {
            switch m.lowercased() {
            case "ctrl": f.insert(.maskControl)
            case "shift": f.insert(.maskShift)
            case "alt": f.insert(.maskAlternate)
            case "win": f.insert(.maskCommand) // mac: win → Command
            default: break
            }
        }
        return f
    }

    // resolveKeyCode 解析键名为 CGKeyCode。支持 "vk:0xHH"/"vk:DD" 直接透传 (与
    // keyinject 一致, 但 vk 数值是 Windows VK, 在 mac 无意义, 仅当用户显式按 mac
    // keycode 传入时才有效——一般不用)。
    private static func resolveKeyCode(_ key: String) -> CGKeyCode? {
        let k = key.lowercased()
        if let code = keyCodeMap[k] {
            return code
        }
        if k.hasPrefix("vk:") {
            let raw = String(k.dropFirst(3))
            if raw.hasPrefix("0x"), let v = UInt16(raw.dropFirst(2), radix: 16) {
                return CGKeyCode(v)
            }
            if let v = UInt16(raw) {
                return CGKeyCode(v)
            }
        }
        return nil
    }

    /// 单次按键: modifiers 经 flags 叠加, keyDown → keyUp。
    static func tap(_ combo: KeyComboPayload) {
        guard ensureTrusted() else { return }
        postKey(combo, down: true)
        postKey(combo, down: false)
    }

    /// 顺序模拟多个组合 (key.seq)。
    static func sequence(_ combos: [KeyComboPayload]) {
        guard ensureTrusted() else { return }
        for c in combos {
            postKey(c, down: true)
            postKey(c, down: false)
        }
    }

    /// 按下并保持 (key.hold)。仅 keyDown, 不抬起; 与 release 成对。
    static func hold(_ combo: KeyComboPayload) {
        guard ensureTrusted() else { return }
        postKey(combo, down: true)
    }

    /// 抬起之前 hold 的组合 (key.release)。
    static func release(_ combo: KeyComboPayload) {
        guard ensureTrusted() else { return }
        postKey(combo, down: false)
    }

    private static func postKey(_ combo: KeyComboPayload, down: Bool) {
        guard let keyCode = resolveKeyCode(combo.key) else {
            NSLog("[WindInput] KeySynthesizer: 未知键名 \(combo.key), 跳过")
            return
        }
        guard let ev = CGEvent(keyboardEventSource: nil, virtualKey: keyCode, keyDown: down) else {
            return
        }
        ev.flags = flags(for: combo.modifiers)
        ev.post(tap: .cgSessionEventTap)
    }

    // ensureTrusted 检查辅助功能授权; 未授权时弹一次系统请求 (打开系统设置→隐私与
    // 安全性→辅助功能) 并返回 false, 本次按键放弃。授权后续触发即生效。
    private static var promptedOnce = false
    private static func ensureTrusted() -> Bool {
        if AXIsProcessTrusted() {
            return true
        }
        if !promptedOnce {
            promptedOnce = true
            let opt = kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String
            _ = AXIsProcessTrustedWithOptions([opt: true] as CFDictionary)
            NSLog("[WindInput] KeySynthesizer: 需在 系统设置→隐私与安全性→辅助功能 授权清风输入法后按键功能方可生效")
        }
        return false
    }
}
