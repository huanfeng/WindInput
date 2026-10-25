// GNOME 光标折算的纯函数：桌面名、前端名、frontend × scale。不需要 Fcitx5。

#include "GnomeCaretGeom.h"
#include "TestHarness.h"

#include <cmath>
#include <cstdio>

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
        {"逻辑 DIP：sf>1 且不像缓冲缩放 → 1（与 sf≈1 非 ibus 同值）", 1.5, 2.0, 1.5, "dbus", 1.0},
        {"逻辑 DIP：ibus 也不改用输出缩放", 1.5, 2.0, 1.5, "ibus", 1.0},
        {"sf 缺失当 1：dbus → 1", 0.0, 2.0, 1.5, "dbus", 1.0},
        {"sf 缺失当 1：ibus → 输出缩放", 0.0, 2.0, 1.5, "ibus", 1.5},
        {"缓冲缩放缺失当 1：sf=2 不贴近 → 1", 2.0, 0.0, 1.5, "dbus", 1.0},
        {"sf 刚好 1.01：仍算 ≤1.01，ibus 用输出", 1.01, 2.0, 1.25, "ibus", 1.25},
        {"sf 刚超过 1.01 且远离 buf：DIP → 1", 1.02, 2.0, 1.25, "fcitx4", 1.0},
        {"sf 刚超过 1.01 且贴近 buf：预乘", 2.2, 2.0, 1.25, "dbus", 2.2},
    };
    for (const Spot& s : spots) {
        testharness::g_case = s.name;
        checkNear(s.name, spotScaleForGnomeExt(s.sf, s.buf, s.out, s.fe), s.expect);
    }
    // 审查指出的两条冗余分支：返回值相同，合成后仍锁住。
    CASE("spotScale：sf≤1.01 非 ibus 与逻辑 DIP 都是 1");
    checkNear("sf1 dbus", spotScaleForGnomeExt(1.0, 2.0, 1.5, "dbus"), 1.0);
    checkNear("dip dbus", spotScaleForGnomeExt(1.5, 2.0, 1.5, "dbus"), 1.0);

    CASE("windowRel：无扩展折中，sf≤1.01 不乘缓冲缩放");
    checkPx("sf1", windowRelCaretToX11(10, 20, 18, 1.0, 2.0), 10, 20, 18);
    checkPx("预乘", windowRelCaretToX11(10, 20, 18, 2.0, 2.0), 10, 20, 18);
    checkPx("边界 0.25", windowRelCaretToX11(10, 20, 18, 1.75, 2.0), 10, 20, 18);
    checkPx("逻辑 DIP", windowRelCaretToX11(10, 20, 18, 1.5, 2.0), 20, 40, 36);
    checkPx("|sf-buf|=0.26", windowRelCaretToX11(10, 20, 18, 1.74, 2.0), 20, 40, 36);
    checkPx("高度缺失", windowRelCaretToX11(10, 20, 0, 1.5, 2.0), 20, 40, 32);
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

    TEST_MAIN_END()
}
