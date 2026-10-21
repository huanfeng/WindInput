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
std::optional<GnomeSpot> translateRelativeSpot(int32_t x, int32_t y, int32_t w, int32_t h,
                                               double clientScale);

} // namespace windlinux
