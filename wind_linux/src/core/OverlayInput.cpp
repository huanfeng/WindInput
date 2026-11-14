#include "OverlayInput.h"

#include "ExtProtocol.h"

#include <algorithm>
#include <cmath>

namespace windlinux {

OverlayPress overlayPressAction(uint32_t kind, uint8_t button)
{
    switch (kind) {
    case OVERLAY_KIND_STATUS:
        return button == 1 ? OverlayPress::Drag : button == 3 ? OverlayPress::Menu : OverlayPress::None;
    case OVERLAY_KIND_TOOLTIP:
        return button == 3 ? OverlayPress::Menu : OverlayPress::None;
    case OVERLAY_KIND_TOAST:
        // 滚轮（4/5）不算点击：指针停在 Toast 上滚动页面时不该把它关掉。
        return button >= 1 && button <= 3 ? OverlayPress::Close : OverlayPress::None;
    case OVERLAY_KIND_TOOLBAR:
    case OVERLAY_KIND_SOFTKEYBOARD:
        return OverlayPress::Forward; // 不知道按在哪：当作拖动区外
    default:
        return OverlayPress::None;
    }
}

OverlayPress overlayPressAction(uint32_t kind, uint8_t button, int32_t x, int32_t y,
                                const std::vector<Rect>& drag)
{
    if (!isPanelOverlayKind(kind)) {
        return overlayPressAction(kind, button);
    }
    if (button == 1) {
        for (const Rect& r : drag) {
            if (rectContains(r, x, y)) {
                return OverlayPress::Drag;
            }
        }
    }
    return OverlayPress::Forward;
}

const char* panelPosKind(uint32_t kind)
{
    switch (kind) {
    case OVERLAY_KIND_TOOLBAR:
        return EXT_KIND_POS_TOOLBAR;
    case OVERLAY_KIND_SOFTKEYBOARD:
        return EXT_KIND_POS_SOFTKEYBOARD;
    default:
        return nullptr;
    }
}

int32_t overlayMenuTarget(uint32_t kind)
{
    switch (kind) {
    case OVERLAY_KIND_STATUS:
        return MENU_TARGET_STATUS;
    case OVERLAY_KIND_TOOLTIP:
        return MENU_TARGET_TOOLTIP;
    default:
        return MENU_TARGET_MAIN;
    }
}

uint32_t hoverDeferMs(int32_t last, int32_t raw, bool tipShown)
{
    if (last < 0 || !tipShown || raw == last) {
        return 0;
    }
    return raw >= 0 ? kTipSwitchMs : kTipGraceMs;
}

bool headingToTip(int32_t px, int32_t py, int32_t nx, int32_t ny, const Rect& tip)
{
    const int32_t l = tip.x, t = tip.y, r = tip.x + tip.w, b = tip.y + tip.h;
    const int32_t dx = nx - px, dy = ny - py;
    // 提示在哪一侧：沿那条轴是「靠近」，另一条轴上离开提示跨度是「偏离」。
    const bool beside = l >= px || r <= px;
    if (!beside && t < py && b > py) {
        return false; // 已在提示上
    }
    const int32_t toward = l >= px ? dx : r <= px ? -dx : t >= py ? dy : -dy;
    const int32_t lateral = beside ? dy : dx;
    const int32_t lo = beside ? t : l;
    const int32_t hi = beside ? b : r;
    const int32_t pos = beside ? ny : nx;
    const int32_t away = pos > hi ? lateral : pos < lo ? -lateral : 0;
    return toward > 0 && away <= toward;
}

int32_t tipGap(int32_t x, int32_t y, const Rect& tip)
{
    const int32_t l = tip.x, t = tip.y, r = tip.x + tip.w, b = tip.y + tip.h;
    if (l >= x) {
        return l - x;
    }
    if (r <= x) {
        return x - r;
    }
    if (t >= y) {
        return t - y;
    }
    if (b <= y) {
        return y - b;
    }
    return 0;
}

int32_t tipRecheckHover(bool onTip, bool onCandidate, int32_t candidateHit, int32_t held)
{
    if (onTip) {
        return kKeepHover;
    }
    const int32_t target = onCandidate && candidateHit >= 0 ? candidateHit : -1;
    return target == held ? kKeepHover : target;
}

Rect dragOverlay(int32_t px, int32_t py, int32_t grabDx, int32_t grabDy,
                 const OverlayFramePayload& geo, const Rect& workArea)
{
    OverlayFramePayload p = geo;
    p.place = OVERLAY_PLACE_ABSOLUTE;
    // 窗口左上 = 指针 − 抓取偏移；ABSOLUTE 的坐标指内容盒，加回扩边。
    p.x = px - grabDx + p.contentX;
    p.y = py - grabDy + p.contentY;
    return placeOverlay(p, workArea, 0, 0);
}

bool rectContains(const Rect& r, int32_t x, int32_t y)
{
    return x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;
}

Rect clampToWorkArea(const Rect& r, const Rect& wa)
{
    Rect out = r;
    if (out.x + out.w > wa.x + wa.w) {
        out.x = wa.x + wa.w - out.w;
    }
    if (out.x < wa.x) {
        out.x = wa.x;
    }
    if (out.y + out.h > wa.y + wa.h) {
        out.y = wa.y + wa.h - out.h;
    }
    if (out.y < wa.y) {
        out.y = wa.y;
    }
    return out;
}

bool hasFullscreenState(const std::vector<uint32_t>& states, uint32_t fullscreenAtom)
{
    if (fullscreenAtom == 0) {
        return false;
    }
    for (uint32_t a : states) {
        if (a == fullscreenAtom) {
            return true;
        }
    }
    return false;
}

Rect outputWorkArea(const Rect& output, const std::optional<Rect>& net)
{
    if (!net) {
        return output;
    }
    const int32_t l = std::max(output.x, net->x);
    const int32_t t = std::max(output.y, net->y);
    const int32_t r = std::min(output.x + output.w, net->x + net->w);
    const int32_t b = std::min(output.y + output.h, net->y + net->h);
    if (r <= l || b <= t) {
        return output;
    }
    return Rect{l, t, r - l, b - t};
}

int outputIndexAt(const std::vector<Rect>& outputs, int32_t x, int32_t y)
{
    for (size_t i = 0; i < outputs.size(); ++i) {
        if (rectContains(outputs[i], x, y)) {
            return int(i);
        }
    }
    return -1;
}

int panelOutputIndex(const std::vector<Rect>& outputs, const Rect& window, int fallback)
{
    if (outputs.empty()) {
        return -1;
    }
    const int i = outputIndexAt(outputs, window.x + window.w / 2, window.y + window.h / 2);
    if (i >= 0) {
        return i;
    }
    return fallback >= 0 && size_t(fallback) < outputs.size() ? fallback : 0;
}

PanelDragPlacement placePanelDrag(const std::vector<Rect>& outputs, const Rect& desired, int fallback)
{
    const int index = panelOutputIndex(outputs, desired, fallback);
    return {index, index >= 0 ? clampToWorkArea(desired, outputs[size_t(index)]) : desired};
}

namespace {
double validScale(double scale)
{
    return scale > 0 ? scale : 1.0;
}
} // namespace

Rect scaleRect(const Rect& logical, double scale)
{
    const double s = validScale(scale);
    return Rect{int32_t(std::lround(logical.x * s)), int32_t(std::lround(logical.y * s)),
                int32_t(std::lround(logical.w * s)), int32_t(std::lround(logical.h * s))};
}

Rect layerUsableArea(const Rect& outputLogical, int32_t availW, int32_t availH)
{
    Rect r = outputLogical;
    if (availW > 0 && availW <= outputLogical.w) {
        r.w = availW;
    }
    if (availH > 0 && availH <= outputLogical.h) {
        r.h = availH;
    }
    return r;
}

std::pair<int32_t, int32_t> layerMarginFor(int32_t x, int32_t y, const Rect& outputLogical,
                                           double scale)
{
    const double s = validScale(scale);
    return {int32_t(std::lround(x / s)) - outputLogical.x,
            int32_t(std::lround(y / s)) - outputLogical.y};
}

std::pair<int32_t, int32_t> layerOriginFor(int32_t marginLeft, int32_t marginTop,
                                           const Rect& outputLogical, double scale)
{
    const double s = validScale(scale);
    return {int32_t(std::lround((outputLogical.x + marginLeft) * s)),
            int32_t(std::lround((outputLogical.y + marginTop) * s))};
}

} // namespace windlinux
