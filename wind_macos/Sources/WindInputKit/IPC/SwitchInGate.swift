import Foundation

/// 「刚切入本输入法」的一次性标记，只在**真正切换输入源**时补发 `IME_ACTIVATED`。
///
/// `activateServer` 在切应用、切文本框时都会触发，而服务端对 `IME_ACTIVATED` 会按
/// `input.default` 重置中英 / 全半角 / 标点；Windows 只在切入输入法时发它。直接在
/// `activateServer` 里补发，就会把用户手切的状态在同一应用内的每次焦点跳转里拉回默认值。
/// 所以用输入源切换通知置标记，由随后的激活消费一次。
///
/// 通知与 `activateServer` 谁先到不保证：通知先到 → 标记等激活来消费；激活先到 → 通知到时
/// 已有活动控制器，由调用方当场消费（见 `InputController.inputSourceSwitchedIn`）。
public struct SwitchInGate {
    public private(set) var pending = false
    /// 上一次通知里选中的是不是本输入法；`nil` = 还没收到过通知。
    private var selectedIsSelf: Bool?

    public init() {}

    /// 输入源切换通知：当前选中的是不是本输入法。切到别家 = 作废之前的标记。
    ///
    /// 只有「从别家（或首次）切到本输入法」这个跳变才置标记：系统一次切换会连发多条通知，
    /// 都选中本输入法时，后几条不能再次置位，否则一次切入会补发多次 `IME_ACTIVATED`。
    public mutating func selected(isSelf: Bool) {
        if isSelf {
            if selectedIsSelf != true { pending = true }
        } else {
            pending = false
        }
        selectedIsSelf = isSelf
    }

    /// 取走标记；返回是否该补发 `IME_ACTIVATED`。一次切入只消费一次。
    public mutating func consume() -> Bool {
        defer { pending = false }
        return pending
    }
}
