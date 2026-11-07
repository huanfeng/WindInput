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
    (void)buf;
    // GTK3/GTK4/Qt 报的 RelativeRect 都已按自身 sf 预乘（fcitx5-gtk / fcitx5-qt 上游），kimpanel
    // 扩展也一律用客户端 sf 当除数。显示器 150% 时 GTK3 报整数 2、缓冲缩放可能是 1，同样按 sf 除。
    if (sf > 1.01) {
        return sf;
    }
    // sf≤1.01 的 ibus：相对矩形常已 ×输出缩放却没带 scale。
    if (frontend && std::strcmp(frontend, "ibus") == 0) {
        return out;
    }
    return 1.0;
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
    // GTK3/GTK4/Qt 报 RelativeRect 时都已按自身 sf 预乘（fcitx5-gtk / fcitx5-qt 上游），逻辑坐标 =
    // 矩形 / sf。sf 贴近缓冲缩放时两者抵消、原样；sf 远离缓冲缩放（如显示器 150% 时 GTK3 报整数 2、
    // 缓冲缩放 1）要先除 sf 再乘 buf，否则离窗口原点越远偏得越多。sf≤1.01 不动。
    const bool rescale = sf > 1.01 && std::fabs(sf - buf) > 0.25;
    return scaleRectToX11(left, top, height, rescale ? buf / sf : 1.0);
}

CaretPx absoluteCaretToX11(int left, int top, int height, double scaleFactor, double x11BufScale)
{
    const double buf = x11BufScale > 0.0 ? x11BufScale : 1.0;
    const double sf = scaleFactor > 0.01 ? scaleFactor : 1.0;
    const bool premult = sf > 1.01 && std::fabs(sf - buf) <= 0.25;
    return scaleRectToX11(left, top, height, premult ? 1.0 : buf);
}

double xftDpiToX11Scale(double xftDpi, double textScaling)
{
    const double tsf = (textScaling >= 0.5 && textScaling <= 3.0) ? textScaling : 1.0;
    double s = std::round(xftDpi / 96.0 / tsf * 100.0) / 100.0;
    return s < 1.0 ? 1.0 : (s > 4.0 ? 4.0 : s);
}

double xsettingsTextScaling(const uint8_t* data, size_t len)
{
    constexpr char kName[] = "Gdk/UnscaledDPI";
    constexpr size_t kNameLen = sizeof(kName) - 1;
    if (!data || len < 12 || data[0] > 1) {
        return 1.0;
    }
    const bool msb = data[0] == 1;
    auto rd = [&](size_t at, int bytes) {
        uint32_t v = 0;
        for (int i = 0; i < bytes; ++i) {
            const int shift = msb ? (bytes - 1 - i) * 8 : i * 8;
            v |= uint32_t(data[at + i]) << shift;
        }
        return v;
    };
    auto pad4 = [](size_t n) { return (n + 3) & ~size_t(3); };
    const uint32_t count = rd(8, 4);
    size_t at = 12;
    for (uint32_t i = 0; i < count; ++i) {
        if (len - at < 4) {
            return 1.0;
        }
        const uint8_t type = data[at];
        const size_t nameLen = rd(at + 2, 2);
        const size_t nameAt = at + 4;
        const size_t valueAt = nameAt + pad4(nameLen) + 4; // 名字之后是 last-change serial
        size_t valueLen = 0;
        if (type == 0) {
            valueLen = 4;
        } else if (type == 1) {
            if (valueAt + 4 > len) {
                return 1.0;
            }
            valueLen = 4 + pad4(rd(valueAt, 4));
        } else if (type == 2) {
            valueLen = 8;
        } else {
            return 1.0;
        }
        if (valueAt + valueLen > len) {
            return 1.0;
        }
        if (type == 0 && nameLen == kNameLen && std::memcmp(data + nameAt, kName, kNameLen) == 0) {
            const int32_t dpi1024 = int32_t(rd(valueAt, 4));
            return dpi1024 > 0 ? dpi1024 / 1024.0 / 96.0 : 1.0;
        }
        at = valueAt + valueLen;
    }
    return 1.0;
}

double stageToX11Mul(double shellScale, double x11BufScale)
{
    const double buf = x11BufScale > 0.0 ? x11BufScale : 1.0;
    const double shell = shellScale > 0.01 ? shellScale : 1.0;
    return (std::fabs(shell - buf) <= 0.25) ? 1.0 : (buf / shell);
}

} // namespace windlinux
