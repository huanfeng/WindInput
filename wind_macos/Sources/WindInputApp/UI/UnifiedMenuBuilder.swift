import Cocoa
import WindInputKit

// UnifiedMenuBuilder — 把 Go 下发的统一菜单树 ([MenuItemData]) 构建为原生 NSMenu。
//
// 三处菜单共用同一构建逻辑, 保证「输入方案 / 检索范围 / 全半角 / 中文标点 / 简入繁出 /
// 主题 / 设置…」结构、勾选、行为完全一致:
//   1. 候选框空白处右键菜单 (CandidatePanel)        —— 进程内 NSMenu
//   2. 菜单栏状态指示器下拉菜单 (ModeStatusController) —— 进程内 NSMenu
//   3. 系统输入源菜单 (InputController.menu)          —— IMK 经 doCommandBySelector 路由
//
// 两种点击派发方式 (见 Dispatch):
//   - inProcess: 普通 AppKit 菜单, builder 自身作 target, 点击直接回调 onAction(id)。
//     菜单 id 存进 NSMenuItem.tag, builder 读 tag 回调。
//   - imkCommand: 系统输入菜单由 IMK 在另一上下文绘制, 选中后经 doCommandBySelector
//     回到输入法进程; item.target 必须为 nil (走 IMK 命令路由), action 为 controller 上
//     实现的 selector。菜单 id 同样存进 NSMenuItem.tag —— tag 是基础整型属性, 跨 IMK
//     边界可靠 (representedObject 跨进程不保留)。
//
// 生命周期 (inProcess): NSMenuItem.target 不强引用, 调用方必须持有 builder 实例。
final class UnifiedMenuBuilder: NSObject {
    /// 叶子项点击的派发方式。
    enum Dispatch {
        /// 进程内菜单 (NSStatusItem / 候选框右键): builder 直接处理点击, 回调 onAction(id)。
        case inProcess((Int) -> Void)
        /// 系统输入菜单 (IMKInputController.menu): item.target=nil, action 为指定 selector
        /// (在 controller 上实现), 由 IMK 经 doCommandBySelector 路由; id 经 item.tag 回传。
        case imkCommand(action: Selector)
    }

    private var onAction: ((Int) -> Void)?

    /// 用菜单树构建一个全新 NSMenu。
    func build(_ items: [MenuItemData], dispatch: Dispatch) -> NSMenu {
        let menu = NSMenu()
        populate(menu, with: items, dispatch: dispatch)
        return menu
    }

    /// 把菜单树的顶层项追加进已存在的 menu (供菜单栏菜单在 header 之后填充统一项)。
    func populate(_ menu: NSMenu, with items: [MenuItemData], dispatch: Dispatch) {
        if case let .inProcess(cb) = dispatch { self.onAction = cb }
        menu.autoenablesItems = false // 用 Go 下发的 disabled 位, 不让 AppKit 自动判定
        for it in items { menu.addItem(makeItem(it, dispatch: dispatch)) }
    }

    /// 递归构建单个菜单项 (含子菜单)。
    private func makeItem(_ it: MenuItemData, dispatch: Dispatch) -> NSMenuItem {
        if it.separator { return .separator() }
        let item = NSMenuItem(title: it.label, action: nil, keyEquivalent: "")
        item.state = it.checked ? .on : .off
        item.tag = Int(it.id) // 菜单 id 载体 (两种派发都靠它回传)
        if !it.children.isEmpty {
            let sub = NSMenu()
            sub.autoenablesItems = false
            for c in it.children { sub.addItem(makeItem(c, dispatch: dispatch)) }
            item.submenu = sub
            item.isEnabled = true
        } else {
            item.isEnabled = !it.disabled
            switch dispatch {
            case .inProcess:
                item.target = self
                item.action = #selector(menuAction(_:))
            case let .imkCommand(action):
                item.target = nil // IMK 经 doCommandBySelector 路由到 controller
                item.action = action
            }
        }
        return item
    }

    @objc private func menuAction(_ sender: NSMenuItem) {
        onAction?(sender.tag)
    }
}
