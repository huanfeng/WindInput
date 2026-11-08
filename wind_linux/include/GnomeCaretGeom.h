// GNOME Wayland 光标折到 X11 的纯换算（不碰 Fcitx5、不碰 D-Bus）。
// 会话是不是 GNOME、前端名、scale 组合都在这里，单测直接覆盖。见 docs/design/linux-port.md §7.10。
#pragma once

#include <cstddef>
#include <cstdint>

namespace windlinux {

struct CaretPx {
    int x = 0;
    int y = 0;
    int h = 0;
};

/// `XDG_CURRENT_DESKTOP` 是冒号分隔的桌面名列表（如 `ubuntu:GNOME`）。
/// 任一段与 `GNOME` 全等（大小写不敏感）才算 GNOME。`GNOME-Classic` 单独一段不算，
/// 经典会话实际是 `GNOME-Classic:GNOME`。空串 / 不含该段（KDE、treeland）不算。
bool xdgCurrentDesktopIsGnome(const char* xdgCurrentDesktop);

/// 无 IM-v2 时候选仍可能画在 X11 面板上的前端名：dbus / ibus / fcitx4。
bool isX11PanelFallbackFrontend(const char* frontend);

/// 扩展公式 `coord * (shellScale / spotScale)` 的除数：声明客户端矩形所在坐标系。
/// - sf>1.01：矩形已按 sf 预乘（GTK3/GTK4/Qt 都这样报，与 kimpanel 一致），除数就是 sf；
/// - sf≤1.01 且 frontend 为 ibus：相对矩形常已 ×输出缩放，却没带 scale；
/// - 其余按 1。`x11BufScale` 保留在签名里，不参与判定。
double spotScaleForGnomeExt(double clientSf, double x11BufScale, double outputScale,
                            const char* frontend);

/// 客户端矩形乘上 mul。高度 ≤0 时按 16。
CaretPx scaleRectToX11(int left, int top, int height, double mul);

/// RelativeRect、扩展没答上来时的折中：sf>1 且不贴近缓冲缩放时按「矩形 / sf × 缓冲缩放」，其余原样。
/// sf≤1.01 时乘数恒为 1（不再单独写一条恒真分支）。
CaretPx windowRelCaretToX11(int left, int top, int height, double scaleFactor, double x11BufScale);

/// 无 RelativeRect：已按缓冲缩放预乘则原样，否则乘缓冲缩放。不加窗口原点。
CaretPx absoluteCaretToX11(int left, int top, int height, double scaleFactor, double x11BufScale);

/// `Xft.dpi` → X11 坐标缩放（四舍五入到 0.01，夹在 [1, 4]）。GNOME 写的 `Xft.dpi` 是
/// 96 × 显示器缩放 × `text-scaling-factor`，折坐标时 `textScaling` 传字体缩放把它除掉；
/// 光栅化传 1（字跟着字体缩放放大）。`textScaling` 不在 [0.5, 3] 内（含 NaN）按 1。
double xftDpiToX11Scale(double xftDpi, double textScaling);

/// 从 `_XSETTINGS_SETTINGS` 属性（XSETTINGS 规范的二进制格式）取字体缩放：
/// gsd-xsettings 写 `Gdk/UnscaledDPI` = 96 × 1024 × text-scaling-factor，与 `Xft.dpi` 同源。
/// 没有这一项（非 GNOME）、数据截断或格式不认识时按 1。
double xsettingsTextScaling(const uint8_t* data, size_t len);

/// 扩展回包的 stage 坐标折到 X11 缓冲像素：shell 贴近缓冲缩放则 1，否则 buf/shell。
double stageToX11Mul(double shellScale, double x11BufScale);

} // namespace windlinux
