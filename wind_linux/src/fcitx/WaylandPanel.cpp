#include "WaylandPanel.h"

#include "WaylandIm.h"

#include <fcitx/addoninstance.h>
#include <wayland_public.h>

#include <fcitx-utils/log.h>
#include <fcitx/addonmanager.h>

#include <wayland-client.h>
#include "input-method-unstable-v1-client-protocol.h"
#include "input-method-unstable-v2-client-protocol.h"
#include "relative-pointer-unstable-v1-client-protocol.h"
#include "viewporter-client-protocol.h"
// 生成的头文件里有名为 `namespace` 的参数（C 合法、C++ 关键字），包含时改个名
#define namespace namespace_
#include "wlr-layer-shell-unstable-v1-client-protocol.h"
#undef namespace
#include "xdg-output-unstable-v1-client-protocol.h"

#include <algorithm>
#include "ExtProtocol.h"
#include "Menu.h"

#include <fcitx-utils/event.h>
#include <fcitx-utils/misc.h>

#include <cmath>
#include <cstring>
#include <sys/mman.h>
#include <sys/uio.h>
#include <unistd.h>

#include <cstdlib>

FCITX_DECLARE_LOG_CATEGORY(windinput_log);

#define WIND_DEBUG() FCITX_LOGC(windinput_log, Debug)
#define WIND_WARN() FCITX_LOGC(windinput_log, Warn)
#define WIND_INFO() FCITX_LOGC(windinput_log, Info)

namespace windlinux {

namespace {

/// 读本进程内任意地址：地址无效时返回 false 而不是崩溃（`rawProxyOf` 在猜指针）。
bool safeRead(const void* addr, void* out, size_t n)
{
    struct iovec local{out, n};
    struct iovec remote{const_cast<void*>(addr), n};
    return process_vm_readv(getpid(), &local, 1, &remote, 1, 0) == ssize_t(n);
}

} // namespace

/// libwayland 的 C 回调落点（需要访问私有成员，故做成友元）。
struct WaylandListeners {
    using LayerSurf = WaylandCandidatePanel::LayerSurf;
    static void registryGlobal(void* data, wl_registry*, uint32_t name, const char* iface,
                               uint32_t version)
    {
        static_cast<WaylandCandidatePanel*>(data)->onGlobal(name, iface, version);
    }
    static void registryRemove(void* data, wl_registry*, uint32_t name)
    {
        static_cast<WaylandCandidatePanel*>(data)->onGlobalRemove(name);
    }

    static void bufferRelease(void* data, wl_buffer* b)
    {
        static_cast<WaylandCandidatePanel*>(data)->releaseBuffer(b);
    }

    static const wl_pointer_listener kPtr;
    static void seatCaps(void* data, wl_seat* seat, uint32_t caps)
    {
        auto* self = static_cast<WaylandCandidatePanel*>(data);
        if ((caps & WL_SEAT_CAPABILITY_POINTER) && !self->pointer_) {
            self->pointer_ = wl_seat_get_pointer(seat);
            wl_pointer_add_listener(self->pointer_, &kPtr, self);
            self->ensureRelativePointer();
        } else if (!(caps & WL_SEAT_CAPABILITY_POINTER) && self->pointer_) {
            if (self->relPtr_) {
                zwp_relative_pointer_v1_destroy(self->relPtr_);
                self->relPtr_ = nullptr;
            }
            wl_pointer_destroy(self->pointer_);
            self->pointer_ = nullptr;
        }
    }
    static void seatName(void*, wl_seat*, const char*) {}

    // 鼠标：本类自己绑一份 wl_pointer（与 Fcitx5 classicui 那份互不影响，各收各的事件）。
    static void ptrEnter(void* data, wl_pointer*, uint32_t, wl_surface* surface, wl_fixed_t x,
                         wl_fixed_t y)
    {
        auto* self = static_cast<WaylandCandidatePanel*>(data);
        self->retired_.pointerEnter(self->retiredKey(surface));
        if (int32_t ox, oy; self->screenMenu_ && self->screenMenuOffset(surface, ox, oy)) {
            self->menuPtrSurface_ = surface;
            self->menuPtrOffX_ = ox;
            self->menuPtrOffY_ = oy;
            self->layerPtrX_ = wl_fixed_to_double(x);
            self->layerPtrY_ = wl_fixed_to_double(y);
            if (self->pendingMenu_ && self->pendingMenu_->locate) {
                self->pendingMenu_->x = ox + int32_t(std::lround(self->layerPtrX_));
                self->pendingMenu_->y = oy + int32_t(std::lround(self->layerPtrY_));
                self->pendingMenu_->locate = false;
                if (self->catcher_.mapped) {
                    self->sendPendingMenu();
                }
                return;
            }
            self->reportScreenMenuPointer(MENU_POINTER_MOTION, 0);
            return;
        }
        if (auto* ls = self->layerSurfOf(surface)) {
            WIND_DEBUG() << "指针进入 layer kind=" << ls->kind;
            self->pointerLayer_ = ls;
            self->layerPtrX_ = wl_fixed_to_double(x);
            self->layerPtrY_ = wl_fixed_to_double(y);
            if (WaylandCandidatePanel::isPanel(*ls)) {
                // 进入即带位置：当一次移动报上去（悬停高亮由服务端判）
                self->panelPointer(*ls, PANEL_POINTER_MOTION, 0);
                return;
            }
            ls->hovered = true;
            // 指针停在上面：暂停到时收起（同 X11 的「交互中不计时」）
            self->layers_[ls->kind].hideTimer.reset();
            return;
        }
        if (!self->isPopupSurface(surface)) {
            return;
        }
        // 子 surface 的原点就是位图原点；主 surface 的原点是位图里的 mainOrigin（裁切时非 0）。
        const double sc = self->reportedScale_;
        self->ptrOffX_ = surface == self->child_ ? 0 : self->mainOriginX_ / sc;
        self->ptrOffY_ = surface == self->child_ ? 0 : self->mainOriginY_ / sc;
        self->pointerInside_ = true;
        self->pointerX_ = wl_fixed_to_double(x) + self->ptrOffX_;
        self->pointerY_ = wl_fixed_to_double(y) + self->ptrOffY_;
        WIND_DEBUG() << "指针进入候选窗（" << (surface == self->child_ ? "子" : "主") << " surface），位图逻辑坐标 ("
                     << self->pointerX_ << "," << self->pointerY_ << ")";
        self->updateHover();
        self->updatePopupStatusHover();
    }
    static void ptrLeave(void* data, wl_pointer*, uint32_t, wl_surface* surface)
    {
        auto* self = static_cast<WaylandCandidatePanel*>(data);
        self->retired_.pointerLeave(self->retiredKey(surface));
        self->reapRetired(false); // 指针停在退役 surface 上时拖着没销毁的，此刻可以了
        if (surface && surface == self->menuPtrSurface_) {
            self->menuPtrSurface_ = nullptr;
            return;
        }
        if (self->pointerLayer_ && surface == self->pointerLayer_->surface) {
            LayerSurf* ls = self->pointerLayer_;
            self->pointerLayer_ = nullptr;
            if (WaylandCandidatePanel::isPanel(*ls)) {
                // 拖动靠按下时的隐式抓取收事件，离开了就收不到松开：按当下位置收尾（同 X11）
                if (ls->dragging) {
                    self->endPanelDrag(*ls, true);
                }
                self->panelPointerGone(*ls);
                return;
            }
            ls->hovered = false;
            if (!ls->dragging) {
                self->armOverlayHide(ls->kind); // 交互结束：重新给满一份时长
            }
            return;
        }
        if (!self->isPopupSurface(surface)) {
            return;
        }
        self->pointerInside_ = false;
        self->click_.cancel();
        self->decorClick_.cancel();
        self->setHover(-1);
        self->updatePopupStatusHover();
    }
    static void ptrMotion(void* data, wl_pointer*, uint32_t, wl_fixed_t x, wl_fixed_t y)
    {
        auto* self = static_cast<WaylandCandidatePanel*>(data);
        if (self->menuPtrSurface_) {
            self->layerPtrX_ = wl_fixed_to_double(x);
            self->layerPtrY_ = wl_fixed_to_double(y);
            self->reportScreenMenuPointer(MENU_POINTER_MOTION, 0);
            return;
        }
        if (LayerSurf* ls = self->pointerLayer_) {
            self->layerPtrX_ = wl_fixed_to_double(x);
            self->layerPtrY_ = wl_fixed_to_double(y);
            // 有 relative-pointer 时拖动走相对位移（见 relMotion）；绝对坐标是相对 surface 的，
            // surface 自己在跟着动，按它算会把还没生效的移动重复计入，气泡来回乱跳。
            if (WaylandCandidatePanel::isPanel(*ls)) {
                if (!ls->dragging) {
                    self->panelPointer(*ls, PANEL_POINTER_MOTION, 0);
                } else if (!self->relPtr_) {
                    const int32_t dx = int32_t(std::lround(self->layerPtrX_ - ls->grabX));
                    const int32_t dy = int32_t(std::lround(self->layerPtrY_ - ls->grabY));
                    self->movePanelDrag(*ls, dx, dy);
                    // 只扣除已消费的整数位移，保留慢速移动 / 分数缩放产生的小数余量。
                    ls->grabX += dx;
                    ls->grabY += dy;
                }
                return;
            }
            if (ls->dragging && !self->relPtr_) {
                self->moveStatusDrag(*ls, self->layerPtrX_, self->layerPtrY_);
            }
            return;
        }
        if (!self->pointerInside_) {
            return;
        }
        self->pointerX_ = wl_fixed_to_double(x) + self->ptrOffX_;
        self->pointerY_ = wl_fixed_to_double(y) + self->ptrOffY_;
        if (self->menuOpen()) {
            self->reportMenuPointer(MENU_POINTER_MOTION, 0);
            return;
        }
        self->updateHover();
        self->updatePopupStatusHover();
    }
    static void ptrButton(void* data, wl_pointer*, uint32_t, uint32_t, uint32_t button,
                          uint32_t state)
    {
        auto* self = static_cast<WaylandCandidatePanel*>(data);
        // linux/input-event-codes.h：BTN_LEFT / BTN_RIGHT / BTN_MIDDLE
        constexpr uint32_t kBtnLeft = 0x110, kBtnRight = 0x111, kBtnMiddle = 0x112;
        const bool pressed = state == WL_POINTER_BUTTON_STATE_PRESSED;
        if (self->menuPtrSurface_) {
            // 菜单项与菜单外的点击都报上去：命中、关菜单由服务端判（同 X11 抓指针时的做法）
            if (pressed && (button == kBtnLeft || button == kBtnMiddle || button == kBtnRight)) {
                self->reportScreenMenuPointer(MENU_POINTER_PRESS,
                                              button == kBtnLeft ? 1 : button == kBtnMiddle ? 2 : 3);
            }
            return;
        }
        if (self->screenMenu_) {
            return; // 底板没映射上之前的点击：不当作别的交互
        }
        if (LayerSurf* ls = self->pointerLayer_; ls && WaylandCandidatePanel::isPanel(*ls)) {
            // 工具栏 / 软键盘：按 X11 的编号（左 1 中 2 右 3）报，侧键等不报（Windows 两个窗口都不处理）
            const uint32_t b = button == kBtnLeft ? 1 : button == kBtnMiddle ? 2 : button == kBtnRight ? 3 : 0;
            if (ls->dragging) {
                // 拖动中：只认左键松开收尾；别的键服务端没见过这次拖动的按下，也不让它看到
                if (b == 1 && !pressed) {
                    self->endPanelDrag(*ls, true);
                }
                return;
            }
            if (b == 0) {
                return;
            }
            if (pressed) {
                const auto [px, py] = self->panelPointerPos();
                if (overlayPressAction(ls->kind, uint8_t(b), px, py, ls->geo.drag) == OverlayPress::Drag) {
                    // 拖动区里的左键：本端拖，按下不转给服务端（它会自己再报一次「已移动」，与 pos.* 重复）
                    self->beginPanelDrag(*ls);
                    return;
                }
            }
            self->panelPointer(*ls, pressed ? PANEL_POINTER_PRESS : PANEL_POINTER_RELEASE, b);
            return;
        }
        if (LayerSurf* ls = self->pointerLayer_) {
            if (ls->kind == OVERLAY_KIND_STATUS && button == kBtnLeft) {
                // 左键拖动气泡（松开时按位置报服务端，后者转为「固定位置」并落盘）
                if (pressed) {
                    self->beginStatusDrag(*ls, self->layerPtrX_, self->layerPtrY_);
                } else {
                    self->endStatusDrag(*ls);
                }
            } else if (ls->kind == OVERLAY_KIND_STATUS && button == kBtnRight && pressed && !ls->dragging) {
                // 右键气泡：开状态菜单，落点是气泡在输出上的位置 + 指针在气泡内的位置
                const auto [left, top] = self->layerTopLeft(*ls);
                self->openScreenMenu(MENU_TARGET_STATUS, left + int32_t(std::lround(self->layerPtrX_)),
                                     top + int32_t(std::lround(self->layerPtrY_)));
            } else if (ls->kind == OVERLAY_KIND_TOAST && pressed
                       && (button == kBtnLeft || button == kBtnMiddle || button == kBtnRight)) {
                // 点 Toast 提前收起（同 X11）
                WIND_DEBUG() << "Toast 被点击，提前收起";
                self->layers_[OVERLAY_KIND_TOAST].on = false;
                self->layers_[OVERLAY_KIND_TOAST].onLayer = false;
                self->layers_[OVERLAY_KIND_TOAST].hideTimer.reset();
                self->hideLayer(*ls);
            }
            return;
        }
        if (!self->pointerInside_ || self->screenMenu_) {
            return;
        }
        if (!pressed) {
            // 左键点选在松开时生效（ClickGate）：按下时的隐式抓取让松开仍落在本 surface 上
            if (button == kBtnLeft) {
                const auto [px, py] = self->pointerPx();
                const int32_t decor = self->decorClick_.release(self->decorAtPointer(), px, py);
                const int32_t idx = self->click_.release(self->hitAtPointer());
                if (idx != kNoHit && self->cb_.select) {
                    WIND_DEBUG() << "Wayland 候选窗点选 " << idx << " @(" << px << "," << py << ")";
                    self->cb_.select(idx);
                }
                if (decor >= 0 && self->cb_.decorClick) {
                    self->cb_.decorClick(uint32_t(decor));
                }
            }
            return;
        }
        if (self->menuOpen()) {
            // 菜单的命中、高亮、子菜单都在服务端；按 X11 的编号（左 1 中 2 右 3）报上去
            if (button == kBtnLeft || button == kBtnMiddle || button == kBtnRight) {
                self->reportMenuPointer(MENU_POINTER_PRESS,
                                        button == kBtnLeft ? 1 : button == kBtnMiddle ? 2 : 3);
            }
            return;
        }
        if (button == kBtnRight && self->pointerOnPopupStatus() && self->cb_.contextMenu) {
            // 右键 popup 里的状态气泡：状态菜单
            self->setHover(-1);
            self->openContextMenu(MENU_TARGET_STATUS);
            return;
        }
        if (button == kBtnLeft) {
            self->click_.press(self->hitAtPointer());
            const auto [px, py] = self->pointerPx();
            self->decorClick_.press(self->decorAtPointer(), px, py);
        } else if (button == kBtnRight && self->cb_.contextMenu) {
            // 命中候选 → 候选菜单，空白 / 翻页按钮 → 主菜单（同 X11 与 Windows）。
            // 悬停先清掉：菜单开着时候选窗收不到悬停，残留的高亮会一直挂着。
            const int32_t hit = self->hitAtPointer();
            self->setHover(-1);
            self->openContextMenu(contextMenuTarget(hit));
        }
    }
    static void ptrAxis(void* data, wl_pointer*, uint32_t, uint32_t axis, wl_fixed_t value)
    {
        auto* self = static_cast<WaylandCandidatePanel*>(data);
        if (LayerSurf* ls = self->pointerLayer_; ls && WaylandCandidatePanel::isPanel(*ls)) {
            // 工具栏 / 软键盘：同 X11 的滚轮按钮编号，4 = 上、5 = 下（正值 = 向下）
            if (!ls->dragging && !self->screenMenu_ && axis == WL_POINTER_AXIS_VERTICAL_SCROLL
                && value != 0) {
                self->panelPointer(*ls, PANEL_POINTER_WHEEL, value > 0 ? 5 : 4);
            }
            return;
        }
        // 只认竖直滚轮（axis 0）；正值 = 向下，协议上「正 = 上滚」与它相反。
        if (!self->pointerInside_ || axis != WL_POINTER_AXIS_VERTICAL_SCROLL || value == 0
            || !self->cb_.scroll) {
            return;
        }
        self->cb_.scroll(value > 0 ? -120 : 120);
    }

    static void relMotion(void* data, zwp_relative_pointer_v1*, uint32_t, uint32_t, wl_fixed_t dx,
                          wl_fixed_t dy, wl_fixed_t, wl_fixed_t)
    {
        auto* self = static_cast<WaylandCandidatePanel*>(data);
        if (LayerSurf* ls = self->pointerLayer_; ls && ls->dragging) {
            if (WaylandCandidatePanel::isPanel(*ls)) {
                self->movePanelDragBy(*ls, wl_fixed_to_double(dx), wl_fixed_to_double(dy));
            } else {
                self->moveStatusDragBy(*ls, wl_fixed_to_double(dx), wl_fixed_to_double(dy));
            }
        }
    }

    static void catcherConfigure(void* data, zwlr_layer_surface_v1*, uint32_t serial, uint32_t w,
                                 uint32_t h)
    {
        static_cast<WaylandCandidatePanel*>(data)->catcherConfigured(serial, w, h);
    }
    static void catcherClosed(void* data, zwlr_layer_surface_v1*)
    {
        auto* self = static_cast<WaylandCandidatePanel*>(data);
        if (self->catcher_.layer) {
            zwlr_layer_surface_v1_destroy(self->catcher_.layer);
            self->catcher_.layer = nullptr;
        }
        self->catcher_.configured = self->catcher_.waiting = self->catcher_.mapped = false;
        self->closeScreenMenu("closed");
    }

    // ── layer-shell ──
    static void layerConfigure(void* data, zwlr_layer_surface_v1*, uint32_t serial, uint32_t w,
                               uint32_t h)
    {
        auto* ls = static_cast<WaylandCandidatePanel::LayerSurf*>(data);
        if (!ls->mapped) {
            WIND_DEBUG() << "layer kind=" << ls->kind << " configure " << w << "x" << h;
        }
        ls->owner->layerConfigured(*ls, serial);
    }
    static void layerClosed(void* data, zwlr_layer_surface_v1*)
    {
        auto* ls = static_cast<WaylandCandidatePanel::LayerSurf*>(data);
        ls->owner->layerClosed(*ls);
    }
    static void probeConfigure(void* data, zwlr_layer_surface_v1*, uint32_t serial, uint32_t w,
                               uint32_t h)
    {
        auto* o = static_cast<WaylandCandidatePanel::Output*>(data);
        o->owner->probeConfigured(*o, serial, w, h);
    }
    static void probeClosed(void* data, zwlr_layer_surface_v1*)
    {
        // 合成器收回了（输出要没了）：不再量，可用区退回整块输出；输出还在的话要等下一次 onGlobal 才补建。
        auto* o = static_cast<WaylandCandidatePanel::Output*>(data);
        WaylandCandidatePanel* self = o->owner;
        self->destroyWorkProbe(*o, true);
        if (self->onWorkChanged_) {
            self->onWorkChanged_();
        }
    }

    // ── 输出与缩放 ──
    static void outGeometry(void*, wl_output*, int32_t, int32_t, int32_t, int32_t, int32_t,
                            const char*, const char*, int32_t)
    {
    }
    static void outMode(void* data, wl_output*, uint32_t flags, int32_t w, int32_t h, int32_t)
    {
        auto* o = static_cast<WaylandCandidatePanel::Output*>(data);
        if (flags & WL_OUTPUT_MODE_CURRENT) {
            o->modeW = w;
            o->modeH = h;
            o->owner->scaleMaybeChanged();
        }
    }
    static void outDone(void*, wl_output*) {}
    static void outName(void* data, wl_output*, const char* name)
    {
        auto* o = static_cast<WaylandCandidatePanel::Output*>(data);
        o->wlName = name ? name : "";
    }
    static void outDesc(void*, wl_output*, const char*) {}
    static void outScale(void* data, wl_output*, int32_t factor)
    {
        auto* o = static_cast<WaylandCandidatePanel::Output*>(data);
        o->scaleInt = factor > 0 ? factor : 1;
        o->owner->scaleMaybeChanged();
    }
    static void xdgPos(void* data, zxdg_output_v1*, int32_t x, int32_t y)
    {
        auto* o = static_cast<WaylandCandidatePanel::Output*>(data);
        o->logX = x;
        o->logY = y;
    }
    static void xdgSize(void* data, zxdg_output_v1*, int32_t w, int32_t h)
    {
        auto* o = static_cast<WaylandCandidatePanel::Output*>(data);
        o->logW = w;
        o->logH = h;
        o->owner->scaleMaybeChanged();
    }
    static void xdgDone(void*, zxdg_output_v1*) {}
    static void xdgName(void*, zxdg_output_v1*, const char*) {}
    static void xdgDesc(void*, zxdg_output_v1*, const char*) {}
    // 本回调的 data 是占位包装（见 ensurePopup），真正的 this 存在占位内存的 kFakeSelfOffset 处。
    static constexpr size_t kFakeSelfOffset = 512;
    static void surfaceEnter(void* data, wl_surface*, wl_output* out)
    {
        auto* self = *reinterpret_cast<WaylandCandidatePanel**>(static_cast<char*>(data) +
                                                                kFakeSelfOffset);
        for (auto& o : self->outputs_) {
            if (o->wl == out) {
                self->currentOutput_ = o.get();
                self->scaleMaybeChanged();
                break;
            }
        }
    }
    static void surfaceLeave(void*, wl_surface*, wl_output*) {}

    /// 退役 surface 取消映射后发的 sync 回来了：合成器已处理完取消映射，此前的指针事件也已送达。
    static void retiredSynced(void* data, wl_callback* cb, uint32_t)
    {
        auto* r = static_cast<WaylandCandidatePanel::Retired*>(data);
        wl_callback_destroy(cb);
        r->sync = nullptr;
        r->owner->retired_.synced(reinterpret_cast<uintptr_t>(r->surface));
        r->owner->reapRetired(false);
    }

    static void popupRect(void*, zwp_input_popup_surface_v2*, int32_t x, int32_t y, int32_t w,
                          int32_t h)
    {
        // 合成器告知的文本光标矩形（popup 自己的坐标系里）。仅诊断用：摆位是合成器的事。
        static int32_t last[4] = {-1, -1, -1, -1};
        if (last[0] != x || last[1] != y || last[2] != w || last[3] != h) {
            last[0] = x, last[1] = y, last[2] = w, last[3] = h;
            WIND_DEBUG() << "popup text_input_rectangle (" << x << "," << y << " " << w << "x" << h
                        << ")";
        }
    }
};

namespace {
const wl_registry_listener kRegistryListener = {WaylandListeners::registryGlobal,
                                                WaylandListeners::registryRemove};
const wl_buffer_listener kBufferListener = {WaylandListeners::bufferRelease};
const zwp_input_popup_surface_v2_listener kPopupListener = {WaylandListeners::popupRect};
const wl_callback_listener kRetiredSyncListener = {WaylandListeners::retiredSynced};
// name / description 是 wl_output v4 的事件（libwayland 1.20 起的头文件才有）：头文件没有时只绑 v3，
// 收不到名字（host.display 与 pos.* 都不报 output）。逐成员赋值免得各版本头文件成员数不同。
#ifdef WL_OUTPUT_NAME_SINCE_VERSION
constexpr uint32_t kOutputMaxVersion = 4;
#else
constexpr uint32_t kOutputMaxVersion = 3;
#endif
const wl_output_listener kOutputListener = [] {
    wl_output_listener l{};
    l.geometry = WaylandListeners::outGeometry;
    l.mode = WaylandListeners::outMode;
    l.done = WaylandListeners::outDone;
    l.scale = WaylandListeners::outScale;
#ifdef WL_OUTPUT_NAME_SINCE_VERSION
    l.name = WaylandListeners::outName;
    l.description = WaylandListeners::outDesc;
#endif
    return l;
}();
const zxdg_output_v1_listener kXdgOutputListener = [] {
    zxdg_output_v1_listener l{};
    l.logical_position = WaylandListeners::xdgPos;
    l.logical_size = WaylandListeners::xdgSize;
    l.done = WaylandListeners::xdgDone;
    l.name = WaylandListeners::xdgName;
    l.description = WaylandListeners::xdgDesc;
    return l;
}();
const wl_surface_listener kSurfaceListener = [] {
    wl_surface_listener l{};
    l.enter = WaylandListeners::surfaceEnter;
    l.leave = WaylandListeners::surfaceLeave;
    return l;
}();
const wl_seat_listener kSeatListener = {WaylandListeners::seatCaps, WaylandListeners::seatName};
const zwp_relative_pointer_v1_listener kRelPtrListener = {WaylandListeners::relMotion};
const zwlr_layer_surface_v1_listener kCatcherListener = [] {
    zwlr_layer_surface_v1_listener l{};
    l.configure = WaylandListeners::catcherConfigure;
    l.closed = WaylandListeners::catcherClosed;
    return l;
}();
const zwlr_layer_surface_v1_listener kLayerListener = [] {
    zwlr_layer_surface_v1_listener l{};
    l.configure = WaylandListeners::layerConfigure;
    l.closed = WaylandListeners::layerClosed;
    return l;
}();
const zwlr_layer_surface_v1_listener kProbeListener = [] {
    zwlr_layer_surface_v1_listener l{};
    l.configure = WaylandListeners::probeConfigure;
    l.closed = WaylandListeners::probeClosed;
    return l;
}();
} // namespace

// 绑的是 wl_pointer v1，较新的 frame / axis_* 事件不会来；逐成员赋值免得各版本头文件的成员数不同而告警。
const wl_pointer_listener WaylandListeners::kPtr = [] {
    wl_pointer_listener l{};
    l.enter = ptrEnter;
    l.leave = ptrLeave;
    l.motion = ptrMotion;
    l.button = ptrButton;
    l.axis = ptrAxis;
    return l;
}();

WaylandCandidatePanel::WaylandCandidatePanel(fcitx::Instance* instance) : instance_(instance)
{
    dragPreview_.owner = this;
    statusLs_.owner = this;
    statusLs_.kind = OVERLAY_KIND_STATUS;
    toastLs_.owner = this;
    toastLs_.kind = OVERLAY_KIND_TOAST;
    for (uint32_t i = 0; i < 2; ++i) {
        panels_[i].owner = this;
        panels_[i].kind = OVERLAY_KIND_TOOLBAR + i;
    }
    menuIdleMs_ = menuIdleTimeoutMs(std::getenv("WIND_MENU_IDLE_TIMEOUT_MS"));
    menuMaxMs_ = menuMaxGrabMs(std::getenv("WIND_MENU_MAX_GRAB_MS"));
    forceClip_ = waylandSkinClipForced(std::getenv("WINDINPUT_WAYLAND_SKIN_CLIP"));
    if (forceClip_) {
        WIND_INFO() << "WINDINPUT_WAYLAND_SKIN_CLIP=1：皮肤候选窗只显示窗体左上起的部分（不拆层）";
    }
    // wayland 模块：Fcitx5 自己开的 Wayland 连接。回调对已存在的连接也会补调一次。
    auto* wayland = instance_->addonManager().addon("wayland", true);
    if (!wayland) {
        WIND_DEBUG() << "没有 wayland 模块，Wayland 候选窗不可用";
        return;
    }
    connCreated_ = wayland->call<fcitx::IWaylandModule::addConnectionCreatedCallback>(
        [this](const std::string& name, wl_display* display, fcitx::FocusGroup*) {
            onConnection(name, display);
        });
    connClosed_ = wayland->call<fcitx::IWaylandModule::addConnectionClosedCallback>(
        [this](const std::string&, wl_display* display) { onClosed(display); });
}

WaylandCandidatePanel::~WaylandCandidatePanel()
{
    connCreated_.reset();
    connClosed_.reset();
    cb_ = Callbacks{}; // 拆连接时会补报面板的离开：析构途中不回调引擎
    teardown(display_ != nullptr);
}

bool WaylandCandidatePanel::handles(const fcitx::InputContext* ic) const
{
    if (!ic) {
        return false;
    }
    if (std::strcmp(ic->frontend(), "wayland_v2") == 0) {
        return true;
    }
    // input-method-v1（KWin）：没绑到面板全局时接不住，留给 X11 那条路（与改动前一致）
    return inputPanel_ && std::strcmp(ic->frontend(), "wayland") == 0;
}

void WaylandCandidatePanel::onConnection(const std::string& name, wl_display* display)
{
    if (display_) {
        return; // 只服务第一条连接（桌面会话只有一个）
    }
    display_ = display;
    registry_ = wl_display_get_registry(display_);
    wl_registry_add_listener(registry_, &kRegistryListener, this);
    wl_display_flush(display_);
    WIND_INFO() << "Wayland 候选窗：借用 Fcitx5 的连接 " << name;
}

void WaylandCandidatePanel::onClosed(wl_display* display)
{
    if (display == display_) {
        teardown(false);
    }
}

void WaylandCandidatePanel::onGlobal(uint32_t name, const char* iface, uint32_t version)
{
    if (std::strcmp(iface, "wl_compositor") == 0) {
        compositorVersion_ = version < 4 ? version : 4;
        compositor_ = static_cast<wl_compositor*>(
            wl_registry_bind(registry_, name, &wl_compositor_interface, compositorVersion_));
    } else if (std::strcmp(iface, "wl_shm") == 0) {
        shm_ = static_cast<wl_shm*>(wl_registry_bind(registry_, name, &wl_shm_interface, 1));
    } else if (std::strcmp(iface, "wl_output") == 0) {
        auto o = std::make_unique<Output>();
        o->owner = this;
        o->name = name;
        const uint32_t v = version < kOutputMaxVersion ? version : kOutputMaxVersion;
        o->version = v;
        o->wl = static_cast<wl_output*>(wl_registry_bind(registry_, name, &wl_output_interface, v));
        wl_output_add_listener(o->wl, &kOutputListener, o.get());
        attachXdgOutput(*o);
        outputs_.push_back(std::move(o));
    } else if (std::strcmp(iface, "zxdg_output_manager_v1") == 0 && !xdgManager_) {
        const uint32_t v = version < 3 ? version : 3;
        xdgManager_ = static_cast<zxdg_output_manager_v1*>(
            wl_registry_bind(registry_, name, &zxdg_output_manager_v1_interface, v));
        for (auto& o : outputs_) {
            attachXdgOutput(*o);
        }
    } else if (std::strcmp(iface, "wp_viewporter") == 0 && !viewporter_) {
        viewporter_ = static_cast<wp_viewporter*>(
            wl_registry_bind(registry_, name, &wp_viewporter_interface, 1));
    } else if (std::strcmp(iface, "zwlr_layer_shell_v1") == 0 && !layerShell_) {
        layerShellVersion_ = version < 4 ? version : 4;
        WIND_INFO() << "layer-shell v" << layerShellVersion_ << " 可用";
        layerShell_ = static_cast<zwlr_layer_shell_v1*>(
            wl_registry_bind(registry_, name, &zwlr_layer_shell_v1_interface, layerShellVersion_));
        if (onLayerShell_) {
            onLayerShell_();
        }
    } else if (std::strcmp(iface, "wl_subcompositor") == 0 && !subcomp_) {
        subcomp_ = static_cast<wl_subcompositor*>(
            wl_registry_bind(registry_, name, &wl_subcompositor_interface, 1));
    } else if (std::strcmp(iface, "zwp_relative_pointer_manager_v1") == 0 && !relPtrMgr_) {
        relPtrMgr_ = static_cast<zwp_relative_pointer_manager_v1*>(
            wl_registry_bind(registry_, name, &zwp_relative_pointer_manager_v1_interface, 1));
        ensureRelativePointer();
    } else if (std::strcmp(iface, "zwp_input_panel_v1") == 0 && !inputPanel_) {
        inputPanel_ = static_cast<zwp_input_panel_v1*>(
            wl_registry_bind(registry_, name, &zwp_input_panel_v1_interface, 1));
        WIND_INFO() << "input-method-v1 面板可用（KWin）：Wayland 原生应用的候选窗走 overlay 面板";
        // handles() 随之变了：焦点若已在 v1 应用里，让引擎重报宿主显示（不等坐标、缩放）
        if (onScaleChanged_) {
            onScaleChanged_();
        }
    } else if (std::strcmp(iface, "wl_seat") == 0 && !seat_) {
        seat_ = static_cast<wl_seat*>(wl_registry_bind(registry_, name, &wl_seat_interface, 1));
        wl_seat_add_listener(seat_, &kSeatListener, this);
    }
    // 输出、compositor、layer-shell 到达的先后不定：哪个最后到，哪次补建。
    ensureWorkProbes();
}

void WaylandCandidatePanel::attachXdgOutput(Output& o)
{
    if (!xdgManager_ || o.xdg) {
        return;
    }
    o.xdg = zxdg_output_manager_v1_get_xdg_output(xdgManager_, o.wl);
    zxdg_output_v1_add_listener(o.xdg, &kXdgOutputListener, &o);
}

double WaylandCandidatePanel::outputScale(const Output& o)
{
    double s = o.scaleInt > 0 ? o.scaleInt : 1;
    // 分数缩放下 wl_output.scale 是向上取整的整数，真实比例要看「模式像素 / 逻辑尺寸」；
    // 用面积比的平方根，转屏（模式宽高与逻辑宽高互换）时同样成立。
    if (o.modeW > 0 && o.modeH > 0 && o.logW > 0 && o.logH > 0) {
        s = std::sqrt(double(o.modeW) * o.modeH / (double(o.logW) * o.logH));
    }
    s = std::round(s * 100.0) / 100.0;
    return s < 1.0 ? 1.0 : (s > 4.0 ? 4.0 : s);
}

double WaylandCandidatePanel::scale() const
{
    const Output* o = currentOutput_ ? currentOutput_ : (outputs_.empty() ? nullptr : outputs_[0].get());
    return o ? outputScale(*o) : 1.0;
}

void WaylandCandidatePanel::scaleMaybeChanged()
{
    const double s = scale();
    if (s == lastNotifiedScale_) {
        return;
    }
    lastNotifiedScale_ = s;
    WIND_INFO() << "输出缩放变为 " << s;
    if (onScaleChanged_) {
        onScaleChanged_();
    }
}

void WaylandCandidatePanel::teardown(bool alive)
{
    // 面板随连接没了：指针还在上面的话补报离开（服务端的悬停 / 按下态要收掉）
    for (LayerSurf& ls : panels_) {
        panelPointerGone(ls);
    }
    if (alive) {
        for (auto& m : menuSurfs_) {
            if (m.sub) {
                wl_subsurface_destroy(m.sub);
            }
            if (m.viewport) {
                wp_viewport_destroy(m.viewport);
            }
            if (m.surface) {
                wl_surface_destroy(m.surface);
            }
        }
        if (catcher_.layer) {
            zwlr_layer_surface_v1_destroy(catcher_.layer);
        }
        if (catcher_.viewport) {
            wp_viewport_destroy(catcher_.viewport);
        }
        if (catcher_.surface) {
            wl_surface_destroy(catcher_.surface);
        }
        if (subcomp_) {
            wl_subcompositor_destroy(subcomp_);
        }
        if (relPtr_) {
            zwp_relative_pointer_v1_destroy(relPtr_);
        }
        if (relPtrMgr_) {
            zwp_relative_pointer_manager_v1_destroy(relPtrMgr_);
        }
        for (LayerSurf* ls : {&statusLs_, &toastLs_, &panels_[0], &panels_[1], &dragPreview_}) {
            if (ls->layer) {
                zwlr_layer_surface_v1_destroy(ls->layer);
            }
            if (ls->viewport) {
                wp_viewport_destroy(ls->viewport);
            }
            if (ls->surface) {
                wl_surface_destroy(ls->surface);
            }
        }
        if (layerShell_) {
            if (layerShellVersion_ >= 3) {
                zwlr_layer_shell_v1_destroy(layerShell_);
            } else {
                wl_proxy_destroy(reinterpret_cast<wl_proxy*>(layerShell_));
            }
        }
        dropPopup();
        reapRetired(true);
        if (buffer_) {
            wl_buffer_destroy(buffer_);
        }
        if (panelSurf_) {
            zwp_input_panel_surface_v1_destroy(panelSurf_);
        }
        if (childSub_) {
            wl_subsurface_destroy(childSub_);
        }
        if (childViewport_) {
            wp_viewport_destroy(childViewport_);
        }
        if (child_) {
            wl_surface_destroy(child_);
        }
        if (surface_) {
            wl_surface_destroy(surface_);
        }
        if (inputPanel_) {
            zwp_input_panel_v1_destroy(inputPanel_);
        }
        if (viewport_) {
            wp_viewport_destroy(viewport_);
        }
        if (viewporter_) {
            wp_viewporter_destroy(viewporter_);
        }
        for (auto& o : outputs_) {
            destroyWorkProbe(*o, true);
            if (o->xdg) {
                zxdg_output_v1_destroy(o->xdg);
            }
            wl_output_destroy(o->wl);
        }
        if (xdgManager_) {
            zxdg_output_manager_v1_destroy(xdgManager_);
        }
        if (pointer_) {
            wl_pointer_destroy(pointer_);
        }
        if (seat_) {
            wl_seat_destroy(seat_);
        }
        if (shm_) {
            wl_shm_destroy(shm_);
        }
        if (compositor_) {
            wl_compositor_destroy(compositor_);
        }
        if (registry_) {
            wl_registry_destroy(registry_);
        }
        if (display_) {
            wl_display_flush(display_);
        }
    }
    for (LayerSurf* ls : {&statusLs_, &toastLs_, &panels_[0], &panels_[1], &dragPreview_}) {
        ls->surface = nullptr;
        ls->layer = nullptr;
        ls->viewport = nullptr;
        ls->configured = ls->waiting = ls->mapped = ls->wanted = false;
        ls->hovered = ls->dragging = false;
        ls->outputName = ls->boundOutput = 0;
        ls->reqValid = false;
    }
    pendingPanelMotion_.reset();
    disarm(panelMotionFlush_);
    disarm(panelRetry_);
    for (auto& m : menuSurfs_) {
        m.surface = nullptr;
        m.sub = nullptr;
        m.viewport = nullptr;
        m.mapped = false;
    }
    catcher_.surface = nullptr;
    catcher_.layer = nullptr;
    catcher_.viewport = nullptr;
    catcher_.configured = catcher_.waiting = catcher_.mapped = false;
    subcomp_ = nullptr;
    screenMenu_ = false;
    pendingMenu_.reset();
    for (auto& q : queuedMenus_) {
        q.reset();
    }
    catcherOutput_ = 0;
    menuOriginX_ = menuOriginY_ = 0;
    panelMenu_.clear();
    menuPtrSurface_ = nullptr;
    pointerLayer_ = nullptr;
    relPtr_ = nullptr;
    relPtrMgr_ = nullptr;
    layerShell_ = nullptr;
    reapRetired(true, false); // 连接已死时到这里才丢（只释放本端结构）
    buffer_ = nullptr;
    surface_ = nullptr;
    child_ = nullptr;
    childSub_ = nullptr;
    childViewport_ = nullptr;
    childMapped_ = false;
    mainOriginX_ = mainOriginY_ = 0;
    lastMode_ = WaylandSkinMode::Whole;
    surfaceRole_ = Role::None;
    panelSurf_ = nullptr;
    inputPanel_ = nullptr;
    popup_ = nullptr;
    im_ = nullptr;
    viewport_ = nullptr;
    viewporter_ = nullptr;
    outputs_.clear();
    currentOutput_ = nullptr;
    xdgManager_ = nullptr;
    pointer_ = nullptr;
    seat_ = nullptr;
    shm_ = nullptr;
    compositor_ = nullptr;
    registry_ = nullptr;
    display_ = nullptr;
}

void WaylandCandidatePanel::dropPopup()
{
    if (popup_) {
        zwp_input_popup_surface_v2_destroy(popup_);
        popup_ = nullptr;
    }
    im_ = nullptr;
}

void WaylandCandidatePanel::releaseBuffer(wl_buffer* b)
{
    if (b == buffer_) {
        buffer_ = nullptr;
    }
    wl_buffer_destroy(b);
}

zwp_input_method_v2* WaylandCandidatePanel::rawProxyOf(void* wrapper)
{
    // Fcitx5 的 `fcitx::wayland::ZwpInputMethodV2` 把 C 代理存在某个成员里（版本间偏移不同），
    // 且开发包不给头文件。它构造时把 `this` 登记为该代理的 listener 用户数据，于是可以反推：
    // 逐个 8 字节槽取候选指针，校验「是 wl_proxy、类名对、用户数据正是这个包装对象」，三条都
    // 对才认。校验不过就老实返回空——宁可这一路不可用，也不去碰一个猜错的指针。
    constexpr size_t kScanBytes = 192;
    for (size_t off = 0; off + sizeof(void*) <= kScanBytes; off += sizeof(void*)) {
        void* cand = nullptr;
        std::memcpy(&cand, static_cast<char*>(wrapper) + off, sizeof(cand));
        if (!cand || (reinterpret_cast<uintptr_t>(cand) & 7) != 0) {
            continue;
        }
        // wl_proxy 开头是 wl_object { const wl_interface* interface; const void* implementation; uint32 id; }
        const void* iface = nullptr;
        if (!safeRead(cand, &iface, sizeof(iface)) || !iface) {
            continue;
        }
        const char* namePtr = nullptr;
        if (!safeRead(iface, &namePtr, sizeof(namePtr)) || !namePtr) {
            continue;
        }
        char name[32] = {};
        if (!safeRead(namePtr, name, sizeof(name) - 1)) {
            continue;
        }
        if (std::strcmp(name, "zwp_input_method_v2") != 0) {
            continue;
        }
        auto* proxy = static_cast<wl_proxy*>(cand);
        if (wl_proxy_get_user_data(proxy) != wrapper) {
            continue;
        }
        return reinterpret_cast<zwp_input_method_v2*>(proxy);
    }
    return nullptr;
}

bool WaylandCandidatePanel::ensurePopup(fcitx::InputContext* ic)
{
    if (ic && std::strcmp(ic->frontend(), "wayland") == 0 && inputPanel_) {
        return ensurePanelV1();
    }
    return ensurePopupV2(ic);
}

bool WaylandCandidatePanel::ensureSurface()
{
    if (!surface_) {
        surface_ = wl_compositor_create_surface(compositor_);
        // Fcitx5 classicui 对本连接上每个 wl_surface 的指针进入事件都会取 user_data 当它自己的
        // 包装对象解引用（`surface->userData()`）：我们的 surface 它不认识，user_data 为空就崩
        // （实测鼠标一移上候选窗就带崩 fcitx5）。给一块全零的内存，它读到的 userData 是空指针，
        // 按「不是我的窗口」直接返回。包装类内部布局没有头文件可对，故留足余量而不依赖偏移。
        // 注意 wl_proxy_add_listener 会把 user_data 覆盖成它的 data 参数：listener 的 data 也必须
        // 传占位（实测传 this 时，classicui 把 this 当它的窗口对象解引用，照样崩）；
        // this 另存进占位内存的 kFakeSelfOffset 处供回调取回。
        static char fakeWrapper[1024] = {};
        *reinterpret_cast<WaylandCandidatePanel**>(fakeWrapper + WaylandListeners::kFakeSelfOffset) =
            this;
        wl_surface_add_listener(surface_, &kSurfaceListener, fakeWrapper);
        wl_surface_set_user_data(surface_, fakeWrapper);
        WIND_INFO() << "Wayland 候选窗：surface 已建";
    }
    return surface_ != nullptr;
}

bool WaylandCandidatePanel::ensurePanelV1()
{
    if (surface_ && surfaceRole_ != Role::PanelV1 && surfaceRole_ != Role::None) {
        retireSurface(); // 拿过 v2 popup 角色的 surface 不能再当面板
    }
    if (!ensureSurface()) {
        return false;
    }
    if (panelSurf_) {
        return true;
    }
    // overlay 面板：KWin 把它摆在文本光标下方（放不下翻到上方、夹进可用区域），重新映射、光标矩形
    // 或尺寸变了都会重摆。角色没有 destroy 请求，收起时整个 surface 退役（见 hidePopup）。
    panelSurf_ = zwp_input_panel_v1_get_input_panel_surface(inputPanel_, surface_);
    zwp_input_panel_surface_v1_set_overlay_panel(panelSurf_);
    surfaceRole_ = Role::PanelV1;
    WIND_DEBUG() << "Wayland 候选窗：已把 surface 登记为 input-method-v1 overlay 面板";
    return true;
}

bool WaylandCandidatePanel::ensurePopupV2(fcitx::InputContext* ic)
{
    auto* waylandim = instance_->addonManager().addon("waylandim");
    if (!waylandim) {
        return false;
    }
    auto* wrapper = waylandim->call<fcitx::IWaylandIMModule::getInputMethodV2>(ic);
    if (!wrapper) {
        if (!warnedNoIm_) {
            warnedNoIm_ = true;
            WIND_WARN() << "waylandim 没给出 zwp_input_method_v2，Wayland 候选窗不可用";
        }
        return false;
    }
    zwp_input_method_v2* im = rawProxyOf(wrapper);
    if (!im) {
        if (!warnedProbe_) {
            warnedProbe_ = true;
            WIND_WARN() << "认不出 Fcitx5 输入法对象里的 wl 代理（Fcitx5 内部布局与预期不同），"
                           "Wayland 候选窗不可用";
        }
        return false;
    }
    if (surface_ && surfaceRole_ == Role::PanelV1) {
        retireSurface();
    }
    if (!ensureSurface()) {
        return false;
    }
    if (popup_ && im_ == im) {
        return true;
    }
    dropPopup();
    popup_ = zwp_input_method_v2_get_input_popup_surface(im, surface_);
    zwp_input_popup_surface_v2_add_listener(popup_, &kPopupListener, this);
    im_ = im;
    surfaceRole_ = Role::PopupV2;
    WIND_DEBUG() << "Wayland 候选窗：已把 surface 登记为 input popup";
    return true;
}

bool WaylandCandidatePanel::show(fcitx::InputContext* ic, const SharedFrame& f)
{
    layers_[0].frame = f;
    layers_[0].on = true;
    return present(ic);
}

bool WaylandCandidatePanel::showOverlay(fcitx::InputContext* ic, uint32_t kind,
                                        const SharedFrame& f, const OverlayFramePayload& geo)
{
    if (isPanelOverlayKind(kind)) {
        return showPanel(kind, f, geo);
    }
    if (kind < 1 || kind >= kLayers) {
        return false;
    }
    Layer& l = layers_[kind];
    const bool wasInPopup = l.on && !l.onLayer;
    l.frame = f;
    l.geo = geo;
    l.geo.width = f.width;
    l.geo.height = f.height;
    l.on = true;
    const bool onLayer = wantsLayer(kind, l.geo);
    if (l.onLayer && !onLayer) {
        hideLayer(*layerSurfFor(kind)); // 改回跟随光标：撤掉屏幕上那份
    }
    l.onLayer = onLayer;
    armOverlayHide(kind);
    if (onLayer) {
        showOnLayer(*layerSurfFor(kind), f, l.geo);
        // popup 里还合成着这一层的上一帧（之前跟随光标）就重排一遍；ic 可能不是 Wayland 的
        if (wasInPopup && handles(ic)) {
            present(ic);
        }
        return true;
    }
    return present(ic);
}

void WaylandCandidatePanel::armOverlayHide(uint32_t kind)
{
    Layer& l = layers_[kind];
    l.hideTimer.reset();
    if (!l.on || l.geo.durationMs <= 0) {
        return;
    }
    if (kind == OVERLAY_KIND_STATUS && (screenMenu_ || menuOpen() || popupStatusHovered_)) {
        return; // 气泡的菜单开着 / 指针停在上面：等结束再计时
    }
    if (LayerSurf* ls = l.onLayer ? layerSurfFor(kind) : nullptr; ls && (ls->hovered || ls->dragging)) {
        return; // 交互中不计时，结束时再给满一份
    }
    l.hideTimer = instance_->eventLoop().addTimeEvent(
        CLOCK_MONOTONIC, fcitx::now(CLOCK_MONOTONIC) + uint64_t(l.geo.durationMs) * 1000, 0,
        [this, kind](fcitx::EventSourceTime*, uint64_t) {
            // 回调里不能销毁自己：只改状态，计时器对象留到下次 show / hide 再换。
            WIND_DEBUG() << "浮层 kind=" << kind << " 到时自动收起";
            Layer& layer = layers_[kind];
            layer.on = false;
            if (layer.onLayer) {
                layer.onLayer = false;
                hideLayer(*layerSurfFor(kind));
            } else if (auto* ic = instance_->mostRecentInputContext()) {
                present(ic);
            } else {
                hidePopup();
            }
            return true;
        });
}

void WaylandCandidatePanel::hideOverlay(uint32_t kind)
{
    if (LayerSurf* ls = panelSurf(kind)) {
        hidePanel(*ls);
        return;
    }
    if (kind < 1 || kind >= kLayers || !layers_[kind].on) {
        return;
    }
    WIND_DEBUG() << "浮层 kind=" << kind << " 被服务端收起";
    layers_[kind].on = false;
    layers_[kind].hideTimer.reset();
    if (layers_[kind].onLayer) {
        layers_[kind].onLayer = false;
        hideLayer(*layerSurfFor(kind));
        return;
    }
    if (auto* ic = instance_->mostRecentInputContext()) {
        present(ic);
    } else {
        hidePopup();
    }
}

void WaylandCandidatePanel::hide()
{
    if (!layers_[0].on) {
        return;
    }
    layers_[0].on = false;
    pointerInside_ = false;
    hover_ = -1; // 服务端那边随候选窗收起自行复位悬停，这里只清本端状态
    rects_.clear();
    if (auto* ic = instance_->mostRecentInputContext()) {
        present(ic);
    } else {
        hidePopup();
    }
}

void WaylandCandidatePanel::hideAll()
{
    for (auto& l : layers_) {
        l.on = false;
        l.onLayer = false;
        l.hideTimer.reset();
    }
    closeScreenMenu(nullptr);
    hideLayer(statusLs_);
    hideLayer(toastLs_);
    for (auto& m : menus_) {
        m.on = false;
    }
    lastMenuMotion_.reset();
    pointerInside_ = false;
    hover_ = -1;
    rects_.clear();
    hidePopup();
}

bool WaylandCandidatePanel::present(fcitx::InputContext* ic)
{
    // 凑出要显示的层并排版。候选恒在左上角（命中表坐标因此不受影响）；tooltip 在已知悬停行时
    // 贴在候选窗右侧、与该行顶对齐，否则与状态气泡、Toast 一起竖排在候选下方。层与层之间的
    // 间隙由各层自带的阴影扩边提供。
    struct Placed {
        const SharedFrame* f;
        int32_t x, y; // 菜单的阴影扩边可能落到负坐标（超出 surface 左上，被裁掉）
    };
    Placed placed[kLayers + kMenuLevels];
    int n = 0;
    auto usable = [](const Layer& l) {
        const SharedFrame& f = l.frame;
        return l.on && !l.onLayer && f.width != 0 && f.height != 0 && f.stride >= f.width * 4
               && f.bgra.size() >= size_t(f.stride) * f.height;
    };
    uint32_t candW = 0, candH = 0, canvasW = 0, canvasH = 0;
    popupStatusRect_.reset();
    auto grow = [&](const Placed& p) {
        canvasW = std::max<int64_t>(canvasW, int64_t(p.x) + p.f->width);
        canvasH = std::max<int64_t>(canvasH, int64_t(p.y) + p.f->height);
    };
    if (usable(layers_[0])) {
        placed[n++] = {&layers_[0].frame, 0, 0};
        candW = layers_[0].frame.width;
        candH = layers_[0].frame.height;
        grow(placed[0]);
    }
    int32_t stackY = int32_t(candH);
    for (int k = 1; k < kLayers; ++k) {
        if (!usable(layers_[k])) {
            continue;
        }
        const SharedFrame* f = &layers_[k].frame;
        Placed p{f, 0, int32_t(stackY)};
        bool stacked = true;
        if (k == 1 && candW > 0) {
            // 按服务端的落位（横排：行下方 / 行上方；竖排：窗口右 / 左侧）摆：它的坐标系里候选窗
            // 左上是 (screenX, screenY)，我们的位图里候选窗在 (0,0)，平移一下即可。
            Rect r = placeOverlay(layers_[1].geo, virtualWorkArea(), -layers_[0].frame.screenX,
                                  -layers_[0].frame.screenY);
            p = {f, r.x, r.y};
            stacked = false;
        }
        if (stacked) {
            stackY += int32_t(f->height);
        }
        if (k == OVERLAY_KIND_STATUS) {
            popupStatusRect_ = Rect{p.x, p.y, int32_t(f->width), int32_t(f->height)};
        }
        placed[n++] = p;
        grow(p);
    }
    bool menuOn = false;
    for (size_t k = 0; k < kMenuLevels; ++k) {
        if (menus_[k].on && menus_[k].frame.width && menus_[k].frame.height
            && menus_[k].frame.bgra.size()
                   >= size_t(menus_[k].frame.stride) * menus_[k].frame.height) {
            placed[n] = {&menus_[k].frame, menus_[k].rect.x, menus_[k].rect.y};
            grow(placed[n]);
            ++n;
            menuOn = true;
        }
    }
    if (n == 0) {
        hidePopup();
        return true;
    }
    if (!ready() || !ensurePopup(ic)) {
        return false;
    }
    // 合成器按主 surface 摆位：皮肤带外扩时按窗体拆层 / 裁切（没皮肤时锚块是整张，照旧整张贴）。
    Rect anchor{0, 0, int32_t(canvasW), int32_t(canvasH)};
    if (candW > 0) {
        const Rect box = candidateAnchorBox(skin_, candW, candH);
        if (box.x != 0 || box.y != 0 || uint32_t(box.w) != candW || uint32_t(box.h) != candH) {
            anchor = box;
        }
    }
    const WaylandSkinPlan plan = planWaylandSkin(anchor, canvasW, canvasH, subcomp_ != nullptr, forceClip_);
    // 菜单开着时整张位图都收鼠标（菜单项的命中在服务端按位图坐标做）。
    if (menuOn) {
        candW = canvasW;
        candH = canvasH;
    }
    if (n == 1 && placed[0].x == 0 && placed[0].y == 0) {
        const SharedFrame& f = *placed[0].f;
        return attachPixels(f.bgra.data(), f.width, f.height, f.stride, candW, candH, !menuOn, plan);
    }
    const uint32_t stride = canvasW * 4;
    std::vector<uint8_t> canvas(size_t(stride) * canvasH, 0);
    for (int i = 0; i < n; ++i) {
        const SharedFrame& f = *placed[i].f;
        // 裁掉落在 surface 左 / 上之外的部分
        const int32_t sx = std::max(0, -placed[i].x), sy = std::max(0, -placed[i].y);
        for (int32_t r = sy; r < int32_t(f.height); ++r) {
            uint8_t* dst = canvas.data() + size_t(placed[i].y + r) * stride
                           + size_t(placed[i].x + sx) * 4;
            const uint8_t* src = f.bgra.data() + size_t(r) * f.stride + size_t(sx) * 4;
            // 预乘 alpha 的 over：dst = src + dst * (1 - srcA)。多数层互不重叠（dst 为 0，等于拷贝）；
            // tooltip 会压到候选窗的阴影边上，需要真混合。
            for (uint32_t px = 0; px < f.width - sx; ++px, dst += 4, src += 4) {
                const uint32_t inv = 255 - src[3];
                for (int ch = 0; ch < 4; ++ch) {
                    dst[ch] = uint8_t(std::min<uint32_t>(255, src[ch] + (dst[ch] * inv + 127) / 255));
                }
            }
        }
    }
    return attachPixels(canvas.data(), canvasW, canvasH, stride, candW, candH, !menuOn, plan);
}

wl_buffer* WaylandCandidatePanel::makeBuffer(const uint8_t* data, uint32_t w, uint32_t h,
                                             uint32_t stride)
{
    const size_t size = size_t(stride) * h;
    int fd = memfd_create("windinput-frame", MFD_CLOEXEC);
    if (fd < 0 || ftruncate(fd, off_t(size)) != 0) {
        WIND_WARN() << "memfd 建不起来，丢这一帧";
        if (fd >= 0) {
            close(fd);
        }
        return nullptr;
    }
    void* map = mmap(nullptr, size, PROT_WRITE, MAP_SHARED, fd, 0);
    if (map == MAP_FAILED) {
        close(fd);
        return nullptr;
    }
    // 服务给的是预乘 alpha 的 BGRA，小端下与 WL_SHM_FORMAT_ARGB8888 同一字节序、同一预乘约定。
    std::memcpy(map, data, size);
    munmap(map, size);
    wl_shm_pool* pool = wl_shm_create_pool(shm_, fd, int32_t(size));
    close(fd); // 合成器已持有自己的引用
    wl_buffer* buf = wl_shm_pool_create_buffer(pool, 0, int32_t(w), int32_t(h), int32_t(stride),
                                               WL_SHM_FORMAT_ARGB8888);
    wl_shm_pool_destroy(pool);
    wl_buffer_add_listener(buf, &kBufferListener, this);
    return buf;
}

void WaylandCandidatePanel::applyScale(wl_surface* s, wp_viewport*& vp, uint32_t w, uint32_t h)
{
    // 位图按物理像素光栅化（服务端用上报的缩放）；折回逻辑尺寸交给合成器，否则 150% 缩放下
    // 会按 1.5 倍大小再被放大一次。有 viewporter 时按逻辑尺寸设目标大小（分数缩放也准）；没有时
    // 只能处理整数缩放。
    const double sc = reportedScale_;
    if (viewporter_ && (sc != 1.0 || vp)) {
        if (!vp) {
            vp = wp_viewporter_get_viewport(viewporter_, s);
        }
        if (sc == 1.0) {
            wp_viewport_set_destination(vp, -1, -1);
        } else {
            wp_viewport_set_destination(vp, std::max(1, int(std::lround(w / sc))),
                                        std::max(1, int(std::lround(h / sc))));
        }
    } else if (!viewporter_ && sc >= 2.0 && std::fabs(sc - std::round(sc)) < 0.01) {
        wl_surface_set_buffer_scale(s, int(std::round(sc)));
    } else if (!viewporter_ && sc != 1.0) {
        static bool warned = false;
        if (!warned) {
            warned = true;
            WIND_WARN() << "合成器没有 wp_viewporter，分数缩放 " << sc << " 下界面大小会不准";
        }
    }
}

void WaylandCandidatePanel::setSkin(CandidateSkin skin)
{
    // 服务端先推皮肤区域、再推这一帧（设计 §17.2）：只记下，贴这一帧时设输入区。
    skin_ = std::move(skin);
}

bool WaylandCandidatePanel::attachPixels(const uint8_t* data, uint32_t w, uint32_t h,
                                         uint32_t stride, uint32_t inputW, uint32_t inputH,
                                         bool skinMask, const WaylandSkinPlan& plan)
{
    const double sc = reportedScale_;
    // 只有候选那一块接收鼠标：旁边的 tooltip、状态气泡、Toast 以及透明留白不拦指针
    // （否则指针滑进 tooltip 会触发离开 / 悬停抖动，Toast 还会挡住下面的应用）。
    // 坐标是位图像素，(ox, oy) 是该 surface 原点在位图里的位置。
    auto inputRegion = [&](int32_t ox, int32_t oy) {
        wl_region* region = wl_compositor_create_region(compositor_);
        auto add = [&](const Rect& px) {
            const Rect r = toLogicalCover(Rect{px.x - ox, px.y - oy, px.w, px.h}, sc);
            wl_region_add(region, r.x, r.y, r.w, r.h);
        };
        if (inputW > 0 && inputH > 0) {
            // 皮肤带输入区表：候选那一块只按表收（穿透装饰、透明外扩区不收，设计 §17.3）。
            auto skin = skinMask ? candidateInputRegion(skin_, inputW, inputH) : std::nullopt;
            if (skin) {
                for (const Rect& px : *skin) {
                    add(px);
                }
            } else {
                add(Rect{0, 0, int32_t(inputW), int32_t(inputH)});
            }
        }
        if (popupStatusRect_) {
            add(*popupStatusRect_); // 状态气泡也收鼠标：悬停保持、右键菜单
        }
        return region;
    };
    auto commitTo = [&](wl_surface* s, wl_buffer* b, uint32_t bw, uint32_t bh) {
        wl_surface_attach(s, b, 0, 0);
        if (compositorVersion_ >= 4) {
            wl_surface_damage_buffer(s, 0, 0, int32_t(bw), int32_t(bh));
        } else {
            wl_surface_damage(s, 0, 0, int32_t(bw), int32_t(bh));
        }
    };

    WaylandSkinPlan p = plan;
    if (p.mode == WaylandSkinMode::Split && !ensureChild()) {
        p = planWaylandSkin(Rect{p.originX, p.originY, int32_t(p.mainW), int32_t(p.mainH)}, w, h,
                            false, true); // 子 surface 建不起来：退回裁切
    }
    wl_buffer* buf = nullptr;
    if (p.mode == WaylandSkinMode::Split) {
        // 子 surface：整张位图，原点 = 位图原点，位置 = −窗体左上（逻辑坐标）。
        wl_buffer* full = makeBuffer(data, w, h, stride);
        // 主 surface：只占窗体、全透明、不收鼠标——合成器按它摆位与翻转。
        std::vector<uint8_t> clear(size_t(p.mainW) * 4 * p.mainH, 0);
        buf = makeBuffer(clear.data(), p.mainW, p.mainH, p.mainW * 4);
        if (!full || !buf) {
            if (full) {
                wl_buffer_destroy(full);
            }
            if (buf) {
                wl_buffer_destroy(buf);
            }
            return false;
        }
        applyScale(child_, childViewport_, w, h);
        wl_subsurface_set_position(childSub_, -int32_t(std::lround(p.originX / sc)),
                                   -int32_t(std::lround(p.originY / sc)));
        wl_region* childRegion = inputRegion(0, 0);
        wl_surface_set_input_region(child_, childRegion);
        wl_region_destroy(childRegion);
        commitTo(child_, full, w, h);
        wl_surface_commit(child_); // 同步模式：随下面主 surface 的提交一起生效
        childMapped_ = true;
        wl_region* none = wl_compositor_create_region(compositor_);
        wl_surface_set_input_region(surface_, none);
        wl_region_destroy(none);
    } else {
        if (p.mode == WaylandSkinMode::Clip) {
            // 只贴窗体左上起的部分：逐行拷出来（不能按原 stride 直接给 makeBuffer，末行会越界读）。
            std::vector<uint8_t> part(size_t(p.mainW) * 4 * p.mainH);
            for (uint32_t r = 0; r < p.mainH; ++r) {
                std::memcpy(part.data() + size_t(r) * p.mainW * 4,
                            data + size_t(p.originY + r) * stride + size_t(p.originX) * 4,
                            size_t(p.mainW) * 4);
            }
            buf = makeBuffer(part.data(), p.mainW, p.mainH, p.mainW * 4);
        } else {
            buf = makeBuffer(data, w, h, stride);
        }
        if (!buf) {
            return false;
        }
        unmapChild();
        wl_region* region = inputRegion(p.originX, p.originY);
        wl_surface_set_input_region(surface_, region);
        wl_region_destroy(region);
    }
    if (p.mode != lastMode_ || p.originX != mainOriginX_ || p.originY != mainOriginY_) {
        static const char* const kNames[] = {"整张", "拆层", "裁切"};
        WIND_DEBUG() << "Wayland 候选窗贴法：" << kNames[int(p.mode)] << " 主 surface " << p.mainW << "x"
                     << p.mainH << " 原点 (" << p.originX << "," << p.originY << ")";
        lastMode_ = p.mode;
    }
    mainOriginX_ = p.originX;
    mainOriginY_ = p.originY;
    applyScale(surface_, viewport_, p.mainW, p.mainH);
    commitTo(surface_, buf, p.mainW, p.mainH);
    wl_surface_commit(surface_);
    wl_display_flush(display_);
    buffer_ = buf;
    return true;
}

bool WaylandCandidatePanel::ensureChild()
{
    if (child_) {
        return true;
    }
    if (!subcomp_ || !surface_) {
        return false;
    }
    child_ = wl_compositor_create_surface(compositor_);
    if (!child_) {
        return false;
    }
    // 与主 surface 同一块占位 user_data（classicui 会读本连接上每个 surface 的 user_data，见 ensureSurface）
    void* fake = wl_surface_get_user_data(surface_);
    wl_surface_add_listener(child_, &kSurfaceListener, fake);
    wl_surface_set_user_data(child_, fake);
    childSub_ = wl_subcompositor_get_subsurface(subcomp_, child_, surface_);
    wl_subsurface_place_below(childSub_, surface_);
    childMapped_ = false;
    WIND_DEBUG() << "Wayland 候选窗：建子 surface（皮肤拆层）";
    return true;
}

void WaylandCandidatePanel::unmapChild()
{
    if (!child_ || !childMapped_) {
        return;
    }
    wl_surface_attach(child_, nullptr, 0, 0);
    wl_surface_commit(child_);
    childMapped_ = false;
}

uintptr_t WaylandCandidatePanel::retiredKey(wl_surface* s) const
{
    if (s && s == child_) {
        return reinterpret_cast<uintptr_t>(surface_);
    }
    for (const auto& r : retiredRes_) {
        if (s && s == r->child) {
            return reinterpret_cast<uintptr_t>(r->surface);
        }
    }
    return reinterpret_cast<uintptr_t>(s);
}

void WaylandCandidatePanel::reportMenuPointer(uint32_t event, uint32_t button)
{
    const int32_t x = int32_t(std::lround(pointerX_ * reportedScale_));
    const int32_t y = int32_t(std::lround(pointerY_ * reportedScale_));
    if (event == MENU_POINTER_MOTION) {
        // 同一像素不重复报：每次报都是一趟往返
        if (lastMenuMotion_ && *lastMenuMotion_ == std::make_pair(x, y)) {
            return;
        }
        lastMenuMotion_ = std::make_pair(x, y);
    }
    if (cb_.menuPointer) {
        cb_.menuPointer(event, button, x, y);
    }
}

bool WaylandCandidatePanel::menuOpen() const
{
    for (const auto& m : menus_) {
        if (m.on) {
            return true;
        }
    }
    return false;
}

Rect WaylandCandidatePanel::virtualWorkArea() const
{
    // 菜单的落位在服务端按「工作区」做；Wayland 下没有屏幕坐标，约定坐标系的原点就是 popup
    // surface 的左上角，工作区取输出的大小（位图像素）。合成器如何摆 popup 在这里不可知，
    // 所以靠近屏幕下 / 右缘时菜单可能被合成器裁掉一部分——比让菜单整个出不来强。
    const Output* o = currentOutput_ ? currentOutput_ : (outputs_.empty() ? nullptr : outputs_[0].get());
    int32_t w = 3840, h = 2160;
    if (o) {
        const double s = outputScale(*o);
        const int32_t lw = o->logW > 0 ? o->logW : int32_t(o->modeW / std::max(1.0, s));
        const int32_t lh = o->logH > 0 ? o->logH : int32_t(o->modeH / std::max(1.0, s));
        if (lw > 0 && lh > 0) {
            w = int32_t(std::lround(lw * reportedScale_));
            h = int32_t(std::lround(lh * reportedScale_));
        }
    }
    return Rect{0, 0, w, h};
}

bool WaylandCandidatePanel::showMenuLevel(fcitx::InputContext* ic, uint32_t level,
                                          const SharedFrame& f, const OverlayFramePayload& p)
{
    if (level >= kMenuLevels) {
        return false;
    }
    if (!screenMenu_ && panelMenuPending()) {
        openPanelMenu();
    }
    if (screenMenu_) {
        return showScreenMenuLevel(level, f, p);
    }
    OverlayFramePayload geo = p;
    geo.width = f.width;
    geo.height = f.height;
    menus_[level].frame = f;
    menus_[level].rect = placeOverlay(geo, virtualWorkArea(), 0, 0);
    menus_[level].on = true;
    return present(ic);
}

void WaylandCandidatePanel::hideMenuLevel(uint32_t level)
{
    if (screenMenu_) {
        hideScreenMenuLevel(level);
        return;
    }
    if (level >= kMenuLevels || !menus_[level].on) {
        return;
    }
    menus_[level].on = false;
    lastMenuMotion_.reset();
    if (auto* ic = instance_->mostRecentInputContext()) {
        present(ic);
    } else {
        hidePopup();
    }
    if (!menuOpen()) {
        armOverlayHide(OVERLAY_KIND_STATUS); // 菜单收了：气泡重新计时
    }
}

void WaylandCandidatePanel::closeMenu()
{
    closeScreenMenu(nullptr);
    if (!menuOpen()) {
        return;
    }
    for (auto& m : menus_) {
        m.on = false;
    }
    lastMenuMotion_.reset();
    if (auto* ic = instance_->mostRecentInputContext()) {
        present(ic);
    } else {
        hidePopup();
    }
    armOverlayHide(OVERLAY_KIND_STATUS);
}

bool WaylandCandidatePanel::pointerOnPopupStatus() const
{
    if (!pointerInside_ || !popupStatusRect_) {
        return false;
    }
    const Rect& r = *popupStatusRect_;
    const double px = pointerX_ * reportedScale_, py = pointerY_ * reportedScale_;
    return px >= r.x && px < r.x + r.w && py >= r.y && py < r.y + r.h;
}

void WaylandCandidatePanel::updatePopupStatusHover()
{
    const bool on = pointerOnPopupStatus();
    if (on == popupStatusHovered_) {
        return;
    }
    popupStatusHovered_ = on;
    if (on) {
        layers_[OVERLAY_KIND_STATUS].hideTimer.reset(); // 指针停在上面：不消失
    } else {
        armOverlayHide(OVERLAY_KIND_STATUS); // 离开：重新给满一份时长
    }
}

int32_t WaylandCandidatePanel::hitAtPointer() const
{
    const auto [px, py] = pointerPx();
    return hitTest(rects_, px, py);
}

std::pair<int32_t, int32_t> WaylandCandidatePanel::pointerPx() const
{
    // 命中表是位图像素坐标，指针是 surface 逻辑坐标，差的就是服务端的光栅化缩放。
    return {int32_t(std::lround(pointerX_ * reportedScale_)),
            int32_t(std::lround(pointerY_ * reportedScale_))};
}

int32_t WaylandCandidatePanel::decorAtPointer() const
{
    if (hitAtPointer() != kNoHit) {
        return -1;
    }
    const auto [px, py] = pointerPx();
    return decorAt(skin_, px, py);
}

void WaylandCandidatePanel::updateHover()
{
    const int32_t idx = hitAtPointer();
    // 悬停只报候选（>=0）：命中表里的 -1/-2 是翻页按钮，悬停协议里 -1 表示「无」。
    setHover(idx != kNoHit && idx >= 0 ? idx : -1);
}

void WaylandCandidatePanel::setHover(int32_t index)
{
    if (index == hover_) {
        return;
    }
    hover_ = index;
    if (cb_.hover) {
        cb_.hover(index);
    }
}

void WaylandCandidatePanel::hidePopup()
{
    if (!surface_ || !display_) {
        return;
    }
    click_.cancel(); // 收起后的松开不再选
    decorClick_.cancel();
    // 销毁 popup 角色（合成器据此摘窗，下一次显示重建时按当时的文本光标矩形重新摆位——只摘 buffer
    // 不销毁角色，treeland 会让候选窗一直钉在第一次出现的地方），但 **wl_surface 本身留着**：
    // 它一旦销毁，鼠标恰好正进入它时迟到的 `wl_pointer.enter` 带的是已销毁对象（libwayland 交给
    // 回调的是 NULL），Fcitx5 classicui 的指针回调对它直接解引用 → 崩 fcitx5（实测鼠标一移上
    // 候选窗就崩）。surface 活着，那些事件的 user_data（见 ensurePopup 里的占位）才有效。
    // 先摘 buffer 提交（surface 取消映射），再销毁角色：反过来会让合成器有一瞬把无角色的
    // surface 当普通内容画（实测 Toast 收起时闪出整块纯黑）。
    unmapChild();
    wl_surface_attach(surface_, nullptr, 0, 0);
    wl_surface_commit(surface_);
    if (surfaceRole_ == Role::PanelV1) {
        // v1 面板角色拆不掉，而 KWin 只认一个面板：Fcitx5 classicui 显示自己的窗口时会无声顶掉
        // 我们的，留着这个 surface 下次贴图也不会再出现。整个 surface 退役，下次显示换新的。
        retireSurface();
    } else {
        dropPopup();
    }
    wl_display_flush(display_);
    buffer_ = nullptr;
}

void WaylandCandidatePanel::retireSurface()
{
    if (!surface_) {
        return;
    }
    // 先取消映射（hidePopup 已做过的话重复一次无害）；sync 回来时合成器已处理完它（见 RetiredSurfaces.h）。
    // 子 surface 跟着主 surface 一起退役。
    unmapChild();
    wl_surface_attach(surface_, nullptr, 0, 0);
    wl_surface_commit(surface_);
    auto r = std::make_unique<Retired>();
    r->owner = this;
    r->surface = surface_;
    r->viewport = viewport_;
    r->role = panelSurf_;
    r->child = child_;
    r->childSub = childSub_;
    r->childViewport = childViewport_;
    r->sync = wl_display_sync(display_);
    wl_callback_add_listener(r->sync, &kRetiredSyncListener, r.get());
    retired_.retire(reinterpret_cast<uintptr_t>(surface_));
    retiredRes_.push_back(std::move(r));
    WIND_DEBUG() << "Wayland 候选窗：surface 退役，待销毁 " << retired_.size();
    surface_ = nullptr;
    viewport_ = nullptr;
    panelSurf_ = nullptr;
    child_ = nullptr;
    childSub_ = nullptr;
    childViewport_ = nullptr;
    childMapped_ = false;
    surfaceRole_ = Role::None;
    dropPopup();
    // 新 surface 上还没有指针：悬停状态归零（退役 surface 的离开事件不再走候选窗那条路）。
    // 只清本端、不报服务端（同 hide()：服务端随候选窗收起自行复位）——这里可能处在 present()
    // 中途，同步 IPC 一旦超时会经 noteStall 重入收起。
    pointerInside_ = false;
    hover_ = -1;
    popupStatusHovered_ = false;
}

void WaylandCandidatePanel::reapRetired(bool all, bool alive)
{
    const auto ids = all ? retired_.takeAll() : retired_.takeDestroyable();
    for (uintptr_t id : ids) {
        auto it = std::find_if(retiredRes_.begin(), retiredRes_.end(), [id](const auto& r) {
            return reinterpret_cast<uintptr_t>(r->surface) == id;
        });
        if (it == retiredRes_.end()) {
            continue;
        }
        Retired& r = **it;
        // wl_callback 与面板角色都没有 destroy 请求，*_destroy 只释放本端代理：连接死了也照样释放，
        // 免得代理的 listener 还指着下面要释放的 Retired
        if (r.sync) {
            wl_callback_destroy(r.sync);
        }
        if (r.role) {
            zwp_input_panel_surface_v1_destroy(r.role);
        }
        if (alive) {
            if (r.childSub) {
                wl_subsurface_destroy(r.childSub);
            }
            if (r.childViewport) {
                wp_viewport_destroy(r.childViewport);
            }
            if (r.child) {
                wl_surface_destroy(r.child);
            }
            if (r.viewport) {
                wp_viewport_destroy(r.viewport);
            }
            wl_surface_destroy(r.surface);
        }
        retiredRes_.erase(it);
    }
    if (!ids.empty()) {
        WIND_DEBUG() << "Wayland 候选窗：销毁退役 surface " << ids.size() << " 个，余 "
                     << retired_.size();
    }
}

// ── 屏幕锚定的浮层（wlr-layer-shell）────────────────────────────────

WaylandCandidatePanel::LayerSurf* WaylandCandidatePanel::layerSurfFor(uint32_t kind)
{
    return kind == OVERLAY_KIND_STATUS ? &statusLs_ : kind == OVERLAY_KIND_TOAST ? &toastLs_ : nullptr;
}

WaylandCandidatePanel::LayerSurf* WaylandCandidatePanel::layerSurfOf(wl_surface* s)
{
    if (!s) {
        return nullptr;
    }
    for (LayerSurf* ls : {&statusLs_, &toastLs_, &panels_[0], &panels_[1], &dragPreview_}) {
        if (s == ls->surface) {
            return ls;
        }
    }
    return nullptr;
}

WaylandCandidatePanel::LayerSurf* WaylandCandidatePanel::panelSurf(uint32_t kind)
{
    return isPanelOverlayKind(kind) ? &panels_[kind - OVERLAY_KIND_TOOLBAR] : nullptr;
}

bool WaylandCandidatePanel::isPanel(const LayerSurf& ls)
{
    return isPanelOverlayKind(ls.kind);
}

bool WaylandCandidatePanel::wantsLayer(uint32_t kind, const OverlayFramePayload& geo) const
{
    // 屏幕锚点（Toast 全部、气泡的屏幕锚点模式）与固定位置（气泡拖过 / 设了固定）按屏幕摆；
    // 跟随光标（FLIP）、跟随候选（tooltip）只能留在 popup 里由合成器按光标摆。
    // 工具栏 / 软键盘常驻屏幕，一律走 layer-shell。
    if (layerShell_ && isPanelOverlayKind(kind)) {
        return true;
    }
    if (!layerShell_ || (kind != OVERLAY_KIND_STATUS && kind != OVERLAY_KIND_TOAST)) {
        return false;
    }
    return geo.place == OVERLAY_PLACE_ANCHOR || geo.place == OVERLAY_PLACE_ABSOLUTE;
}

std::pair<int32_t, int32_t> WaylandCandidatePanel::outputLogicalSize() const
{
    const Output* o = currentOutput_ ? currentOutput_ : (outputs_.empty() ? nullptr : outputs_[0].get());
    const Rect r = o ? outputLogicalRect(*o) : noOutputRect();
    return {r.w, r.h};
}

void WaylandCandidatePanel::placeLayer(LayerSurf& ls)
{
    const OverlayFramePayload& g = ls.geo;
    const double sc = reportedScale_;
    auto lg = [sc](double px) { return int32_t(std::lround(px / sc)); };
    ls.lw = std::max(1, lg(g.width));
    ls.lh = std::max(1, lg(g.height));
    if (ls.dragging) {
        return; // 拖动中位置归指针管，新帧（如状态变化）不该把它弹回去
    }
    // 服务端的坐标 / 边距都针对内容盒（不含阴影扩边）；layer surface 摆的是整张位图，换算一下。
    const int32_t cx = lg(g.contentX), cy = lg(g.contentY);
    const int32_t cw = lg(g.contentW ? g.contentW : g.width);
    const int32_t ch = lg(g.contentH ? g.contentH : g.height);
    const int32_t m = lg(g.margin);
    const int32_t padR = ls.lw - cx - cw, padB = ls.lh - cy - ch;
    constexpr uint32_t T = ZWLR_LAYER_SURFACE_V1_ANCHOR_TOP, B = ZWLR_LAYER_SURFACE_V1_ANCHOR_BOTTOM,
                       L = ZWLR_LAYER_SURFACE_V1_ANCHOR_LEFT, R = ZWLR_LAYER_SURFACE_V1_ANCHOR_RIGHT;
    ls.mTop = ls.mRight = ls.mBottom = ls.mLeft = 0;
    if (g.place == OVERLAY_PLACE_ABSOLUTE) {
        // 固定位置：坐标系是输出（可用区）左上，单位位图像素；拖动结束时就是按这套报上去的。
        ls.anchor = T | L;
        ls.mLeft = lg(g.x - g.contentX);
        ls.mTop = lg(g.y - g.contentY);
        return;
    }
    switch (g.anchor) {
    case OVERLAY_ANCHOR_TOP_LEFT: ls.anchor = T | L; ls.mTop = m - cy; ls.mLeft = m - cx; break;
    case OVERLAY_ANCHOR_TOP_RIGHT: ls.anchor = T | R; ls.mTop = m - cy; ls.mRight = m - padR; break;
    case OVERLAY_ANCHOR_BOTTOM_LEFT: ls.anchor = B | L; ls.mBottom = m - padB; ls.mLeft = m - cx; break;
    case OVERLAY_ANCHOR_BOTTOM_RIGHT: ls.anchor = B | R; ls.mBottom = m - padB; ls.mRight = m - padR; break;
    case OVERLAY_ANCHOR_TOP_CENTER: ls.anchor = T; ls.mTop = m - cy; break;
    case OVERLAY_ANCHOR_BOTTOM_CENTER: ls.anchor = B; ls.mBottom = m - padB; break;
    case OVERLAY_ANCHOR_CENTER:
    default: ls.anchor = 0; break; // 不锚定任何边 = 合成器居中
    }
}

void WaylandCandidatePanel::showOnLayer(LayerSurf& ls, const SharedFrame& f,
                                        const OverlayFramePayload& geo)
{
    if (!ready() || f.width == 0 || f.height == 0 || f.stride < f.width * 4
        || f.bgra.size() < size_t(f.stride) * f.height) {
        return;
    }
    ls.frame = f;
    ls.geo = geo;
    ls.wanted = true;
    placeLayer(ls);
    WIND_DEBUG() << "layer 浮层 kind=" << ls.kind << " place=" << geo.place << " anchor(wire)=" << geo.anchor
                << " " << ls.lw << "x" << ls.lh << " 锚定边=" << ls.anchor << " 边距 t" << ls.mTop << " r"
                << ls.mRight << " b" << ls.mBottom << " l" << ls.mLeft << " 已映射=" << ls.mapped;
    mapLayer(ls);
}

void WaylandCandidatePanel::mapLayer(LayerSurf& ls)
{
    if (!ls.surface) {
        ls.surface = wl_compositor_create_surface(compositor_);
        // 同 popup 的 surface：classicui 会把它的 user_data 当自己的窗口包装对象读（见 ensurePopup）。
        // 本 surface 不挂 wl_surface 监听（add_listener 会把 user_data 换掉）。
        wl_surface_set_user_data(ls.surface, ls.fakeUserData);
    }
    if (!ls.layer) {
        // 气泡 / Toast 的输出传空：由合成器挑（通常是当前焦点所在的输出）。OVERLAY 层在任务栏
        // （TOP 层）之上。工具栏 / 软键盘按自己的坐标定了输出，放 TOP 层：全屏应用能盖住它们
        // （Wayland 拿不到前台全屏状态，全屏隐藏靠这个近似），也不压过锁屏和通知。
        const bool panel = isPanel(ls);
        const Output* out = panel ? outputByName(ls.outputName) : nullptr;
        const char* ns = ls.kind == OVERLAY_KIND_TOAST           ? "windinput-toast"
                         : ls.kind == OVERLAY_KIND_TOOLBAR      ? "windinput-toolbar"
                         : ls.kind == OVERLAY_KIND_SOFTKEYBOARD ? "windinput-softkeyboard"
                                                                : "windinput-status";
        ls.layer = zwlr_layer_shell_v1_get_layer_surface(
            layerShell_, ls.surface, out ? out->wl : nullptr,
            panel ? ZWLR_LAYER_SHELL_V1_LAYER_TOP : ZWLR_LAYER_SHELL_V1_LAYER_OVERLAY, ns);
        ls.boundOutput = ls.outputName;
        zwlr_layer_surface_v1_add_listener(ls.layer, &kLayerListener, &ls);
        zwlr_layer_surface_v1_set_keyboard_interactivity(ls.layer, 0); // 不抢键盘
        // 0 = 自己不占独占区，但避让别人的（任务栏），锚在底边时落在任务栏上方
        zwlr_layer_surface_v1_set_exclusive_zone(ls.layer, 0);
        ls.configured = ls.waiting = ls.mapped = false;
    }
    if (ls.configured) {
        commitLayer(ls);
        return;
    }
    if (!ls.waiting) {
        // layer-shell 的映射流程：先不带 buffer 提交一次状态，等 configure、应答后才能贴图
        zwlr_layer_surface_v1_set_size(ls.layer, uint32_t(ls.lw), uint32_t(ls.lh));
        zwlr_layer_surface_v1_set_anchor(ls.layer, ls.anchor);
        zwlr_layer_surface_v1_set_margin(ls.layer, ls.mTop, ls.mRight, ls.mBottom, ls.mLeft);
        wl_surface_commit(ls.surface);
        ls.waiting = true;
        wl_display_flush(display_);
    }
}

void WaylandCandidatePanel::layerConfigured(LayerSurf& ls, uint32_t serial)
{
    zwlr_layer_surface_v1_ack_configure(ls.layer, serial);
    ls.configured = true;
    ls.waiting = false;
    if (ls.wanted && !ls.mapped) {
        commitLayer(ls);
    }
}

void WaylandCandidatePanel::commitLayer(LayerSurf& ls)
{
    const SharedFrame& f = ls.frame;
    // 拖动源保持映射和输入区域，透明像素保留按下时的隐式抓取。
    // 预览单独映射到目标输出；跨屏不拆源角色，松手仍能收到。
    const bool dragSource = isPanel(ls) && ls.dragging && &ls != &dragPreview_;
    std::vector<uint8_t> transparent;
    if (dragSource) {
        transparent.resize(f.bgra.size(), 0);
    }
    wl_buffer* buf = makeBuffer(dragSource ? transparent.data() : f.bgra.data(),
                                 f.width, f.height, f.stride);
    if (!buf) {
        return;
    }
    zwlr_layer_surface_v1_set_size(ls.layer, uint32_t(ls.lw), uint32_t(ls.lh));
    zwlr_layer_surface_v1_set_anchor(ls.layer, ls.anchor);
    zwlr_layer_surface_v1_set_margin(ls.layer, ls.mTop, ls.mRight, ls.mBottom, ls.mLeft);
    applyScale(ls.surface, ls.viewport, f.width, f.height);
    if (&ls == &dragPreview_) {
        wl_region* empty = wl_compositor_create_region(compositor_);
        wl_surface_set_input_region(ls.surface, empty);
        wl_region_destroy(empty);
    }
    wl_surface_attach(ls.surface, buf, 0, 0);
    if (compositorVersion_ >= 4) {
        wl_surface_damage_buffer(ls.surface, 0, 0, int32_t(f.width), int32_t(f.height));
    } else {
        wl_surface_damage(ls.surface, 0, 0, int32_t(f.width), int32_t(f.height));
    }
    wl_surface_commit(ls.surface);
    wl_display_flush(display_);
    ls.mapped = true;
}

void WaylandCandidatePanel::hideLayer(LayerSurf& ls)
{
    ls.wanted = false;
    ls.dragging = false;
    if (!ls.surface || !display_) {
        return;
    }
    if (ls.mapped) {
        // 先摘 buffer 提交（取消映射），再连角色对象一起销毁：下次显示建新的角色对象、走全新的
        // 「空提交 → configure」。只摘 buffer 不换角色对象时，treeland 在状态没变的重新提交上
        // 不再发 configure（实测：菜单底板第二次打开就一直等不到 configure）。
        wl_surface_attach(ls.surface, nullptr, 0, 0);
        wl_surface_commit(ls.surface);
    }
    if (ls.layer) {
        zwlr_layer_surface_v1_destroy(ls.layer);
        ls.layer = nullptr;
    }
    ls.mapped = ls.configured = ls.waiting = false;
    wl_display_flush(display_);
}

void WaylandCandidatePanel::layerClosed(LayerSurf& ls)
{
    // 合成器收回了这个层（如所在输出被拔掉）：丢掉角色对象，surface 留着，下次显示重建角色
    WIND_DEBUG() << "layer surface kind=" << ls.kind << " 被合成器关闭";
    const bool wasLive = ls.configured;
    if (&ls == &dragPreview_) {
        hideLayer(ls);
        for (auto& panel : panels_) {
            endPanelDrag(panel, false);
        }
        return;
    }
    if (isPanel(ls)) {
        hideLayer(dragPreview_);
    }
    if (ls.layer) {
        zwlr_layer_surface_v1_destroy(ls.layer);
        ls.layer = nullptr;
    }
    ls.configured = ls.waiting = ls.mapped = false;
    ls.dragging = false;
    if (isPanel(ls)) {
        panelPointerGone(ls);
        // 常驻的面板不能等服务端下一帧（可能很久不来）：按现有输出重摆。刚建就被关（没 configure
        // 过）的不重试，免得合成器一直拒、这边一直建
        if (ls.wanted && wasLive) {
            ls.reqValid = false;
            armTimer(panelRetry_, 0, &WaylandCandidatePanel::onPanelRetry);
        } else if (ls.wanted) {
            WIND_WARN() << "layer surface kind=" << ls.kind << " 未映射就被合成器关闭，不再重试";
        }
    }
}

std::pair<int32_t, int32_t> WaylandCandidatePanel::layerTopLeft(const LayerSurf& ls) const
{
    // 合成器不告诉 layer surface 的实际位置：按锚点 + 边距 + 输出逻辑尺寸推算。
    // 别人的独占区（任务栏）不计在内，锚在底 / 右边时可能差一个任务栏的厚度。
    const auto [ow, oh] = outputLogicalSize();
    constexpr uint32_t T = ZWLR_LAYER_SURFACE_V1_ANCHOR_TOP, B = ZWLR_LAYER_SURFACE_V1_ANCHOR_BOTTOM,
                       L = ZWLR_LAYER_SURFACE_V1_ANCHOR_LEFT, R = ZWLR_LAYER_SURFACE_V1_ANCHOR_RIGHT;
    const int32_t x = (ls.anchor & L) ? ls.mLeft
                      : (ls.anchor & R) ? ow - ls.mRight - ls.lw
                                        : (ow - ls.lw) / 2;
    const int32_t y = (ls.anchor & T) ? ls.mTop
                      : (ls.anchor & B) ? oh - ls.mBottom - ls.lh
                                        : (oh - ls.lh) / 2;
    return {x, y};
}

void WaylandCandidatePanel::beginStatusDrag(LayerSurf& ls, double x, double y)
{
    if (!ls.layer || !ls.mapped) {
        return;
    }
    // 改成左上锚定、边距 = 当前左上角，此后边距直接跟着指针走
    const auto [left, top] = layerTopLeft(ls);
    ls.anchor = ZWLR_LAYER_SURFACE_V1_ANCHOR_TOP | ZWLR_LAYER_SURFACE_V1_ANCHOR_LEFT;
    ls.mTop = top;
    ls.mLeft = left;
    ls.mRight = ls.mBottom = 0;
    ls.grabX = x;
    ls.grabY = y;
    ls.dragging = true;
    dragAccX_ = dragAccY_ = 0;
    WIND_DEBUG() << "开始拖动状态气泡 @(" << left << "," << top << ") 相对指针 "
                << (relPtr_ ? "有" : "无");
    layers_[ls.kind].hideTimer.reset();
}

void WaylandCandidatePanel::moveStatusDrag(LayerSurf& ls, double x, double y)
{
    if (!ls.layer) {
        return;
    }
    // 指针坐标是相对 surface 的：surface 跟着动以后，指针仍应停在按下时那一点
    const int32_t dx = int32_t(std::lround(x - ls.grabX));
    const int32_t dy = int32_t(std::lround(y - ls.grabY));
    if (dx == 0 && dy == 0) {
        return;
    }
    const auto [ow, oh] = outputLogicalSize();
    // 内容盒不出屏（阴影扩边可以出去），同 X11 的 dragOverlay
    const double sc = reportedScale_;
    const int32_t cx = int32_t(std::lround(ls.geo.contentX / sc));
    const int32_t cy = int32_t(std::lround(ls.geo.contentY / sc));
    const int32_t cw = int32_t(std::lround((ls.geo.contentW ? ls.geo.contentW : ls.geo.width) / sc));
    const int32_t ch = int32_t(std::lround((ls.geo.contentH ? ls.geo.contentH : ls.geo.height) / sc));
    ls.mLeft = std::clamp(ls.mLeft + dx, -cx, std::max(-cx, ow - cx - cw));
    ls.mTop = std::clamp(ls.mTop + dy, -cy, std::max(-cy, oh - cy - ch));
    zwlr_layer_surface_v1_set_margin(ls.layer, ls.mTop, 0, 0, ls.mLeft);
    wl_surface_commit(ls.surface);
    wl_display_flush(display_);
}

void WaylandCandidatePanel::ensureRelativePointer()
{
    if (relPtrMgr_ && pointer_ && !relPtr_) {
        relPtr_ = zwp_relative_pointer_manager_v1_get_relative_pointer(relPtrMgr_, pointer_);
        zwp_relative_pointer_v1_add_listener(relPtr_, &kRelPtrListener, this);
    }
}

void WaylandCandidatePanel::moveStatusDragBy(LayerSurf& ls, double dx, double dy)
{
    if (!ls.layer) {
        return;
    }
    dragAccX_ += dx;
    dragAccY_ += dy;
    const int32_t ix = int32_t(dragAccX_), iy = int32_t(dragAccY_);
    if (ix == 0 && iy == 0) {
        return;
    }
    dragAccX_ -= ix;
    dragAccY_ -= iy;
    // 复用按绝对坐标的那条：把位移当作「指针离按下点的偏移」喂进去
    moveStatusDrag(ls, ls.grabX + ix, ls.grabY + iy);
}

void WaylandCandidatePanel::endStatusDrag(LayerSurf& ls)
{
    if (!ls.dragging) {
        return;
    }
    ls.dragging = false;
    if (auto at = statusContentOrigin(); at && cb_.statusMoved) {
        cb_.statusMoved(at->first, at->second);
    }
    if (!ls.hovered) {
        armOverlayHide(ls.kind);
    }
}

std::optional<std::pair<int32_t, int32_t>> WaylandCandidatePanel::statusContentOrigin() const
{
    if (!statusLs_.mapped) {
        return std::nullopt;
    }
    const auto [x, y] = layerTopLeft(statusLs_);
    const double sc = reportedScale_;
    return std::make_pair(int32_t(std::lround(x * sc)) + statusLs_.geo.contentX,
                          int32_t(std::lround(y * sc)) + statusLs_.geo.contentY);
}

// ── 工具栏 / 软键盘（wlr-layer-shell，TOP 层）──────────────────────────────

Rect WaylandCandidatePanel::outputLogicalRect(const Output& o)
{
    const double s = std::max(1.0, outputScale(o));
    const int32_t w = o.logW > 0 ? o.logW : int32_t(o.modeW / s);
    const int32_t h = o.logH > 0 ? o.logH : int32_t(o.modeH / s);
    const Rect none = (w > 0 && h > 0) ? Rect{} : noOutputRect();
    return Rect{o.logX, o.logY, w > 0 ? w : none.w, h > 0 ? h : none.h};
}

Rect WaylandCandidatePanel::usableLogicalRect(const Output& o)
{
    return layerUsableArea(outputLogicalRect(o), o.availW, o.availH);
}

Rect WaylandCandidatePanel::noOutputRect()
{
    WIND_DEBUG() << "没有可用的输出尺寸，按 1920x1080 兜底";
    return Rect{0, 0, 1920, 1080};
}

void WaylandCandidatePanel::ensureWorkProbes()
{
    if (!compositor_ || !layerShell_) {
        return;
    }
    for (auto& op : outputs_) {
        Output& o = *op;
        if (o.probe) {
            continue;
        }
        if (!o.probeSurface) {
            o.probeSurface = wl_compositor_create_surface(compositor_);
            wl_surface_set_user_data(o.probeSurface, o.fakeUserData); // 同 mapLayer
        }
        // 层与面板相同（TOP）；独占区 0 = 自己不占、但避让别人的，与面板受的约束一致。
        o.probe = zwlr_layer_shell_v1_get_layer_surface(layerShell_, o.probeSurface, o.wl,
                                                        ZWLR_LAYER_SHELL_V1_LAYER_TOP,
                                                        "windinput-probe");
        zwlr_layer_surface_v1_add_listener(o.probe, &kProbeListener, &o);
        zwlr_layer_surface_v1_set_keyboard_interactivity(o.probe, 0);
        zwlr_layer_surface_v1_set_exclusive_zone(o.probe, 0);
        zwlr_layer_surface_v1_set_anchor(o.probe, ZWLR_LAYER_SURFACE_V1_ANCHOR_TOP
                                                      | ZWLR_LAYER_SURFACE_V1_ANCHOR_BOTTOM
                                                      | ZWLR_LAYER_SURFACE_V1_ANCHOR_LEFT
                                                      | ZWLR_LAYER_SURFACE_V1_ANCHOR_RIGHT);
        zwlr_layer_surface_v1_set_size(o.probe, 0, 0);
        // 只做这一次空提交换 configure；之后从不贴 buffer，surface 一直不映射。
        wl_surface_commit(o.probeSurface);
    }
    if (display_) {
        wl_display_flush(display_);
    }
}

void WaylandCandidatePanel::destroyWorkProbe(Output& o, bool alive)
{
    if (alive) {
        if (o.probe) {
            zwlr_layer_surface_v1_destroy(o.probe);
        }
        if (o.probeSurface) {
            wl_surface_destroy(o.probeSurface);
        }
    }
    o.probe = nullptr;
    o.probeSurface = nullptr;
    o.availW = o.availH = 0;
}

void WaylandCandidatePanel::probeConfigured(Output& o, uint32_t serial, uint32_t w, uint32_t h)
{
    zwlr_layer_surface_v1_ack_configure(o.probe, serial);
    if (int32_t(w) == o.availW && int32_t(h) == o.availH) {
        return;
    }
    o.availW = int32_t(w);
    o.availH = int32_t(h);
    WIND_DEBUG() << "输出 " << (o.wlName.empty() ? std::to_string(o.name) : o.wlName) << " 可用区 "
                 << w << "x" << h;
    if (onWorkChanged_) {
        onWorkChanged_();
    }
}

WaylandCandidatePanel::Output* WaylandCandidatePanel::outputByName(uint32_t name) const
{
    for (const auto& o : outputs_) {
        if (o->name == name) {
            return o.get();
        }
    }
    return nullptr;
}

void WaylandCandidatePanel::onGlobalRemove(uint32_t name)
{
    auto it = std::find_if(outputs_.begin(), outputs_.end(),
                           [name](const auto& o) { return o->name == name; });
    if (it == outputs_.end()) {
        return;
    }
    Output* o = it->get();
    WIND_INFO() << "输出 " << (o->wlName.empty() ? std::to_string(name) : o->wlName) << " 已移除";
    if (currentOutput_ == o) {
        currentOutput_ = nullptr;
    }
    destroyWorkProbe(*o, true);
    if (o->xdg) {
        zxdg_output_v1_destroy(o->xdg);
    }
    if (o->version >= 3) {
        wl_output_release(o->wl);
    } else {
        wl_output_destroy(o->wl);
    }
    outputs_.erase(it);
    // 面板在这块屏上：换到还在的屏上重摆（合成器多半也会发 closed，重摆是幂等的）
    for (LayerSurf& ls : panels_) {
        if (ls.wanted && ls.outputName == name) {
            ls.reqValid = false;
            armTimer(panelRetry_, 0, &WaylandCandidatePanel::onPanelRetry);
        }
    }
    scaleMaybeChanged();
}

std::optional<HostOutput> WaylandCandidatePanel::hostOutput() const
{
    if (!layerShell_ || !ready()) {
        return std::nullopt;
    }
    const Output* o = currentOutput_ ? currentOutput_ : (outputs_.empty() ? nullptr : outputs_[0].get());
    if (!o) {
        return std::nullopt;
    }
    HostOutput out;
    out.name = o->wlName;
    out.work = scaleRect(usableLogicalRect(*o), reportedScale_);
    return out;
}

bool WaylandCandidatePanel::showPanel(uint32_t kind, const SharedFrame& f, const OverlayFramePayload& p)
{
    LayerSurf* ls = panelSurf(kind);
    if (!ls) {
        return false;
    }
    if (!layerShell_) {
        if (!warnedNoLayerShell_) {
            warnedNoLayerShell_ = true;
            WIND_WARN() << "合成器没有 wlr-layer-shell：Wayland 下不显示工具栏 / 软键盘";
        }
        return false;
    }
    if (!ready() || f.width == 0 || f.height == 0 || f.stride < f.width * 4
        || f.bgra.size() < size_t(f.stride) * f.height) {
        return false;
    }
    OverlayFramePayload geo = p;
    geo.width = f.width;
    geo.height = f.height;
    const double sc = reportedScale_;
    ls->frame = f;
    ls->lw = std::max(1, int32_t(std::lround(f.width / sc)));
    ls->lh = std::max(1, int32_t(std::lround(f.height / sc)));
    if ((ls->mapped || ls->waiting)
        && (ls->dragging
            || (ls->reqValid && p.x == ls->reqX && p.y == ls->reqY && f.width == ls->pixW
                && f.height == ls->pixH))) {
        // 拖动中：只换像素、不挪——服务端还不知道新位置，帧里是拖动前的坐标。
        // 原地重绘（悬停高亮、按键反馈…）：沿用上次的落位；拖完到服务端回新坐标之前来的帧也走这里。
    } else {
        // EXACT 原样摆，再夹进窗口中心所在输出（不在任何输出上取候选窗所在的那块）：服务端记下的
        // 位置可能在换过布局的屏幕外面。
        std::vector<Rect> logical, px;
        for (const auto& o : outputs_) {
            logical.push_back(outputLogicalRect(*o));
            px.push_back(scaleRect(logical.back(), sc));
        }
        int fallback = 0;
        for (size_t i = 0; i < outputs_.size(); ++i) {
            if (outputs_[i].get() == currentOutput_) {
                fallback = int(i);
            }
        }
        const Rect wa = px.empty() ? scaleRect(noOutputRect(), sc) : px[size_t(fallback)];
        Rect r = placeOverlay(geo, wa, 0, 0);
        const int i = panelOutputIndex(px, r, fallback);
        // 夹进可用区（不压任务栏）；边距只用原点，与整块输出相同。
        Rect outLogical = noOutputRect();
        ls->outputName = 0;
        if (i >= 0) {
            outLogical = usableLogicalRect(*outputs_[size_t(i)]);
            ls->outputName = outputs_[size_t(i)]->name;
        }
        r = clampToWorkArea(r, scaleRect(outLogical, sc));
        const auto [ml, mt] = layerMarginFor(r.x, r.y, outLogical, sc);
        ls->anchor = ZWLR_LAYER_SURFACE_V1_ANCHOR_TOP | ZWLR_LAYER_SURFACE_V1_ANCHOR_LEFT;
        ls->mLeft = ml;
        ls->mTop = mt;
        ls->mRight = ls->mBottom = 0;
        ls->reqValid = true;
        ls->reqX = p.x;
        ls->reqY = p.y;
        ls->pixW = f.width;
        ls->pixH = f.height;
        WIND_DEBUG() << "layer 面板 kind=" << kind << " " << ls->lw << "x" << ls->lh << " 输出 "
                     << (i >= 0 ? outputs_[size_t(i)]->wlName : std::string("?")) << " 边距 l" << ml
                     << " t" << mt << " 已映射=" << ls->mapped;
    }
    ls->geo = geo;
    if (ls->dragging) {
        dragPreview_.frame = f;
        dragPreview_.geo = geo;
        mapLayer(dragPreview_);
    }
    ls->wanted = true;
    if (ls->layer && ls->boundOutput != ls->outputName) {
        // 换屏：layer surface 的输出建角色时就定了，只能连角色一起重建（指针在上面的话合成器会发离开）
        hideLayer(*ls);
        ls->wanted = true;
    }
    mapLayer(*ls);
    return true;
}

void WaylandCandidatePanel::hidePanel(LayerSurf& ls)
{
    if (!ls.wanted && !ls.layer) {
        return;
    }
    WIND_DEBUG() << "layer 面板 kind=" << ls.kind << " 收起";
    panelPointerGone(ls);
    if (pointerLayer_ == &ls) {
        pointerLayer_ = nullptr;
    }
    // 接下来直接销毁源角色，不再提交真实像素，避免收起前闪现一帧。
    endPanelDrag(ls, false, false);
    hideLayer(ls); // 连角色对象一起销毁（treeland 对无变化的重提交不发 configure），拖动作废不报
    ls.reqValid = false;
}

std::pair<int32_t, int32_t> WaylandCandidatePanel::panelPointerPos() const
{
    return {int32_t(std::lround(layerPtrX_ * reportedScale_)),
            int32_t(std::lround(layerPtrY_ * reportedScale_))};
}

void WaylandCandidatePanel::panelPointer(LayerSurf& ls, uint32_t event, uint32_t button)
{
    const auto [x, y] = panelPointerPos();
    ls.pointerIn = event != PANEL_POINTER_LEAVE;
    if (event == PANEL_POINTER_MOTION) {
        // 每次上报都是一趟同步往返：本轮分发里的移动只报最后一个（同 X11 按批合并）
        if (pendingPanelMotion_ && pendingPanelMotion_->kind != ls.kind) {
            flushPanelMotion();
        }
        pendingPanelMotion_ = PanelMotion{ls.kind, x, y};
        armTimer(panelMotionFlush_, 0, &WaylandCandidatePanel::flushPanelMotion);
        return;
    }
    flushPanelMotion(); // 保序：积着的移动先报
    if (event == PANEL_POINTER_PRESS || event == PANEL_POINTER_RELEASE) {
        // 服务端可能因此开菜单（右键在按下时、设置键在松开时）：记下面板在哪块屏上
        panelMenu_.note(ls.boundOutput, fcitx::now(CLOCK_MONOTONIC));
    }
    if (cb_.panelPointer) {
        cb_.panelPointer(ls.kind, event, button, x, y);
    }
}

bool WaylandCandidatePanel::panelMenuPending() const
{
    // 菜单帧在服务端处理完这次按下后紧接着推来；窗口容得下一次卡顿。期间本端别处发起的菜单
    // 请求会清掉它（clearPanelMenuPending）
    return layerShell_ && subcomp_ && ready() && panelMenu_.pending(fcitx::now(CLOCK_MONOTONIC));
}

void WaylandCandidatePanel::openPanelMenu()
{
    closeMenu(); // popup 里若还开着菜单，先收（不报：服务端开新菜单时自己会关旧的）
    screenMenu_ = true;
    screenMenuShown_ = false;
    pendingMenu_.reset(); // 菜单已由服务端开好，不用再请求
    catcherOutput_ = panelMenu_.take(); // 一次按下只认一个菜单
    menuOriginX_ = menuOriginY_ = 0;
    if (const Output* o = outputByName(catcherOutput_)) {
        const Rect r = scaleRect(outputLogicalRect(*o), reportedScale_);
        menuOriginX_ = r.x;
        menuOriginY_ = r.y;
    }
    WIND_DEBUG() << "面板发起的菜单：底板开在输出 " << catcherOutput_ << "，原点 (" << menuOriginX_ << ","
                 << menuOriginY_ << ")";
    armTimer(menuMax_, menuMaxMs_, &WaylandCandidatePanel::onMenuMaxExpired);
    armTimer(menuIdle_, menuIdleMs_, &WaylandCandidatePanel::onMenuIdleExpired);
    armTimer(menuWait_, 2000, &WaylandCandidatePanel::onMenuWaitExpired); // 底板迟迟映射不上
    showCatcher();
}

void WaylandCandidatePanel::flushPanelMotion()
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

void WaylandCandidatePanel::panelPointerGone(LayerSurf& ls)
{
    if (!ls.pointerIn) {
        return;
    }
    ls.pointerIn = false;
    flushPanelMotion();
    // 坐标无意义：服务端的 LEAVE 不更新「最后指针」。
    if (cb_.panelPointer) {
        cb_.panelPointer(ls.kind, PANEL_POINTER_LEAVE, 0, 0, 0);
    }
}

void WaylandCandidatePanel::beginPanelDrag(LayerSurf& ls)
{
    if (!ls.layer || !ls.mapped) {
        return;
    }
    flushPanelMotion();
    ls.dragging = true;
    ls.grabX = layerPtrX_;
    ls.grabY = layerPtrY_;
    const Output* o = outputByName(ls.boundOutput);
    const Rect out = o ? outputLogicalRect(*o) : noOutputRect();
    ls.dragX = out.x + ls.mLeft;
    ls.dragY = out.y + ls.mTop;
    dragPreview_.kind = ls.kind;
    dragPreview_.frame = ls.frame;
    dragPreview_.geo = ls.geo;
    dragPreview_.lw = ls.lw;
    dragPreview_.lh = ls.lh;
    dragPreview_.anchor = ls.anchor;
    dragPreview_.outputName = ls.boundOutput;
    dragPreview_.mLeft = ls.mLeft;
    dragPreview_.mTop = ls.mTop;
    dragPreview_.mRight = dragPreview_.mBottom = 0;
    dragPreview_.wanted = true;
    mapLayer(dragPreview_);
    commitLayer(ls);
    dragAccX_ = dragAccY_ = 0;
    WIND_DEBUG() << "开始拖动 layer 面板 kind=" << ls.kind << " @(" << ls.mLeft << "," << ls.mTop
                 << ") 相对指针 " << (relPtr_ ? "有" : "无");
}

void WaylandCandidatePanel::movePanelDrag(LayerSurf& ls, int32_t dx, int32_t dy)
{
    if (!ls.layer || (dx == 0 && dy == 0)) {
        return;
    }
    ls.dragX += dx;
    ls.dragY += dy;
    std::vector<Rect> outputs;
    int fallback = 0;
    for (size_t i = 0; i < outputs_.size(); ++i) {
        outputs.push_back(usableLogicalRect(*outputs_[i]));
        if (outputs_[i]->name == dragPreview_.outputName) {
            fallback = int(i);
        }
    }
    const Rect desired{ls.dragX, ls.dragY, ls.lw, ls.lh};
    const auto placement = placePanelDrag(outputs, desired, fallback);
    const int index = placement.output;
    const Rect out = index >= 0 ? outputs[size_t(index)] : noOutputRect();
    const uint32_t target = index >= 0 ? outputs_[size_t(index)]->name : 0;
    if (dragPreview_.layer && dragPreview_.boundOutput != target) {
        hideLayer(dragPreview_);
    }
    const Rect r = placement.rect;
    dragPreview_.outputName = target;
    dragPreview_.mLeft = r.x - out.x;
    dragPreview_.mTop = r.y - out.y;
    dragPreview_.wanted = true;
    mapLayer(dragPreview_);
}

void WaylandCandidatePanel::movePanelDragBy(LayerSurf& ls, double dx, double dy)
{
    dragAccX_ += dx;
    dragAccY_ += dy;
    const int32_t ix = int32_t(dragAccX_), iy = int32_t(dragAccY_);
    dragAccX_ -= ix;
    dragAccY_ -= iy;
    movePanelDrag(ls, ix, iy);
}

void WaylandCandidatePanel::endPanelDrag(LayerSurf& ls, bool report, bool restoreSource)
{
    if (!ls.dragging) {
        return;
    }
    ls.dragging = false;
    const uint32_t target = dragPreview_.outputName;
    const int32_t left = dragPreview_.mLeft, top = dragPreview_.mTop;
    hideLayer(dragPreview_);
    if (!report) {
        if (restoreSource && ls.mapped && ls.layer) {
            commitLayer(ls);
        }
        return;
    }
    if (ls.layer && ls.boundOutput != target) {
        hideLayer(ls);
    }
    ls.outputName = target;
    ls.mLeft = left;
    ls.mTop = top;
    ls.wanted = true;
    mapLayer(ls);
    if (!cb_.panelMoved) {
        return;
    }
    // 松手只报这一次 `pos.*`（拖动中的移动一个都不回传）；输出名与 host.display 同源。
    const Output* o = outputByName(ls.outputName);
    const Rect out = o ? outputLogicalRect(*o) : noOutputRect();
    const auto [x, y] = layerOriginFor(ls.mLeft, ls.mTop, out, reportedScale_);
    const std::string name = o ? o->wlName : std::string();
    WIND_DEBUG() << "layer 面板 kind=" << ls.kind << " 拖动松手，上报内容左上 (" << x + ls.geo.contentX
                 << "," << y + ls.geo.contentY << ") output=" << name;
    cb_.panelMoved(ls.kind, x + ls.geo.contentX, y + ls.geo.contentY, name);
}

void WaylandCandidatePanel::onPanelRetry()
{
    for (LayerSurf& ls : panels_) {
        if (ls.wanted && !ls.dragging) {
            const SharedFrame f = ls.frame;
            const OverlayFramePayload geo = ls.geo;
            ls.reqValid = false;
            showPanel(ls.kind, f, geo);
        }
    }
}

// ── 屏幕坐标系的菜单（状态气泡右键）──────────────────────────────────

void WaylandCandidatePanel::armTimer(std::unique_ptr<fcitx::EventSourceTime>& t, uint32_t ms,
                                     void (WaylandCandidatePanel::*fn)())
{
    const uint64_t due = fcitx::now(CLOCK_MONOTONIC) + uint64_t(ms) * 1000;
    if (t) {
        t->setTime(due);
        t->setOneShot();
        return;
    }
    t = instance_->eventLoop().addTimeEvent(CLOCK_MONOTONIC, due, 0,
                                            [this, fn](fcitx::EventSourceTime*, uint64_t) {
                                                (this->*fn)();
                                                return true;
                                            });
}

void WaylandCandidatePanel::disarm(std::unique_ptr<fcitx::EventSourceTime>& t)
{
    // 只停不毁：会从这些计时器自己的回调里进来
    if (t) {
        t->setEnabled(false);
    }
}

bool WaylandCandidatePanel::openMenuAtPointer(int32_t target)
{
    if (!layerShell_ || !subcomp_ || !ready()) {
        return false;
    }
    openScreenMenu(target, 0, 0, true);
    return true;
}

void WaylandCandidatePanel::openContextMenu(int32_t target)
{
    if (layerShell_ && subcomp_) {
        // 合成进 popup 位图的话，菜单一大位图跟着变大，合成器重摆整块（翻到光标上方、夹回屏内），
        // 候选窗被挤走；而且 popup 的屏幕位置不可知，开不了接住菜单外点击的底板。
        openScreenMenu(target, 0, 0, true);
        return;
    }
    if (cb_.contextMenu) {
        cb_.contextMenu(target, int32_t(std::lround(pointerX_ * reportedScale_)),
                        int32_t(std::lround(pointerY_ * reportedScale_)));
    }
}

void WaylandCandidatePanel::openScreenMenu(int32_t target, int32_t x, int32_t y, bool locate)
{
    if (!layerShell_ || !subcomp_ || !ready()) {
        WIND_WARN() << "开不了屏幕菜单：layer-shell/subcompositor 缺";
        return;
    }
    WIND_DEBUG() << "请求屏幕菜单 target=" << target << " @(" << x << "," << y << ")";
    closeMenu(); // popup 里若还开着菜单，先收（不报：服务端开新菜单时自己会关旧的）
    screenMenu_ = true;
    screenMenuShown_ = false;
    pendingMenu_ = PendingMenu{target, x, y, locate};
    panelMenu_.clear(); // 本端请求的菜单，不是面板的
    catcherOutput_ = 0;
    menuOriginX_ = menuOriginY_ = 0;
    layers_[OVERLAY_KIND_STATUS].hideTimer.reset(); // 菜单开着期间气泡不消失
    armTimer(menuMax_, menuMaxMs_, &WaylandCandidatePanel::onMenuMaxExpired);
    armTimer(menuIdle_, menuIdleMs_, &WaylandCandidatePanel::onMenuIdleExpired);
    showCatcher();
    if (locate) {
        // 指针一直不进底板（理论上松开右键、底板映射后合成器就会发进入）：别让底板挡着桌面
        armTimer(menuWait_, 2000, &WaylandCandidatePanel::onMenuWaitExpired);
    }
    if (catcher_.mapped) {
        sendPendingMenu();
    }
}

void WaylandCandidatePanel::showCatcher()
{
    if (!catcher_.surface) {
        catcher_.surface = wl_compositor_create_surface(compositor_);
        wl_surface_set_user_data(catcher_.surface, catcher_.fakeUserData);
    }
    if (!catcher_.layer) {
        const Output* out = outputByName(catcherOutput_);
        catcher_.layer = zwlr_layer_shell_v1_get_layer_surface(
            layerShell_, catcher_.surface, out ? out->wl : nullptr, ZWLR_LAYER_SHELL_V1_LAYER_OVERLAY,
            "windinput-menu");
        zwlr_layer_surface_v1_add_listener(catcher_.layer, &kCatcherListener, this);
        zwlr_layer_surface_v1_set_keyboard_interactivity(catcher_.layer, 0);
        // 铺满整个输出、无视别人的独占区：surface 坐标 = 输出坐标
        zwlr_layer_surface_v1_set_exclusive_zone(catcher_.layer, -1);
        zwlr_layer_surface_v1_set_anchor(catcher_.layer, ZWLR_LAYER_SURFACE_V1_ANCHOR_TOP
                                                             | ZWLR_LAYER_SURFACE_V1_ANCHOR_BOTTOM
                                                             | ZWLR_LAYER_SURFACE_V1_ANCHOR_LEFT
                                                             | ZWLR_LAYER_SURFACE_V1_ANCHOR_RIGHT);
        zwlr_layer_surface_v1_set_size(catcher_.layer, 0, 0);
        catcher_.configured = catcher_.waiting = catcher_.mapped = false;
    }
    if (!catcher_.mapped && !catcher_.waiting && !catcher_.configured) {
        wl_surface_commit(catcher_.surface);
        catcher_.waiting = true;
        wl_display_flush(display_);
    } else if (catcher_.configured && !catcher_.mapped) {
        catcherConfigured(0, uint32_t(catcher_.w), uint32_t(catcher_.h));
    }
}

void WaylandCandidatePanel::catcherConfigured(uint32_t serial, uint32_t w, uint32_t h)
{
    if (serial) {
        zwlr_layer_surface_v1_ack_configure(catcher_.layer, serial);
    }
    catcher_.configured = true;
    catcher_.waiting = false;
    catcher_.w = int32_t(w);
    catcher_.h = int32_t(h);
    WIND_DEBUG() << "菜单底板 configure " << w << "x" << h;
    if (!screenMenu_ || catcher_.mapped || w == 0 || h == 0) {
        return;
    }
    // 透明底板：1x1 全透明像素经 viewporter 拉满（没有 viewporter 就老老实实建整张）
    const bool tiny = viewporter_ != nullptr;
    const uint32_t bw = tiny ? 1 : w, bh = tiny ? 1 : h;
    std::vector<uint8_t> px(size_t(bw) * bh * 4, 0);
    wl_buffer* buf = makeBuffer(px.data(), bw, bh, bw * 4);
    if (!buf) {
        closeScreenMenu("no_buffer");
        return;
    }
    if (tiny) {
        if (!catcher_.viewport) {
            catcher_.viewport = wp_viewporter_get_viewport(viewporter_, catcher_.surface);
        }
        wp_viewport_set_destination(catcher_.viewport, int32_t(w), int32_t(h));
    }
    wl_surface_attach(catcher_.surface, buf, 0, 0);
    wl_surface_commit(catcher_.surface);
    wl_display_flush(display_);
    catcher_.mapped = true;
    sendPendingMenu();
    for (uint32_t level = 0; level < kMenuLevels; ++level) {
        if (queuedMenus_[level]) {
            const QueuedMenu q = std::move(*queuedMenus_[level]);
            queuedMenus_[level].reset();
            showScreenMenuLevel(level, q.frame, q.geo);
        }
    }
}

void WaylandCandidatePanel::sendPendingMenu()
{
    if (!pendingMenu_ || pendingMenu_->locate) {
        return; // 落点待定：等指针进入底板（见 ptrEnter）
    }
    const PendingMenu m = *pendingMenu_;
    pendingMenu_.reset();
    const double sc = reportedScale_;
    const Rect wa{0, 0, int32_t(std::lround(catcher_.w * sc)), int32_t(std::lround(catcher_.h * sc))};
    // 菜单帧迟迟不来（服务没了 / 不响应）：底板挡着整个桌面的鼠标，不能一直留着
    armTimer(menuWait_, 2000, &WaylandCandidatePanel::onMenuWaitExpired);
    if (cb_.screenMenu) {
        cb_.screenMenu(m.target, int32_t(std::lround(m.x * sc)), int32_t(std::lround(m.y * sc)), wa);
    }
}

bool WaylandCandidatePanel::showScreenMenuLevel(uint32_t level, const SharedFrame& f,
                                                const OverlayFramePayload& p)
{
    if (f.width == 0 || f.height == 0 || f.bgra.size() < size_t(f.stride) * f.height) {
        return false;
    }
    if (!catcher_.mapped) {
        // 面板发起的菜单：帧比底板的 configure 先到，映射后补贴
        queuedMenus_[level] = QueuedMenu{f, p};
        return true;
    }
    disarm(menuWait_);
    armTimer(menuIdle_, menuIdleMs_, &WaylandCandidatePanel::onMenuIdleExpired);
    if (!menuSurfs_[level].mapped) {
        WIND_DEBUG() << "屏幕菜单第 " << level << " 级出现 " << f.width << "x" << f.height;
    }
    const double sc = reportedScale_;
    OverlayFramePayload geo = p;
    geo.width = f.width;
    geo.height = f.height;
    // 面板坐标 → 底板（输出）坐标；气泡菜单原点为 0
    geo.x -= menuOriginX_;
    geo.y -= menuOriginY_;
    geo.altX -= menuOriginX_;
    geo.altY -= menuOriginY_;
    const Rect wa{0, 0, int32_t(std::lround(catcher_.w * sc)), int32_t(std::lround(catcher_.h * sc))};
    const Rect r = placeOverlay(geo, wa, 0, 0);
    MenuSurf& m = menuSurfs_[level];
    if (!m.surface) {
        m.surface = wl_compositor_create_surface(compositor_);
        wl_surface_set_user_data(m.surface, m.fakeUserData);
        m.sub = wl_subcompositor_get_subsurface(subcomp_, m.surface, catcher_.surface);
        wl_subsurface_set_desync(m.sub); // 子菜单的像素自己提交就生效，不等底板
    }
    m.x = int32_t(std::lround(r.x / sc));
    m.y = int32_t(std::lround(r.y / sc));
    wl_buffer* buf = makeBuffer(f.bgra.data(), f.width, f.height, f.stride);
    if (!buf) {
        return false;
    }
    wl_subsurface_set_position(m.sub, m.x, m.y); // 位置随底板的下一次提交生效
    applyScale(m.surface, m.viewport, f.width, f.height);
    wl_surface_attach(m.surface, buf, 0, 0);
    if (compositorVersion_ >= 4) {
        wl_surface_damage_buffer(m.surface, 0, 0, int32_t(f.width), int32_t(f.height));
    } else {
        wl_surface_damage(m.surface, 0, 0, int32_t(f.width), int32_t(f.height));
    }
    wl_surface_commit(m.surface);
    wl_surface_commit(catcher_.surface);
    wl_display_flush(display_);
    m.mapped = true;
    screenMenuShown_ = true;
    return true;
}

void WaylandCandidatePanel::hideScreenMenuLevel(uint32_t level)
{
    if (level >= kMenuLevels) {
        return;
    }
    queuedMenus_[level].reset();
    MenuSurf& m = menuSurfs_[level];
    if (m.mapped && m.surface) {
        wl_surface_attach(m.surface, nullptr, 0, 0);
        wl_surface_commit(m.surface);
        wl_display_flush(display_);
        m.mapped = false;
    }
    if (!screenMenuShown_) {
        return; // 服务端开新菜单前先清旧级别的「隐藏」帧：菜单还没出来，不算收
    }
    for (const auto& s : menuSurfs_) {
        if (s.mapped) {
            return;
        }
    }
    closeScreenMenu(nullptr); // 最后一级也收了：服务端关的菜单，不用再报
    // 收的若是面板的菜单，紧接着的菜单帧（「更多…」的主菜单）照样认领到那块屏的底板上
    panelMenu_.serverClosed(fcitx::now(CLOCK_MONOTONIC));
}

void WaylandCandidatePanel::closeScreenMenu(const char* reason)
{
    if (!screenMenu_) {
        return;
    }
    WIND_DEBUG() << "收起屏幕菜单" << (reason ? std::string("：") + reason : std::string());
    screenMenu_ = false;
    pendingMenu_.reset();
    for (auto& q : queuedMenus_) {
        q.reset();
    }
    catcherOutput_ = 0;
    menuOriginX_ = menuOriginY_ = 0;
    menuPtrSurface_ = nullptr;
    lastMenuMotion_.reset();
    disarm(menuWait_);
    disarm(menuIdle_);
    disarm(menuMax_);
    for (auto& m : menuSurfs_) {
        if (m.mapped && m.surface) {
            wl_surface_attach(m.surface, nullptr, 0, 0);
            wl_surface_commit(m.surface);
            m.mapped = false;
        }
    }
    if (catcher_.surface && catcher_.mapped) {
        wl_surface_attach(catcher_.surface, nullptr, 0, 0);
        wl_surface_commit(catcher_.surface);
    }
    if (catcher_.layer) {
        // 同 hideLayer：角色对象一并销毁，下次打开从头走 configure
        zwlr_layer_surface_v1_destroy(catcher_.layer);
        catcher_.layer = nullptr;
    }
    catcher_.mapped = catcher_.configured = catcher_.waiting = false;
    if (display_) {
        wl_display_flush(display_);
    }
    if (reason && cb_.menuDismissed) {
        cb_.menuDismissed(reason);
    }
    armOverlayHide(OVERLAY_KIND_STATUS); // 菜单收了：气泡重新计时
}

bool WaylandCandidatePanel::screenMenuOffset(wl_surface* s, int32_t& ox, int32_t& oy) const
{
    if (!s) {
        return false;
    }
    if (s == catcher_.surface) {
        ox = oy = 0;
        return true;
    }
    for (const auto& m : menuSurfs_) {
        if (m.surface == s) {
            ox = m.x;
            oy = m.y;
            return true;
        }
    }
    return false;
}

void WaylandCandidatePanel::reportScreenMenuPointer(uint32_t event, uint32_t button)
{
    const double sc = reportedScale_;
    // 底板坐标 → 菜单帧的坐标系（面板发起的菜单加回输出原点）
    const int32_t x = int32_t(std::lround((menuPtrOffX_ + layerPtrX_) * sc)) + menuOriginX_;
    const int32_t y = int32_t(std::lround((menuPtrOffY_ + layerPtrY_) * sc)) + menuOriginY_;
    if (event == MENU_POINTER_MOTION) {
        if (lastMenuMotion_ && *lastMenuMotion_ == std::make_pair(x, y)) {
            return;
        }
        lastMenuMotion_ = std::make_pair(x, y);
    } else {
        // 按下才续空闲计时：移动不续，服务卡死时指针还在动，续下去底板就永远收不掉
        armTimer(menuIdle_, menuIdleMs_, &WaylandCandidatePanel::onMenuIdleExpired);
    }
    if (cb_.menuPointer) {
        cb_.menuPointer(event, button, x, y);
    }
}

void WaylandCandidatePanel::onMenuWaitExpired()
{
    if (screenMenu_) {
        bool any = false;
        for (const auto& m : menuSurfs_) {
            any = any || m.mapped;
        }
        if (!any) {
            closeScreenMenu("no_menu");
        }
    }
}

void WaylandCandidatePanel::onMenuIdleExpired()
{
    closeScreenMenu("idle");
}

void WaylandCandidatePanel::onMenuMaxExpired()
{
    closeScreenMenu("max_grab");
}

} // namespace windlinux
