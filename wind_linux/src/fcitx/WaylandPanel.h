// Wayland 候选窗：把服务光栅化好的 BGRA 帧贴到由合成器按文本光标摆位的 surface 上——
// input-method-v2 的 popup surface（treeland 等），或 input-method-v1 的 overlay 面板（KWin）。
//
// X11 路径（X11Panel）靠 `cursorRect()` 是屏幕坐标这一条自己摆窗口；Wayland 客户端报的却是
// 窗口相对坐标，应用的屏幕位置只有合成器知道。popup surface 正是为此设计的：把 wl_surface 交给
// `zwp_input_method_v2.get_input_popup_surface`，合成器按文本光标矩形自己摆（含翻转、避让）。
// 所以本类**不接收也不使用任何坐标**。
//
// 连接：借 Fcitx5 wayland 模块已建好的 `wl_display`（popup 必须与输入法对象同一连接），对象建在
// 默认队列上，事件由 Fcitx5 的事件读取线程 / 主线程分发，本类不另起 IO。
// 输入法对象：由 waylandim 前端持有（一个座位只能有一个输入法绑定），经 addon 函数
// `getInputMethodV2(ic)` 取得——它返回 Fcitx5 私有包装类，C 层指针见 `rawProxyOf`。
// v1 不需要输入法对象：`zwp_input_panel_v1` 全局直接从本类的 registry 绑（KWin 只把它发给自己
// 拉起的输入法连接，即 Fcitx5 的默认连接）。v1 的面板角色没有 destroy 请求、KWin 又全局只认
// 一个面板，故每个显示周期换新 surface，旧的经 RetiredSurfaces 延迟销毁
// （docs/design/linux-kde-input-method-v1.md）。
//
// 皮肤的装饰可以外扩到窗体之外，而合成器只按主 surface 摆位（surface 左上贴光标左下）：主 surface
// 只占窗体、整张位图放在子 surface 上（位置 = −窗体左上），没有 wl_subcompositor 时只贴窗体左上起的
// 部分（设计 theme-skin-animation.md §17.3「内容偏移」，`planWaylandSkin`）。
//
// 例外是工具栏 / 软键盘（kind 10 / 11）：它们常驻屏幕、不跟光标，各开一个 TOP 层的 layer surface，
// 按「所在输出 + top|left 边距」摆。坐标系见 OverlayInput.h 的「Wayland 面板坐标」。KDE（v1 会话）
// 同样走 layer-shell，不碰 v1 面板（KWin 全局只认一个面板）。
#pragma once

#include "Codec.h"
#include "ExtProtocol.h"
#include "Menu.h"
#include "OverlayInput.h"
#include "RetiredSurfaces.h"
#include "ShmFrame.h"

#include <fcitx-utils/event.h>
#include <fcitx-utils/handlertable.h>
#include <fcitx/addoninstance.h>
#include <fcitx/inputcontext.h>
#include <fcitx/instance.h>

#include <functional>
#include <memory>
#include <optional>
#include <utility>
#include <string>
#include <vector>

struct wl_display;
struct wl_registry;
struct wl_compositor;
struct wl_shm;
struct wl_surface;
struct wl_buffer;
struct wl_callback;
struct wl_seat;
struct wl_pointer;
struct wl_output;
struct zxdg_output_manager_v1;
struct zxdg_output_v1;
struct wp_viewporter;
struct wp_viewport;
struct zwp_input_method_v2;
struct zwp_input_popup_surface_v2;
struct zwp_input_panel_v1;
struct zwp_input_panel_surface_v1;
struct zwlr_layer_shell_v1;
struct zwlr_layer_surface_v1;
struct zwp_relative_pointer_manager_v1;
struct wl_subcompositor;
struct wl_subsurface;
struct zwp_relative_pointer_v1;

namespace windlinux {

class WaylandCandidatePanel {
public:
    explicit WaylandCandidatePanel(fcitx::Instance* instance);
    ~WaylandCandidatePanel();

    /// 该 IC 是否由本类负责：Wayland 原生前端 `wayland_v2`，或 `wayland`（input-method-v1）且
    /// 绑到了 `zwp_input_panel_v1`。其余（XIM / DBus 下的 X11 应用…）走 X11 窗口。
    bool handles(const fcitx::InputContext* ic) const;
    /// 已拿到连接且 wl_compositor / wl_shm 就绪。
    bool ready() const { return compositor_ && shm_; }

    /// 贴一帧并显示。失败（没就绪 / 取不到输入法对象 / 校验不过）返回 false，调用方不退路——
    /// 只是这一帧不显示，原因已记日志。
    bool show(fcitx::InputContext* ic, const SharedFrame& frame);
    /// 只收起候选这一层（其余浮层还在就继续显示）。
    void hide();
    /// 浮层（kind 同 ExtProtocol.h 的 OVERLAY_KIND_*：1 tooltip / 2 状态气泡 / 3 Toast）。
    /// 两条路：
    /// - 跟着光标 / 候选走的（tooltip、跟随光标的状态气泡）：只有 popup 这一个 surface 能由合成器
    ///   按光标摆位，所以与候选合成进同一张位图（候选在上，其余竖排在下，tooltip 按服务端落位）；
    /// - 按屏幕摆的（Toast、屏幕锚点 / 固定位置的状态气泡）：合成器有 wlr-layer-shell 时各开一个
    ///   overlay 层 surface，按锚点 + 边距摆，层级在任务栏之上；没有 layer-shell 退回 popup。
    /// `geo.durationMs` > 0 到点自动收起（指针停在上面时暂停计时）。
    bool showOverlay(fcitx::InputContext* ic, uint32_t kind, const SharedFrame& frame,
                     const OverlayFramePayload& geo);
    /// 工具栏 / 软键盘也走这一对：合成器没有 layer-shell 时不显示（记一次 warn），返回 false。
    void hideOverlay(uint32_t kind);
    /// 这一帧该不该走 layer-shell（屏幕锚点 / 固定位置的 Toast、状态气泡，以及工具栏 / 软键盘，且
    /// 合成器有 layer-shell；tooltip 永远跟着候选）。为真时与焦点在哪种应用无关。
    bool wantsLayer(uint32_t kind, const OverlayFramePayload& geo) const;
    /// 全部层一并收起（焦点走了 / 服务断了）。工具栏 / 软键盘不在内，由 hideOverlay 单独收。
    void hideAll();
    /// 合成器有 layer-shell 时（工具栏 / 软键盘摆在 Wayland 的坐标系里）：焦点输出的名字与工作区，
    /// 坐标系同面板（OverlayInput.h「Wayland 面板坐标」）。焦点输出取候选窗最近进入的那块，之前取
    /// 第一块；名字来自 wl_output v4 的 name 事件，合成器版本不够时为空（`pos.*` 同样不报）。工作区
    /// 取输出原点 + 可用区尺寸（每块输出一个探测 layer surface 量出来，见 layerUsableArea），还没
    /// 量到时取整块输出。没有 layer-shell 时返回空，调用方照旧。
    std::optional<HostOutput> hostOutput() const;
    /// 某块输出的可用区变了（任务栏挪了 / 改了高度、探测 surface 首次 configure）：引擎据此重报
    /// host.display，服务端按新工作区算默认落点。
    void setOnWorkChanged(std::function<void()> cb) { onWorkChanged_ = std::move(cb); }
    /// 候选窗所在输出的界面缩放因子（1.0 = 100%）。取自 wl_output 的模式尺寸 / xdg-output 的逻辑
    /// 尺寸（分数缩放下也准），没有 xdg-output 时退回 wl_output 的整数缩放；候选窗出现过后跟着它
    /// 进入的输出走，之前用第一个输出。还没连上 / 没有输出信息时返回 1.0。
    double scale() const;
    /// 告知本类「服务端此刻按多大的缩放在光栅化」。贴图时按它折回逻辑尺寸——帧里的缩放字段是
    /// 取整的，分数缩放下不能用。引擎每次向服务端上报缩放时同步给这里。
    void setReportedScale(double s) { reportedScale_ = s > 0 ? s : 1.0; }
    /// 输出的缩放变了（热插拔、系统设置里改了缩放、候选窗换了屏）：引擎据此重新上报。
    void setOnScaleChanged(std::function<void()> cb) { onScaleChanged_ = std::move(cb); }
    /// 鼠标交互回调（语义同 X11CandidatePanel::Callbacks 的同名项）。
    struct Callbacks {
        std::function<void(int32_t index)> select; // 候选下标；-1 上页 / -2 下页
        std::function<void(int32_t index)> hover;  // -1 = 离开
        std::function<void(int32_t delta)> scroll; // WHEEL_DELTA(120) 倍数，正 = 上滚
        /// 右键：target 同 `menu.open`；x y 是 popup 位图里的像素坐标。
        std::function<void(int32_t target, int32_t x, int32_t y)> contextMenu;
        /// 菜单开着期间的指针事件（event 取 MENU_POINTER_*，button 左 1 中 2 右 3）。
        std::function<void(uint32_t event, uint32_t button, int32_t x, int32_t y)> menuPointer;
        /// 状态气泡拖完：内容盒左上（输出坐标，位图像素）。同 X11 的 statusMoved。
        std::function<void(int32_t x, int32_t y)> statusMoved;
        /// 屏幕坐标系的菜单（状态气泡右键）：x y 与工作区都是输出坐标（位图像素）。
        std::function<void(int32_t target, int32_t x, int32_t y, Rect workArea)> screenMenu;
        /// 本端自己收了屏幕菜单（空闲 / 上限超时、请求没回音）：报服务端复位 menu_open。
        std::function<void(const std::string& reason)> menuDismissed;
        /// 工具栏 / 软键盘拖动区外的指针（语义同 X11CandidatePanel::Callbacks::panelPointer）。
        std::function<void(uint32_t kind, uint32_t event, uint32_t button, int32_t x, int32_t y)>
            panelPointer;
        /// 工具栏 / 软键盘拖完：内容盒左上（面板坐标）与所在输出名（拿不到为空串）。
        std::function<void(uint32_t kind, int32_t x, int32_t y, const std::string& output)> panelMoved;
        /// 点了候选窗上的一块皮肤装饰（同 X11 的 decorClick）。
        std::function<void(uint32_t index)> decorClick;
    };
    /// 状态气泡此刻在 layer-shell 上的内容盒左上（输出坐标，位图像素）；不在 layer-shell 上
    /// 显示时为空（服务端「固定位置」落盘询问用）。
    std::optional<std::pair<int32_t, int32_t>> statusContentOrigin() const;
    /// 自绘菜单（服务端画、服务端命中）：同样并进 popup 位图。菜单的坐标系原点 = popup 位图
    /// 左上角，工作区见 `virtualWorkArea`。
    bool showMenuLevel(fcitx::InputContext* ic, uint32_t level, const SharedFrame& frame,
                       const OverlayFramePayload& p);
    void hideMenuLevel(uint32_t level);
    bool menuOpen() const;
    /// 菜单此刻开在屏幕坐标系（layer-shell）上：菜单帧不论焦点在哪种应用都归这里。
    bool screenMenuActive() const { return screenMenu_; }
    /// 刚在工具栏 / 软键盘上按过（转给了服务端）：随后来的菜单帧是服务端从面板一侧发起的
    /// （工具栏右键、设置键…），坐标是面板坐标，要开在面板所在输出的底板上。
    bool panelMenuPending() const;
    /// 本端另一处发起了菜单请求（X11 候选右键、X11 气泡 / 提示菜单…）：接下来的菜单帧不是面板的。
    void clearPanelMenuPending() { panelMenu_.clear(); }
    /// 工具栏 / 软键盘摆在 layer-shell 上（Wayland 会话）。
    bool layerPanels() const { return layerShell_ != nullptr; }
    /// layer-shell 绑上了：引擎据此重报宿主显示（输出 / 工作区换成 Wayland 这套）并复位全屏态。
    void setOnLayerShell(std::function<void()> cb) { onLayerShell_ = std::move(cb); }
    /// 在指针处开屏幕菜单（状态区「清风输入法菜单」用）：落点等指针进入底板时取。没有
    /// layer-shell / 连接没就绪时返回 false，调用方退回 X11。
    bool openMenuAtPointer(int32_t target);
    /// 本端收起菜单（不报服务端）。
    void closeMenu();
    Rect virtualWorkArea() const;
    void setCallbacks(Callbacks cb) { cb_ = std::move(cb); }
    /// 服务端推来的命中表（位图像素坐标）。
    void setRects(std::vector<CandidateHitRect> rects) { rects_ = std::move(rects); }
    /// 皮肤区域（CMD_CANDIDATE_SKIN）：带输入区表时候选那一块只在表里的矩形接鼠标（穿透装饰、
    /// 透明外扩区不接），否则整块候选窗接。先于对应的帧到，贴帧时生效。
    void setSkin(CandidateSkin skin);

private:
    struct Output {
        WaylandCandidatePanel* owner = nullptr;
        uint32_t name = 0; // registry 名
        wl_output* wl = nullptr;
        zxdg_output_v1* xdg = nullptr;
        int32_t scaleInt = 1;
        int32_t modeW = 0, modeH = 0; // 当前模式（物理像素）
        int32_t logW = 0, logH = 0;   // xdg-output 逻辑尺寸
        int32_t logX = 0, logY = 0;   // xdg-output 逻辑位置（合成器全局坐标）
        uint32_t version = 0;         // 绑定的 wl_output 版本
        std::string wlName;           // wl_output.name（v4 起才有）
        // 可用区探测：四边锚定、独占区 0、尺寸交给合成器、从不贴 buffer（不映射、不挡任何东西）的
        // layer surface。configure 来的尺寸 = 输出扣掉别人独占区（任务栏）后的可用区，0 = 还没量到。
        wl_surface* probeSurface = nullptr;
        zwlr_layer_surface_v1* probe = nullptr;
        int32_t availW = 0, availH = 0;
        char fakeUserData[1024] = {}; // 同 LayerSurf::fakeUserData
    };
    /// 输出在合成器全局逻辑坐标里的矩形（没有 xdg-output 时位置按 0,0、尺寸按模式 / 缩放推算）。
    static Rect outputLogicalRect(const Output& o);
    /// 面板落位用的可用区（逻辑坐标）：outputLogicalRect 的原点 + 探测到的可用区尺寸。
    static Rect usableLogicalRect(const Output& o);
    /// 一块输出都没有时的兜底（逻辑坐标，记一条 debug）。
    static Rect noOutputRect();
    /// 给还没有探测 surface 的输出各建一个（要 compositor 与 layer-shell 都已绑上）。
    void ensureWorkProbes();
    void destroyWorkProbe(Output& o, bool alive);
    void probeConfigured(Output& o, uint32_t serial, uint32_t w, uint32_t h);
    Output* outputByName(uint32_t name) const;
    void onGlobalRemove(uint32_t name);
    static double outputScale(const Output& o);
    void attachXdgOutput(Output& o);
    void scaleMaybeChanged();
    void onConnection(const std::string& name, wl_display* display);
    void onClosed(wl_display* display);
    void onGlobal(uint32_t name, const char* interface, uint32_t version);
    /// 丢掉全部 Wayland 对象（连接已死时不能再调 destroy 请求，故分两种）。
    void teardown(bool connectionAlive);
    bool ensurePopup(fcitx::InputContext* ic);
    bool ensureSurface();
    bool ensurePopupV2(fcitx::InputContext* ic);
    bool ensurePanelV1();
    /// 当前 surface 退役（v1：已摘 buffer），进延迟销毁队列。
    void retireSurface();
    /// 销毁可以销毁的退役 surface；`all` = 连接拆除时全部处理（`alive` 为假时只丢不发请求）。
    void reapRetired(bool all, bool alive = true);
    /// 把各层合成一张位图贴上去；没有任何层在显示就收起 popup。
    bool present(fcitx::InputContext* ic);
    /// `inputW/H`：位图里接收鼠标的那一块（候选窗）的像素尺寸，0 = 整张都不接收。`skinMask`：
    /// 候选那一块按皮肤输入区表收（菜单开着时整张都收，传 false）。`plan`：按皮肤窗体拆层 / 裁切。
    bool attachPixels(const uint8_t* data, uint32_t w, uint32_t h, uint32_t stride,
                      uint32_t inputW, uint32_t inputH, bool skinMask, const WaylandSkinPlan& plan);
    /// 子 surface（拆层时放整张位图）：没有就建，挂在当前 surface 下面。
    bool ensureChild();
    /// 子 surface 摘 buffer（随下一次主 surface 提交生效）。
    void unmapChild();
    /// 指针事件里的 surface 是不是候选窗这一组（主 surface 或子 surface）。
    bool isPopupSurface(const wl_surface* s) const { return s && (s == surface_ || s == child_); }
    /// RetiredSurfaces 按主 surface 记指针：子 surface 换成它挂的那个主 surface。
    uintptr_t retiredKey(wl_surface* s) const;
    void hidePopup();
    void dropPopup();
    /// 位图 → wl_buffer（memfd + wl_shm）；失败返回 nullptr。
    wl_buffer* makeBuffer(const uint8_t* data, uint32_t w, uint32_t h, uint32_t stride);
    /// 位图像素 → 合成器的逻辑尺寸（viewporter / 整数 buffer scale）。
    void applyScale(wl_surface* s, wp_viewport*& vp, uint32_t w, uint32_t h);
    void releaseBuffer(wl_buffer* b);
    static zwp_input_method_v2* rawProxyOf(void* wrapper);

    fcitx::Instance* instance_;
    std::unique_ptr<fcitx::HandlerTableEntryBase> connCreated_;
    std::unique_ptr<fcitx::HandlerTableEntryBase> connClosed_;

    wl_display* display_ = nullptr;
    wl_registry* registry_ = nullptr;
    wl_compositor* compositor_ = nullptr;
    uint32_t compositorVersion_ = 0;
    wl_shm* shm_ = nullptr;
    std::vector<std::unique_ptr<Output>> outputs_;
    Output* currentOutput_ = nullptr; // 候选窗最近进入的输出
    zxdg_output_manager_v1* xdgManager_ = nullptr;
    wp_viewporter* viewporter_ = nullptr;
    wp_viewport* viewport_ = nullptr; // 随 surface 建 / 毁
    double reportedScale_ = 1.0;
    double lastNotifiedScale_ = 1.0;
    std::function<void()> onScaleChanged_;
    wl_seat* seat_ = nullptr;
    wl_pointer* pointer_ = nullptr;
    wl_surface* surface_ = nullptr;
    // 拆层用的子 surface（随 surface_ 建、随它退役）。
    wl_surface* child_ = nullptr;
    wl_subsurface* childSub_ = nullptr;
    wp_viewport* childViewport_ = nullptr;
    bool childMapped_ = false;
    bool forceClip_ = false; // WINDINPUT_WAYLAND_SKIN_CLIP=1：强制裁切（排障）
    // 主 surface 左上在位图里的位置（位图像素）：裁切时指针坐标要加回它。
    int32_t mainOriginX_ = 0, mainOriginY_ = 0;
    WaylandSkinMode lastMode_ = WaylandSkinMode::Whole; // 只在贴法变了时记日志
    // 指针此刻所在 surface 的原点在位图里的位置（逻辑坐标）：pointerX_/Y_ = 局部坐标 + 它。
    double ptrOffX_ = 0, ptrOffY_ = 0;
    zwp_input_method_v2* im_ = nullptr;      // 当前 popup 挂在哪个输入法对象上
    zwp_input_popup_surface_v2* popup_ = nullptr;
    // surface 拿过哪种角色（角色一经指定终身不变，换另一种只能换 surface）。
    enum class Role { None, PopupV2, PanelV1 };
    Role surfaceRole_ = Role::None;
    zwp_input_panel_v1* inputPanel_ = nullptr;
    zwp_input_panel_surface_v1* panelSurf_ = nullptr; // 当前 surface 的 v1 角色
    struct Retired {
        WaylandCandidatePanel* owner = nullptr;
        wl_surface* surface = nullptr;
        wp_viewport* viewport = nullptr;
        zwp_input_panel_surface_v1* role = nullptr;
        wl_callback* sync = nullptr;
        wl_surface* child = nullptr;
        wl_subsurface* childSub = nullptr;
        wp_viewport* childViewport = nullptr;
    };
    std::vector<std::unique_ptr<Retired>> retiredRes_;
    RetiredSurfaces retired_;
    wl_buffer* buffer_ = nullptr;            // 最近一次 attach 的（release 时销毁）
    CandidateSkin skin_;                     // 最近一条皮肤区域
    // 层 0 = 候选，1..3 = OVERLAY_KIND_TOOLTIP/STATUS/TOAST。
    static constexpr int kLayers = 4;
    struct Layer {
        SharedFrame frame;
        OverlayFramePayload geo; // 浮层的落位说明（tooltip 按它相对候选窗摆）
        bool on = false;
        bool onLayer = false; // 显示在 layer-shell 上（不进 popup 合成）
        std::unique_ptr<fcitx::EventSourceTime> hideTimer;
    };
    Layer layers_[kLayers];
    static constexpr size_t kMenuLevels = OVERLAY_MENU_LEVELS;
    struct MenuLayer {
        SharedFrame frame;
        Rect rect; // 位图在 popup 坐标系里的矩形（含阴影扩边）
        bool on = false;
    };
    MenuLayer menus_[kMenuLevels];
    std::optional<std::pair<int32_t, int32_t>> lastMenuMotion_;

    // ── 屏幕锚定的浮层（wlr-layer-shell）──
    // surface 一经建立就不销毁：同 popup 的 surface，销毁后迟到的指针事件会带崩 classicui；
    // 收起 = 摘 buffer（layer-shell 规定此后要重新走一遍「空提交 → configure」才能再映射）。
    struct LayerSurf {
        WaylandCandidatePanel* owner = nullptr;
        uint32_t kind = 0;
        wl_surface* surface = nullptr;
        zwlr_layer_surface_v1* layer = nullptr;
        wp_viewport* viewport = nullptr;
        bool configured = false; // 收到并应答了 configure，可以贴 buffer
        bool waiting = false;    // 已做空提交、等 configure
        bool mapped = false;
        bool wanted = false;     // 该显示（configure 到了就贴）
        SharedFrame frame;
        OverlayFramePayload geo;
        // 当前摆位（逻辑像素）：锚定边 + 边距。拖动时改成左上锚定。
        uint32_t anchor = 0;
        int32_t mTop = 0, mRight = 0, mBottom = 0, mLeft = 0;
        int32_t lw = 0, lh = 0; // 逻辑尺寸
        bool hovered = false;
        bool dragging = false;
        double grabX = 0, grabY = 0;
        int32_t dragX = 0, dragY = 0; // 未钳制的全局逻辑位置，越过屏幕边界时继续累计
        // ── 工具栏 / 软键盘专用 ──
        uint32_t outputName = 0;  // 该摆在哪块输出（registry 名；0 = 交给合成器挑）
        uint32_t boundOutput = 0; // 当前角色对象建在哪块输出上（输出一经指定不能改，换屏要重建角色）
        bool reqValid = false;    // 下面几项记着上次按哪一帧算的位置
        int32_t reqX = 0, reqY = 0;
        uint32_t pixW = 0, pixH = 0;
        bool pointerIn = false;   // 报过指针事件、还没报离开
        // Fcitx5 classicui 会把本连接上任意 surface 的 user_data 当它的窗口包装对象读：给全零占位。
        char fakeUserData[1024] = {};
    };
    zwlr_layer_shell_v1* layerShell_ = nullptr;
    uint32_t layerShellVersion_ = 0;
    LayerSurf statusLs_;
    LayerSurf toastLs_;
    LayerSurf dragPreview_; // 不接收输入；原 surface 保持映射以保留隐式抓取
    LayerSurf panels_[2]; // 下标 = kind - OVERLAY_KIND_TOOLBAR
    LayerSurf* layerSurfFor(uint32_t kind);
    LayerSurf* panelSurf(uint32_t kind);
    static bool isPanel(const LayerSurf& ls);
    LayerSurf* layerSurfOf(wl_surface* s);
    /// 此刻在输出上的左上角（逻辑像素，按锚点 + 边距 + 输出逻辑尺寸推算）。
    std::pair<int32_t, int32_t> layerTopLeft(const LayerSurf& ls) const;
    void showOnLayer(LayerSurf& ls, const SharedFrame& f, const OverlayFramePayload& geo);
    /// 按 ls 里已算好的锚点 / 边距 / 输出建角色（还没有的话）并贴图或走首次 configure。
    void mapLayer(LayerSurf& ls);
    void hideLayer(LayerSurf& ls);
    void placeLayer(LayerSurf& ls); // 由 geo 算锚点与边距
    void commitLayer(LayerSurf& ls); // configure 之后：设几何 + 贴 buffer + 提交
    void layerConfigured(LayerSurf& ls, uint32_t serial);
    void layerClosed(LayerSurf& ls);
    void armOverlayHide(uint32_t kind);
    void beginStatusDrag(LayerSurf& ls, double x, double y);
    void moveStatusDrag(LayerSurf& ls, double x, double y);
    /// 拖动：按指针的相对位移移动（relative-pointer；不受 surface 自己在动的影响）。
    void moveStatusDragBy(LayerSurf& ls, double dx, double dy);
    void ensureRelativePointer();
    void endStatusDrag(LayerSurf& ls);

    // ── 工具栏 / 软键盘 ──
    bool showPanel(uint32_t kind, const SharedFrame& f, const OverlayFramePayload& geo);
    void hidePanel(LayerSurf& ls);
    /// 指针在面板位图里的像素坐标（layerPtrX_/Y_ × 光栅化缩放）。
    std::pair<int32_t, int32_t> panelPointerPos() const;
    /// 回传一次指针事件（坐标取 panelPointerPos）。移动合并到本轮事件分发之后再报。
    void panelPointer(LayerSurf& ls, uint32_t event, uint32_t button);
    void flushPanelMotion();
    /// 指针还在面板里时它被收起 / 拆掉：补报离开（坐标无意义，服务端的 LEAVE 不更新最后指针）。
    void panelPointerGone(LayerSurf& ls);
    void beginPanelDrag(LayerSurf& ls);
    void movePanelDrag(LayerSurf& ls, int32_t dx, int32_t dy);
    void movePanelDragBy(LayerSurf& ls, double dx, double dy);
    void endPanelDrag(LayerSurf& ls, bool report, bool restoreSource = true);
    /// 合成器收回了面板的角色（输出被拔）/ 输出没了：按现有输出重摆还想显示的面板。
    void onPanelRetry();
    struct PanelMotion {
        uint32_t kind;
        int32_t x, y;
    };
    std::optional<PanelMotion> pendingPanelMotion_;
    std::unique_ptr<fcitx::EventSourceTime> panelMotionFlush_;
    std::unique_ptr<fcitx::EventSourceTime> panelRetry_;
    bool warnedNoLayerShell_ = false;
    std::pair<int32_t, int32_t> outputLogicalSize() const;
    zwp_relative_pointer_manager_v1* relPtrMgr_ = nullptr;
    zwp_relative_pointer_v1* relPtr_ = nullptr;
    double dragAccX_ = 0, dragAccY_ = 0; // 相对位移里还没折成整像素的部分
    LayerSurf* pointerLayer_ = nullptr; // 指针此刻在哪个 layer surface 上（不在为空）
    // 跟随光标、合成在 popup 里的状态气泡：它在 popup 位图里的矩形（像素）。它也收鼠标
    // （悬停保持、右键菜单），所以要进 popup 的输入区。
    std::optional<Rect> popupStatusRect_;
    bool popupStatusHovered_ = false;
    bool pointerOnPopupStatus() const;
    void updatePopupStatusHover();

    // ── 屏幕坐标系的菜单（状态气泡右键）──
    // 一张铺满输出的透明 layer surface（OVERLAY、无视独占区）作底：它的坐标就是输出坐标，
    // 还能接住菜单外的点击（Wayland 不能抓指针，靠它代替）；各级菜单是它的子 surface。
    // surface 同样只建不毁（理由同上）。
    wl_subcompositor* subcomp_ = nullptr;
    struct Catcher {
        wl_surface* surface = nullptr;
        zwlr_layer_surface_v1* layer = nullptr;
        wp_viewport* viewport = nullptr;
        bool configured = false, waiting = false, mapped = false;
        int32_t w = 0, h = 0; // 逻辑尺寸 = 输出大小
        char fakeUserData[1024] = {};
    };
    struct MenuSurf {
        wl_surface* surface = nullptr;
        wl_subsurface* sub = nullptr;
        wp_viewport* viewport = nullptr;
        bool mapped = false;
        int32_t x = 0, y = 0; // 逻辑坐标（相对 catcher）
        char fakeUserData[1024] = {};
    };
    Catcher catcher_;
    MenuSurf menuSurfs_[kMenuLevels];
    bool screenMenu_ = false;
    bool screenMenuShown_ = false; // 本次菜单已有一级显示过（之前来的「隐藏」帧不算收菜单）
    struct PendingMenu {
        int32_t target, x, y; // x y：输出逻辑坐标
        // 落点待定：右键在候选窗（popup / 面板）里，它的屏幕位置不可知，等指针进入底板时取指针的
        // 输出坐标当落点（底板铺满输出，坐标即输出坐标）
        bool locate = false;
    };
    std::optional<PendingMenu> pendingMenu_;
    // 服务端从面板一侧发起的菜单：底板开在面板所在的输出上，菜单帧坐标（面板坐标）减去该输出
    // 原点（位图像素）再贴，指针回报时加回去。气泡菜单两项都是 0（底板由合成器挑输出、坐标即
    // 输出坐标）。
    uint32_t catcherOutput_ = 0; // registry 名；0 = 交给合成器挑
    int32_t menuOriginX_ = 0, menuOriginY_ = 0;
    PanelMenuGate panelMenu_; // 最近一次转给服务端的面板按下 / 松开
    std::function<void()> onLayerShell_;
    std::function<void()> onWorkChanged_;
    // 底板还没 configure 就到了的菜单帧：映射后补贴
    struct QueuedMenu {
        SharedFrame frame;
        OverlayFramePayload geo;
    };
    std::optional<QueuedMenu> queuedMenus_[kMenuLevels];
    void openPanelMenu();
    std::unique_ptr<fcitx::EventSourceTime> menuWait_; // 请求后迟迟没有菜单帧
    std::unique_ptr<fcitx::EventSourceTime> menuIdle_; // 无活动
    std::unique_ptr<fcitx::EventSourceTime> menuMax_;  // 绝对上限（底板挡着整个桌面的鼠标）
    wl_surface* menuPtrSurface_ = nullptr;
    int32_t menuPtrOffX_ = 0, menuPtrOffY_ = 0;
    void openScreenMenu(int32_t target, int32_t x, int32_t y, bool locate = false);
    /// 候选窗里的右键：有 layer-shell 时开在屏幕坐标系（候选窗不动、点菜单外能收），落点取
    /// 指针进入底板时的位置；没有 layer-shell 时退回合成进 popup 位图（旧路径）。
    void openContextMenu(int32_t target);
    void showCatcher();
    void catcherConfigured(uint32_t serial, uint32_t w, uint32_t h);
    void sendPendingMenu();
    bool showScreenMenuLevel(uint32_t level, const SharedFrame& f, const OverlayFramePayload& p);
    void hideScreenMenuLevel(uint32_t level);
    /// 本端收起屏幕菜单；`reason` 非空时报服务端。
    void closeScreenMenu(const char* reason);
    bool screenMenuOffset(wl_surface* s, int32_t& ox, int32_t& oy) const;
    void reportScreenMenuPointer(uint32_t event, uint32_t button);
    void armTimer(std::unique_ptr<fcitx::EventSourceTime>& t, uint32_t ms, void (WaylandCandidatePanel::*fn)());
    static void disarm(std::unique_ptr<fcitx::EventSourceTime>& t);
    void onMenuWaitExpired();
    void onMenuIdleExpired();
    void onMenuMaxExpired();
    uint32_t menuIdleMs_ = kDefaultMenuIdleTimeoutMs;
    uint32_t menuMaxMs_ = kDefaultMenuMaxGrabMs;
    double layerPtrX_ = 0, layerPtrY_ = 0;
    void reportMenuPointer(uint32_t event, uint32_t button);
    Callbacks cb_;
    std::vector<CandidateHitRect> rects_;
    bool pointerInside_ = false;
    double pointerX_ = 0, pointerY_ = 0; // 位图的逻辑坐标（主 / 子 surface 的局部坐标已换算）
    int32_t hover_ = -1;
    ClickGate click_; // 左键点选：按下记命中，松开同一项才选
    DecorClickGate decorClick_; // 皮肤装饰点击：候选命中之外、按下松开同一块装饰
    /// 指针在位图里的命中（kNoHit / 候选下标 / 翻页 -1 -2）。
    int32_t hitAtPointer() const;
    /// 指针下可点的皮肤装饰（`decorAt`），点在候选 / 翻页按钮上的不算，没有返回 -1。
    int32_t decorAtPointer() const;
    /// 指针的位图像素坐标（装饰点击的拖动阈值按它判）。
    std::pair<int32_t, int32_t> pointerPx() const;
    void setHover(int32_t index);
    void updateHover();
    bool warnedProbe_ = false;
    bool warnedNoIm_ = false;

    friend struct WaylandListeners;
};

} // namespace windlinux
