// Fcitx5 输入法引擎：清风输入法在 Linux 上的「薄壳」。
//
// 与 wind_tsf（Windows TSF DLL）、wind_macos（IMKit .app）对位：按键转发给 Rust 服务、
// 把服务的上屏/预编辑写回 InputContext、把服务光栅化好的候选窗帧显示出来。引擎/词库/
// 候选逻辑全在服务里，这里不做任何「中英判定」。
//
// 线程模型：Fcitx5 在主线程（事件循环）调 keyEvent/activate/deactivate，本类在其中**同步**
// 请求服务（UDS 往返 <1ms，2s 超时兜底）。push 通道在后台线程收帧，经 EventDispatcher
// 转回主线程再碰 InputContext——InputContext 与 InputPanel 都不是线程安全的。
#pragma once

#include "Bridge.h"
#include "KeyMap.h"
#include "KeySynth.h"
#include "ResponseRouter.h"
#include "ServiceLauncher.h"
#include "SettingsLauncher.h"
#include "ExtProtocol.h"
#include "HostUi.h"
#include "OverlayInput.h"
#include "ShmFrame.h"

#include <fcitx-utils/event.h>
#include <fcitx-utils/eventdispatcher.h>
#include <fcitx-utils/trackableobject.h>
#include <fcitx-config/configuration.h>
#include <fcitx-config/option.h>
#include <fcitx/addonfactory.h>
#include <fcitx/addoninstance.h>
#include <fcitx/action.h>
#include <fcitx/addonmanager.h>
#include <fcitx/inputcontext.h>
#include <fcitx/inputmethodengine.h>
#include <fcitx/instance.h>

#include <memory>
#include <unordered_set>

namespace windlinux {

class X11CandidatePanel;
class WaylandCandidatePanel;

/// 输入法 / addon 的「配置」：只有一项外部工具（设置程序）。Fcitx5 的配置工具（fcitx5-configtool
/// 5.1.6 起）遇到「整页只有一个 External 项」就直接启动它，不再弹一张空白配置页；更早的版本
/// 显示一页、页上一个按钮。fcitx5-mozc 用的是同一机制（它的 addon 里没有静态 .desc）。
FCITX_CONFIGURATION(WindConfig,
                    fcitx::ExternalOption settings{this, "WindSetting", "清风输入法设置",
                                                   settingsPath()};);

class WindEngine final : public fcitx::InputMethodEngineV2 {
public:
    explicit WindEngine(fcitx::Instance* instance);
    ~WindEngine() override;

    void keyEvent(const fcitx::InputMethodEntry& entry, fcitx::KeyEvent& event) override;
    void activate(const fcitx::InputMethodEntry& entry, fcitx::InputContextEvent& event) override;
    void deactivate(const fcitx::InputMethodEntry& entry, fcitx::InputContextEvent& event) override;
    void reset(const fcitx::InputMethodEntry& entry, fcitx::InputContextEvent& event) override;

    // 托盘 / 面板随中英模式换图标与标签（Fcitx5 的 classicui / notificationitem / kimpanel
    // 都经这两个取）。服务端的模式是全局的，与 IC 无关。
    std::string subMode(const fcitx::InputMethodEntry& entry, fcitx::InputContext& ic) override;
    std::string subModeIconImpl(const fcitx::InputMethodEntry& entry, fcitx::InputContext& ic) override;
    std::string subModeLabelImpl(const fcitx::InputMethodEntry& entry, fcitx::InputContext& ic) override;

    const fcitx::Configuration* getConfig() const override { return &config_; }

private:
    // ── 连接 ──
    /// 连不上服务时按节流拉起它（服务自己持单例锁，重复拉起无害）。
    ServiceLauncher launcher_;
    bool ensureConnected();
    bool reconnect();
    /// 在 request 连接上发一帧、读响应；对端确定没处理时重连并**重试一次**（服务重启后的第一
    /// 个键就自愈、不丢字）。读超时不重试（见 `BridgeClient::retryable`）。
    bool requestWithRetry(const Bytes& frame, Frame& resp);
    /// 发一帧、读掉 ack，失败只记日志。返回是否成功（菜单据此决定要不要本端收起）。
    bool sendAndDrain(const Bytes& frame);
    /// 发一帧不读响应（AsyncFlag 帧）。
    void sendAsync(const Bytes& frame);
    /// 请求失败后调用：刚发生过超时（服务卡死）就本端收起组字 / 候选窗 / 菜单，并记下待恢复
    /// 时对齐。每次卡死只做一次。
    void noteStall();
    /// 卡死后重新连上：复位服务端组字（它可能处理了积压的帧）并重报焦点。
    void resyncAfterStall();

    // ── 焦点 ──
    uint64_t clientToken(fcitx::InputContext* ic) const;
    uint64_t inputScopeMask(fcitx::InputContext* ic) const;
    void sendFocusGained(fcitx::InputContext* ic);
    /// 获焦上报全套：FocusGained（含 host.display）+ 光标 + 前台应用。
    void reportFocus(fcitx::InputContext* ic);
    /// `host.display`：光标坐标是否可用 + 界面缩放 + 所在输出（见 ExtProtocol.h）。
    void sendHostDisplay(fcitx::InputContext* ic);
    /// 焦点（或候选窗）所在输出的名字与工作区，随 `host.display` 上报；工具栏 / 软键盘的
    /// `pos.*` 用同一来源的名字（`reportPanelPos` 的 output），两处要么都报要么都不报。
    HostOutput hostOutput(fcitx::InputContext* ic);
    void sendCaretUpdate(fcitx::InputContext* ic);
    uint16_t prevCharFor(fcitx::InputContext* ic);
    /// 修饰键单击 / CapsLock 状态通知：发一帧 eventType=UP 的 KeyEvent 并应用其响应。
    /// `toggles` 必须是真实锁定态——服务端拿**每一帧**的 toggles 校准大写锁定镜像，
    /// 发 0 等于告诉它「大写锁定关了」。
    void sendKeyUp(fcitx::InputContext* ic, uint32_t vk, uint32_t mods, uint8_t toggles);
    /// 组字中 Alt 的按下 / 松开转发给服务（注释态切换）；返回服务是否吃掉。
    bool sendAltEvent(fcitx::InputContext* ic, uint32_t vk, uint8_t eventType, uint32_t mods, uint8_t toggles);
    bool applyResponse(fcitx::InputContext* ic, const Frame& resp, bool hostShortcut);
    /// 帧里带中英模式就记下；变了则让 Fcitx5 的 UI 模块重取图标。
    void noteMode(const Frame& frame);
    /// 托盘图标 / 标签变了：让 Fcitx5 的 UI 模块重取。
    void refreshModeIcon();

    // ── push ──
    void onPushFrame(Frame frame);
    void onRenderFrame(const HostRenderFramePayload& p);
    void onOverlayFrame(const OverlayFramePayload& p);
    void onExt(const ExtEnvelope& ext);
    void onMenuFrame(uint32_t level, const OverlayFramePayload& p);
    /// 启动设置程序（`settings.open` 信封与状态区入口共用）。只启动自己的设置程序，args 只进参数位。
    void launchSettings(const std::vector<std::string>& args);
    /// 把 Fcitx5 出厂当作「替代触发键」的 Shift 单击留给本引擎（实现处有原因）。
    void claimShiftTap(fcitx::KeyEvent& event);
    fcitx::InputContext* focusedIC();
    /// 两种候选窗都藏起来（哪一种正显示取决于上一帧的焦点 IC，藏的时候不必区分）。
    void hideCandidate();
    /// 收起 Wayland popup 里的全部层（候选 + 浮层）。
    void hideAllWayland();

    // ── 工具栏 / 软键盘（kind 10 / 11）──
    /// 两层都藏起来（服务重启：新服务按自己的状态重新推帧）。
    void hidePanels();
    /// 本端拖完工具栏 / 软键盘：报 `pos.toolbar` / `pos.softkeyboard`（内容盒左上的屏幕坐标）。
    /// `output` 是面板所在输出名，拿不到传空串（不带该字段）。
    void reportPanelPos(uint32_t kind, int32_t x, int32_t y, const std::string& output);
    /// 焦点窗口进入 / 退出全屏：`host.fullscreen`，只在值变化时发。
    void reportFullscreen(bool fullscreen);

    // ── 自绘菜单 ──
    /// 请服务端打开菜单（`menu.open`）：target ≥ 0 候选右键菜单，-1 主菜单；(x, y) 锚点。
    void requestMenu(int32_t target, int32_t x, int32_t y);
    /// 状态区「清风输入法菜单」：在指针处开主菜单。Wayland 会话开在屏幕坐标系（落点等指针进入
    /// 底板时取），X11 按指针的根窗口坐标。
    void openMainMenuAtPointer();

    // ── 命令直通车按键合成（CMD_KEY_TAP / SEQ / HOLD / RELEASE）──
    /// 经 `forwardKey` 交给焦点 IC 的应用：不进输入法引擎，不会回流成组字。
    void onKeySynth(const Frame& frame);
    void forwardKeys(fcitx::InputContext* ic, const std::vector<SynthKey>& keys);
    /// 卡键保护：key.hold 按住的全部抬起，发给当初收到按下的那个 IC（它还在的话）。
    /// 失焦 / 换 IC / reset / 服务重启或断线 / 析构都走这里。
    void releaseHeldKeys(const char* reason);
    /// 按最早的到期时刻（重）设最长保持时间的计时器；没有按住的就不动它（一次性，自然停）。
    void armKeyHoldExpiry();

    fcitx::Instance* instance_;
    BridgeClient bridge_;
    ResponseRouter router_;
    ToggleTapDetector tap_;
    AltHold altHold_;
    /// 候选窗是否开着（服务端推来可见帧时置位，hideCandidate 清）。Alt 转发判据要它：联想窗组合为空。
    bool candidatesVisible_ = false;
    CapsLockTracker caps_;
    uint16_t keySeq_ = 0;
    bool lastReportedSecure_ = false;
    bool serviceSeenOnce_ = false;
    /// 激活时连不上服务、焦点信息没报出去；连上时补报（见 reconnect）。
    bool focusUnreported_ = false;
    /// 发生过超时、还没在重连时对齐过（见 noteStall / resyncAfterStall）。
    bool stallPending_ = false;
    bool untrustedWarned_ = false;
    /// 按下时被本输入法吃掉的键（按硬件键码记）。松开时同样吃掉，免得宿主收到一个没有
    /// 按下的松开——多数宿主无所谓，但有的会据此触发快捷键。
    std::unordered_set<int> eatenKeys_;

    fcitx::TrackableObjectReference<fcitx::InputContext> currentIC_;
    fcitx::EventDispatcher dispatcher_;
    std::unique_ptr<fcitx::EventSourceTime> holdTimer_;
    std::unique_ptr<PushClient> push_;
    ShmFrameReader shm_;
    /// 光栅浮层各层的 SHM 读端（下标 = kind - 1）。与 `shm_` 同样在 SERVICE_READY 时关掉重开。
    ShmFrameReader overlayShm_[3];
    /// 自绘菜单各级的 SHM 读端（下标 = 级）。
    ShmFrameReader menuShm_[OVERLAY_MENU_LEVELS];
    /// 工具栏 / 软键盘的 SHM 读端（下标 = kind - OVERLAY_KIND_TOOLBAR）。
    ShmFrameReader panelShm_[2];
    FullscreenReporter fullscreen_;
    std::unique_ptr<X11CandidatePanel> panel_;
    /// Wayland 原生应用（frontend wayland_v2）的候选窗：popup surface，由合成器摆位。没有 Wayland
    /// 支持（编译时缺 libwayland）或不在 Wayland 会话时为空。
#ifdef WIND_HAVE_WAYLAND
    std::unique_ptr<WaylandCandidatePanel> wlPanel_;
#endif
    /// 「清风输入法设置」：挂进 Fcitx5 状态区（托盘菜单 / kimpanel 面板），点了打开设置程序。
    fcitx::SimpleAction settingsAction_;
    /// 「清风输入法菜单」：同挂状态区，点了在指针处弹出主菜单（Linux 没有工具栏，不组字时
    /// 别无入口；对位 Windows 托盘 / 工具栏的右键菜单）。
    fcitx::SimpleAction menuAction_;
    std::unique_ptr<fcitx::HandlerTableEntry<fcitx::EventHandler>> shiftClaim_;
    std::unique_ptr<fcitx::HandlerTableEntry<fcitx::EventHandler>> capabilityWatch_;
    ModeIndicator mode_;
    WindConfig config_;
    KeyHoldTracker keyHolds_;
    SynthRateLimiter synthRate_;
    uint32_t keyHoldTimeoutMs_ = kDefaultKeyHoldTimeoutMs;
    /// 收到 key.hold 按下的那个 IC：抬起要发回给它，而不是此刻的焦点。
    fcitx::TrackableObjectReference<fcitx::InputContext> keyHoldIC_;
    std::unique_ptr<fcitx::EventSourceTime> keyHoldExpiry_;
};

class WindEngineFactory : public fcitx::AddonFactory {
public:
    fcitx::AddonInstance* create(fcitx::AddonManager* manager) override
    {
        return new WindEngine(manager->instance());
    }
};

} // namespace windlinux
