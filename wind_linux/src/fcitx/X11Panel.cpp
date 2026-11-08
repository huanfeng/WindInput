#include "X11Panel.h"

#include "ExtProtocol.h"
#include "GnomeCaretGeom.h"
#include "Menu.h"

#include <fcitx-utils/log.h>

#include <algorithm>
#include <cmath>
#include <cstdlib>
#include <cstring>
#include <string>
#include <time.h>
#include <xcb/randr.h>
#include <xcb/shape.h>

FCITX_DECLARE_LOG_CATEGORY(windinput_log);
#define WIND_DEBUG() FCITX_LOGC(windinput_log, Debug)
#define WIND_WARN() FCITX_LOGC(windinput_log, Warn)
#define WIND_INFO() FCITX_LOGC(windinput_log, Info)

namespace windlinux {

namespace {

xcb_atom_t internAtom(xcb_connection_t* c, const std::string& name)
{
    auto cookie = xcb_intern_atom(c, 0, uint16_t(name.size()), name.c_str());
    xcb_intern_atom_reply_t* r = xcb_intern_atom_reply(c, cookie, nullptr);
    xcb_atom_t a = r ? r->atom : xcb_atom_t(XCB_ATOM_NONE);
    std::free(r);
    return a;
}

/// 找一个 32 位 TrueColor visual（ARGB）；没有返回 nullptr。
xcb_visualtype_t* findArgbVisual(xcb_screen_t* s)
{
    for (auto d = xcb_screen_allowed_depths_iterator(s); d.rem; xcb_depth_next(&d)) {
        if (d.data->depth != 32) {
            continue;
        }
        for (auto v = xcb_depth_visuals_iterator(d.data); v.rem; xcb_visualtype_next(&v)) {
            if (v.data->_class == XCB_VISUAL_CLASS_TRUE_COLOR) {
                return v.data;
            }
        }
    }
    return nullptr;
}

/// 停掉一个一次性计时器（不销毁：调用方可能正处在它的回调里）。
void disarm(std::unique_ptr<fcitx::EventSourceTime>& t)
{
    if (t) {
        t->setEnabled(false);
    }
}

bool armed(const std::unique_ptr<fcitx::EventSourceTime>& t)
{
    return t && t->isEnabled();
}

/// 该深度的 ZPixmap 每像素位数（X 服务器声明的格式表）。
int bitsPerPixel(xcb_connection_t* c, uint8_t depth)
{
    const xcb_setup_t* setup = xcb_get_setup(c);
    for (auto f = xcb_setup_pixmap_formats_iterator(setup); f.rem; xcb_format_next(&f)) {
        if (f.data->depth == depth) {
            return f.data->bits_per_pixel;
        }
    }
    return 0;
}

} // namespace

X11CandidatePanel::X11CandidatePanel(fcitx::EventLoop& loop, Callbacks cb)
    : loop_(loop), cb_(std::move(cb)),
      menuIdleMs_(menuIdleTimeoutMs(std::getenv("WIND_MENU_IDLE_TIMEOUT_MS"))),
      menuMaxGrabMs_(menuMaxGrabMs(std::getenv("WIND_MENU_MAX_GRAB_MS")))
{
    cand_.instance = "wind-candidate";
    cand_.interactive = true;
    overlays_[OVERLAY_KIND_TOOLTIP - 1].instance = "wind-tooltip";
    overlays_[OVERLAY_KIND_STATUS - 1].instance = "wind-status";
    overlays_[OVERLAY_KIND_TOAST - 1].instance = "wind-toast";
    for (Surface& o : overlays_) {
        o.interactive = true;
        o.overlay = true;
    }
    static const char* const kMenuNames[kMenuLevels] = {"wind-menu-0", "wind-menu-1", "wind-menu-2",
                                                        "wind-menu-3", "wind-menu-4", "wind-menu-5"};
    for (size_t k = 0; k < kMenuLevels; ++k) {
        menus_[k].instance = kMenuNames[k];
        menus_[k].interactive = true; // 收鼠标（抓指针期间事件落在菜单窗口上）
    }
    panels_[OVERLAY_KIND_TOOLBAR - OVERLAY_KIND_TOOLBAR].instance = "wind-toolbar";
    panels_[OVERLAY_KIND_SOFTKEYBOARD - OVERLAY_KIND_TOOLBAR].instance = "wind-softkeyboard";
    for (Surface& p : panels_) {
        p.interactive = true;
        p.overlay = true;
        p.panel = true;
    }
}

X11CandidatePanel::~X11CandidatePanel()
{
    cb_ = Callbacks{}; // 析构途中不再回调引擎（摘面板会补报 LEAVE）
    dropConnection();
}

bool X11CandidatePanel::ensureConnection()
{
    if (conn_ && !xcb_connection_has_error(conn_)) {
        return true;
    }
    dropConnection();
    const char* display = std::getenv("DISPLAY");
    if (!display || !*display) {
        if (!warnedNoDisplay_) {
            WIND_WARN() << "没有 DISPLAY：X11 候选窗不可用（Wayland 原生应用走 input popup，见 WaylandPanel）";
            warnedNoDisplay_ = true;
        }
        return false;
    }
    conn_ = xcb_connect(nullptr, &screenNum_);
    if (xcb_connection_has_error(conn_)) {
        WIND_WARN() << "连不上 X 服务器 " << display << "，候选窗不显示";
        xcb_disconnect(conn_);
        conn_ = nullptr;
        return false;
    }
    auto it = xcb_setup_roots_iterator(xcb_get_setup(conn_));
    for (int i = 0; i < screenNum_ && it.rem; ++i) {
        xcb_screen_next(&it);
    }
    screen_ = it.data;
    const xcb_query_extension_reply_t* shape = xcb_get_extension_data(conn_, &xcb_shape_id);
    shapeAvailable_ = shape && shape->present;
    // RandR ≥ 1.3（GetScreenResourcesCurrent / GetOutputPrimary）：按输出取名字与工作区。
    randrAvailable_ = false;
    const xcb_query_extension_reply_t* randr = xcb_get_extension_data(conn_, &xcb_randr_id);
    if (randr && randr->present) {
        auto* v =
            xcb_randr_query_version_reply(conn_, xcb_randr_query_version(conn_, 1, 3), nullptr);
        randrAvailable_ = v && (v->major_version > 1 || v->minor_version >= 3);
        std::free(v);
    }
    ioEvent_ = loop_.addIOEvent(xcb_get_file_descriptor(conn_), fcitx::IOEventFlag::In,
                                [this](fcitx::EventSourceIO*, int, fcitx::IOEventFlags) {
                                    onReadable();
                                    return true;
                                });
    watchRoot();
    return true;
}

void X11CandidatePanel::dropConnection()
{
    // 只从 ensureConnection 进来，而后者只在贴帧路径（push 帧，经 EventDispatcher）上调——
    // 不在 X 的 IO 回调、也不在任何计时器回调里（查指针 / 工作区连接坏了只返回空，不重连），
    // 所以销毁 IO 事件源是安全的。计时器仍一律只停不毁：将来谁从回调里走进来也不至于释放
    // 正在执行的自己（菜单空闲计时器曾经就是：dropMenu → resyncAfterMenu → 查指针 → 重连）。
    ioEvent_.reset();
    disarm(grabRetry_);
    disarm(menuIdle_);
    disarm(menuDeadline_);
    pointerGrabbed_ = false; // 连接一断，服务器端的抓取随之失效
    pendingMotion_.reset();
    menuActive_ = false;
    // 交互态随窗口一起作废（拖动中断线：隐式抓取随连接消失，不会有松开事件来收尾）。
    disarm(hoverDefer_);
    disarm(tipLeave_);
    disarm(statusMenuWait_);
    statusMenuHold_ = false;
    drag_ = Drag{};
    dragOutputs_.clear();
    disarm(activeCheck_);
    activeWindow_ = 0;
    activeWatched_ = false; // 订阅随连接一起没了
    if (conn_) {
        if (dragCursor_) {
            xcb_free_cursor(conn_, dragCursor_);
            dragCursor_ = 0;
        }
        destroyWindow(cand_);
        for (Surface& o : overlays_) {
            destroyWindow(o);
        }
        for (Surface& m : menus_) {
            destroyWindow(m);
        }
        for (Surface& p : panels_) {
            destroyWindow(p);
        }
        pendingPanelMotion_.reset();
        xcb_disconnect(conn_);
        conn_ = nullptr;
    }
    screen_ = nullptr;
}

bool X11CandidatePanel::hasCompositor()
{
    xcb_atom_t sel = internAtom(conn_, "_NET_WM_CM_S" + std::to_string(screenNum_));
    if (sel == XCB_ATOM_NONE) {
        return false;
    }
    auto r = xcb_get_selection_owner_reply(conn_, xcb_get_selection_owner(conn_, sel), nullptr);
    bool owned = r && r->owner != XCB_NONE;
    std::free(r);
    return owned;
}

bool X11CandidatePanel::ensureWindow(Surface& s, bool argb)
{
    if (s.window && s.argb == argb) {
        return true;
    }
    destroyWindow(s);
    xcb_visualid_t visual = screen_->root_visual;
    s.depth = screen_->root_depth;
    s.colormap = 0;
    if (argb) {
        xcb_visualtype_t* v = findArgbVisual(screen_);
        if (!v) {
            argb = false; // 有合成器却没有 32 位 visual：极少见，按无合成器处理
        } else {
            visual = v->visual_id;
            s.depth = 32;
            s.colormap = xcb_generate_id(conn_);
            xcb_create_colormap(conn_, XCB_COLORMAP_ALLOC_NONE, s.colormap, screen_->root, visual);
        }
    }
    s.argb = argb;
    s.window = xcb_generate_id(conn_);
    // 值的顺序必须与掩码位从低到高一致：BACK_PIXEL, BORDER_PIXEL, OVERRIDE_REDIRECT,
    // EVENT_MASK, COLORMAP。ARGB visual 与根窗口深度不同，border/colormap 必须显式给，
    // 否则 CreateWindow 报 BadMatch。
    uint32_t mask = XCB_CW_BACK_PIXEL | XCB_CW_BORDER_PIXEL | XCB_CW_OVERRIDE_REDIRECT
        | XCB_CW_EVENT_MASK | XCB_CW_COLORMAP;
    uint32_t events = 0;
    if (s.interactive) {
        events = XCB_EVENT_MASK_BUTTON_PRESS | XCB_EVENT_MASK_POINTER_MOTION
            | XCB_EVENT_MASK_LEAVE_WINDOW;
    }
    if (&s == &cand_) {
        // 左键点选在松开时生效（ClickGate）；按下时的隐式抓取保证松开落回本窗口
        events |= XCB_EVENT_MASK_BUTTON_RELEASE;
    }
    if (s.overlay) {
        // 拖动靠按下时的隐式抓取收移动与松开（指针拖出窗口也照收，松手即结束——不会卡住）；
        // 进入用于悬停提示「指针已挪进来」。
        events |= XCB_EVENT_MASK_BUTTON_RELEASE | XCB_EVENT_MASK_ENTER_WINDOW;
    }
    uint32_t values[] = {
        0,
        0,
        1, // override-redirect：不归窗口管理器管，不抢焦点、不加边框
        events,
        s.colormap ? s.colormap : screen_->default_colormap,
    };
    xcb_create_window(conn_, s.depth, s.window, screen_->root, 0, 0, 1, 1, 0,
                      XCB_WINDOW_CLASS_INPUT_OUTPUT, visual, mask, values);
    // 给截图 / 调试工具认窗用（`xdotool search --classname wind-candidate`）。
    std::string cls = std::string(s.instance) + '\0' + "WindInput" + '\0';
    xcb_change_property(conn_, XCB_PROP_MODE_REPLACE, s.window, XCB_ATOM_WM_CLASS,
                        XCB_ATOM_STRING, 8, uint32_t(cls.size()), cls.data());
    std::string name = std::string("WindInput ") + s.instance;
    xcb_change_property(conn_, XCB_PROP_MODE_REPLACE, s.window, XCB_ATOM_WM_NAME,
                        XCB_ATOM_STRING, 8, uint32_t(name.size()), name.data());
    // 窗口类型提示：合成器据此不给它加阴影/动画（阴影已画在位图里）。
    xcb_atom_t type = internAtom(conn_, "_NET_WM_WINDOW_TYPE");
    xcb_atom_t kind = internAtom(conn_, s.interactive && !s.overlay
                                            ? "_NET_WM_WINDOW_TYPE_POPUP_MENU"
                                            : "_NET_WM_WINDOW_TYPE_TOOLTIP");
    if (type && kind) {
        xcb_change_property(conn_, XCB_PROP_MODE_REPLACE, s.window, type, XCB_ATOM_ATOM, 32, 1,
                            &kind);
    }
    // 工具栏 / 软键盘点了不能抢焦点（焦点一走，Fcitx5 的输入上下文就换了）：override-redirect
    // 本就不归窗口管理器管，再声明 WM_HINTS.input = False，且不挂 WM_PROTOCOLS（没有 WM_TAKE_FOCUS）。
    if (s.panel) {
        const uint32_t hints[9] = {1 /* InputHint */, 0 /* input = False */};
        xcb_change_property(conn_, XCB_PROP_MODE_REPLACE, s.window, XCB_ATOM_WM_HINTS,
                            XCB_ATOM_WM_HINTS, 32, 9, hints);
    }
    // 不收鼠标的窗口：输入区设为空，点击穿透到下面的应用。
    if (!s.interactive && shapeAvailable_) {
        xcb_shape_rectangles(conn_, XCB_SHAPE_SO_SET, XCB_SHAPE_SK_INPUT,
                             XCB_CLIP_ORDERING_UNSORTED, s.window, 0, 0, 0, nullptr);
    }
    s.gc = xcb_generate_id(conn_);
    xcb_create_gc(conn_, s.gc, s.window, 0, nullptr);
    WIND_DEBUG() << s.instance << " 已建："
                 << (s.argb ? "ARGB（有合成器）" : "根 visual + XShape（无合成器）");
    return true;
}

void X11CandidatePanel::destroyWindow(Surface& s)
{
    s.hideTimer.reset();
    if (s.panel) {
        panelPointerGone(s);
        s.reqX = s.reqY = INT32_MIN;
    }
    if (!conn_) {
        return;
    }
    if (s.pixmap) {
        xcb_free_pixmap(conn_, s.pixmap);
        s.pixmap = 0;
    }
    if (s.gc) {
        xcb_free_gc(conn_, s.gc);
        s.gc = 0;
    }
    if (s.window) {
        xcb_destroy_window(conn_, s.window);
        s.window = 0;
    }
    if (s.colormap) {
        xcb_free_colormap(conn_, s.colormap);
        s.colormap = 0;
    }
    s.pixW = s.pixH = 0;
    s.mapped = false;
    s.hovered = false;
    if (drag_.active && &s == surfaceOf(drag_.kind)) {
        drag_ = Drag{}; // 窗口没了，它的隐式抓取与松开事件也就没了
        dragOutputs_.clear();
    }
    if (&s == &menus_[0]) {
        pointerGrabbed_ = false; // 抓取窗口销毁，X 服务器随之放掉抓取
    }
}

void X11CandidatePanel::upload(Surface& s, const SharedFrame& f)
{
    if (s.pixW != f.width || s.pixH != f.height) {
        if (s.pixmap) {
            xcb_free_pixmap(conn_, s.pixmap);
        }
        s.pixmap = xcb_generate_id(conn_);
        xcb_create_pixmap(conn_, s.depth, s.pixmap, s.window, uint16_t(f.width),
                          uint16_t(f.height));
        s.pixW = f.width;
        s.pixH = f.height;
    }
    // 服务端给的是 BGRA 预乘 alpha，小端下恰好就是 32 位 ARGB visual 的像素格式（合成器
    // 也要预乘）。深度 24 的 ZPixmap 同样是每像素 4 字节 BGRx，alpha 字节被忽略。
    const uint32_t rowBytes = f.width * 4;
    const uint8_t* src = f.bgra.data();
    std::vector<uint8_t> packed;
    if (f.stride != rowBytes) { // 去掉行尾填充，PutImage 要紧凑行
        packed.resize(size_t(rowBytes) * f.height);
        for (uint32_t y = 0; y < f.height; ++y) {
            std::memcpy(&packed[size_t(y) * rowBytes], src + size_t(y) * f.stride, rowBytes);
        }
        src = packed.data();
    }
    // 单个请求有长度上限（无 BIG-REQUESTS 时 256KB），按行切块。
    uint32_t maxBytes = xcb_get_maximum_request_length(conn_) * 4 - 64;
    uint32_t rowsPerChunk = std::max<uint32_t>(1, maxBytes / rowBytes);
    for (uint32_t y = 0; y < f.height; y += rowsPerChunk) {
        uint32_t rows = std::min(rowsPerChunk, f.height - y);
        xcb_put_image(conn_, XCB_IMAGE_FORMAT_Z_PIXMAP, s.pixmap, s.gc, uint16_t(f.width),
                      uint16_t(rows), 0, int16_t(y), 0, s.depth, rows * rowBytes,
                      src + size_t(y) * rowBytes);
    }
    // 位图挂成窗口背景：重绘（被遮挡后露出）由 X 服务器自己做，不用处理 Expose。
    xcb_change_window_attributes(conn_, s.window, XCB_CW_BACK_PIXMAP, &s.pixmap);
    xcb_clear_area(conn_, 0, s.window, 0, 0, uint16_t(f.width), uint16_t(f.height));
}

void X11CandidatePanel::applyShape(Surface& s, const SharedFrame& f, uint8_t kind)
{
    if (!shapeAvailable_) {
        return;
    }
    // 按行扫 alpha，把 alpha >= 128 的连续段合成矩形：圆角外、阴影淡处被抠掉。
    std::vector<xcb_rectangle_t> rects;
    for (uint32_t y = 0; y < f.height; ++y) {
        const uint8_t* row = f.bgra.data() + size_t(y) * f.stride;
        uint32_t x = 0;
        while (x < f.width) {
            while (x < f.width && row[x * 4 + 3] < 128) {
                ++x;
            }
            uint32_t start = x;
            while (x < f.width && row[x * 4 + 3] >= 128) {
                ++x;
            }
            if (x > start) {
                rects.push_back({int16_t(start), int16_t(y), uint16_t(x - start), 1});
            }
        }
    }
    xcb_shape_rectangles(conn_, XCB_SHAPE_SO_SET, kind, XCB_CLIP_ORDERING_YX_SORTED, s.window, 0,
                         0, uint32_t(rects.size()), rects.data());
}

Rect X11CandidatePanel::workArea() const
{
    return Rect{0, 0, screen_->width_in_pixels, screen_->height_in_pixels};
}

bool X11CandidatePanel::present(Surface& s, const SharedFrame& f, const Rect& r, bool raise)
{
    uint32_t geo[] = {uint32_t(r.x), uint32_t(r.y), f.width, f.height, XCB_STACK_MODE_ABOVE};
    uint16_t mask = XCB_CONFIG_WINDOW_X | XCB_CONFIG_WINDOW_Y | XCB_CONFIG_WINDOW_WIDTH
        | XCB_CONFIG_WINDOW_HEIGHT;
    if (raise) {
        mask |= XCB_CONFIG_WINDOW_STACK_MODE;
    }
    xcb_configure_window(conn_, s.window, mask, geo);
    s.x = r.x;
    s.y = r.y;
    upload(s, f);
    if (!s.argb) {
        applyShape(s, f, XCB_SHAPE_SK_BOUNDING); // 输入区随之裁掉
    } else if (s.overlay && !s.panel) {
        // 真透明时外形不裁，但阴影 / 圆角外的透明处不该接住点击（点到的是下面的应用）。
        // 工具栏 / 软键盘不裁：淡出时服务端把整窗透明度乘进了像素，假隐藏（悬停唤回）那帧
        // 处处 alpha≈1，按 alpha 裁出的输入区是空的——指针穿透下去，悬停永远唤不回来。
        // 整个矩形都收指针，同 Windows 上 alpha=1 的分层窗口仍可命中。
        applyShape(s, f, XCB_SHAPE_SK_INPUT);
    }
    if (!s.mapped) {
        if (s.overlay) {
            // 悬停基线取「出现那一刻」的指针位置：弹在静止指针下不算悬停（见 HoverGate）。
            s.hovered = false;
            if (auto* p = xcb_query_pointer_reply(conn_, xcb_query_pointer(conn_, screen_->root),
                                                  nullptr)) {
                s.gate.rebase(p->root_x, p->root_y);
                std::free(p);
            }
        }
        xcb_map_window(conn_, s.window);
        s.mapped = true;
    }
    xcb_flush(conn_);
    WIND_DEBUG() << s.instance << " " << f.width << "x" << f.height << " @ (" << r.x << ","
                 << r.y << ")";
    return true;
}

void X11CandidatePanel::unmap(Surface& s)
{
    s.hideTimer.reset();
    unmapWindow(s);
}

void X11CandidatePanel::unmapWindow(Surface& s)
{
    if (conn_ && s.window && s.mapped) {
        xcb_unmap_window(conn_, s.window);
        xcb_flush(conn_);
    }
    s.mapped = false;
    s.hovered = false;
    // 拖动中被摘掉（服务端推隐藏帧 / 服务重启）：X 在窗口不可见时自动放掉隐式抓取，不会再有
    // 松开事件来收尾——就地归位，不报落点（用户没松手，谈不上「摆到了哪」）。
    if (drag_.active && &s == surfaceOf(drag_.kind)) {
        endDrag(false);
    }
    if (s.panel) {
        panelPointerGone(s); // 指针停在面板上时它被摘了：X 不会再发离开
    }
}

bool X11CandidatePanel::show(const SharedFrame& f, int32_t x, int32_t y, bool absolute)
{
    if (f.width == 0 || f.height == 0 || f.bgra.empty() || !ensureConnection()) {
        return false;
    }
    if (!ensureWindow(cand_, hasCompositor())) {
        return false;
    }
    if (bitsPerPixel(conn_, cand_.depth) != 32) {
        WIND_WARN() << "深度 " << int(cand_.depth) << " 的像素格式不是 32 bpp，候选窗不显示";
        return false;
    }
    Rect r = placePanel(x, y, int32_t(f.width), int32_t(f.height), workArea(), absolute);
    candShiftX_ = r.x - x;
    candShiftY_ = r.y - y;
    return present(cand_, f, r);
}

void X11CandidatePanel::hide()
{
    unmap(cand_);
    rects_.clear();
    hover_ = -1;
    click_.cancel();
    cancelTipTimers();
    // tooltip 挂在候选窗上：候选窗藏了它必须一起藏（服务端也会推隐藏帧，这里兜底）。
    hideOverlay(OVERLAY_KIND_TOOLTIP);
}

X11CandidatePanel::Surface* X11CandidatePanel::overlay(uint32_t kind)
{
    if (kind < 1 || kind > kOverlayCount) {
        return nullptr;
    }
    return &overlays_[kind - 1];
}

X11CandidatePanel::Surface* X11CandidatePanel::panelSurface(uint32_t kind)
{
    if (!isPanelOverlayKind(kind)) {
        return nullptr;
    }
    return &panels_[kind - OVERLAY_KIND_TOOLBAR];
}

X11CandidatePanel::Surface* X11CandidatePanel::surfaceOf(uint32_t kind)
{
    Surface* s = overlay(kind);
    return s ? s : panelSurface(kind);
}

bool X11CandidatePanel::showOverlay(uint32_t kind, const SharedFrame& f,
                                    const OverlayFramePayload& p)
{
    if (isPanelOverlayKind(kind)) {
        return showPanel(kind, f, p);
    }
    Surface* s = overlay(kind);
    if (!s || f.width == 0 || f.height == 0 || f.bgra.empty() || !ensureConnection()) {
        return false;
    }
    if (!ensureWindow(*s, hasCompositor())) {
        return false;
    }
    if (bitsPerPixel(conn_, s->depth) != 32) {
        return false;
    }
    // 尺寸取像素那一份（通知与 SHM 间无锁，读到的可能已是更新的一帧），内容盒偏移随之不变。
    OverlayFramePayload geo = p;
    geo.width = f.width;
    geo.height = f.height;
    Rect r;
    if (dragging(OVERLAY_KIND_STATUS) && kind == OVERLAY_KIND_STATUS && s->mapped) {
        // 拖动中：只换像素、不重新落位，免得状态刷新把气泡从指针下拽走（同 Windows
        // `StatusTip::show` 的 drag_pin 分支）。内容盒偏移变了就按新偏移保住内容左上。
        r = Rect{s->x + s->geo.contentX - geo.contentX, s->y + s->geo.contentY - geo.contentY,
                 int32_t(f.width), int32_t(f.height)};
    } else {
        r = placeOverlay(geo, workArea(), candShiftX_, candShiftY_);
    }
    s->geo = geo;
    present(*s, f, r);
    armHide(kind);
    return true;
}

void X11CandidatePanel::armHide(uint32_t kind)
{
    Surface* s = overlay(kind);
    if (!s) {
        return;
    }
    s->hideTimer.reset();
    // 交互中不计时：交互结束时 refreshHold 再给满一份（同 Windows 的「交互结束重新计时」）。
    if (s->geo.durationMs <= 0 || !s->mapped || held(kind)) {
        return;
    }
    s->hideTimer = loop_.addTimeEvent(
        CLOCK_MONOTONIC, fcitx::now(CLOCK_MONOTONIC) + uint64_t(s->geo.durationMs) * 1000, 0,
        [this, kind](fcitx::EventSourceTime*, uint64_t) {
            // 回调里不能销毁正在执行的自己：只摘窗口，计时器对象留到下次 show/hide 再换。
            if (Surface* o = overlay(kind); o && !held(kind)) {
                unmapWindow(*o);
            }
            return true;
        });
}

bool X11CandidatePanel::held(uint32_t kind) const
{
    const Surface& s = overlays_[kind - 1];
    switch (kind) {
    case OVERLAY_KIND_STATUS:
        return s.hovered || dragging(OVERLAY_KIND_STATUS) || statusMenuHold_;
    case OVERLAY_KIND_TOAST:
        return s.hovered;
    default:
        return false; // tooltip 常驻到下一帧，去留跟着候选悬停走
    }
}

void X11CandidatePanel::refreshHold(uint32_t kind)
{
    Surface* s = overlay(kind);
    if (!s || !s->mapped) {
        return;
    }
    if (held(kind)) {
        s->hideTimer.reset();
    } else if (!s->hideTimer) {
        armHide(kind);
    }
}

void X11CandidatePanel::hideOverlay(uint32_t kind)
{
    if (Surface* ps = panelSurface(kind)) {
        unmap(*ps);
        return;
    }
    Surface* s = overlay(kind);
    if (!s) {
        return;
    }
    unmap(*s);
    if (kind == OVERLAY_KIND_TOOLTIP) {
        disarm(tipLeave_);
        // 指针停在提示上时，本端报的悬停一直留在它所属的候选。提示被服务端收掉（又打了字、
        // 候选刷新了）之后指针不在候选窗上，就没有别的事件会纠正它——下次移回同一个候选时
        // 会因「与上次报的相同」而不报，提示再也出不来。这里就地忘掉（服务端那边已不在悬停）。
        if (hover_ >= 0 && !pointerOn(cand_, pointerPosition())) {
            hover_ = -1;
            disarm(hoverDefer_);
        }
    }
}

void X11CandidatePanel::hideAllOverlays()
{
    statusMenuHold_ = false;
    disarm(statusMenuWait_);
    for (uint32_t k = 1; k <= kOverlayCount; ++k) {
        hideOverlay(k);
    }
}

std::optional<std::pair<int32_t, int32_t>> X11CandidatePanel::statusContentOrigin() const
{
    const Surface& s = overlays_[OVERLAY_KIND_STATUS - 1];
    if (!s.mapped) {
        return std::nullopt;
    }
    return std::make_pair(s.x + s.geo.contentX, s.y + s.geo.contentY);
}

void X11CandidatePanel::onReadable()
{
    // 每轮都重查 conn_：事件处理会回调到引擎（上报失败时本端收菜单…），这条链现在不会断开
    // 连接，但别让循环的正确性依赖「回调链永远不碰连接」。
    while (conn_) {
        xcb_generic_event_t* ev = xcb_poll_for_event(conn_);
        if (!ev) {
            break;
        }
        handleEvent(ev);
        std::free(ev);
    }
    flushMenuMotion();
    flushPanelMotion();
    if (conn_ && xcb_connection_has_error(conn_) && ioEvent_) {
        WIND_WARN() << "X 连接断开，下一帧重连";
        // 不能在 IO 回调里销毁自己所属的 EventSourceIO，只先停掉它（断开的 fd 恒可读，
        // 不停会空转）。真正的清理由下一次 show 里 ensureConnection 见 has_error 时做。
        ioEvent_->setEnabled(false);
    }
}

void X11CandidatePanel::handleEvent(xcb_generic_event_t* ev)
{
    const uint8_t type = ev->response_type & 0x7F;
    if (type == XCB_PROPERTY_NOTIFY) {
        // 全屏检测：根窗口换了活动窗口，或活动窗口的 _NET_WM_STATE 变了（菜单开着也照收）。
        auto* e = reinterpret_cast<xcb_property_notify_event_t*>(ev);
        if (screen_ && e->window == screen_->root && e->atom == atomActiveWindow_) {
            refreshActiveWindow();
        } else if (e->window == activeWindow_ && activeWindow_ && e->atom == atomWmState_) {
            checkFullscreen();
        }
        return;
    }
    if (menuOpen() && type != 0) {
        // 菜单抓指针时，指针所在的面板收到一个离开（mode = Grab）：照样转给服务端，否则它的
        // 「指针在面板内」一直挂着（菜单开着期间面板收不到别的事件，收起后指针多半已在别处）。
        if (type == XCB_LEAVE_NOTIFY) {
            auto* e = reinterpret_cast<xcb_leave_notify_event_t*>(ev);
            const uint32_t k = overlayKindOf(e->event);
            if (isPanelOverlayKind(k)) {
                handlePanelEvent(k, ev);
                return;
            }
        }
        handleMenuEvent(ev);
        return;
    }
    // 浮层窗口上的事件：按下 / 松开 / 移动 / 进入 / 离开的 event 字段都在同一偏移（X11 协议里
    // 这几种事件结构的前 16 字节同形），按 motion 读窗口即可。
    switch (ev->response_type & 0x7F) {
    case XCB_BUTTON_PRESS:
    case XCB_BUTTON_RELEASE:
    case XCB_MOTION_NOTIFY:
    case XCB_ENTER_NOTIFY:
    case XCB_LEAVE_NOTIFY:
        if (uint32_t k = overlayKindOf(reinterpret_cast<xcb_motion_notify_event_t*>(ev)->event)) {
            if (isPanelOverlayKind(k)) {
                handlePanelEvent(k, ev);
            } else {
                handleOverlayEvent(k, ev);
            }
            return;
        }
        break;
    default:
        break;
    }
    switch (ev->response_type & 0x7F) {
    case XCB_BUTTON_PRESS: {
        auto* e = reinterpret_cast<xcb_button_press_event_t*>(ev);
        if (e->event != cand_.window) {
            break;
        }
        disarm(hoverDefer_); // 点下去就以点的这一处为准，别让延后的悬停随后改掉它
        if (e->detail == 1) {
            click_.press(hitTest(rects_, e->event_x, e->event_y)); // 松开时才选，见 ClickGate
        } else if (e->detail == 3 && cb_.contextMenu) {
            // 右键：命中候选 → 候选菜单，空白 / 翻页按钮 → 主菜单（同 Windows right_click）。
            // 悬停先清掉：菜单开着时候选窗收不到移动事件，残留的高亮会一直挂着。
            setHover(-1);
            cb_.contextMenu(contextMenuTarget(hitTest(rects_, e->event_x, e->event_y)),
                            e->root_x, e->root_y);
        } else if ((e->detail == 4 || e->detail == 5) && cb_.scroll) {
            cb_.scroll(e->detail == 4 ? 120 : -120);
        }
        break;
    }
    case XCB_BUTTON_RELEASE: {
        auto* e = reinterpret_cast<xcb_button_release_event_t*>(ev);
        if (e->event != cand_.window || e->detail != 1) {
            break;
        }
        const int32_t idx = click_.release(hitTest(rects_, e->event_x, e->event_y));
        if (idx != kNoHit && cb_.select) {
            cb_.select(idx);
        }
        break;
    }
    case XCB_MOTION_NOTIFY: {
        auto* e = reinterpret_cast<xcb_motion_notify_event_t*>(ev);
        if (e->event != cand_.window) {
            break;
        }
        int32_t idx = hitTest(rects_, e->event_x, e->event_y);
        // 悬停只报候选（>=0）：命中表里的 -1/-2 是翻页按钮，而悬停协议里 -1 表示「无」。
        candidateHover(idx >= 0 ? idx : -1, std::make_pair(int32_t(e->root_x), int32_t(e->root_y)));
        break;
    }
    case XCB_LEAVE_NOTIFY:
        if (reinterpret_cast<xcb_leave_notify_event_t*>(ev)->event == cand_.window) {
            candidateHover(-1);
        }
        break;
    case 0: {
        auto* e = reinterpret_cast<xcb_generic_error_t*>(ev);
        WIND_DEBUG() << "X 错误 code=" << int(e->error_code) << " major=" << int(e->major_code);
        break;
    }
    default:
        break;
    }
}

void X11CandidatePanel::setHover(int32_t index)
{
    disarm(hoverDefer_);
    if (index == hover_) {
        return;
    }
    hover_ = index;
    if (cb_.hover) {
        cb_.hover(index);
    }
}

} // namespace windlinux

// ── 浮层交互 ──────────────────────────────────────────────────────────

namespace windlinux {

uint32_t X11CandidatePanel::overlayKindOf(xcb_window_t w) const
{
    if (w == 0) {
        return 0;
    }
    for (uint32_t k = 1; k <= kOverlayCount; ++k) {
        if (overlays_[k - 1].window == w) {
            return k;
        }
    }
    for (size_t i = 0; i < kPanelCount; ++i) {
        if (panels_[i].window == w) {
            return OVERLAY_KIND_TOOLBAR + uint32_t(i);
        }
    }
    return 0;
}

void X11CandidatePanel::armTimer(std::unique_ptr<fcitx::EventSourceTime>& t, uint32_t ms,
                                 void (X11CandidatePanel::*fn)())
{
    const uint64_t due = fcitx::now(CLOCK_MONOTONIC) + uint64_t(ms) * 1000;
    if (t) {
        t->setTime(due);
        t->setOneShot();
        return;
    }
    t = loop_.addTimeEvent(CLOCK_MONOTONIC, due, 0,
                           [this, fn](fcitx::EventSourceTime*, uint64_t) {
                               (this->*fn)();
                               return true;
                           });
}

bool X11CandidatePanel::pointerOn(const Surface& s,
                                  const std::optional<std::pair<int32_t, int32_t>>& p) const
{
    return p && s.mapped && rectContains(rectOf(s), p->first, p->second);
}

void X11CandidatePanel::cancelTipTimers()
{
    disarm(hoverDefer_);
    disarm(tipLeave_);
}

void X11CandidatePanel::candidateHover(int32_t raw, std::optional<std::pair<int32_t, int32_t>> at)
{
    const Surface& tip = overlays_[OVERLAY_KIND_TOOLTIP - 1];
    const auto prev = std::exchange(lastPointer_, at);
    const std::optional<int32_t> gap =
        at && tip.mapped ? std::optional<int32_t>(tipGap(at->first, at->second, rectOf(tip))) : std::nullopt;
    const bool toward = prev && gap && tipBest_ && *gap < *tipBest_ &&
                        headingToTip(prev->first, prev->second, at->first, at->second, rectOf(tip));
    if (raw == hover_) {
        disarm(hoverDefer_); // 回到了原处：待报的变化作废
        return;
    }
    const uint32_t ms = hoverDeferMs(hover_, raw, tip.mapped);
    if (ms == 0) {
        setHover(raw);
        return;
    }
    // 已在等：只换目标、不顺延（去往「无」的宽限不能被一路擦过的候选无限拖长）——除非正朝
    // 提示去，那时按 kTipSwitchMs 顺延，停下来才切换。
    pendingHover_ = raw;
    const uint64_t now = fcitx::now(CLOCK_MONOTONIC);
    if (!armed(hoverDefer_)) {
        hoverDue_ = now + uint64_t(ms) * 1000;
        tipBest_ = gap;
        armTimer(hoverDefer_, ms, &X11CandidatePanel::onHoverDeferred);
    } else if (toward) {
        tipBest_ = gap;
        if (now + uint64_t(kTipSwitchMs) * 1000 > hoverDue_) {
            hoverDue_ = now + uint64_t(kTipSwitchMs) * 1000;
            armTimer(hoverDefer_, kTipSwitchMs, &X11CandidatePanel::onHoverDeferred);
        }
    }
}

void X11CandidatePanel::onHoverDeferred()
{
    // 到期时指针已在提示上：撤掉这次变化，悬停留在提示所属的候选（Windows resolve_deferred）。
    if (pointerOn(overlays_[OVERLAY_KIND_TOOLTIP - 1], pointerPosition())) {
        return;
    }
    setHover(pendingHover_);
}

void X11CandidatePanel::onTipLeaveGrace()
{
    if (menuOpen()) {
        return; // 菜单开着：去留等菜单收起时 resyncAfterMenu 再定
    }
    const auto p = pointerPosition();
    const bool onCand = pointerOn(cand_, p);
    const int32_t hit = onCand ? hitTest(rects_, p->first - cand_.x, p->second - cand_.y) : kNoHit;
    const int32_t h =
        tipRecheckHover(pointerOn(overlays_[OVERLAY_KIND_TOOLTIP - 1], p), onCand, hit, hover_);
    if (h != kKeepHover) {
        setHover(h);
    }
}

void X11CandidatePanel::handleOverlayEvent(uint32_t kind, xcb_generic_event_t* ev)
{
    Surface& s = overlays_[kind - 1];
    switch (ev->response_type & 0x7F) {
    case XCB_BUTTON_PRESS: {
        auto* e = reinterpret_cast<xcb_button_press_event_t*>(ev);
        switch (overlayPressAction(kind, e->detail)) {
        case OverlayPress::Drag:
            if (drag_.active) {
                break; // 已在拖别的（不会同时有两个隐式抓取，防御）
            }
            drag_.active = true;
            drag_.kind = kind;
            drag_.grabDx = e->root_x - s.x;
            drag_.grabDy = e->root_y - s.y;
            setDragCursor(true);
            refreshHold(kind);
            break;
        case OverlayPress::Menu:
            if (kind == OVERLAY_KIND_TOOLTIP) {
                cancelTipTimers(); // 菜单开着期间提示去留等菜单收起再定
            } else {
                // 请求到菜单出现之间也不能让气泡到点消失；菜单若没来（服务没了），兜底放开。
                statusMenuHold_ = true;
                armTimer(statusMenuWait_, 2000, &X11CandidatePanel::onStatusMenuWaitExpired);
                refreshHold(kind);
            }
            if (cb_.overlayMenu) {
                cb_.overlayMenu(overlayMenuTarget(kind), e->root_x, e->root_y, e->event_x,
                                e->event_y);
            }
            break;
        case OverlayPress::Close:
            WIND_DEBUG() << s.instance << " 被点击，提前收起";
            unmap(s);
            break;
        case OverlayPress::None:
        case OverlayPress::Forward: // 只有工具栏 / 软键盘会给，三层浮层走不到
            break;
        }
        break;
    }
    case XCB_BUTTON_RELEASE: {
        auto* e = reinterpret_cast<xcb_button_release_event_t*>(ev);
        if (dragging(OVERLAY_KIND_STATUS) && kind == OVERLAY_KIND_STATUS && e->detail == 1) {
            moveDrag(e->root_x, e->root_y);
            endDrag(true);
        }
        break;
    }
    case XCB_MOTION_NOTIFY: {
        auto* e = reinterpret_cast<xcb_motion_notify_event_t*>(ev);
        if (kind == OVERLAY_KIND_STATUS && dragging(OVERLAY_KIND_STATUS)) {
            if (!(e->state & XCB_KEY_BUT_MASK_BUTTON_1)) {
                endDrag(true); // 松开事件丢了（按住期间窗口被重建等）：按当下位置收尾
            } else {
                moveDrag(e->root_x, e->root_y);
            }
            break;
        }
        if (kind == OVERLAY_KIND_TOOLTIP) {
            cancelTipTimers(); // 指针在提示上：待报的悬停变化与离开宽限都作废
        }
        if (s.gate.accept(e->root_x, e->root_y) && !s.hovered) {
            s.hovered = true;
            refreshHold(kind);
        }
        break;
    }
    case XCB_ENTER_NOTIFY:
        if (kind == OVERLAY_KIND_TOOLTIP) {
            cancelTipTimers();
        }
        break;
    case XCB_LEAVE_NOTIFY: {
        auto* e = reinterpret_cast<xcb_leave_notify_event_t*>(ev);
        // 抓取引起的离开（菜单抓指针）不是指针离开；移进自己的子窗口也不是（浮层没有子窗口）。
        if (e->mode != XCB_NOTIFY_MODE_NORMAL || e->detail == XCB_NOTIFY_DETAIL_INFERIOR) {
            break;
        }
        if (kind == OVERLAY_KIND_TOOLTIP) {
            if (hover_ >= 0) {
                armTimer(tipLeave_, kTipGraceMs, &X11CandidatePanel::onTipLeaveGrace);
            }
        } else if (!dragging(OVERLAY_KIND_STATUS) || kind != OVERLAY_KIND_STATUS) {
            s.hovered = false;
            refreshHold(kind); // 交互结束：重新给满一份时长
        }
        break;
    }
    default:
        break;
    }
}

void X11CandidatePanel::moveDrag(int32_t rootX, int32_t rootY)
{
    Surface* sp = surfaceOf(drag_.kind);
    if (!drag_.active || !sp || !conn_ || !sp->window || !sp->mapped) {
        return;
    }
    Surface& s = *sp;
    Rect r;
    if (s.panel) {
        // 面板：窗口跟着指针走，夹进指针所在输出的工作区（可以拖到另一块屏上）。
        const Rect want{rootX - drag_.grabDx, rootY - drag_.grabDy, int32_t(s.pixW),
                        int32_t(s.pixH)};
        std::vector<Rect> rects;
        for (const OutputInfo& o : dragOutputs_) {
            rects.push_back(o.rect);
        }
        const int i = outputIndexAt(rects, rootX, rootY);
        r = clampToWorkArea(want,
                            i >= 0 ? dragOutputs_[i].work : panelWorkArea(want, dragOutputs_));
    } else {
        r = dragOverlay(rootX, rootY, drag_.grabDx, drag_.grabDy, s.geo, workArea());
    }
    if (r.x == s.x && r.y == s.y) {
        return;
    }
    const uint32_t xy[] = {uint32_t(r.x), uint32_t(r.y)};
    xcb_configure_window(conn_, s.window, XCB_CONFIG_WINDOW_X | XCB_CONFIG_WINDOW_Y, xy);
    xcb_flush(conn_);
    s.x = r.x;
    s.y = r.y;
}

void X11CandidatePanel::endDrag(bool report)
{
    if (!drag_.active) {
        return;
    }
    setDragCursor(false);
    const uint32_t kind = drag_.kind;
    drag_ = Drag{};
    Surface* s = surfaceOf(kind);
    if (s && s->panel) {
        // 面板松手只报这一次 `pos.*`（拖动中的移动一个都不回传）。输出名取松手时窗口中心所在的
        // 那块，与 host.display 同源（都是 RandR 名字，拿不到就都不报）。
        if (report && s->mapped && cb_.panelMoved) {
            const Rect r = rectOf(*s);
            std::string output;
            for (const OutputInfo& o : dragOutputs_) {
                if (rectContains(o.rect, r.x + r.w / 2, r.y + r.h / 2)) {
                    output = o.name;
                    break;
                }
            }
            WIND_DEBUG() << s->instance << " 拖动松手，上报内容左上 (" << s->x + s->geo.contentX
                         << "," << s->y + s->geo.contentY << ") output=" << output;
            cb_.panelMoved(kind, s->x + s->geo.contentX, s->y + s->geo.contentY, output);
        }
        dragOutputs_.clear();
        return;
    }
    Surface& st = overlays_[OVERLAY_KIND_STATUS - 1];
    // 松手就报（同 Windows WM_LBUTTONUP，没挪也报）：固定位置模式下它就是新的落点，跟随光标
    // 模式下服务端不落盘——判据只在服务端（`save_status_tip_pos`）。
    if (report && st.mapped && cb_.statusMoved) {
        WIND_DEBUG() << "状态气泡拖动松手，上报内容左上 (" << st.x + st.geo.contentX << ","
                     << st.y + st.geo.contentY << ")";
        cb_.statusMoved(st.x + st.geo.contentX, st.y + st.geo.contentY);
    }
    refreshHold(OVERLAY_KIND_STATUS);
}

void X11CandidatePanel::setDragCursor(bool on)
{
    Surface* sp = surfaceOf(drag_.kind);
    if (!conn_ || !sp || !sp->window) {
        return;
    }
    Surface& s = *sp;
    if (on && !dragCursor_) {
        // 光标字体里的 fleur（四向箭头，XC_fleur = 52，掩码是下一个字形），同 Windows IDC_SIZEALL。
        xcb_font_t font = xcb_generate_id(conn_);
        xcb_open_font(conn_, font, 6, "cursor");
        dragCursor_ = xcb_generate_id(conn_);
        xcb_create_glyph_cursor(conn_, dragCursor_, font, font, 52, 53, 0, 0, 0, 0xFFFF, 0xFFFF,
                                0xFFFF);
        xcb_close_font(conn_, font);
    }
    const uint32_t cursor = on ? dragCursor_ : uint32_t(XCB_NONE);
    xcb_change_window_attributes(conn_, s.window, XCB_CW_CURSOR, &cursor);
    xcb_flush(conn_);
}

void X11CandidatePanel::onStatusMenuWaitExpired()
{
    if (menuOpen()) {
        return; // 菜单来了：它收起时再放开
    }
    statusMenuHold_ = false;
    refreshHold(OVERLAY_KIND_STATUS);
}

void X11CandidatePanel::resyncAfterMenu()
{
    statusMenuHold_ = false;
    disarm(statusMenuWait_);
    const auto p = pointerPosition();
    // 悬停提示：指针在它上面就留下，否则按指针处重定候选悬停（Windows `set_menu_open(false)`：
    // 光标在气泡上留下、否则隐藏）。
    const bool onCand = pointerOn(cand_, p);
    const int32_t hit = onCand ? hitTest(rects_, p->first - cand_.x, p->second - cand_.y) : kNoHit;
    const int32_t h =
        tipRecheckHover(pointerOn(overlays_[OVERLAY_KIND_TOOLTIP - 1], p), onCand, hit, hover_);
    if (h != kKeepHover) {
        setHover(h);
    }
    // 气泡 / Toast：菜单开着期间收不到它们的移动与离开，按指针的真实位置重定悬停，再恢复计时。
    for (uint32_t k : {OVERLAY_KIND_STATUS, OVERLAY_KIND_TOAST}) {
        Surface& s = overlays_[k - 1];
        s.hovered = pointerOn(s, p);
        if (p) {
            s.gate.rebase(p->first, p->second);
        }
        refreshHold(k);
    }
}

} // namespace windlinux

// ── 自绘菜单 ──────────────────────────────────────────────────────────

namespace windlinux {

bool X11CandidatePanel::menuOpen() const
{
    for (const Surface& m : menus_) {
        if (m.mapped) {
            return true;
        }
    }
    return false;
}

bool X11CandidatePanel::showMenuLevel(uint32_t level, const SharedFrame& f,
                                      const OverlayFramePayload& p)
{
    if (level >= kMenuLevels || f.width == 0 || f.height == 0 || f.bgra.empty()
        || !ensureConnection()) {
        return false;
    }
    Surface& s = menus_[level];
    const xcb_window_t before = s.window;
    if (!ensureWindow(s, hasCompositor()) || bitsPerPixel(conn_, s.depth) != 32) {
        return false;
    }
    const bool recreated = before != 0 && s.window != before;
    OverlayFramePayload geo = p;
    geo.width = f.width;
    geo.height = f.height;
    const bool first = !menuOpen();
    // 新映射的一级提到最上（更深的子菜单总是后出现，自然压在父级上面）；已在显示的只换像素 /
    // 位置，不重排 z 序。
    present(s, f, placeOverlay(geo, workArea(), 0, 0), !s.mapped);
    if (first) {
        if (!armed(menuDeadline_)) {
            armMenuDeadline(); // 第 0 级被重建时 first 也为真：上限不因此顺延
        }
        // 菜单开着期间指针事件只进菜单：悬停提示的延后 / 宽限到期也不该来动它（去留等菜单收起）。
        menuActive_ = true;
        cancelTipTimers();
        disarm(statusMenuWait_); // 气泡的菜单来了，保持改由菜单收起来放开
    }
    // 第 0 级是抓取窗口：首次出现要抓；合成器起停让它换 visual 重建过，旧窗口连同抓取一起
    // 没了（destroyWindow 已把 pointerGrabbed_ 归位），也要重抓——否则点菜单外再也看不见。
    if (level == 0 && (first || recreated) && !pointerGrabbed_ && !armed(grabRetry_)) {
        grabAttempts_ = 0;
        grabPointer();
    }
    armMenuIdle();
    return true;
}

void X11CandidatePanel::hideMenuLevel(uint32_t level)
{
    if (level >= kMenuLevels) {
        return;
    }
    unmap(menus_[level]);
    if (!menuOpen()) {
        dropMenu();
    }
}

void X11CandidatePanel::closeMenu(const char* reason)
{
    if (!menuOpen()) {
        return;
    }
    WIND_DEBUG() << "本端收起菜单：" << (reason ? reason : "（不报服务端）");
    dropMenu();
    if (reason && cb_.menuDismissed) {
        cb_.menuDismissed(reason);
    }
}

void X11CandidatePanel::dropMenu()
{
    for (Surface& m : menus_) {
        unmap(m);
    }
    releasePointer();
    // 只停不毁：本函数会从这几个计时器自己的回调里进来（空闲超时、抓取重试）。
    disarm(grabRetry_);
    disarm(menuIdle_);
    disarm(menuDeadline_);
    pendingMotion_.reset();
    if (menuActive_) {
        menuActive_ = false;
        resyncAfterMenu();
    }
}

void X11CandidatePanel::noteMenuActivity()
{
    if (menuOpen()) {
        armMenuIdle();
    }
}

void X11CandidatePanel::armMenuIdle()
{
    const uint64_t due = fcitx::now(CLOCK_MONOTONIC) + uint64_t(menuIdleMs_) * 1000;
    if (menuIdle_) {
        menuIdle_->setTime(due);
        menuIdle_->setOneShot();
        return;
    }
    menuIdle_ = loop_.addTimeEvent(CLOCK_MONOTONIC, due, 0,
                                   [this](fcitx::EventSourceTime*, uint64_t) {
                                       if (menuOpen()) {
                                           WIND_INFO() << "菜单空闲 " << menuIdleMs_
                                                       << "ms 无操作，本端收起";
                                           dropMenu();
                                           if (cb_.menuDismissed) {
                                               cb_.menuDismissed("idle");
                                           }
                                       }
                                       return true;
                                   });
}

void X11CandidatePanel::armMenuDeadline()
{
    // 从第一级出现算起，不续：见 Menu.h kDefaultMenuMaxGrabMs。
    armTimer(menuDeadline_, menuMaxGrabMs_, &X11CandidatePanel::onMenuDeadline);
}

void X11CandidatePanel::onMenuDeadline()
{
    if (menuOpen()) {
        WIND_WARN() << "菜单已开 " << menuMaxGrabMs_ << "ms（抓指针上限），本端收起";
        closeMenu("max_grab");
    }
}

void X11CandidatePanel::grabPointer()
{
    if (pointerGrabbed_ || !conn_ || !menus_[0].window || !menus_[0].mapped) {
        return;
    }
    // owner_events = 1：指针在我们自己的窗口（各级菜单、候选窗）上时事件照常报给那个窗口，
    // 在别处时报给抓取窗口——于是「点菜单外」看得见，且那一下不会漏给下面的应用（同原生菜单）。
    // 只抓指针不抓键盘：键盘必须照旧走宿主 → Fcitx5 → 服务端 forward_menu_key。
    const uint16_t mask = XCB_EVENT_MASK_BUTTON_PRESS | XCB_EVENT_MASK_BUTTON_RELEASE
        | XCB_EVENT_MASK_POINTER_MOTION;
    auto cookie = xcb_grab_pointer(conn_, 1, menus_[0].window, mask, XCB_GRAB_MODE_ASYNC,
                                   XCB_GRAB_MODE_ASYNC, XCB_NONE, XCB_NONE, XCB_CURRENT_TIME);
    xcb_grab_pointer_reply_t* r = xcb_grab_pointer_reply(conn_, cookie, nullptr);
    const uint8_t status = r ? r->status : uint8_t(0xFF);
    std::free(r);
    if (status == XCB_GRAB_STATUS_SUCCESS) {
        // 不在这里销毁 grabRetry_：本函数可能正是它的回调（一次性计时器，触发后自己就停了）。
        pointerGrabbed_ = true;
        return;
    }
    // 别的客户端还抓着（典型：刚点的是托盘菜单，它的抓取要等菜单收起才放）：每 50ms 再试，
    // 最多 1 秒。抓不住菜单照样能用——Esc / 失焦 / 空闲超时照常收，只是点菜单外看不见。
    if (++grabAttempts_ > 20) {
        WIND_WARN() << "抓不住指针（status=" << int(status) << "），菜单外的点击将无法收起菜单";
        return;
    }
    const uint64_t due = fcitx::now(CLOCK_MONOTONIC) + 50 * 1000;
    if (grabRetry_) {
        grabRetry_->setTime(due);
        grabRetry_->setOneShot();
        return;
    }
    grabRetry_ = loop_.addTimeEvent(CLOCK_MONOTONIC, due, 0,
                                    [this](fcitx::EventSourceTime*, uint64_t) {
                                        grabPointer();
                                        return true;
                                    });
}

void X11CandidatePanel::releasePointer()
{
    if (pointerGrabbed_ && conn_) {
        xcb_ungrab_pointer(conn_, XCB_CURRENT_TIME);
        xcb_flush(conn_);
    }
    pointerGrabbed_ = false;
}

void X11CandidatePanel::handleMenuEvent(xcb_generic_event_t* ev)
{
    switch (ev->response_type & 0x7F) {
    case XCB_MOTION_NOTIFY: {
        auto* e = reinterpret_cast<xcb_motion_notify_event_t*>(ev);
        pendingMotion_ = std::make_pair(int32_t(e->root_x), int32_t(e->root_y));
        break;
    }
    case XCB_BUTTON_PRESS: {
        auto* e = reinterpret_cast<xcb_button_press_event_t*>(ev);
        if (e->detail < 1 || e->detail > 3) {
            break; // 滚轮（4/5）与侧键：Windows 菜单不处理，这里也不报
        }
        flushMenuMotion();
        if (!menuOpen()) {
            break; // 积着的移动没报上去、本端已收菜单（服务无响应）：这一下不再报
        }
        armMenuIdle();
        if (cb_.menuPointer) {
            cb_.menuPointer(MENU_POINTER_PRESS, e->detail, e->root_x, e->root_y);
        }
        break;
    }
    default:
        break; // 松开 / 离开：菜单只看按下与移动（同 Windows 的 wnd_proc）
    }
}

void X11CandidatePanel::flushMenuMotion()
{
    if (!pendingMotion_) {
        return;
    }
    auto [x, y] = *pendingMotion_;
    pendingMotion_.reset();
    if (!menuOpen()) {
        return;
    }
    // 移动不续空闲计时：服务卡死时指针还在动，续下去菜单就永远收不掉、指针一直被抓着。
    // 移动带来的高亮重绘是新菜单帧，那条路（showMenuLevel）会续——服务活着才有帧。
    if (cb_.menuPointer) {
        cb_.menuPointer(MENU_POINTER_MOTION, 0, x, y);
    }
}

namespace {

/// 从 RESOURCE_MANAGER 文本里取 `Xft.dpi`（行首、冒号后空白任意）。取不到返回 0。
double parseXftDpi(const std::string& rm)
{
    size_t pos = 0;
    while (pos < rm.size()) {
        size_t end = rm.find('\n', pos);
        if (end == std::string::npos) {
            end = rm.size();
        }
        const std::string line = rm.substr(pos, end - pos);
        pos = end + 1;
        if (line.compare(0, 8, "Xft.dpi:") == 0) {
            return std::atof(line.c_str() + 8);
        }
    }
    return 0.0;
}

double clampScale(double s)
{
    s = std::round(s * 100.0) / 100.0;
    return s < 1.0 ? 1.0 : (s > 4.0 ? 4.0 : s);
}

} // namespace

double X11CandidatePanel::hostScale(double textScaling)
{
    if (!conn_) {
        ensureConnection(); // 没有连接才连：不会销毁任何现有窗口 / 事件源
    }
    if (connectionUsable() && screen_) {
        xcb_get_property_cookie_t ck = xcb_get_property(
            conn_, 0, screen_->root, XCB_ATOM_RESOURCE_MANAGER, XCB_ATOM_STRING, 0, 65536);
        if (xcb_get_property_reply_t* r = xcb_get_property_reply(conn_, ck, nullptr)) {
            const int len = xcb_get_property_value_length(r);
            const double dpi =
                len > 0 ? parseXftDpi(std::string(static_cast<const char*>(xcb_get_property_value(r)),
                                                  size_t(len)))
                        : 0.0;
            std::free(r);
            if (dpi > 0.0) {
                return xftDpiToX11Scale(dpi, textScaling);
            }
        }
    }
    if (const char* gdk = std::getenv("GDK_SCALE"); gdk && *gdk) {
        if (double v = std::atof(gdk); v > 0.0) {
            return clampScale(v);
        }
    }
    return 1.0;
}

double X11CandidatePanel::textScaling()
{
    if (!conn_) {
        ensureConnection();
    }
    if (!connectionUsable()) {
        return 1.0;
    }
    const xcb_atom_t sel = internAtom(conn_, "_XSETTINGS_S" + std::to_string(screenNum_));
    const xcb_atom_t prop = internAtom(conn_, "_XSETTINGS_SETTINGS");
    if (sel == XCB_ATOM_NONE || prop == XCB_ATOM_NONE) {
        return 1.0;
    }
    auto owner = xcb_get_selection_owner_reply(conn_, xcb_get_selection_owner(conn_, sel), nullptr);
    const xcb_window_t win = owner ? owner->owner : XCB_NONE;
    std::free(owner);
    if (win == XCB_NONE) {
        return 1.0;
    }
    xcb_get_property_cookie_t ck =
        xcb_get_property(conn_, 0, win, prop, XCB_GET_PROPERTY_TYPE_ANY, 0, 65536);
    xcb_get_property_reply_t* r = xcb_get_property_reply(conn_, ck, nullptr);
    if (!r) {
        return 1.0;
    }
    const int len = r->format == 8 ? xcb_get_property_value_length(r) : 0;
    const double ts = xsettingsTextScaling(static_cast<const uint8_t*>(xcb_get_property_value(r)),
                                           size_t(len > 0 ? len : 0));
    std::free(r);
    return ts;
}

bool X11CandidatePanel::connectionUsable() const
{
    return conn_ && screen_ && !xcb_connection_has_error(conn_);
}

std::optional<Rect> X11CandidatePanel::screenWorkArea()
{
    if (!connectionUsable()) {
        return std::nullopt;
    }
    return workArea();
}

std::optional<std::pair<int32_t, int32_t>> X11CandidatePanel::pointerPosition()
{
    // 只读现有连接、不重连：调用方有计时器回调与 X 事件回调（菜单收起后重定悬停…），在那里
    // 重连会销毁正在执行的事件源。连接坏了就当「不知道指针在哪」，下一帧贴图时再重连。
    if (!connectionUsable()) {
        return std::nullopt;
    }
    auto* r = xcb_query_pointer_reply(conn_, xcb_query_pointer(conn_, screen_->root), nullptr);
    if (!r) {
        return std::nullopt;
    }
    std::pair<int32_t, int32_t> p{r->root_x, r->root_y};
    std::free(r);
    return p;
}

} // namespace windlinux

// ── 工具栏 / 软键盘 ──────────────────────────────────────────────────

namespace windlinux {

bool X11CandidatePanel::showPanel(uint32_t kind, const SharedFrame& f, const OverlayFramePayload& p)
{
    Surface* s = panelSurface(kind);
    if (!s || f.width == 0 || f.height == 0 || f.bgra.empty() || !ensureConnection()) {
        return false;
    }
    if (!ensureWindow(*s, hasCompositor()) || bitsPerPixel(conn_, s->depth) != 32) {
        return false;
    }
    OverlayFramePayload geo = p;
    geo.width = f.width;
    geo.height = f.height;
    const bool wasMapped = s->mapped;
    Rect r{s->x, s->y, int32_t(f.width), int32_t(f.height)};
    if (dragging(kind) && wasMapped) {
        // 拖动中：只换像素、不挪——服务端还不知道新位置，帧里是拖动前的坐标。
    } else if (wasMapped && p.x == s->reqX && p.y == s->reqY && f.width == s->pixW
               && f.height == s->pixH) {
        // 原地重绘（悬停高亮、按键反馈…）：沿用上次夹回的结果，不再为每帧往返查输出布局。
    } else {
        // EXACT 原样摆，再夹进所在输出的工作区：服务端记下的位置可能在换过布局的屏幕外面。
        const Rect want = placeOverlay(geo, workArea(), 0, 0);
        r = clampToWorkArea(want, panelWorkArea(want, queryOutputs()));
        s->reqX = p.x;
        s->reqY = p.y;
    }
    s->geo = geo;
    // 只在新映射时提到最上：常驻面板每次重绘都提层的话，会盖住它自己弹出的菜单和候选窗。
    return present(*s, f, r, !wasMapped);
}

void X11CandidatePanel::handlePanelEvent(uint32_t kind, xcb_generic_event_t* ev)
{
    Surface& s = *panelSurface(kind);
    switch (ev->response_type & 0x7F) {
    case XCB_BUTTON_PRESS: {
        auto* e = reinterpret_cast<xcb_button_press_event_t*>(ev);
        if (drag_.active) {
            break; // 拖动中按别的键：服务端没见过这次拖动的按下，也不让它看到这一下
        }
        if (e->detail == 4 || e->detail == 5) {
            panelPointer(kind, PANEL_POINTER_WHEEL, e->detail, e->event_x, e->event_y);
            break;
        }
        if (e->detail > 5) {
            break; // 横向滚轮 / 侧键：Windows 工具栏与软键盘都不处理
        }
        if (overlayPressAction(kind, e->detail, e->event_x, e->event_y, s.geo.drag)
            == OverlayPress::Drag) {
            // 拖动区里的左键：本端拖，按下不转给服务端（它会自己再报一次「已移动」，与 pos.* 重复）。
            // 靠按下时的隐式抓取收移动与松开，拖出窗口也照收。
            drag_.active = true;
            drag_.kind = kind;
            drag_.grabDx = e->root_x - s.x;
            drag_.grabDy = e->root_y - s.y;
            dragOutputs_ = queryOutputs();
            setDragCursor(true);
            break;
        }
        panelPointer(kind, PANEL_POINTER_PRESS, e->detail, e->event_x, e->event_y);
        break;
    }
    case XCB_BUTTON_RELEASE: {
        auto* e = reinterpret_cast<xcb_button_release_event_t*>(ev);
        if (dragging(kind)) {
            if (e->detail == 1) {
                moveDrag(e->root_x, e->root_y);
                endDrag(true);
            }
            break;
        }
        if (drag_.active || e->detail > 3) {
            break; // 滚轮的松开没有意义（按下已报成一次滚动）
        }
        panelPointer(kind, PANEL_POINTER_RELEASE, e->detail, e->event_x, e->event_y);
        break;
    }
    case XCB_MOTION_NOTIFY: {
        auto* e = reinterpret_cast<xcb_motion_notify_event_t*>(ev);
        if (dragging(kind)) {
            if (!(e->state & XCB_KEY_BUT_MASK_BUTTON_1)) {
                endDrag(true); // 松开事件丢了：按当下位置收尾（同状态气泡）
            } else {
                moveDrag(e->root_x, e->root_y);
            }
            break;
        }
        if (drag_.active) {
            break;
        }
        panelPointer(kind, PANEL_POINTER_MOTION, 0, e->event_x, e->event_y);
        break;
    }
    case XCB_LEAVE_NOTIFY: {
        auto* e = reinterpret_cast<xcb_leave_notify_event_t*>(ev);
        // 抓取引起的离开（菜单抓指针、拖完松手时指针已在外面）也报：之后面板收不到事件，
        // 服务端的悬停 / 按下态要在这里收掉。移进子窗口不算（面板没有子窗口）。
        if (e->detail == XCB_NOTIFY_DETAIL_INFERIOR || !s.pointerIn) {
            break;
        }
        s.pointerIn = false;
        panelPointer(kind, PANEL_POINTER_LEAVE, 0, e->event_x, e->event_y);
        break;
    }
    default:
        break;
    }
}

void X11CandidatePanel::panelPointer(uint32_t kind, uint32_t event, uint32_t button, int32_t x,
                                     int32_t y)
{
    if (event != PANEL_POINTER_LEAVE) {
        if (Surface* s = panelSurface(kind)) {
            s->pointerIn = true;
        }
    }
    if (event == PANEL_POINTER_MOTION) {
        if (pendingPanelMotion_ && pendingPanelMotion_->kind != kind) {
            flushPanelMotion();
        }
        pendingPanelMotion_ = PanelMotion{kind, x, y};
        return;
    }
    flushPanelMotion(); // 保序：积着的移动先报
    if (cb_.panelPointer) {
        cb_.panelPointer(kind, event, button, x, y);
    }
}

void X11CandidatePanel::flushPanelMotion()
{
    if (!pendingPanelMotion_) {
        return;
    }
    const PanelMotion m = *pendingPanelMotion_;
    pendingPanelMotion_.reset();
    if (cb_.panelPointer) {
        cb_.panelPointer(m.kind, PANEL_POINTER_MOTION, 0, m.x, m.y);
    }
}

void X11CandidatePanel::panelPointerGone(Surface& s)
{
    if (!s.pointerIn) {
        return;
    }
    s.pointerIn = false;
    // 坐标无意义：服务端的 LEAVE 不更新「最后指针」。
    panelPointer(OVERLAY_KIND_TOOLBAR + uint32_t(&s - panels_), PANEL_POINTER_LEAVE, 0, 0, 0);
}

std::optional<Rect> X11CandidatePanel::netWorkArea()
{
    if (!connectionUsable() || !atomWorkArea_) {
        return std::nullopt;
    }
    // _NET_WORKAREA 每个虚拟桌面一组 (x, y, w, h)，取当前桌面那组（_NET_CURRENT_DESKTOP，没有按 0）。
    uint32_t desktop = 0;
    if (atomCurrentDesktop_) {
        auto ck = xcb_get_property(conn_, 0, screen_->root, atomCurrentDesktop_,
                                   XCB_ATOM_CARDINAL, 0, 1);
        if (auto* r = xcb_get_property_reply(conn_, ck, nullptr)) {
            if (r->format == 32 && xcb_get_property_value_length(r) >= 4) {
                desktop = *static_cast<const uint32_t*>(xcb_get_property_value(r));
            }
            std::free(r);
        }
    }
    auto ck = xcb_get_property(conn_, 0, screen_->root, atomWorkArea_, XCB_ATOM_CARDINAL, 0, 4096);
    auto* r = xcb_get_property_reply(conn_, ck, nullptr);
    if (!r) {
        return std::nullopt;
    }
    std::optional<Rect> out;
    const uint32_t n = r->format == 32 ? uint32_t(xcb_get_property_value_length(r)) / 4 : 0;
    if (n >= 4) {
        const uint32_t* v = static_cast<const uint32_t*>(xcb_get_property_value(r));
        const uint32_t base = (desktop + 1) * 4 <= n ? desktop * 4 : 0;
        out = Rect{int32_t(v[base]), int32_t(v[base + 1]), int32_t(v[base + 2]),
                   int32_t(v[base + 3])};
    }
    std::free(r);
    return out;
}

std::vector<X11CandidatePanel::OutputInfo> X11CandidatePanel::queryOutputs()
{
    std::vector<OutputInfo> out;
    if (!randrAvailable_ || !connectionUsable()) {
        return out;
    }
    auto* res = xcb_randr_get_screen_resources_current_reply(
        conn_, xcb_randr_get_screen_resources_current(conn_, screen_->root), nullptr);
    if (!res) {
        return out;
    }
    xcb_randr_output_t primary = XCB_NONE;
    if (auto* pr = xcb_randr_get_output_primary_reply(
            conn_, xcb_randr_get_output_primary(conn_, screen_->root), nullptr)) {
        primary = pr->output;
        std::free(pr);
    }
    const std::optional<Rect> net = netWorkArea();
    const xcb_randr_output_t* outs = xcb_randr_get_screen_resources_current_outputs(res);
    const int n = xcb_randr_get_screen_resources_current_outputs_length(res);
    for (int i = 0; i < n; ++i) {
        auto* info = xcb_randr_get_output_info_reply(
            conn_, xcb_randr_get_output_info(conn_, outs[i], res->config_timestamp), nullptr);
        if (!info) {
            continue;
        }
        // 只认点亮了的（挂在某个 CRTC 上）：没接的口、接了但关掉的屏都没有 CRTC。
        if (info->crtc != XCB_NONE) {
            auto* crtc = xcb_randr_get_crtc_info_reply(
                conn_, xcb_randr_get_crtc_info(conn_, info->crtc, res->config_timestamp), nullptr);
            if (crtc && crtc->width > 0 && crtc->height > 0) {
                OutputInfo o;
                o.name.assign(reinterpret_cast<const char*>(xcb_randr_get_output_info_name(info)),
                              size_t(xcb_randr_get_output_info_name_length(info)));
                o.rect = Rect{crtc->x, crtc->y, crtc->width, crtc->height};
                o.work = outputWorkArea(o.rect, net);
                // 主输出排最前：落在所有输出之外的面板夹回到它上面。
                if (outs[i] == primary) {
                    out.insert(out.begin(), std::move(o));
                } else {
                    out.push_back(std::move(o));
                }
            }
            std::free(crtc);
        }
        std::free(info);
    }
    std::free(res);
    return out;
}

Rect X11CandidatePanel::panelWorkArea(const Rect& r, const std::vector<OutputInfo>& outputs)
{
    if (outputs.empty()) {
        return outputWorkArea(workArea(), netWorkArea());
    }
    std::vector<Rect> rects;
    for (const OutputInfo& o : outputs) {
        rects.push_back(o.rect);
    }
    const int i = outputIndexAt(rects, r.x + r.w / 2, r.y + r.h / 2);
    return outputs[i >= 0 ? size_t(i) : 0].work;
}

std::optional<HostOutput> X11CandidatePanel::outputAt(int32_t x, int32_t y)
{
    for (const OutputInfo& o : queryOutputs()) {
        if (rectContains(o.rect, x, y)) {
            return HostOutput{o.name, o.work};
        }
    }
    return std::nullopt;
}

// ── 全屏检测 ──

void X11CandidatePanel::watchRoot()
{
    atomActiveWindow_ = internAtom(conn_, "_NET_ACTIVE_WINDOW");
    atomWmState_ = internAtom(conn_, "_NET_WM_STATE");
    atomFullscreen_ = internAtom(conn_, "_NET_WM_STATE_FULLSCREEN");
    atomWorkArea_ = internAtom(conn_, "_NET_WORKAREA");
    atomCurrentDesktop_ = internAtom(conn_, "_NET_CURRENT_DESKTOP");
    // 事件掩码按客户端分开记：这里只设本连接在根窗口上的掩码，碰不到别的客户端。
    const uint32_t mask = XCB_EVENT_MASK_PROPERTY_CHANGE;
    xcb_change_window_attributes(conn_, screen_->root, XCB_CW_EVENT_MASK, &mask);
    xcb_flush(conn_);
    // 连上那一刻就判一次（别等到用户切窗口）。排到事件循环里做：这里可能正处在贴帧 / 上报
    // 显示环境的路径上，不在其中途回调引擎发请求。
    armTimer(activeCheck_, 0, &X11CandidatePanel::refreshActiveWindow);
}

bool X11CandidatePanel::ownsWindow(xcb_window_t w) const
{
    if (w == 0) {
        return false;
    }
    if (w == cand_.window || overlayKindOf(w) != 0) {
        return true;
    }
    for (const Surface& m : menus_) {
        if (m.window == w) {
            return true;
        }
    }
    return false;
}

void X11CandidatePanel::refreshActiveWindow()
{
    if (!connectionUsable() || !atomActiveWindow_) {
        return;
    }
    xcb_window_t w = XCB_NONE;
    auto ck = xcb_get_property(conn_, 0, screen_->root, atomActiveWindow_, XCB_ATOM_WINDOW, 0, 1);
    if (auto* r = xcb_get_property_reply(conn_, ck, nullptr)) {
        if (r->format == 32 && xcb_get_property_value_length(r) >= 4) {
            w = *static_cast<const xcb_window_t*>(xcb_get_property_value(r));
        }
        std::free(r);
    }
    if (w != activeWindow_) {
        // 撤掉对旧窗口的订阅：掩码按客户端分开记，置 0 只清本连接自己加的那份，别的客户端在
        // 那个窗口上选的事件不受影响。没加过（本端的窗口 / 0）就不碰。旧窗口可能已经销毁，
        // BadWindow 走错误事件、只记 debug。
        if (activeWatched_) {
            const uint32_t none = 0;
            xcb_change_window_attributes(conn_, activeWindow_, XCB_CW_EVENT_MASK, &none);
        }
        activeWindow_ = w;
        activeWatched_ = false;
        // 先订阅再读状态：读和订阅之间的变化不会漏。
        if (w != XCB_NONE && !ownsWindow(w)) {
            const uint32_t mask = XCB_EVENT_MASK_PROPERTY_CHANGE;
            xcb_change_window_attributes(conn_, w, XCB_CW_EVENT_MASK, &mask);
            activeWatched_ = true;
        }
        xcb_flush(conn_);
    }
    checkFullscreen();
}

void X11CandidatePanel::checkFullscreen()
{
    if (!connectionUsable()) {
        return;
    }
    bool fullscreen = false;
    if (activeWindow_ != XCB_NONE && atomWmState_) {
        auto ck = xcb_get_property(conn_, 0, activeWindow_, atomWmState_, XCB_ATOM_ATOM, 0, 64);
        if (auto* r = xcb_get_property_reply(conn_, ck, nullptr)) {
            if (r->format == 32) {
                const auto* a = static_cast<const xcb_atom_t*>(xcb_get_property_value(r));
                const int n = xcb_get_property_value_length(r) / 4;
                fullscreen = hasFullscreenState(std::vector<uint32_t>(a, a + n), atomFullscreen_);
            }
            std::free(r);
        }
    }
    if (cb_.fullscreenChanged) {
        cb_.fullscreenChanged(fullscreen);
    }
}

} // namespace windlinux
