import Foundation

/// 组字中 ⌥ 转发给服务（切换注释态）的一次按住记账。协议权威在协调器 `handle_comment_alt`:
/// 只在组字中转发 ⌥ 的按下与松开 (0xA4 / 0xA5), 修饰位带上这次按住期间按过的
/// Shift / Control / Command (粘滞, 按过就一直带到松开); 吃不吃看回包。
public struct OptionKeyHold {
    /// Shift / Ctrl / Win(Command) 及其左右位 (Win 风格 modifier, 见 `KeyHandler.toModifiers`)。
    static let otherMods: UInt32 = 0x0001 | 0x0002 | 0x0008 | 0x0010 | 0x0020 | 0x0040 | 0x0080

    public private(set) var stickyMods: UInt32 = 0
    private var active = false
    private var forwarded = false

    public init() {}

    /// 转发判据里的「组字中」: 有组合, 或候选窗开着 (联想窗组合为空、候选仍在, 也能切注释)。
    public static func inputSession(hasComposition: Bool, candidatesVisible: Bool) -> Bool {
        hasComposition || candidatesVisible
    }

    /// ⌥ 按下 (flagsChanged 没有自动连发, 每次都是新的一次按住)。
    public mutating func down(forwarded: Bool) {
        active = true
        stickyMods = 0
        self.forwarded = forwarded
    }

    /// 按住期间的修饰键变化。
    public mutating func note(modifiers: UInt32) {
        if active { stickyMods |= modifiers & Self.otherMods }
    }

    /// 松开是否转发: 组字中, 或按下转发过 (按住翻转要靠松开复原, 哪怕期间已上屏)。
    public func shouldForwardUp(composing: Bool) -> Bool {
        composing || (active && forwarded)
    }

    public mutating func up() {
        self = OptionKeyHold()
    }
}
