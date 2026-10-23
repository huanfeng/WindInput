// X11 keysym / 修饰键状态 → Windows VK + 协议修饰位（纯逻辑，不依赖 Fcitx5 头文件）。
//
// 服务端把按键一律当 Windows VK 处理（VK 常量 SSOT：wind-keys/src/keymap.rs）。
// macOS 侧按**物理键码**映射（NSEvent.keyCode，见 KeyHandler.swift）；Linux 这边按
// **keysym** 映射——Fcitx5 的 `Key::sym()` 已经是 XKB 按当前布局解出的键值，与 Windows
// VK「随布局走」的语义一致。代价是 Shift 过的符号键（`!` `@` `{` …）要反查它所在的
// 物理键，这张反查表按 US 布局写（与 Windows 的 VK_OEM_* 同样以 US 布局命名）。
//
// 修饰键状态位取 Fcitx5 `KeyState` 的数值，它们与 X11 的修饰掩码逐位相同，故在这里
// 按 X11 掩码写死，避免纯逻辑层依赖 Fcitx5 头文件：
//   Shift=1<<0  CapsLock=1<<1  Ctrl=1<<2  Alt(Mod1)=1<<3  NumLock(Mod2)=1<<4
//   Super(Mod4)=1<<6  Super2=1<<26  Meta=1<<28
#pragma once

#include <cstdint>

namespace windlinux {

namespace xstate {
constexpr uint32_t Shift = 1u << 0;
constexpr uint32_t CapsLock = 1u << 1;
constexpr uint32_t Ctrl = 1u << 2;
constexpr uint32_t Alt = 1u << 3;
constexpr uint32_t NumLock = 1u << 4;
constexpr uint32_t Super = 1u << 6;
constexpr uint32_t Super2 = 1u << 26;
constexpr uint32_t Meta = 1u << 28;
} // namespace xstate

/// keysym → Windows VK；未覆盖的键返回 0（调用方透传给宿主，不上报服务）。
uint32_t keysymToVK(uint32_t keysym);

/// 修饰键状态 → 协议 KEYMOD_* 通用位（Shift/Ctrl/Alt/Win）。与 macOS 一样只报通用位、
/// 不报左右专用位（KEYMOD_LSHIFT…）：服务端的热键哈希按通用位算。
uint32_t statesToModifiers(uint32_t states);

/// 修饰键状态 → 协议 toggles（TOGGLE_CAPSLOCK / TOGGLE_NUMLOCK）。
uint8_t statesToToggles(uint32_t states);

/// 这一键是否带**宿主快捷键修饰键**（Ctrl / Alt / Super）。服务端回「清组合」时，这类键
/// 仍要交还宿主（组字中按 Ctrl+C 不该被吞）——对位 Swift `KeyHandler.isHostShortcut`。
/// Shift 不算：Shift+字母是大写、Shift+标点是另一个标点，都是正经输入。
bool isHostShortcut(uint32_t states);

/// 可作「单击切换」的修饰键（左右 Shift / Ctrl）→ VK_LSHIFT 等；其它键返回 0。
uint32_t toggleModifierVK(uint32_t keysym);

/// 修饰键单击（tap）检测：按下 → 其间无别的键 → 抬起且按住不超过阈值 = 单击。
///
/// 对位 Windows `KeyEventSink` 的 `TOGGLE_TAP_THRESHOLD_MS`（500ms，长按不切）与 macOS
/// `InputController.handleFlagsChanged`。服务端只在 **keyup** 上处理切换键（TSF 惯例：
/// 吃掉 keydown 不转发、干净单击才在 keyup 转发），故判定为单击时由调用方发一帧
/// eventType=UP 的 KeyEvent。
///
/// 未覆盖：Windows 另有「Shift+鼠标拖选不算单击」（ToggleTapPolicy.h 的四个鼠标信号），
/// Fcitx5 引擎收不到鼠标事件，这一条在 Linux 上缺失。
class ToggleTapDetector {
public:
    static constexpr uint64_t kThresholdMs = 500;

    /// 任意按键按下。返回值无意义；内部记录/作废待判定的修饰键。
    void onPress(uint32_t keysym, uint64_t nowMs);

    /// 任意按键抬起。若构成一次干净单击，返回该修饰键的 VK，否则 0。
    uint32_t onRelease(uint32_t keysym, uint64_t nowMs);

    /// 焦点切换等场合作废待判定状态。
    void reset() { pendingVK_ = 0; }

private:
    uint32_t pendingVK_ = 0;
    uint64_t pressedAtMs_ = 0;
};

/// 候选显示时 Ctrl 如实上报（`KEY_EVENT_MOD_DOWN` / `KEY_EVENT_MOD_UP`）的一次按住记账，对位 TSF
/// `CtrlRawPolicy.h`。协议权威在协调器 `handle_ctrl_raw`：
///
/// - **按下那一刻**组字中（`altInputSession`）才走这条：按下报 MOD_DOWN、松开报 MOD_UP（哪怕那时
///   已不在组字），调用方不再按 ToggleTapDetector 的单击上报；否则这次按住整个走旧路。一次按住
///   只走一条路，服务端不会两边各收到一次。
/// - 按住期间按过别的键（服务端未必看得见，如透传的 Ctrl+C）→ 松开带 `TOGGLE_MOD_NOT_TAP`。
///   时长由服务端判。键本身两向都交还宿主。
class CtrlRaw {
public:
    /// 任意按键按下。返回 VK_LCONTROL / VK_RCONTROL = 要报 MOD_DOWN；0 = 不报（别的键、自动重复、旧路）。
    uint32_t onPress(uint32_t keysym, bool composing);
    /// 任意按键抬起。返回 VK = 要报 MOD_UP（`*notTap` 给出是否带 NOT_TAP）；0 = 不是如实上报的那次按住。
    uint32_t onRelease(uint32_t keysym, bool* notTap);
    /// 焦点切换：作废（松开多半送不到，服务端失焦时也会收起按住翻转）。
    void reset() { *this = CtrlRaw{}; }

private:
    uint32_t vk_ = 0;       // 如实上报中的 Ctrl
    bool dirty_ = false;
    uint32_t legacyVK_ = 0; // 走旧路按住中的 Ctrl：松开前的自动重复不改路
};

/// Alt_L / Alt_R → VK_LMENU / VK_RMENU；其它键返回 0。
uint32_t altVK(uint32_t keysym);

/// Alt 转发判据里的「组字中」：有组合，或候选窗开着（联想窗组合为空、候选仍在，也能切注释）。
inline bool altInputSession(bool hasComposition, bool candidatesVisible)
{
    return hasComposition || candidatesVisible;
}

/// 组字中 Alt 转发给服务（切换注释态）的一次按住记账。协议权威在协调器 `handle_comment_alt`：
/// 只在组字中转发 Alt 的按下与松开（0xA4 / 0xA5），修饰位带上这次按住期间按过的
/// Shift / Ctrl / Super（粘滞，按过就一直带到松开）；吃不吃看回包。
class AltHold {
public:
    /// Alt 按下。已在按住中（自动重复）则保留粘滞位。
    void onAltDown(bool forwarded);
    /// 任意按键按下（含修饰键本身）：按住期间记粘滞位。
    void noteKey(uint32_t keysym, uint32_t states);
    uint32_t stickyMods() const { return sticky_; }
    /// 松开是否转发：组字中，或按下转发过（按住翻转要靠松开复原，哪怕期间已上屏）。
    bool shouldForwardUp(bool composing) const { return composing || (active_ && forwarded_); }
    /// Alt 松开 / 焦点切换：复位。
    void reset() { *this = AltHold{}; }

private:
    bool active_ = false;
    bool forwarded_ = false;
    uint32_t sticky_ = 0;
};

} // namespace windlinux
