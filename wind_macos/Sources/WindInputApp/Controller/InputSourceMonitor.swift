import Carbon
import Cocoa
import os
import WindInputKit

/// 切入判定的诊断日志。NSLog 的内容在统一日志里会被显示成 <private>，这里用 public 才查得了问题。
let switchInLog = Logger(subsystem: "to.feng.inputmethod.WindInput", category: "switch-in")

/// 监听系统输入源切换，维护「刚切入本输入法」标记（`SwitchInGate`）。
///
/// 只在真正切换输入源时才让服务端按 `input.default` 重置中英等状态，见 `SwitchInGate` 的说明。
/// 通知经 DistributedNotificationCenter 投到主队列，与 `activateServer` 同线程，标记无需加锁。
final class InputSourceMonitor {
    static let shared = InputSourceMonitor()

    private var gate = SwitchInGate()
    private var observer: NSObjectProtocol?
    /// 本变体的输入源 ID 前缀（release 与 Debug 变体各自只认自己，同 `main.swift` 的做法）。
    private let selfPrefix = "\(Bundle.main.bundleIdentifier ?? "to.feng.inputmethod.WindInput")."

    private init() {}

    func start() {
        guard observer == nil else { return }
        observer = DistributedNotificationCenter.default().addObserver(
            forName: Notification.Name(rawValue: kTISNotifySelectedKeyboardInputSourceChanged as String),
            object: nil, queue: .main
        ) { [weak self] _ in
            self?.selectedSourceChanged()
        }
        switchInLog.info("monitor started prefix=\(self.selfPrefix, privacy: .public)")
    }

    /// 取走「刚切入」标记（`activateServer` 调）。
    func consumeSwitchIn() -> Bool {
        gate.consume()
    }

    private func selectedSourceChanged() {
        let id = Self.currentSourceID()
        let isSelf = id.hasPrefix(selfPrefix)
        switchInLog.info("selected=\(id, privacy: .public) isSelf=\(isSelf, privacy: .public)")
        gate.selected(isSelf: isSelf)
        // 通知晚于 activateServer 到的情形：已有活动控制器，当场消费并补发。
        if isSelf { InputController.inputSourceSwitchedIn() }
    }

    private static func currentSourceID() -> String {
        guard let src = TISCopyCurrentKeyboardInputSource()?.takeRetainedValue(),
              let p = TISGetInputSourceProperty(src, kTISPropertyInputSourceID)
        else { return "" }
        return Unmanaged<CFString>.fromOpaque(p).takeUnretainedValue() as String
    }
}
