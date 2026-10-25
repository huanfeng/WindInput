// GNOME Shell 扩展 `windinput-caret@windinput` 的 D-Bus 客户端。
// 窗口相对光标 → stage 绝对坐标，供 X11 候选窗在无 IM-v2 的会话下落位。
#pragma once

#include <cstdint>
#include <optional>

namespace windlinux {

struct GnomeSpot {
    int32_t x = 0;
    int32_t y = 0;
    int32_t w = 0;
    int32_t h = 0;
    double shellScale = 1.0;
};

/// 扩展未装 / 未启用 / 调用失败时返回 nullopt（调用方走无扩展折中）。
/// 会话总线连接会缓存。失败后约 3 秒内不再 `g_bus_get_sync` / `g_dbus_connection_call_sync`。
/// 成功结果的「矩形没变就别再问」在调用方，按输入上下文记住，避免另一个窗口的相同相对矩形
/// 沿用这里的绝对坐标。只在 fcitx 主线程调用。
std::optional<GnomeSpot> translateRelativeSpot(int32_t x, int32_t y, int32_t w, int32_t h,
                                               double clientScale);

} // namespace windlinux
