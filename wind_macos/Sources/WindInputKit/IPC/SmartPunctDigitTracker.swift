import Foundation

/// 数字后智能标点的 **prevChar 备用通路**（macOS 侧唯一通路）。
///
/// # 为什么 macOS 只有备用通路
///
/// 服务端 `wind_punct::is_smart_punct_after_digit` 只认 prevChar 的**值**
/// （判 0x30..=0x39），它不知道那个数字是怎么进的文档。Windows 有两条路喂它：
/// TSF 现读文档（主路径）+ `_lastPassthroughDigit`（备用，治 EverEdit 这类读不回
/// 文档的宿主）。macOS 这边 `prevChar` 长期恒 0，功能从未接通。
///
/// IMKit 也能读文档（`selectedRange` + `attributedSubstring`），但那是**跨进程同步
/// 调用**——`InputController` 里已经为此把取选中文本挪出 `activateServer`（实测
/// 1191/4643 个采样点）。每键都读的代价压在按键延迟上，且宿主支持度参差。
/// 故这里**只**跟踪「我们自己送进文档的东西」：不依赖宿主能力，行为在所有 app 里一致。
///
/// # 代价：光标被外部移动时会漂
///
/// 鼠标点击、⌘V 粘贴、别的输入法/程序改文档，我们都看不见。判据因此是**保守**的：
/// 拿不准就清零，回落到「按中文标点出」这个默认行为。少触发一次智能标点，好过在
/// 「12 后面点了一下光标又移回来」这种场景里错出半角。
///
/// # 语义：只报数字，其余一律报 0（＝「不可用」）
///
/// 与 Windows 主路径「如实报出光标前那个字符」**不同**，这里非数字一律归 0。服务端两处
/// 判据都吃得下这个语义，且都吃出正确结果：
///   - 数字后智能标点：本就只认 `0x30..=0x39`，非数字报什么都一样。
///   - 智能符号 press2：`prev_char == 0` 被当作「宿主读不回文档」而退回只信武装态
///     （见 `handle_punct.rs::smart_symbol_press2` 与 smart-symbol-compat-notes 第 3 条），
///     所以连按替换照常工作；而 prev_char 是**数字**时那条守卫会正确地拒绝 press2。
///
/// 后者不是副作用，是修的第二个 bug：快打 `1.1.` 时两个 `.` 落在 500ms 内、同键、模式没变，
/// prev_char 恒 0 的旧行为下守卫形同虚设 → 判成 press2 → `ReplaceBackward{count:1}` 把中间
/// 那个 `1` 删掉换成 `.`。接通 prev_char 后 `'1' != '.'`（武装串末位），这一按回落正常流程。
///
/// # 记录点必须成对
///
/// 键盘侧（透传出去的键）与上屏侧（经引擎写进文档的文本）**两处都要喂**，漏一处的
/// 症状是「某条路径打出的数字后面标点仍出中文」。挂钩点见
/// `BridgeResponseRouter.insertCommitted` 与 `InputController.handle`。
public final class SmartPunctDigitTracker {

    /// 光标前一字符（仅 ASCII 数字；0 = 不可用/不是数字）。
    public private(set) var prevChar: UInt16 = 0

    public init() {}

    /// 焦点切换等「文档换了、我们的记账全部作废」的时刻调用。
    public func reset() {
        prevChar = 0
    }

    /// 这一键**产出的数字字符**（0 = 不产出数字）。
    ///
    /// 「哪些键产出数字」这个判据只能落在客户端：那些键透传出去了，服务端根本看不到。
    ///
    /// ⛔ 反过来，「哪些**标点键**该带上这个值」不是这里的事，别写标点白名单——那等于
    /// 把服务端的 `input.punct.smart_list` 抄一份，两处必然漂移。所有按键如实带值，
    /// 要不要用由 `is_smart_punct_after_digit` 决定。
    ///
    /// ★ 主键盘（0x30-0x39）与小键盘（VK_NUMPAD0-9 = 0x60-0x69）必须都认。只写主键盘
    /// 的话，小键盘数字不但记不上，还会落进「非数字」分支把已记的值清零——症状是
    /// 「小键盘打的数字后面标点仍出中文」。macOS 没有 NumLock，小键盘恒出数字。
    public static func digitChar(vk: UInt32, modifiers: UInt32) -> UInt16 {
        let modShift: UInt32 = 0x0001
        // ⌘/⌃/⌥ + 数字是**宿主快捷键**（⌘2 切标签页、⌃3 切桌面…），一个字符都不往文档写。
        // 记了就是个幻影数字，随后打标点错出半角——这比漏记更糟：漏记只是少触发一次。
        //
        // ⚠️ Windows 的 `_DigitCharFromVk` 只挡 Shift，**不能照抄**：TSF 的 `OnTestKeyDown`
        // 在记录点之前就把这类键滤掉了，而 IMKit 把每个 keyDown 都交到我们手上
        // （`isHostShortcut` 那条路照样问服务、未命中热键时回 PassThrough），这道守卫
        // 只有 macOS 够得着、也只有 macOS 需要。
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
    /// 产出数字则记，否则清零——「没记到」与「明确不是数字」统一成一个出口。退格、
    /// 方向键、字母键都走后者，故「删掉数字再打标点回落中文标点」是这一条的自然结果，
    /// 不必为退格写特判。
    public func noteKeyPassthrough(vk: UInt32, modifiers: UInt32) {
        prevChar = Self.digitChar(vk: vk, modifiers: modifiers)
    }

    /// 经引擎上屏的文本（`insertText` 真写进文档的那一份）。
    ///
    /// 记的是末位字符，与 prevChar 语义严格一致：ASCII 数字则记，否则清零（上屏汉字/
    /// 标点后不该再继承数字状态）。全角数字（U+FF10-FF19）刻意不记——服务端只认
    /// ASCII，记了也不会命中，徒增误判面。
    ///
    /// 空文本不动状态：没有东西写进文档，光标前是什么并没有改变。
    public func noteCommittedText(_ text: String) {
        guard let last = text.unicodeScalars.last else { return }
        prevChar = (last.value >= 0x30 && last.value <= 0x39) ? UInt16(last.value) : 0
    }
}
