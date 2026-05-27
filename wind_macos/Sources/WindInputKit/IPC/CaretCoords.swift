import Foundation
import CoreGraphics

// CaretCoords — Cocoa bottom-left ↔ wire 协议 top-left 坐标转换工具.
//
// macOS 坐标系:
//   - NSScreen / NSWindow / NSView 都是 bottom-left 原点 (Y 向上为正)
//   - IMKTextInput.attributes(forCharacterIndex:lineHeightRectangle:) 返回的
//     NSRect 也是 bottom-left 屏幕坐标, origin 在 rect 的左下角
//
// Wire 协议 (CmdCaretUpdate / Win 端 / Go 端):
//   - top-left 原点 (Y 向下为正), 与 Win32 一致
//   - x,y 表示 caret 的左上角
//
// 转换公式 (单显示器):
//   topLeftY = screen.height - (bottomLeftY + rect.height)
//
// 多显示器: 主屏左下角是全局 (0,0), 副屏向上/右扩展, 副屏可能 Y 为负值. 通用做
// 法是用 caret 所在 NSScreen 的 frame 来做相对转换, 简化版用主屏即可
// (M2.2 阶段不重支持). 详细处理留 M3 候选框集成时按需扩展.
public enum CaretCoords {

    /// 把 IMKit 返回的 caret rect (bottom-left, 屏幕坐标) 转换为 top-left wire 坐标.
    /// `screenHeight` 是 caret 所在屏幕的高度; 简化版用主屏高度即可.
    ///
    /// 返回 (x, y, height) — 都已经是 Int32, 直接喂给 encodeCaretUpdateFrame.
    public static func caretRectToWire(_ rect: CGRect,
                                       screenHeight: CGFloat) -> (x: Int32, y: Int32, height: Int32) {
        let topLeftY = screenHeight - (rect.origin.y + rect.size.height)
        return (
            x: Int32(rect.origin.x.rounded()),
            y: Int32(topLeftY.rounded()),
            height: Int32(rect.size.height.rounded())
        )
    }
}
