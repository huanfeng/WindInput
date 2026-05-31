import Cocoa
import WindInputKit

// UnifiedMenuBuilder — 把 Go 下发的统一菜单树 ([MenuItemData]) 构建为原生 NSMenu。
//
// 候选框空白处右键菜单 (CandidatePanel) 与菜单栏状态菜单 (ModeStatusController) 共用,
// 保证两处「输入方案 / 检索范围 / 全半角 / 中文标点 / 简入繁出 / 主题 / 设置…」菜单
// 的结构、勾选状态与点击行为完全一致 (单一构建逻辑, 单一来源)。
//
// 生命周期: NSMenuItem.target 不强引用, 调用方必须持有 builder 实例 (作为叶子项 target),
// 否则菜单打开后点击无响应。CandidateContentView / ModeStatusController 各持一个。
final class UnifiedMenuBuilder: NSObject {
    private var onAction: ((Int) -> Void)?

    /// 用菜单树构建一个全新 NSMenu; 叶子项点击回调 onAction(id)。
    func build(_ items: [MenuItemData], onAction: @escaping (Int) -> Void) -> NSMenu {
        let menu = NSMenu()
        populate(menu, with: items, onAction: onAction)
        return menu
    }

    /// 把菜单树的顶层项追加进已存在的 menu (供菜单栏菜单在 header 之后填充统一项)。
    func populate(_ menu: NSMenu, with items: [MenuItemData], onAction: @escaping (Int) -> Void) {
        self.onAction = onAction
        menu.autoenablesItems = false // 用 Go 下发的 disabled 位, 不让 AppKit 自动判定
        for it in items { menu.addItem(makeItem(it)) }
    }

    /// 递归构建单个菜单项 (含子菜单)。
    private func makeItem(_ it: MenuItemData) -> NSMenuItem {
        if it.separator { return .separator() }
        let item = NSMenuItem(title: it.label, action: nil, keyEquivalent: "")
        item.state = it.checked ? .on : .off
        if !it.children.isEmpty {
            let sub = NSMenu()
            sub.autoenablesItems = false
            for c in it.children { sub.addItem(makeItem(c)) }
            item.submenu = sub
            item.isEnabled = true
        } else {
            item.target = self
            item.action = #selector(menuAction(_:))
            item.representedObject = Int(it.id)
            item.isEnabled = !it.disabled
        }
        return item
    }

    @objc private func menuAction(_ sender: NSMenuItem) {
        if let id = sender.representedObject as? Int { onAction?(id) }
    }
}
