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
//   2. 把 caretPos (Go 给的 rune 偏移) 映射成 NSRange (UTF-16 unit 偏移),
//      因为 IMKit `client.setMarkedText(_, selectionRange:, replacementRange:)`
//      的 selectionRange 是 NSRange (UTF-16 单位)
//   3. 给上层调试/快照
public struct CompositionState: Equatable {
    /// 当前显示在文本框的 marked text. 空字符串表示无 preedit.
    public var text: String

    /// 光标在 text 里的位置 — 按 rune (UTF-32 code point) 计, 与 Go 端协议一致.
    public var caretRune: Int

    public init(text: String = "", caretRune: Int = 0) {
        self.text = text
        self.caretRune = caretRune
    }

    public var isEmpty: Bool { text.isEmpty }

    public mutating func clear() {
        text = ""
        caretRune = 0
    }

    /// 把 caretRune (rune 偏移) 转换为 NSRange location (UTF-16 unit 偏移).
    /// Swift String 用 UTF-8 存储但 IMKit API 用 UTF-16, NSRange 是 UTF-16 单位.
    /// 例: text = "你好", caretRune=2 (在末尾), NSRange location = 2 (因为 "你好"
    /// 共 2 个 UTF-16 unit; 但 emoji surrogate pair 占 2 个 unit, 一个 rune 占
    /// 2 unit, 必须精确转换不能直接拿 caretRune).
    public func caretInUTF16() -> Int {
        guard caretRune > 0 else { return 0 }
        // 限制 caretRune 不能越过 text 长度 (rune 计)
        let chars = Array(text)
        let bounded = max(0, min(caretRune, chars.count))
        let prefix = String(chars.prefix(bounded))
        return prefix.utf16.count
    }

    /// 全文 UTF-16 长度 (IMKit setMarkedText 的 selectionRange 上界等)
    public var utf16Length: Int { text.utf16.count }
}
