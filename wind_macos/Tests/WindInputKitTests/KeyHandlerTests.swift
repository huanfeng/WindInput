import XCTest
@testable import WindInputKit

/// NSEvent.keyCode → Win VK 映射。只钉住那些「错了不会报错、只会静默丢功能」的约定。
final class KeyHandlerTests: XCTestCase {

    /// 小键盘必须映射成 VK_NUMPAD*（0x60..0x69），**不能**就地折算成主键盘数字。
    ///
    /// 「数字小键盘功能」(`input.numpad_behavior`) 的归一化在服务端 `numpad_to_main` 做，
    /// 那个函数只认 0x60..0x69。在这里提前折算成主键盘数字，等于把该开关架空成恒
    /// `follow_main`——用户选「直接输入数字」也不会有任何反应。
    func testKeypadDigitsMapToNumpadVK() {
        let pairs: [(UInt16, UInt32)] = [
            (0x52, 0x60), (0x53, 0x61), (0x54, 0x62), (0x55, 0x63), (0x56, 0x64),
            (0x57, 0x65), (0x58, 0x66), (0x59, 0x67), (0x5B, 0x68), (0x5C, 0x69),
        ]
        for (mac, vk) in pairs {
            XCTAssertEqual(KeyHandler.toWindowsVK(mac), vk,
                           "kVK 0x\(String(mac, radix: 16)) 应映射为 VK_NUMPAD")
        }
    }

    /// 0x5A 是 F20 而不是小键盘键 —— 小键盘 8/9 的 keyCode 在 0x59 之后跳了一格。
    /// 顺序填表时极易把它当成 Keypad8 而整体错位一位。
    func testF20IsNotAKeypadKey() {
        XCTAssertEqual(KeyHandler.toWindowsVK(0x5A), 0, "0x5A=F20，不该被当成小键盘键")
    }

    /// 只有 F1（注释切换键出厂值）上报；其余 F 键保持透传，不改变它们在宿主里的原义。
    func testOnlyF1AmongFunctionKeysIsMapped() {
        XCTAssertEqual(KeyHandler.toWindowsVK(0x7A), 0x70) // F1
        XCTAssertEqual(KeyHandler.toWindowsVK(0x78), 0)    // F2
        XCTAssertEqual(KeyHandler.toWindowsVK(0x60), 0)    // F5
    }

    func testKeypadOperatorsMapToNumpadOperators() {
        XCTAssertEqual(KeyHandler.toWindowsVK(0x43), 0x6A) // *
        XCTAssertEqual(KeyHandler.toWindowsVK(0x45), 0x6B) // +
        XCTAssertEqual(KeyHandler.toWindowsVK(0x4E), 0x6D) // -
        XCTAssertEqual(KeyHandler.toWindowsVK(0x41), 0x6E) // .
        XCTAssertEqual(KeyHandler.toWindowsVK(0x4B), 0x6F) // /
        XCTAssertEqual(KeyHandler.toWindowsVK(0x4C), 0x0D) // 小键盘 Enter = VK_RETURN
    }

    /// 无对位语义的两个键保持不映射（VK=0 → 不上报 → IMKit 透传），
    /// 好过映射到一个语义不同的 Windows 键上。
    func testKeypadKeysWithoutWindowsCounterpartStayUnmapped() {
        XCTAssertEqual(KeyHandler.toWindowsVK(0x47), 0, "KeypadClear：macOS 无 NumLock 语义")
        XCTAssertEqual(KeyHandler.toWindowsVK(0x51), 0, "KeypadEquals：Windows 小键盘无此键")
    }

    /// 主键盘数字不受影响（回归：补小键盘时把顶排数字覆盖掉过）。
    func testTopRowDigitsUnchanged() {
        XCTAssertEqual(KeyHandler.toWindowsVK(0x1D), 0x30) // 0
        XCTAssertEqual(KeyHandler.toWindowsVK(0x12), 0x31) // 1
        XCTAssertEqual(KeyHandler.toWindowsVK(0x19), 0x39) // 9
    }

    // MARK: - 宿主快捷键判据 (issue #64)

    /// ⌘/⌃/⌥ 组合归宿主：macOS 上 ⌘C/⌘V 是复制粘贴，输入法不能把这一键吞掉。
    func testIsHostShortcut_ModifierKeys() {
        XCTAssertTrue(KeyHandler.isHostShortcut(.command))
        XCTAssertTrue(KeyHandler.isHostShortcut(.control))
        XCTAssertTrue(KeyHandler.isHostShortcut(.option))
        XCTAssertTrue(KeyHandler.isHostShortcut([.command, .shift]))
    }

    /// Shift / CapsLock 是正经输入修饰，不算快捷键组合。
    func testIsHostShortcut_ShiftIsNotAShortcut() {
        XCTAssertFalse(KeyHandler.isHostShortcut(.shift))
        XCTAssertFalse(KeyHandler.isHostShortcut(.capsLock))
        XCTAssertFalse(KeyHandler.isHostShortcut([]))
    }

    /// Command 编成 Win 位（0x0008）——协调器的 `MOD_SHORTCUT` 靠这一位认出 ⌘。
    func testToModifiers_CommandMapsToWinBit() {
        XCTAssertEqual(KeyHandler.toModifiers(.command), 0x0008)
        XCTAssertEqual(KeyHandler.toModifiers([.command, .shift]), 0x0009)
    }
}
