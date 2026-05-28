import Cocoa
import WindInputKit

// KeyHandler — 把 NSEvent 翻译成 Go bridge 期望的 KeyEvent payload.
//
// 设计要点:
//   - Go 服务把按键当 Windows VK 编码处理 (跨平台镜像), 因此 macOS 这边需要把
//     NSEvent.keyCode (Carbon kVK_*) 映射到 Win VK_*.
//   - Modifier 也按 Win 端 ModShift/ModCtrl/ModAlt/ModWin 的 bit 位编码.
//   - macOS 上 Cmd ↔ Win 端 ModWin (Apple 物理 Cmd 键 ≈ Win 键的角色).
//   - 不在这一层做任何"中英判定"逻辑; 让 Go 服务决定怎么处理.
public enum KeyHandler {

    /// 把 NSEvent.keyCode (Carbon kVK_*) 翻译为 Win VK_*.
    /// 未覆盖的键返回 0; 调用方需要丢弃或按 PassThrough 走.
    public static func toWindowsVK(_ macKeyCode: UInt16) -> UInt32 {
        // 映射表参考 Carbon HIToolbox/Events.h 与 Windows VK_* 常量.
        // 注: macOS 字母键 keyCode 与字母在 ASCII 中的位置无关 (历史原因).
        switch macKeyCode {
        // 字母 A-Z (按 keyCode 顺序填表; ASCII 'A'=0x41)
        case 0x00: return 0x41 // A
        case 0x0B: return 0x42 // B
        case 0x08: return 0x43 // C
        case 0x02: return 0x44 // D
        case 0x0E: return 0x45 // E
        case 0x03: return 0x46 // F
        case 0x05: return 0x47 // G
        case 0x04: return 0x48 // H
        case 0x22: return 0x49 // I
        case 0x26: return 0x4A // J
        case 0x28: return 0x4B // K
        case 0x25: return 0x4C // L
        case 0x2E: return 0x4D // M
        case 0x2D: return 0x4E // N
        case 0x1F: return 0x4F // O
        case 0x23: return 0x50 // P
        case 0x0C: return 0x51 // Q
        case 0x0F: return 0x52 // R
        case 0x01: return 0x53 // S
        case 0x11: return 0x54 // T
        case 0x20: return 0x55 // U
        case 0x09: return 0x56 // V
        case 0x0D: return 0x57 // W
        case 0x07: return 0x58 // X
        case 0x10: return 0x59 // Y
        case 0x06: return 0x5A // Z

        // 顶排数字 0-9 (Carbon kVK_ANSI_0..9, 注意 0 在 9 之后)
        case 0x1D: return 0x30 // 0
        case 0x12: return 0x31 // 1
        case 0x13: return 0x32 // 2
        case 0x14: return 0x33 // 3
        case 0x15: return 0x34 // 4
        case 0x17: return 0x35 // 5
        case 0x16: return 0x36 // 6
        case 0x1A: return 0x37 // 7
        case 0x1C: return 0x38 // 8
        case 0x19: return 0x39 // 9

        // 控制键
        case 0x33: return 0x08 // VK_BACK (Delete on Mac keyboard)
        case 0x30: return 0x09 // VK_TAB
        case 0x24: return 0x0D // VK_RETURN
        case 0x35: return 0x1B // VK_ESCAPE
        case 0x31: return 0x20 // VK_SPACE
        case 0x75: return 0x2E // VK_DELETE (Forward Delete)
        case 0x73: return 0x24 // VK_HOME
        case 0x77: return 0x23 // VK_END
        case 0x74: return 0x21 // VK_PRIOR (Page Up)
        case 0x79: return 0x22 // VK_NEXT  (Page Down)
        case 0x7B: return 0x25 // VK_LEFT
        case 0x7E: return 0x26 // VK_UP
        case 0x7C: return 0x27 // VK_RIGHT
        case 0x7D: return 0x28 // VK_DOWN

        // 标点 / OEM
        case 0x29: return 0xBA // VK_OEM_1   ;:
        case 0x18: return 0xBB // VK_OEM_PLUS  =+
        case 0x2B: return 0xBC // VK_OEM_COMMA ,<
        case 0x1B: return 0xBD // VK_OEM_MINUS -_
        case 0x2F: return 0xBE // VK_OEM_PERIOD .>
        case 0x2C: return 0xBF // VK_OEM_2   /?
        case 0x32: return 0xC0 // VK_OEM_3   `~
        case 0x21: return 0xDB // VK_OEM_4   [{
        case 0x2A: return 0xDC // VK_OEM_5   \|
        case 0x1E: return 0xDD // VK_OEM_6   ]}
        case 0x27: return 0xDE // VK_OEM_7   '"

        default:
            return 0
        }
    }

    /// 把 NSEvent.modifierFlags 编码为 Go 端 Win 风格 modifier bit 位.
    /// macOS Cmd 映射为 Win ModWin (与 Win 端 ModWin 0x0008 一致).
    public static func toModifiers(_ flags: NSEvent.ModifierFlags) -> UInt32 {
        var m: UInt32 = 0
        // 0x0001 ModShift / 0x0002 ModCtrl / 0x0004 ModAlt / 0x0008 ModWin
        if flags.contains(.shift)    { m |= 0x0001 }
        if flags.contains(.control)  { m |= 0x0002 }
        if flags.contains(.option)   { m |= 0x0004 }
        if flags.contains(.command)  { m |= 0x0008 }
        // CapsLock 由 Toggles 字段单独表达 (0x0100 ModCapsLock 是 hotkey hash 用,
        // KeyEvent 走 Toggles), 这里不放进 modifiers.
        return m
    }

    /// 把 NSEvent 编码成可发送的 KeyEvent 帧字节 (含 header).
    /// `seq` 由调用方维护自增, 用于服务端 stale 检测.
    public static func encodeKeyEvent(_ event: NSEvent, seq: UInt16) -> Data? {
        let vk = toWindowsVK(event.keyCode)
        // VK==0 的键我们不发, 让 IMKit 自行 PassThrough.
        // (Modifier keys 本身的 keyDown/keyUp 在 IMKit 通常以 .flagsChanged 走, 这里 nil)
        guard vk != 0 else { return nil }

        let mods = toModifiers(event.modifierFlags)
        var toggles: UInt8 = 0
        if event.modifierFlags.contains(.capsLock) {
            toggles |= 0x01 // ToggleCapsLock
        }
        let kind: KeyEventType
        switch event.type {
        case .keyDown: kind = .down
        case .keyUp:   kind = .up
        default:       return nil
        }

        let payload = KeyEventPayload(
            keyCode: vk,
            scanCode: 0,             // macOS 不暴露 PS/2 scan code; 留 0
            modifiers: mods,
            eventType: kind,
            toggles: toggles,
            eventSeq: seq,
            prevChar: 0              // M2.1 暂不取 caret 前字符
        )
        return BinaryCodec.encodeKeyEventFrame(payload)
    }
}
