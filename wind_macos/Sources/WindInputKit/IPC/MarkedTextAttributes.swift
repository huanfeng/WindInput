import AppKit

// MarkedTextAttributes — 组合串 (marked text) 属性字典的收口。
//
// # 这不是「装饰」，是让 selectionRange 能活着到达宿主的前提
//
// IMKit 的 `setMarkedText(_:selectionRange:replacementRange:)` 若收到**纯字符串**，
// 会认定「输入法没提供分句信息」，于是替我们合成一份默认分句 —— 整个组合串当作唯一一个
// 未转换分句 —— 并把转发给宿主的 `selectedRange` **覆写**成那个分句的范围 `{0, 全长}`。
// 我们精心算出的 `{caret, 0}` 在这一步被整个丢弃。
//
// 现象与判据 (2026-09-02 用 `wind-ui-rust` 的 `WINDUI_IME=1` 诊断抓到):
// ```text
// [windui-ime] marked="sf"  utf16_len=2 selected={0,2} → caret=0
// [windui-ime] marked="sfg" utf16_len=3 selected={0,3} → caret=0
// ```
// `location` 恒 0、`length` 恒等于全长 —— 而本仓**唯一**传 selectionRange 的调用点
// (`BridgeResponseRouter.applyMarkedText`) 传的 `length` 永远是 0。两者之间只隔着
// IMKit 一层。
//
// 后果不止于自绘宿主: 严格按 `selectedRange` 画组合内插入符的应用 (Chrome/Electron 系、
// 各类终端、Qt 应用、以及我们自己的 wind-ui-rust) 都会把光标画在组合串**最前面**。
// `NSTextView` 系 (TextEdit / Safari 原生输入框) 看着正常，是因为它按「整段是活动分句」
// 处理、光标由自己的排版器决定 —— 它根本没用我们给的那个数，属于被宽容宿主兜住。
//
// 解法就是本文件: 带上 `markedClauseSegment`，声明「分句信息由输入法提供」，IMKit 便不再
// 插手，`selectionRange` 原样透传。Rime/鼠须管等能正常工作的输入法走的都是这条路。
public enum MarkedTextAttributes {

    /// 补齐组合串属性字典中**必不可少**的两项，保留 `base` 已有的取值。
    ///
    /// `base` 通常来自 `IMKInputController.markForStyle(_:at:)`（它会按系统主题给出
    /// 合适的下划线样式与颜色）。但那是个 Objective-C API，返回什么由系统版本决定，
    /// **不能假定它一定含 `markedClauseSegment`** —— 少了它就退回上面描述的坏路径，
    /// 且只有真机才看得出来。故此处一律兜底。
    ///
    /// `controller` 拿不到时 (`base == nil`) 也能独立工作：两项都由本函数给出。
    public static func ensureClauseSegment(
        _ base: [NSAttributedString.Key: Any]? = nil
    ) -> [NSAttributedString.Key: Any] {
        var attrs = base ?? [:]
        // ★ 关键项。值是分句序号，我们整串只有一节，恒 0。
        if attrs[.markedClauseSegment] == nil {
            attrs[.markedClauseSegment] = 0
        }
        // 下划线由宿主按属性画 (NSTextView 系)。自绘宿主 (wind-ui-rust) 不读属性、
        // 自己画，故这一项对它是无害冗余。缺了它在 NSTextView 里组合串会毫无标记，
        // 与正文混为一谈。
        if attrs[.underlineStyle] == nil {
            attrs[.underlineStyle] = NSUnderlineStyle.single.rawValue
        }
        return attrs
    }

    /// 是否已声明分句信息 —— 即 IMKit 会不会放过 `selectionRange`。守门测试用。
    public static func declaresClauseSegment(_ attrs: [NSAttributedString.Key: Any]) -> Bool {
        return attrs[.markedClauseSegment] != nil
    }
}
