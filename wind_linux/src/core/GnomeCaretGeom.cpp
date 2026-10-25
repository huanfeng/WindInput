#include "GnomeCaretGeom.h"

#include <cmath>
#include <cstring>

namespace windlinux {
namespace {

bool segmentIsGnome(const char* s, size_t n)
{
    constexpr char kName[] = "gnome";
    if (n != sizeof(kName) - 1) {
        return false;
    }
    for (size_t i = 0; i < n; ++i) {
        char c = s[i];
        if (c >= 'A' && c <= 'Z') {
            c = char(c - 'A' + 'a');
        }
        if (c != kName[i]) {
            return false;
        }
    }
    return true;
}

} // namespace

bool xdgCurrentDesktopIsGnome(const char* xdgCurrentDesktop)
{
    if (!xdgCurrentDesktop || !*xdgCurrentDesktop) {
        return false;
    }
    const char* p = xdgCurrentDesktop;
    while (*p) {
        const char* colon = std::strchr(p, ':');
        const size_t n = colon ? size_t(colon - p) : std::strlen(p);
        if (segmentIsGnome(p, n)) {
            return true;
        }
        if (!colon) {
            break;
        }
        p = colon + 1;
    }
    return false;
}

bool isX11PanelFallbackFrontend(const char* frontend)
{
    return frontend
        && (std::strcmp(frontend, "dbus") == 0 || std::strcmp(frontend, "ibus") == 0
            || std::strcmp(frontend, "fcitx4") == 0);
}

double spotScaleForGnomeExt(double clientSf, double x11BufScale, double outputScale,
                            const char* frontend)
{
    const double sf = clientSf > 0.01 ? clientSf : 1.0;
    const double buf = x11BufScale > 0.0 ? x11BufScale : 1.0;
    const double out = outputScale > 0.01 ? outputScale : buf;
    // scale-monitor-framebuffer 下 St 主题 scale 多为 1；toX11 再用扩展回包的 shellScale。
    constexpr double kShellHint = 1.0;
    if (sf > 1.01 && std::fabs(sf - buf) <= 0.25) {
        return sf; // 已按缓冲缩放预乘
    }
    // sf≤1.01 且非 ibus，与「sf>1.01 但不贴近缓冲缩放」都按主题 scale 1，合成一条。
    if (sf <= 1.01 && frontend && std::strcmp(frontend, "ibus") == 0) {
        return out;
    }
    return kShellHint;
}

CaretPx scaleRectToX11(int left, int top, int height, double mul)
{
    const int h0 = height > 0 ? height : 16;
    return {int(std::lround(left * mul)), int(std::lround(top * mul)), int(std::lround(h0 * mul))};
}

CaretPx windowRelCaretToX11(int left, int top, int height, double scaleFactor, double x11BufScale)
{
    const double buf = x11BufScale > 0.0 ? x11BufScale : 1.0;
    const double sf = scaleFactor > 0.01 ? scaleFactor : 1.0;
    // 只有「分数 sf 且不像缓冲缩放」才是逻辑 DIP，要乘 buf。sf≤1.01 时这条为假，乘数恒为 1。
    const bool logicalDip = sf > 1.01 && std::fabs(sf - buf) > 0.25;
    return scaleRectToX11(left, top, height, logicalDip ? buf : 1.0);
}

CaretPx absoluteCaretToX11(int left, int top, int height, double scaleFactor, double x11BufScale)
{
    const double buf = x11BufScale > 0.0 ? x11BufScale : 1.0;
    const double sf = scaleFactor > 0.01 ? scaleFactor : 1.0;
    const bool premult = sf > 1.01 && std::fabs(sf - buf) <= 0.25;
    return scaleRectToX11(left, top, height, premult ? 1.0 : buf);
}

double stageToX11Mul(double shellScale, double x11BufScale)
{
    const double buf = x11BufScale > 0.0 ? x11BufScale : 1.0;
    const double shell = shellScale > 0.01 ? shellScale : 1.0;
    return (std::fabs(shell - buf) <= 0.25) ? 1.0 : (buf / shell);
}

} // namespace windlinux
