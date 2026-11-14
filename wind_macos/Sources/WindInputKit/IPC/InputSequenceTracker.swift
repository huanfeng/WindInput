import Foundation

/// 发往服务端的 `KeyEventPayload.seqChar`：输入序列里最近进文档的字符代表值（`unknown` = 不知道）。
///
/// 服务端据此判中英/数字交界补空格、英文后智能标点（设计见
/// `docs/design/auto-spacing-and-latin-smart-punct.md`）。取值规则与 Windows
/// `wind_tsf/include/InputSequenceChar.h` 逐条一致——服务端只看**类别**，透传键报代表值即可：
/// 字母 `a`、数字本身、空格 ` `、其余可见符号 `?`，编辑/导航/功能键与快捷键报 0。
///
/// # 与 `SmartPunctDigitTracker` 为什么分开
///
/// 数字记账在**组字更新**（marked text）时也会清零或改写——那是智能符号 press2 守卫的需要。
/// 本记账不能跟：组字内容没有写进文档，跟了的话「打 `nihao` → Esc 取消」会留下编码末位 `o`，
/// 下一笔中文就被误补一个空格。这里只在**真写进文档**的那几处更新。
public final class InputSequenceTracker {

    /// 「在记账、但不知道」。0 只来自不带这个字段的旧客户端，服务端对两者处理不同（Rust
    /// `wind_ipc::protocol::SEQ_CHAR_UNKNOWN`）。
    public static let unknown: UInt16 = 0xFFFF

    /// 报给服务端的值：记下之后用户点过鼠标（光标可能被挪到别处）就是「不知道」。
    public var seqChar: UInt16 {
        mouseDownCount() == mouseAtUpdate ? recorded : Self.unknown
    }

    /// 登录以来的鼠标按下次数（app 层注入 `CGEventSource.counterForEventType`）。默认恒 0 =
    /// 不识别鼠标挪光标（测试、拿不到计数时）。
    ///
    /// 只比计数、不读宿主光标位置：`selectedRange()` 是跨进程调用，放不进按键热路径。
    /// 每次记账都存下当时的计数，所以「点选候选再上屏」这类点击发生在记账之前，不会误判。
    public var mouseDownCount: () -> UInt32 = { 0 } {
        didSet { mouseAtUpdate = mouseDownCount() }
    }

    private var recorded: UInt16 = InputSequenceTracker.unknown
    private var mouseAtUpdate: UInt32 = 0

    public init() {}

    private func record(_ c: UInt16) {
        recorded = c
        mouseAtUpdate = mouseDownCount()
    }

    /// 焦点切换、文档被我们看不见的方式改写（合成按键、纯删除）时调用。
    public func reset() {
        record(Self.unknown)
    }

    /// 这一键透传进宿主后的代表字符（Windows VK 口径，见 `KeyHandler.toWindowsVK`）。
    public static func charFromVk(vk: UInt32, modifiers: UInt32) -> UInt16 {
        let modShift: UInt32 = 0x0001
        let modNonShift: UInt32 = 0x0002 | 0x0004 | 0x0008   // Ctrl / Alt / Win(⌘)：宿主快捷键
        if modifiers & modNonShift != 0 { return unknown }
        let shift = modifiers & modShift != 0
        switch vk {
        case 0x30...0x39: return shift ? 0x3F : UInt16(vk)   // Shift+数字是 !@#…
        case 0x41...0x5A: return 0x61                        // 'a'
        case 0x60...0x69: return UInt16(0x30 + (vk - 0x60))  // 小键盘数字
        case 0x20: return 0x20
        case 0x6A...0x6F, 0xBA...0xC0, 0xDB...0xDE, 0xE2: return 0x3F  // 可见符号 → '?'
        default: return unknown
        }
    }

    public func noteKeyPassthrough(vk: UInt32, modifiers: UInt32) {
        record(Self.charFromVk(vk: vk, modifiers: modifiers))
    }

    /// 经引擎写进文档的文本：末尾码点的首个 UTF-16 单元（辅助平面取高代理）。
    /// 空文本不动状态：没有东西写进文档。
    public func noteCommittedText(_ text: String) {
        guard let last = text.unicodeScalars.last else { return }
        record(String(last).utf16.first ?? 0)
    }

    /// 光标跨过了一个已知是符号的字符（智能跳过右标点）。
    public func noteCursorCrossedSymbol() {
        record(0x3F)
    }
}
