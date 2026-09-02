import Foundation

// CompositionState — IME 端 composition (marked text + cursor) 跟踪.
//
// 用途:
//   - Go 端 CmdUpdateComposition 推 "当前 preedit 文本 + 光标位置", IME 端记下
//     最新一次的状态, 调 client.setMarkedText(...) 写入文本框.
//   - Go 端 CmdCommitText 推 commit, IME 端调 client.insertText(...) 并清状态.
//   - Go 端 CmdClearComposition 推清 preedit, IME 端 setMarkedText("") 并清状态.
//
// 这里的 state 主要用于:
//   1. 重复推送同样内容时短路 (避免无谓的 setMarkedText 重画)
//   2. 持有组合区光标, 供 setMarkedText 的 selectionRange 使用
//   3. 给上层调试/快照
public struct CompositionState: Equatable {
    /// 当前显示在文本框的 marked text. 空字符串表示无 preedit.
    public var text: String

    /// 光标在 text 里的位置 — 按 **UTF-16 code unit** 计。
    ///
    /// # 单位由两端共同决定, 不是随便挑的
    ///
    /// - **上游**: 服务端 `UpdateComposition.caret_pos` 出自 `preedit_cursor::caret_utf16`,
    ///   注释写明「TSF 要 UTF-16 偏移」——发过来的就是 UTF-16 单元数。
    /// - **下游**: IMKit `setMarkedText(_:selectionRange:replacementRange:)` 的
    ///   `NSRange` 亦以 UTF-16 单元计。
    ///
    /// 两端同为 UTF-16, 中间**不该有任何换算**。本字段一度叫 `caretRune` 并按 rune
    /// (code point) 语义再折算一次, 对 BMP 内的汉字与 ASCII 恰好等值, 故**用中文怎么测
    /// 都测不出来**; 只有组合区含扩展 B 区生僻字 (surrogate pair, 占 2 个单元) 且光标
    /// 不在串尾时才偏移——例 `"𠮷zh"` 光标在「𠮷」后, 服务端给 2, 旧算法当成「前 2 个
    /// 字符」得出 3。改名即为让这类误用在编译期就现形。
    public var caretUTF16: Int

    public init(text: String = "", caretUTF16: Int = 0) {
        self.text = text
        self.caretUTF16 = caretUTF16
    }

    public var isEmpty: Bool { text.isEmpty }

    public mutating func clear() {
        text = ""
        caretUTF16 = 0
    }

    /// 供 `setMarkedText` 用的 `NSRange.location`: 就是 `caretUTF16`, 只做区间钳位。
    ///
    /// 钳位不可省: 服务端与本端对组合串的认知有一拍延迟 (待定标点并入前缀、宿主拒收
    /// 部分内容), caret 越界会让 IMKit 抛 range 异常。越界一律退到**串尾**而非 0 ——
    /// 组合期的编辑点绝大多数时候就在末尾, 退到 0 会让光标停在刚打出的字母之前。
    public func caretInUTF16() -> Int {
        return max(0, min(caretUTF16, utf16Length))
    }

    /// 全文 UTF-16 长度 (IMKit setMarkedText 的 selectionRange 上界等)
    public var utf16Length: Int { text.utf16.count }
}
