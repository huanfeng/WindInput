// GNOME Wayland 光标折到 X11 的纯换算（不碰 Fcitx5、不碰 D-Bus）。
// 会话是不是 GNOME、前端名、scale 组合都在这里，单测直接覆盖。见 docs/design/linux-port.md §7.10。
#pragma once

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
/// - sf 贴近 Xft 缓冲缩放：矩形已预乘，除数就是 sf；
/// - sf≤1.01 且 frontend 为 ibus：相对矩形常已 ×输出缩放，却没带 scale；
/// - 其余（sf≤1.01 的其它前端，以及 sf>1 但不贴近缓冲缩放的逻辑 DIP）主题 scale 按 1。
double spotScaleForGnomeExt(double clientSf, double x11BufScale, double outputScale,
                            const char* frontend);

/// 客户端矩形乘上 mul。高度 ≤0 时按 16。
CaretPx scaleRectToX11(int left, int top, int height, double mul);

/// RelativeRect、扩展没答上来时的折中：逻辑 DIP（sf>1 且不贴近缓冲缩放）乘缓冲缩放，其余原样。
/// sf≤1.01 时乘数恒为 1（不再单独写一条恒真分支）。
CaretPx windowRelCaretToX11(int left, int top, int height, double scaleFactor, double x11BufScale);

/// 无 RelativeRect：已按缓冲缩放预乘则原样，否则乘缓冲缩放。不加窗口原点。
CaretPx absoluteCaretToX11(int left, int top, int height, double scaleFactor, double x11BufScale);

/// 扩展回包的 stage 坐标折到 X11 缓冲像素：shell 贴近缓冲缩放则 1，否则 buf/shell。
double stageToX11Mul(double shellScale, double x11BufScale);

} // namespace windlinux
