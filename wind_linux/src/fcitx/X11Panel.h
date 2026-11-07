// X11 候选窗 + 光栅浮层 + 自绘菜单：把服务光栅化好的 BGRA 帧贴到 override-redirect 顶层窗口上。
//
// 对位 macOS `CandidatePanel.swift`（NSPanel）。像素不在本进程画，这里只负责：建窗、贴图、
// 落位（翻转/钳制规则在纯逻辑 `placePanel`）、隐藏，以及把鼠标点击/悬停/滚轮翻成
// CMD_CANDIDATE_SELECT/HOVER/SCROLL 交回服务（结果经 push 通道异步回来）。三层浮层的鼠标交互
// （悬停保持、拖动、右键菜单、点击关闭）的规则在纯逻辑 `OverlayInput.h`。
//
// 自建 xcb 连接（不借 Fcitx5 的 xcb 模块）：候选窗与 Fcitx5 自己的 UI 无关，自己的连接可以
// 在 xcb 模块未加载时照常工作；连接的 fd 挂到 Fcitx5 事件循环上，鼠标事件在主线程处理。
//
// 透明：有合成器（`_NET_WM_CM_S<n>` 有人持有）时用 32 位 ARGB visual，圆角与软件阴影按
// alpha 真透明；没有合成器时 ARGB 窗口的透明像素会画成黑色，于是退到根窗口 visual +
// XShape 按 alpha 抠形（alpha < 128 的像素抠掉）——圆角保住，半透明阴影退化成实色或消失。
#pragma once

#include "Codec.h"
#include "ExtProtocol.h"
#include "OverlayInput.h"
#include "ShmFrame.h"

#include <fcitx-utils/event.h>

#include <functional>
#include <memory>
#include <optional>
#include <string>
#include <utility>
#include <vector>
#include <xcb/xcb.h>

namespace windlinux {

class X11CandidatePanel {
public:
    struct Callbacks {
        std::function<void(int32_t index)> select; // 候选下标；-1 上页 / -2 下页
        std::function<void(int32_t index)> hover;  // -1 = 离开
        std::function<void(int32_t delta)> scroll; // WHEEL_DELTA(120) 倍数，正 = 上滚
        /// 候选窗上右键：target ≥ 0 为命中的候选（页内下标），-1 为空白 / 翻页按钮（主菜单）；
        /// (x, y) 为根窗口坐标。
        std::function<void(int32_t target, int32_t x, int32_t y)> contextMenu;
        /// 菜单打开期间的指针事件（MENU_POINTER_*、X11 按键号、根窗口坐标）→ CMD_MENU_POINTER。
        /// 上报失败时引擎就地 `closeMenu(nullptr)`——服务不响应，没人画菜单也没人收它，指针却
        /// 还被抓着，整个桌面的鼠标都会失灵。
        std::function<void(uint32_t event, uint32_t button, int32_t x, int32_t y)> menuPointer;
        /// 本端自己把菜单收掉了（空闲超时…），reason 报给服务端复位 menu_open。
        std::function<void(const std::string& reason)> menuDismissed;
        /// 右键状态气泡 / 悬停提示：target 为 MENU_TARGET_STATUS / MENU_TARGET_TOOLTIP，(x, y)
        /// 根窗口坐标（菜单锚点），(lx, ly) 右键点在该浮层位图内的坐标（提示按段 / 按行命中用）。
        std::function<void(int32_t target, int32_t x, int32_t y, int32_t lx, int32_t ly)>
            overlayMenu;
        /// 状态气泡拖动松手：内容左上的屏幕坐标 → `pos.status_tip`（落不落盘由服务端定）。
        std::function<void(int32_t x, int32_t y)> statusMoved;
        /// 工具栏 / 软键盘上拖动区以外的指针事件（PANEL_POINTER_*、X11 按键号、位图内坐标）
        /// → CMD_PANEL_POINTER。命中、高亮、点击都在服务端。
        std::function<void(uint32_t kind, uint32_t event, uint32_t button, int32_t x, int32_t y)>
            panelPointer;
        /// 工具栏 / 软键盘本地拖完松手：内容左上的屏幕坐标与所在输出名（拿不到为空）→ `pos.*`。
        std::function<void(uint32_t kind, int32_t x, int32_t y, const std::string& output)>
            panelMoved;
        /// 活动窗口的全屏状态（`_NET_ACTIVE_WINDOW` 的 `_NET_WM_STATE_FULLSCREEN`）。每次重查都调，
        /// 去重在引擎（`FullscreenReporter`）。
        std::function<void(bool fullscreen)> fullscreenChanged;
    };

    X11CandidatePanel(fcitx::EventLoop& loop, Callbacks cb);
    ~X11CandidatePanel();

    /// 贴一帧并显示。`x/y` 为服务端建议的左上角（wire 坐标 = 根窗口坐标）。
    /// 没有 X 连接（无 DISPLAY / 连不上）时返回 false、什么都不做。
    bool show(const SharedFrame& frame, int32_t x, int32_t y, bool absolute);
    void hide();
    /// 命中矩形（CMD_CANDIDATE_RECTS，晚于帧到达）。坐标是位图内的像素。
    void setRects(std::vector<CandidateHitRect> rects) { rects_ = std::move(rects); }

    /// 光栅浮层（CMD_OVERLAY_FRAME：tooltip / 状态气泡 / Toast）：各占一个窗口，落位见
    /// `placeOverlay`。`durationMs > 0` 时到点自己藏（计时归宿主，同 macOS `.app`）。
    /// 工具栏 / 软键盘（kind 10 / 11）也走这里，见 `showPanel`。
    /// 鼠标交互对位 Windows（见 OverlayInput.h）：悬停 / 拖动 / 菜单开着时暂停自动隐藏，
    /// 悬停提示在指针移进它时保持、右键弹菜单，状态气泡可拖、可右键，Toast 点一下就关。
    bool showOverlay(uint32_t kind, const SharedFrame& frame, const OverlayFramePayload& p);
    void hideOverlay(uint32_t kind);
    void hideAllOverlays();
    /// 状态气泡此刻内容左上的屏幕坐标（应 `pos.status_tip.query`）；不在屏上返回空。
    std::optional<std::pair<int32_t, int32_t>> statusContentOrigin() const;

    /// 自绘菜单第 `level` 级（CMD_OVERLAY_FRAME kind = OVERLAY_KIND_MENU + level）：按 EXACT
    /// 原样摆放。第一级出现时抓住指针（点菜单外能看见、并且不漏给下面的应用）、起空闲计时。
    /// 菜单打开期间，本连接上的全部指针事件（含候选窗上的）只报给 `menuPointer`。
    bool showMenuLevel(uint32_t level, const SharedFrame& frame, const OverlayFramePayload& p);
    /// 服务端推来的隐藏帧：摘掉这一级；全部摘掉后放开指针（这条路不报 dismiss——是服务端关的）。
    void hideMenuLevel(uint32_t level);
    /// 本端收菜单（全部级）。`reason` 非空时经 `menuDismissed` 报给服务端。
    void closeMenu(const char* reason);
    bool menuOpen() const;
    /// 有菜单相关的键盘活动（按键转给服务端的那一刻）：空闲计时重新起算。
    void noteMenuActivity();
    /// X11 会话的界面缩放因子：根窗口 RESOURCE_MANAGER 里的 `Xft.dpi / 96`（桌面环境设缩放时都会写它），
    /// 没有时看环境变量 `GDK_SCALE`，再没有按 1.0。已连接上 X 才读得到；没连过（还没显示过候选窗）
    /// 时在这里连一次——只在「没有连接」时才连，不会重连销毁现有窗口。
    /// `textScaling`：从 `Xft.dpi` 里除掉的 GNOME 字体缩放（见 `xftDpiToX11Scale`）。光栅化用默认 1，
    /// GNOME 折光标坐标传实际值。`GDK_SCALE` 回落不除。
    double hostScale(double textScaling = 1.0);
    /// 字体缩放：XSETTINGS（`_XSETTINGS_S<屏号>` 的属主窗口上的 `_XSETTINGS_SETTINGS`）里的
    /// `Gdk/UnscaledDPI`，见 `xsettingsTextScaling`。没有 XSETTINGS 管理者、没有这一项时按 1。
    /// 与 `hostScale` 一样只在没有连接时才连。
    double textScaling();
    /// 工作区（根窗口，不分显示器）。还没连上 / 连接已坏时返回空（不在这里重连）。
    std::optional<Rect> screenWorkArea();
    /// 当前指针位置（根窗口坐标）。还没连上 / 连接已坏时返回空（不在这里重连）。
    std::optional<std::pair<int32_t, int32_t>> pointerPosition();
    /// 屏幕点所在输出的 RandR 名字与工作区（输出矩形 ∩ `_NET_WORKAREA`）。没有 RandR、连接不可用、
    /// 点不在任何已点亮的输出上时返回空（调用方回落到 `screenWorkArea`、不报输出名）。
    std::optional<HostOutput> outputAt(int32_t x, int32_t y);

private:
    /// 一个 override-redirect 窗口及其位图。候选窗与三层浮层各一个，共用一条 X 连接。
    struct Surface {
        const char* instance = ""; // WM_CLASS 实例名（xdotool search --classname）
        bool interactive = false;  // 收鼠标（候选窗 / 菜单 / 浮层）
        bool overlay = false;      // 光栅浮层之一：另收进入 / 松开、窗口类型是 TOOLTIP
        bool panel = false;        // 工具栏 / 软键盘：常驻、指针转给服务端、WM_HINTS.input = False
        xcb_window_t window = 0;
        xcb_pixmap_t pixmap = 0;
        xcb_gcontext_t gc = 0;
        xcb_colormap_t colormap = 0;
        bool argb = false;
        uint8_t depth = 0;
        uint32_t pixW = 0;
        uint32_t pixH = 0;
        bool mapped = false;
        int32_t x = 0; // 窗口左上（根窗口坐标），最近一次 present / 拖动
        int32_t y = 0;
        std::unique_ptr<fcitx::EventSourceTime> hideTimer;
        // ── 仅浮层 ──
        OverlayFramePayload geo; // 最近一帧（尺寸取像素那一份）：内容盒偏移、自动隐藏时长
        HoverGate gate;          // 窗口出现那一刻的指针位置为基线
        bool hovered = false;    // 指针真实移动到它上面、尚未离开
        // ── 仅工具栏 / 软键盘 ──
        bool pointerIn = false; // 已报过移动、还没报离开：摘窗时要补一个 LEAVE
        int32_t reqX = INT32_MIN; // 服务端最近要的窗口左上（EXACT）：没变就不重算夹回
        int32_t reqY = INT32_MIN;
    };
    static constexpr size_t kOverlayCount = 3;
    static constexpr size_t kPanelCount = 2;
    static constexpr size_t kMenuLevels = OVERLAY_MENU_LEVELS;

    /// 连上并就绪（必要时重连）。**只许**在贴帧路径上调：重连会销毁 IO 事件源与窗口，
    /// 从 X 事件 / 计时器回调里调就是释放正在执行的自己。
    bool ensureConnection();
    void dropConnection();
    /// 现有连接可用（不重连）。
    bool connectionUsable() const;
    bool hasCompositor();
    bool ensureWindow(Surface& s, bool argb);
    void destroyWindow(Surface& s);
    /// `raise` = 顺带提到最上层。菜单的重绘（高亮变化）不能重排 z 序：子菜单翻到左侧压在
    /// 父菜单上时，父菜单一次重绘就会把它盖住（同 Windows `plan_render` 的教训）。
    bool present(Surface& s, const SharedFrame& frame, const Rect& r, bool raise = true);
    void unmap(Surface& s);
    /// 摘窗口并归位它的交互态（悬停 / 拖动），不动它的自动隐藏计时器——计时器回调里调用时
    /// 不能销毁正在执行的自己。
    void unmapWindow(Surface& s);
    void upload(Surface& s, const SharedFrame& frame);
    void applyShape(Surface& s, const SharedFrame& frame, uint8_t kind);
    Surface* overlay(uint32_t kind);
    /// 工具栏 / 软键盘的 Surface；别的 kind 返回空。
    Surface* panelSurface(uint32_t kind);
    /// 三层浮层或两块面板，按 kind 取。
    Surface* surfaceOf(uint32_t kind);
    /// 哪一层浮层 / 哪块面板的窗口（都不是返回 0）。
    uint32_t overlayKindOf(xcb_window_t w) const;
    Rect workArea() const;
    static Rect rectOf(const Surface& s) { return Rect{s.x, s.y, int32_t(s.pixW), int32_t(s.pixH)}; }
    void onReadable();
    void handleEvent(xcb_generic_event_t* ev);
    void setHover(int32_t index);

    // ── 浮层交互（OverlayInput.h 是纯逻辑那一半）──
    void handleOverlayEvent(uint32_t kind, xcb_generic_event_t* ev);

    // ── 工具栏 / 软键盘 ──
    bool showPanel(uint32_t kind, const SharedFrame& frame, const OverlayFramePayload& p);
    /// 拖动区外的事件原样转给服务端；拖动区里的左键本地拖（moveDrag / endDrag）。
    void handlePanelEvent(uint32_t kind, xcb_generic_event_t* ev);
    /// 报一个指针事件。移动先积着、按批末尾或下一个非移动事件之前再报（每次上报都是一趟同步请求）。
    void panelPointer(uint32_t kind, uint32_t event, uint32_t button, int32_t x, int32_t y);
    void flushPanelMotion();
    /// 指针还算在面板里（报过移动、没报离开）就补报 LEAVE：窗口被摘 / 销毁时 X 不会再发离开，
    /// 服务端「指针在面板内」会一直挂着。
    void panelPointerGone(Surface& s);
    /// 当前点亮的输出（RandR）：名字、矩形、工作区。没有 RandR 时为空。
    struct OutputInfo {
        std::string name;
        Rect rect;
        Rect work;
    };
    std::vector<OutputInfo> queryOutputs();
    /// `_NET_WORKAREA`（当前桌面那一块）。没有时返回空。
    std::optional<Rect> netWorkArea();
    /// 面板窗口 `r` 该夹进的工作区：中心所在输出的工作区；不在任何输出上时取 RandR 主输出（再没有
    /// 取第一块）；没有 RandR 时回落到根窗口的 `_NET_WORKAREA`（再没有取整个根窗口）。
    Rect panelWorkArea(const Rect& r, const std::vector<OutputInfo>& outputs);

    // ── 全屏检测 ──
    /// 连上后订阅根窗口属性变化（`_NET_ACTIVE_WINDOW`），并排一次立即检查。
    void watchRoot();
    /// 重读 `_NET_ACTIVE_WINDOW`：换了窗口就撤掉对旧窗口的属性订阅、订阅新窗口，然后重判全屏。
    void refreshActiveWindow();
    void checkFullscreen();
    bool ownsWindow(xcb_window_t w) const;
    /// 候选窗上的悬停变化：悬停提示正显示时按 `hoverDeferMs` 延后，给挪进提示留时间；
    /// `at` 是指针的根窗口坐标，正朝提示去时顺延（`headingToTip`）。
    void candidateHover(int32_t raw, std::optional<std::pair<int32_t, int32_t>> at = std::nullopt);
    void onHoverDeferred();
    void onTipLeaveGrace();
    void cancelTipTimers();
    /// 自动隐藏是否暂停：悬停 / 拖动 / 右键菜单开着（Windows `StatusTip::interacting`）。
    bool held(uint32_t kind) const;
    /// 按 `held` 暂停或（交互结束时）重新给满一份时长。
    void refreshHold(uint32_t kind);
    void armHide(uint32_t kind);
    bool dragging(uint32_t kind) const { return drag_.active && drag_.kind == kind; }
    void moveDrag(int32_t rootX, int32_t rootY);
    void endDrag(bool report);
    void setDragCursor(bool on);
    /// 菜单收起后按指针的真实位置重定：悬停提示去留、候选悬停、气泡 / Toast 的悬停。
    void resyncAfterMenu();
    bool pointerOn(const Surface& s, const std::optional<std::pair<int32_t, int32_t>>& p) const;
    void armTimer(std::unique_ptr<fcitx::EventSourceTime>& t, uint32_t ms,
                  void (X11CandidatePanel::*fn)());
    /// 菜单打开期间的指针事件：移动合并到本批末尾再报，按下立即报（先把积着的移动报掉保序）。
    void handleMenuEvent(xcb_generic_event_t* ev);
    void flushMenuMotion();
    void grabPointer();
    void releasePointer();
    void armMenuIdle();
    void armMenuDeadline();
    void onMenuDeadline();
    /// 摘掉全部菜单窗口、放开指针、停计时（不报 dismiss）。计时器只停不毁（会从它们自己的
    /// 回调里进来）。
    void dropMenu();

    fcitx::EventLoop& loop_;
    Callbacks cb_;
    xcb_connection_t* conn_ = nullptr;
    xcb_screen_t* screen_ = nullptr;
    int screenNum_ = 0;
    std::unique_ptr<fcitx::EventSourceIO> ioEvent_;
    bool warnedNoDisplay_ = false;
    bool shapeAvailable_ = false;

    Surface cand_;
    Surface overlays_[kOverlayCount]; // 下标 = kind - 1（OVERLAY_KIND_TOOLTIP..TOAST）
    /// 候选窗实际落点 − 服务端建议落点（被翻转/钳制过时非零）。FOLLOW_CANDIDATE 的 tooltip 据此平移。
    int32_t candShiftX_ = 0;
    int32_t candShiftY_ = 0;

    std::vector<CandidateHitRect> rects_;
    int32_t hover_ = -1; // 最近报给服务端的悬停
    ClickGate click_;    // 左键点选：按下记命中，松开同一项才选
    int32_t pendingHover_ = -1;
    std::unique_ptr<fcitx::EventSourceTime> hoverDefer_; // 延后报的悬停变化
    uint64_t hoverDue_ = 0;                              // hoverDefer_ 的到期时刻（微秒）
    std::optional<std::pair<int32_t, int32_t>> lastPointer_; // 上一次移动的根窗口坐标
    std::optional<int32_t> tipBest_; // 本次宽限里离提示最近的距离（tipGap），刷新新低才顺延
    std::unique_ptr<fcitx::EventSourceTime> tipLeave_;   // 离开悬停提示后的宽限

    struct Drag {
        bool active = false;
        uint32_t kind = 0;  // 拖的是哪一层（状态气泡 / 工具栏 / 软键盘）
        int32_t grabDx = 0; // 按下时指针相对窗口左上
        int32_t grabDy = 0;
    } drag_;
    xcb_cursor_t dragCursor_ = 0;
    /// 右键状态气泡请求了菜单：菜单开着（或还在路上）期间气泡不自动消失。菜单收起时清；
    /// 请求石沉大海（服务没了）由 statusMenuWait_ 兜底清掉，免得气泡永远不走。
    bool statusMenuHold_ = false;
    std::unique_ptr<fcitx::EventSourceTime> statusMenuWait_;
    void onStatusMenuWaitExpired();
    /// 菜单是否开过（showMenuLevel 首级 → dropMenu）：只在真有菜单收起时重定指针态。
    bool menuActive_ = false;

    Surface menus_[kMenuLevels];
    bool pointerGrabbed_ = false;
    int grabAttempts_ = 0;
    std::unique_ptr<fcitx::EventSourceTime> grabRetry_;
    std::unique_ptr<fcitx::EventSourceTime> menuIdle_;
    /// 抓指针的绝对上限（从第一级出现算起，不续），见 Menu.h。
    std::unique_ptr<fcitx::EventSourceTime> menuDeadline_;
    uint32_t menuIdleMs_;
    uint32_t menuMaxGrabMs_;
    std::optional<std::pair<int32_t, int32_t>> pendingMotion_;

    Surface panels_[kPanelCount]; // 下标 = kind - OVERLAY_KIND_TOOLBAR
    /// 拖面板期间的输出布局（按下时取一次，拖动中每次移动都按它挑工作区，不再往返查 RandR）。
    std::vector<OutputInfo> dragOutputs_;
    struct PanelMotion {
        uint32_t kind;
        int32_t x;
        int32_t y;
    };
    std::optional<PanelMotion> pendingPanelMotion_;
    bool randrAvailable_ = false;

    // 全屏检测用的原子（连上时取一次）与当前订阅着的活动窗口。
    xcb_atom_t atomActiveWindow_ = 0;
    xcb_atom_t atomWmState_ = 0;
    xcb_atom_t atomFullscreen_ = 0;
    xcb_atom_t atomWorkArea_ = 0;
    xcb_atom_t atomCurrentDesktop_ = 0;
    xcb_window_t activeWindow_ = 0;
    /// 对 activeWindow_ 的 PropertyChange 订阅是不是本端加的（只撤自己加的）。
    bool activeWatched_ = false;
    std::unique_ptr<fcitx::EventSourceTime> activeCheck_;
};

} // namespace windlinux
