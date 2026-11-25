import Foundation

/// 数字后智能标点的 prevChar 记账（macOS 侧唯一通路）。
///
/// 不现读文档（IMKit 读文档是跨进程同步调用，压在按键延迟上），只跟踪我们自己送进文档的东西，
/// 行为在所有 app 里一致；代价是光标被外部移动时会漂，故判据保守：拿不准就清零，回落中文标点。
/// 只报 ASCII 数字，其余一律报 0（不可用）：智能符号 press2 把 0 当作「读不回文档」只信武装态，
/// 而数字会正确拒绝 press2（快打 `1.1.` 不会删掉中间的 `1`）。
/// 键盘侧与上屏侧两处都要喂，挂钩点见 `BridgeResponseRouter.insertCommitted` 与
/// `InputController.passThroughToHost`。背景见 docs/architecture/smart-symbol-compat-notes.md 第 5 条。
public final class SmartPunctDigitTracker {

    /// 光标前一字符（仅 ASCII 数字；0 = 不可用/不是数字）。
    public private(set) var prevChar: UInt16 = 0

    public init() {}

    /// 焦点切换等「文档换了、我们的记账全部作废」的时刻调用。
    public func reset() {
        prevChar = 0
    }

    /// 这一键产出的数字字符（0 = 不产出数字）。判据只能落在客户端：这些键透传出去，服务端看不到。
    ///
    /// 别在这里写标点白名单：那等于抄一份服务端的 smart_list，必然漂移；要不要用由
    /// `is_smart_punct_after_digit` 决定。主键盘（0x30-0x39）与小键盘（0x60-0x69）都要认，
    /// 漏了小键盘还会把已记的值清零。
    public static func digitChar(vk: UInt32, modifiers: UInt32) -> UInt16 {
        let modShift: UInt32 = 0x0001
        // ⌘/⌃/⌥ + 数字是宿主快捷键，不往文档写字；记了就是幻影数字，比漏记更糟。
        // Windows 的 `_DigitCharFromVk` 只挡 Shift，因为 TSF 在记录点之前已滤掉快捷键，
        // IMKit 则把每个 keyDown 都交过来，这道守卫只有 macOS 需要。
        let modNonShift: UInt32 = 0x0002 | 0x0004 | 0x0008   // Ctrl / Alt / Win(⌘)
        if modifiers & modNonShift != 0 {
            return 0
        }
        if vk >= 0x30 && vk <= 0x39 {
            // Shift+主键盘数字产出的是符号（!@#…）而非数字，不能当数字记。
            return (modifiers & modShift) != 0 ? 0 : UInt16(vk)
        }
        if vk >= 0x60 && vk <= 0x69 {
            return UInt16(0x30 + (vk - 0x60))
        }
        return 0
    }

    /// 按键透传回宿主（服务端返 PassThrough / ClearThenPassThrough / 未命中的快捷键）。
    ///
    /// 产出数字则记，否则清零。退格、方向键、字母键都走后者，「删掉数字再打标点回落中文标点」
    /// 是自然结果，不必为退格写特判。
    public func noteKeyPassthrough(vk: UInt32, modifiers: UInt32) {
        prevChar = Self.digitChar(vk: vk, modifiers: modifiers)
    }

    /// 经引擎上屏的文本（`insertText` 真写进文档的那一份）。
    ///
    /// 记末位字符：ASCII 数字则记，否则清零。全角数字刻意不记（服务端只认 ASCII）。
    ///
    /// 空文本不动状态：没有东西写进文档，光标前是什么并没有改变。
    public func noteCommittedText(_ text: String) {
        guard let last = text.unicodeScalars.last else { return }
        prevChar = (last.value >= 0x30 && last.value <= 0x39) ? UInt16(last.value) : 0
    }
}
