import Foundation

/// 候选显示时 Ctrl 如实上报 (`KeyEventType.modDown` / `.modUp`) 的一次按住记账, 对位 TSF
/// `CtrlRawPolicy.h` 与 Linux `CtrlRaw`。协议权威在协调器 `handle_ctrl_raw`:
///
/// - **按下那一刻**组字中 (`OptionKeyHold.inputSession`) 才走这条: 按下报 modDown、松开报 modUp
///   (哪怕那时已不在组字), 不再走 `sendModifierTap` 的单击上报; 否则这次按住整个走旧路。
///   一次按住只走一条路, 服务端不会两边各收到一次。
/// - 按住期间按过别的键 (服务端未必看得见, 如交还宿主的 Ctrl+C) → 松开带 `notTapBit`。时长由服务端判。
/// - 键本身不吃 (flagsChanged 照常放行)。
public struct CtrlRawReport {
    /// `KeyPayload.toggles` 的「平台看见过别的输入」位, 与 protocol.rs `TOGGLE_MOD_NOT_TAP` 一致。
    public static let notTapBit: UInt8 = 0x10

    public private(set) var vk: UInt32?
    public private(set) var dirty = false

    public init() {}

    /// Ctrl 按下 (flagsChanged 没有自动连发, 每次都是新的一次按住)。返回是否报 modDown。
    public mutating func down(vk: UInt32, composing: Bool) -> Bool {
        if self.vk == vk {
            self = CtrlRawReport()           // 同侧再按下 = 上次松开丢了 (Ctrl+点击切走等): 丢掉残留
        } else if self.vk != nil {
            dirty = true                     // 另一侧 Ctrl 还按着: 那次不再是单击
        }
        guard composing else { return false }
        self.vk = vk
        dirty = false
        return true
    }

    /// 按住期间别的键或别的修饰键。
    public mutating func noteOther() {
        if vk != nil { dirty = true }
    }

    /// `KeyPayload.toggles` 的 CapsLock 位, 与普通按键帧 (`KeyHandler`) 同口径。
    public static func toggles(capsLock: Bool, notTap: Bool) -> UInt8 {
        (capsLock ? UInt8(0x01) : 0) | (notTap ? notTapBit : 0)
    }

    /// Ctrl 松开。nil = 不是如实上报的那次按住 (交给旧的单击判定); 否则返回是否不算单击。
    public mutating func up(vk: UInt32) -> Bool? {
        guard self.vk == vk else { return nil }
        let notTap = dirty
        self = CtrlRawReport()
        return notTap
    }
}
