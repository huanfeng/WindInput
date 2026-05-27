import Foundation

// TextInputClient — IMKTextInput 的最小化抽象, 仅暴露我们 commit/composition 路径
// 实际需要的几个方法. 目的:
//   1. 把 InputMethodKit 的 IMKTextInput Objective-C 协议从 applyResponse 逻辑解耦,
//      让纯 Swift 单测能 mock (IMKTextInput 是 @objc protocol, 实际客户端是 NSApp
//      内部 _NSCFCharacterSet 之类的内部类型, 测试里没法直接构造)
//   2. 将来 caret 坐标、selectedRange 等扩展能集中加在这个协议
//
// 适配规则:
//   - IMKInputController.handle 把 client (sender: Any) 转成 IMKTextInput, 再调
//     IMKTextInputAdapter(wrapping:) 包成 TextInputClient
//   - 单测里用 InMemoryTextInputClient 实现协议, 记录调用历史
public protocol TextInputClient: AnyObject {
    /// 替换 marked text 或在当前光标插入新文本 (commit).
    /// replacementRange = NSRange(NSNotFound, NSNotFound) 表示"替换当前 marked
    /// text" (IMKit 标准用法).
    func insertText(_ text: String, replacementRange: NSRange)

    /// 设置 preedit. text 为空字符串等于清除 preedit.
    /// selectionRange 是 text 内的光标位置 (UTF-16 unit).
    /// replacementRange 同 insertText.
    func setMarkedText(_ text: String, selectionRange: NSRange, replacementRange: NSRange)
}
