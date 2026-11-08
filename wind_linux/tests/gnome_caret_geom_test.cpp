// GNOME 光标折算的纯函数：桌面名、前端名、frontend × scale。不需要 Fcitx5。

#include "GnomeCaretGeom.h"
#include "TestHarness.h"

#include <cmath>
#include <cstdio>
#include <string>
#include <vector>

using namespace windlinux;

namespace {

bool near(double a, double b)
{
    return std::fabs(a - b) < 1e-9;
}

void checkNear(const char* expr, double got, double expect)
{
    if (!near(got, expect)) {
        std::printf("  FAIL  %s  [%s]  %s  got=%g expect=%g\n", __FILE__, testharness::g_case,
                    expr, got, expect);
        testharness::g_failures++;
    }
}

void checkPx(const char* what, CaretPx got, int x, int y, int h)
{
    if (got.x != x || got.y != y || got.h != h) {
        std::printf("  FAIL  %s  [%s]  %s  got=(%d,%d,%d) expect=(%d,%d,%d)\n", __FILE__,
                    testharness::g_case, what, got.x, got.y, got.h, x, y, h);
        testharness::g_failures++;
    }
}

/// 按 XSETTINGS 规范拼一份属性：字节序 + 3 填充 + serial + 条数，每条 type/填充/名长/名（4 对齐）/serial/值。
struct XSettingsBuilder {
    bool msb = false;
    std::vector<uint8_t> items;
    uint32_t count = 0;

    void put16(std::vector<uint8_t>& b, uint16_t v) const
    {
        if (msb) {
            b.push_back(uint8_t(v >> 8));
            b.push_back(uint8_t(v));
        } else {
            b.push_back(uint8_t(v));
            b.push_back(uint8_t(v >> 8));
        }
    }
    void put32(std::vector<uint8_t>& b, uint32_t v) const
    {
        for (int i = 0; i < 4; ++i) {
            const int shift = msb ? (3 - i) * 8 : i * 8;
            b.push_back(uint8_t(v >> shift));
        }
    }
    void head(uint8_t type, const std::string& name)
    {
        items.push_back(type);
        items.push_back(0);
        put16(items, uint16_t(name.size()));
        items.insert(items.end(), name.begin(), name.end());
        while (items.size() % 4) {
            items.push_back(0);
        }
        put32(items, 0); // last-change serial
        ++count;
    }
    XSettingsBuilder& integer(const std::string& name, int32_t v)
    {
        head(0, name);
        put32(items, uint32_t(v));
        return *this;
    }
    XSettingsBuilder& string(const std::string& name, const std::string& v)
    {
        head(1, name);
        put32(items, uint32_t(v.size()));
        items.insert(items.end(), v.begin(), v.end());
        while (items.size() % 4) {
            items.push_back(0);
        }
        return *this;
    }
    XSettingsBuilder& color(const std::string& name)
    {
        head(2, name);
        for (int i = 0; i < 4; ++i) {
            put16(items, 0xffff);
        }
        return *this;
    }
    std::vector<uint8_t> build() const
    {
        std::vector<uint8_t> b{uint8_t(msb ? 1 : 0), 0, 0, 0};
        put32(b, 7); // serial
        put32(b, count);
        b.insert(b.end(), items.begin(), items.end());
        return b;
    }
};

double tsOf(const std::vector<uint8_t>& b)
{
    return xsettingsTextScaling(b.data(), b.size());
}

/// gsd-xsettings 那份属性的最小样子：只带 `Gdk/UnscaledDPI`。
std::vector<uint8_t> gsdXSettings(double textScaling)
{
    XSettingsBuilder g;
    g.integer("Xft/DPI", 0).integer("Gdk/UnscaledDPI", int32_t(std::lround(96 * 1024 * textScaling)));
    return g.build();
}

/// 与 WindEngine::resolveWindowRelCaret 有扩展时同一套：spotScale → 扩展（rel × shell/spot + 窗口原点）
/// → × stageToX11Mul。扩展那一段按 extension.js 的公式在这里模拟。
CaretPx viaExtension(int left, int top, int h, double sf, double buf, double out, const char* fe,
                     double shellScale, int originX, int originY)
{
    const double spot = spotScaleForGnomeExt(sf, buf, out, fe);
    const double k = shellScale / spot;
    const int sx = int(std::lround(left * k)) + originX;
    const int sy = int(std::lround(top * k)) + originY;
    const int sh = int(std::lround(h * k));
    const double toX11 = stageToX11Mul(shellScale, buf);
    return {int(std::lround(sx * toX11)), int(std::lround(sy * toX11)), int(std::lround(sh * toX11))};
}

} // namespace

int main()
{
    CASE("XDG_CURRENT_DESKTOP：冒号分段、与 GNOME 全等、忽略大小写");
    CHECK(!xdgCurrentDesktopIsGnome(nullptr));
    CHECK(!xdgCurrentDesktopIsGnome(""));
    CHECK(!xdgCurrentDesktopIsGnome(":"));
    CHECK(xdgCurrentDesktopIsGnome("GNOME"));
    CHECK(xdgCurrentDesktopIsGnome("gnome"));
    CHECK(xdgCurrentDesktopIsGnome("Gnome"));
    CHECK(xdgCurrentDesktopIsGnome("ubuntu:GNOME"));
    CHECK(xdgCurrentDesktopIsGnome("GNOME:ubuntu"));
    CHECK(xdgCurrentDesktopIsGnome("ubuntu:GNOME:extra"));
    CHECK(xdgCurrentDesktopIsGnome("GNOME-Classic:GNOME"));
    CHECK(xdgCurrentDesktopIsGnome(":GNOME"));
    CHECK(xdgCurrentDesktopIsGnome("GNOME:"));
    CHECK(!xdgCurrentDesktopIsGnome("KDE"));
    CHECK(!xdgCurrentDesktopIsGnome("ubuntu:KDE"));
    CHECK(!xdgCurrentDesktopIsGnome("ubuntu:KDE:Plasma"));
    CHECK(!xdgCurrentDesktopIsGnome("GNOME-Classic"));
    CHECK(!xdgCurrentDesktopIsGnome("X-GNOME"));
    CHECK(!xdgCurrentDesktopIsGnome("treeland"));
    CHECK(!xdgCurrentDesktopIsGnome("Deepin"));

    CASE("X11 面板回退前端：只认 dbus / ibus / fcitx4");
    CHECK(isX11PanelFallbackFrontend("dbus"));
    CHECK(isX11PanelFallbackFrontend("ibus"));
    CHECK(isX11PanelFallbackFrontend("fcitx4"));
    CHECK(!isX11PanelFallbackFrontend("wayland"));
    CHECK(!isX11PanelFallbackFrontend("wayland_v2"));
    CHECK(!isX11PanelFallbackFrontend("xim"));
    CHECK(!isX11PanelFallbackFrontend("DBus"));
    CHECK(!isX11PanelFallbackFrontend("IBUS"));
    CHECK(!isX11PanelFallbackFrontend(nullptr));
    CHECK(!isX11PanelFallbackFrontend(""));

    CASE("spotScale：frontend × scale");
    struct Spot {
        const char* name;
        double sf;
        double buf;
        double out;
        const char* fe;
        double expect;
    };
    const Spot spots[] = {
        {"预乘：sf 贴近缓冲缩放", 2.0, 2.0, 2.0, "dbus", 2.0},
        {"预乘：ibus 同样用 sf，不用输出缩放", 2.0, 2.0, 1.5, "ibus", 2.0},
        {"边界：|sf-buf|=0.25 仍算预乘", 1.75, 2.0, 1.5, "dbus", 1.75},
        {"sf≈1 ibus：用输出缩放", 1.0, 2.0, 1.5, "ibus", 1.5},
        {"sf≈1 ibus：输出缺失则退回缓冲缩放", 1.0, 2.0, 0.0, "ibus", 2.0},
        {"sf≈1 dbus：主题 scale 1", 1.0, 2.0, 1.5, "dbus", 1.0},
        {"sf≈1 fcitx4：主题 scale 1", 1.0, 2.0, 2.0, "fcitx4", 1.0},
        {"sf≈1 未知前端：主题 scale 1", 1.0, 2.0, 2.0, "xim", 1.0},
        {"sf≈1 frontend 空", 1.0, 2.0, 2.0, nullptr, 1.0},
        {"sf>1 不贴近缓冲缩放：仍按预乘用 sf", 1.5, 2.0, 1.5, "dbus", 1.5},
        {"sf>1 的 ibus：用 sf，不用输出缩放", 1.5, 2.0, 1.5, "ibus", 1.5},
        {"显示器 150%、GTK3 整数 sf2、缓冲缩放 1：用 sf", 2.0, 1.0, 1.5, "dbus", 2.0},
        {"sf 缺失当 1：dbus → 1", 0.0, 2.0, 1.5, "dbus", 1.0},
        {"sf 缺失当 1：ibus → 输出缩放", 0.0, 2.0, 1.5, "ibus", 1.5},
        {"缓冲缩放缺失：sf=2 仍用 sf", 2.0, 0.0, 1.5, "dbus", 2.0},
        {"sf 刚好 1.01：仍算 ≤1.01，ibus 用输出", 1.01, 2.0, 1.25, "ibus", 1.25},
        {"sf 刚超过 1.01 且远离 buf：用 sf", 1.02, 2.0, 1.25, "fcitx4", 1.02},
        {"sf 刚超过 1.01 且贴近 buf：预乘", 2.2, 2.0, 1.25, "dbus", 2.2},
    };
    for (const Spot& s : spots) {
        testharness::g_case = s.name;
        checkNear(s.name, spotScaleForGnomeExt(s.sf, s.buf, s.out, s.fe), s.expect);
    }
    CASE("spotScale：sf≤1.01 非 ibus 是 1，sf>1 一律是 sf");
    checkNear("sf1 dbus", spotScaleForGnomeExt(1.0, 2.0, 1.5, "dbus"), 1.0);
    checkNear("sf1.5 dbus", spotScaleForGnomeExt(1.5, 2.0, 1.5, "dbus"), 1.5);

    CASE("windowRel：无扩展折中，sf≤1.01 不乘缓冲缩放");
    checkPx("sf1", windowRelCaretToX11(10, 20, 18, 1.0, 2.0), 10, 20, 18);
    checkPx("预乘", windowRelCaretToX11(10, 20, 18, 2.0, 2.0), 10, 20, 18);
    checkPx("边界 0.25", windowRelCaretToX11(10, 20, 18, 1.75, 2.0), 10, 20, 18);
    checkPx("sf 远离 buf：除 sf 乘 buf", windowRelCaretToX11(15, 30, 18, 1.5, 2.0), 20, 40, 24);
    checkPx("|sf-buf|=0.26", windowRelCaretToX11(87, 174, 87, 1.74, 2.0), 100, 200, 100);
    checkPx("高度缺失", windowRelCaretToX11(15, 30, 0, 1.5, 2.0), 20, 40, 21);
    // 显示器 150%、Xwayland 不按原生缩放（根 960×600、Xft.dpi 96）：GTK3 报整数 sf=2 且已预乘。
    // .55 Ubuntu 22.04 实抓 gedit：SetCursorRectV2(52,614,0,36, 2.0)。
    checkPx("GTK3 sf2 / buf1", windowRelCaretToX11(52, 614, 36, 2.0, 1.0), 26, 307, 18);
    // 同一显示器缩放、Xwayland 原生缩放（Xft.dpi 144）：GTK3 仍报 2。
    checkPx("GTK3 sf2 / buf1.5", windowRelCaretToX11(52, 614, 36, 2.0, 1.5), 39, 461, 27);
    checkPx("负高度", windowRelCaretToX11(10, 20, -3, 1.0, 2.0), 10, 20, 16);
    checkPx("sf 缺失当 1", windowRelCaretToX11(10, 20, 18, 0.0, 2.0), 10, 20, 18);

    CASE("absolute：无 RelativeRect，不预乘就乘缓冲缩放，不加原点");
    checkPx("sf1", absoluteCaretToX11(10, 20, 18, 1.0, 2.0), 20, 40, 36);
    checkPx("预乘", absoluteCaretToX11(10, 20, 18, 2.0, 2.0), 10, 20, 18);
    checkPx("不贴近 buf", absoluteCaretToX11(10, 20, 18, 1.5, 2.0), 20, 40, 36);
    checkPx("高度缺失且预乘", absoluteCaretToX11(10, 20, 0, 2.0, 2.0), 10, 20, 16);
    checkPx("分数缓冲缩放 1.5", absoluteCaretToX11(4, 6, 8, 1.0, 1.5), 6, 9, 12);

    CASE("stage → X11：shell 贴近缓冲缩放则不乘");
    checkNear("贴近", stageToX11Mul(2.0, 2.0), 1.0);
    checkNear("边界 0.25", stageToX11Mul(1.75, 2.0), 1.0);
    checkNear("shell 1 buf 2", stageToX11Mul(1.0, 2.0), 2.0);
    checkNear("1.5 / 2", stageToX11Mul(1.5, 2.0), 2.0 / 1.5);
    checkNear("shell 缺失当 1", stageToX11Mul(0.0, 2.0), 2.0);

    CASE("Xft.dpi → 坐标缩放：只开字体缩放 1.3、显示器 1.0，坐标不放大");
    // 楼主数据：Xft.dpi = 96 × 1.3；客户端光标 h=30，错的那套是 h=39。GNOME 实际写整数 dpi。
    checkNear("dpi 124.8", xftDpiToX11Scale(124.8, 1.3), 1.0);
    checkNear("dpi 124", xftDpiToX11Scale(124, 1.3), 1.0);
    checkNear("dpi 125", xftDpiToX11Scale(125, 1.3), 1.0);
    {
        const double buf = xftDpiToX11Scale(124.8, 1.3);
        checkPx("absolute sf1", absoluteCaretToX11(295, 615, 30, 1.0, buf), 295, 615, 30);
        checkPx("windowRel sf1", windowRelCaretToX11(295, 615, 30, 1.0, buf), 295, 615, 30);
        checkNear("扩展 stage→X11", stageToX11Mul(1.0, buf), 1.0);
        checkNear("扩展 spotScale dbus", spotScaleForGnomeExt(1.0, buf, 1.0, "dbus"), 1.0);
        checkNear("扩展 spotScale ibus", spotScaleForGnomeExt(1.0, buf, 1.0, "ibus"), 1.0);
    }
    checkNear("字体缩小 0.8 也回到 1", xftDpiToX11Scale(76.8, 0.8), 1.0);

    CASE("Xft.dpi → 坐标缩放：字体缩放 1.0 时与原换算逐值一致（多屏 100%+150%）");
    for (int dpi = 48; dpi <= 480; ++dpi) {
        double old = std::round(dpi / 96.0 * 100.0) / 100.0;
        old = old < 1.0 ? 1.0 : (old > 4.0 ? 4.0 : old);
        if (!near(xftDpiToX11Scale(dpi, 1.0), old)) {
            std::printf("  FAIL  %s  [%s]  dpi=%d got=%g expect=%g\n", __FILE__, testharness::g_case,
                        dpi, xftDpiToX11Scale(dpi, 1.0), old);
            testharness::g_failures++;
        }
    }
    checkNear("150%", xftDpiToX11Scale(144, 1.0), 1.5);
    checkPx("150% absolute", absoluteCaretToX11(4, 6, 8, 1.0, xftDpiToX11Scale(144, 1.0)), 6, 9, 12);
    checkNear("150% 扩展 shell 1", stageToX11Mul(1.0, xftDpiToX11Scale(144, 1.0)), 1.5);
    checkNear("200%", xftDpiToX11Scale(192, 1.0), 2.0);
    checkPx("200% absolute", absoluteCaretToX11(10, 20, 18, 1.0, xftDpiToX11Scale(192, 1.0)), 20, 40,
            36);

    CASE("Xft.dpi → 坐标缩放：显示器 2.0 叠字体缩放 1.25，只留 2.0");
    checkNear("dpi 240", xftDpiToX11Scale(240, 1.25), 2.0);
    checkPx("absolute sf1", absoluteCaretToX11(10, 20, 18, 1.0, xftDpiToX11Scale(240, 1.25)), 20, 40,
            36);
    checkPx("absolute 预乘 sf2", absoluteCaretToX11(10, 20, 18, 2.0, xftDpiToX11Scale(240, 1.25)), 10,
            20, 18);
    checkNear("扩展 shell 2", stageToX11Mul(2.0, xftDpiToX11Scale(240, 1.25)), 1.0);
    checkNear("显示器 2.0 + 字体 1.3（dpi 249）", xftDpiToX11Scale(249, 1.3), 2.0);

    CASE("Xft.dpi → 坐标缩放：字体缩放读不到 / 越界按 1");
    checkNear("0", xftDpiToX11Scale(144, 0.0), 1.5);
    checkNear("负", xftDpiToX11Scale(144, -1.0), 1.5);
    checkNear("NaN", xftDpiToX11Scale(144, std::nan("")), 1.5);
    checkNear("过大", xftDpiToX11Scale(144, 10.0), 1.5);
    checkNear("上限 4", xftDpiToX11Scale(960, 1.0), 4.0);

    CASE("XSETTINGS：取 Gdk/UnscaledDPI 折成字体缩放");
    {
        // gsd-xsettings 实际会写的几项，UnscaledDPI 排在字符串 / 颜色后面
        auto gsd = [](bool msb) {
            XSettingsBuilder g;
            g.msb = msb;
            g.string("Net/ThemeName", "Yaru").color("Gtk/Color").integer("Xft/DPI", 127795)
                .integer("Gdk/WindowScalingFactor", 1).integer("Gdk/UnscaledDPI", 127795);
            return g.build();
        };
        checkNear("LSB 1.3", tsOf(gsd(false)), 127795 / 1024.0 / 96.0);
        checkNear("MSB 1.3", tsOf(gsd(true)), 127795 / 1024.0 / 96.0);
        checkNear("楼主：Xft.dpi 125 + 字体 1.3 → 坐标 1.0", xftDpiToX11Scale(125, tsOf(gsd(false))), 1.0);
        checkNear("楼主：Xft.dpi 124 + 字体 1.3 → 坐标 1.0", xftDpiToX11Scale(124, tsOf(gsd(true))), 1.0);
    }
    {
        XSettingsBuilder g;
        g.integer("Gdk/WindowScalingFactor", 2).integer("Gdk/UnscaledDPI", 96 * 1024 * 5 / 4);
        checkNear("2.0 + 1.25", xftDpiToX11Scale(240, tsOf(g.build())), 2.0);
    }
    {
        XSettingsBuilder g;
        g.integer("Gdk/UnscaledDPI", 96 * 1024);
        checkNear("字体 1.0 → 150% 不变", xftDpiToX11Scale(144, tsOf(g.build())), 1.5);
    }

    CASE("XSETTINGS：没有这一项 / 截断 / 格式不认识按 1");
    {
        XSettingsBuilder g;
        g.string("Net/ThemeName", "Breeze").integer("Xft/DPI", 98304);
        checkNear("非 GNOME 无 UnscaledDPI", tsOf(g.build()), 1.0);
        XSettingsBuilder h;
        h.integer("Gdk/UnscaledDPI", 127795);
        std::vector<uint8_t> b = h.build();
        b.resize(b.size() - 2);
        checkNear("值被截断", tsOf(b), 1.0);
        checkNear("只有头", tsOf(std::vector<uint8_t>(b.begin(), b.begin() + 12)), 1.0);
        checkNear("空", xsettingsTextScaling(nullptr, 0), 1.0);
        std::vector<uint8_t> bad = h.build();
        bad[12] = 9; // 未知类型
        checkNear("未知类型", tsOf(bad), 1.0);
        std::vector<uint8_t> huge = h.build();
        huge[8] = 0xff; // 条数虚高：读完唯一一条就越界
        huge[9] = 0xff;
        checkNear("条数虚高但前面已命中", tsOf(huge), 127795 / 1024.0 / 96.0);
        XSettingsBuilder z;
        z.integer("Gdk/UnscaledDPI", 0);
        checkNear("值为 0", tsOf(z.build()), 1.0);
    }

    CASE("端到端：#290 显示器 1.0 + 字体 1.3，有扩展 / 无扩展都不放大");
    {
        const double buf = xftDpiToX11Scale(125, xsettingsTextScaling(gsdXSettings(1.3).data(),
                                                                      gsdXSettings(1.3).size()));
        // 有扩展：窗口在 (100,50)，相对光标 (195,565,h=30) → 根坐标 (295,615,30)
        checkPx("有扩展 dbus", viaExtension(195, 565, 30, 1.0, buf, 1.0, "dbus", 1.0, 100, 50), 295, 615,
                30);
        checkPx("有扩展 ibus", viaExtension(195, 565, 30, 1.0, buf, 1.0, "ibus", 1.0, 100, 50), 295, 615,
                30);
        // 无扩展：RelativeRect 折中（不加原点）与绝对坐标两条
        checkPx("无扩展 RelativeRect", windowRelCaretToX11(295, 615, 30, 1.0, buf), 295, 615, 30);
        checkPx("无扩展 绝对坐标", absoluteCaretToX11(295, 615, 30, 1.0, buf), 295, 615, 30);
    }

    CASE("端到端：#215 100%+150%、字体 1.0，有扩展 / 无扩展与改前一致");
    {
        const double buf = xftDpiToX11Scale(144, xsettingsTextScaling(gsdXSettings(1.0).data(),
                                                                      gsdXSettings(1.0).size()));
        const double old = 1.5; // 改前：Xft.dpi 144 / 96
        checkNear("缓冲缩放", buf, old);
        // 第二块屏在 stage x=1920，GTK 报 sf=1、shell 1
        checkPx("有扩展", viaExtension(40, 30, 20, 1.0, buf, 1.5, "dbus", 1.0, 1920, 0),
                viaExtension(40, 30, 20, 1.0, old, 1.5, "dbus", 1.0, 1920, 0).x,
                viaExtension(40, 30, 20, 1.0, old, 1.5, "dbus", 1.0, 1920, 0).y,
                viaExtension(40, 30, 20, 1.0, old, 1.5, "dbus", 1.0, 1920, 0).h);
        checkPx("有扩展 数值", viaExtension(40, 30, 20, 1.0, buf, 1.5, "dbus", 1.0, 1920, 0), 2940, 45, 30);
        checkPx("有扩展 预乘 sf1.5", viaExtension(60, 45, 30, 1.5, buf, 1.5, "dbus", 1.0, 1920, 0), 2940,
                45, 30);
        checkPx("无扩展 RelativeRect", windowRelCaretToX11(40, 30, 20, 1.0, buf), 40, 30, 20);
        checkPx("无扩展 绝对坐标", absoluteCaretToX11(40, 30, 20, 1.0, buf), 60, 45, 30);
    }

    CASE("端到端：显示器 2.0 + 字体 1.25，有扩展 / 无扩展只按 2.0");
    {
        const double buf = xftDpiToX11Scale(240, xsettingsTextScaling(gsdXSettings(1.25).data(),
                                                                      gsdXSettings(1.25).size()));
        checkPx("有扩展 shell 1", viaExtension(10, 20, 18, 1.0, buf, 2.0, "dbus", 1.0, 100, 50), 220, 140,
                36);
        checkPx("有扩展 shell 2 预乘", viaExtension(20, 40, 36, 2.0, buf, 2.0, "dbus", 2.0, 100, 50), 120,
                90, 36);
        checkPx("无扩展 RelativeRect 预乘", windowRelCaretToX11(20, 40, 36, 2.0, buf), 20, 40, 36);
        checkPx("无扩展 绝对坐标", absoluteCaretToX11(10, 20, 18, 1.0, buf), 20, 40, 36);
    }

    CASE("端到端：显示器 150%、Xwayland 缓冲缩放 1，GTK3 报 sf2（.55 gedit 实抓）");
    {
        // 最大化 gedit：窗口原点在工作区左上 (70,27)；SetCursorRectV2(104,228,0,36, 2.0)
        checkPx("有扩展", viaExtension(104, 228, 36, 2.0, 1.0, 1.5, "dbus", 1.0, 70, 27), 122, 141, 18);
        checkPx("无扩展 RelativeRect", windowRelCaretToX11(104, 228, 36, 2.0, 1.0), 52, 114, 18);
    }

    TEST_MAIN_END()
}
