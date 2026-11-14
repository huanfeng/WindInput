// 协议同步守卫：ExtProtocol.h 里每个 cmd id 都必须与 Rust 真源 protocol.rs 的同名常量
// 逐字相等。protocol.rs 路径由编译参数 WIND_PROTOCOL_RS 传入。
//
// 为什么要这条：外部宿主专用的 cmd（候选帧、tooltip、按键合成…）Windows 不用，故不在
// wind_tsf/include/BinaryProtocol.h 里，是本目录第二份手抄。手抄的值改一边忘一边，症状只会是
// 「某类消息静默丢失」，没有任何报错——正是 wind_macos/AGENTS.md「协议同步铁律」防的事。

#include "ExtProtocol.h"
#include "Protocol.h"
#include "SettingsLauncher.h"
#include "TestHarness.h"

#include <algorithm>
#include <fstream>
#include <optional>
#include <regex>
#include <sstream>

#ifndef WIND_PROTOCOL_RS
#error "编译时需 -DWIND_PROTOCOL_RS=\"<仓库>/wind_input/crates/wind-ipc/src/protocol.rs\""
#endif

namespace {

std::string g_src;

/// 在 protocol.rs 里找 `pub const NAME: u16 = 0x....;`（整数类型不限，可为负），找不到返回空。
std::optional<long> rustConst(const std::string& name)
{
    std::regex re("pub const " + name
                  + R"(:\s*[ui]\d+\s*=\s*(-?(?:0x[0-9A-Fa-f_]+|\d+))\s*;)");
    std::smatch m;
    if (!std::regex_search(g_src, m, re)) {
        return std::nullopt;
    }
    std::string v = m[1];
    v.erase(std::remove(v.begin(), v.end(), '_'), v.end());
    return std::stol(v, nullptr, 0);
}

/// 在 protocol.rs 里找 `pub const NAME: &str = "...";`，找不到返回空串。
std::string rustStr(const std::string& name)
{
    std::regex re("pub const " + name + R"re(:\s*&str\s*=\s*"([^"]*)"\s*;)re");
    std::smatch m;
    return std::regex_search(g_src, m, re) ? std::string(m[1]) : std::string();
}

#define SYNC(name, value)                                                                \
    do {                                                                                 \
        std::optional<long> rs = rustConst(name);                                        \
        if (rs != long(value)) {                                                         \
            std::printf("  FAIL  %s: C++=%ld  protocol.rs=%s%ld\n", name, long(value),    \
                        rs ? "" : "(未找到) ", rs.value_or(0));                          \
            testharness::g_failures++;                                                   \
        }                                                                                \
    } while (0)

} // namespace

int main()
{
    std::ifstream in(WIND_PROTOCOL_RS);
    if (!in) {
        std::printf("  FAIL  打不开 %s\n", WIND_PROTOCOL_RS);
        return 1;
    }
    std::stringstream ss;
    ss << in.rdbuf();
    g_src = ss.str();

    CASE("ExtProtocol.h 的外部宿主专用 cmd 与 protocol.rs 一致");
    SYNC("CMD_MENU_ACTION", windlinux::CMD_MENU_ACTION);
    SYNC("CMD_FRONT_CONTEXT", windlinux::CMD_FRONT_CONTEXT);
    SYNC("CMD_HOST_RENDER_FRAME", windlinux::CMD_HOST_RENDER_FRAME);
    SYNC("CMD_CANDIDATE_RECTS", windlinux::CMD_CANDIDATE_RECTS);
    SYNC("CMD_MODE_STATUS", windlinux::CMD_MODE_STATUS);
    SYNC("CMD_MENU_SHOW", windlinux::CMD_MENU_SHOW);
    SYNC("CMD_KEY_TAP", windlinux::CMD_KEY_TAP);
    SYNC("CMD_KEY_SEQ", windlinux::CMD_KEY_SEQ);
    SYNC("CMD_KEY_HOLD", windlinux::CMD_KEY_HOLD);
    SYNC("CMD_KEY_RELEASE", windlinux::CMD_KEY_RELEASE);
    SYNC("CMD_KEY_TYPE", windlinux::CMD_KEY_TYPE);
    SYNC("CMD_OVERLAY_FRAME", windlinux::CMD_OVERLAY_FRAME);
    SYNC("OVERLAY_KIND_TOOLTIP", windlinux::OVERLAY_KIND_TOOLTIP);
    SYNC("OVERLAY_KIND_STATUS", windlinux::OVERLAY_KIND_STATUS);
    SYNC("OVERLAY_KIND_TOAST", windlinux::OVERLAY_KIND_TOAST);
    SYNC("OVERLAY_KIND_MENU", windlinux::OVERLAY_KIND_MENU);
    SYNC("OVERLAY_MENU_LEVELS", windlinux::OVERLAY_MENU_LEVELS);
    SYNC("OVERLAY_PLACE_EXACT", windlinux::OVERLAY_PLACE_EXACT);
    SYNC("OVERLAY_KIND_TOOLBAR", windlinux::OVERLAY_KIND_TOOLBAR);
    SYNC("OVERLAY_KIND_SOFTKEYBOARD", windlinux::OVERLAY_KIND_SOFTKEYBOARD);
    SYNC("CMD_PANEL_POINTER", windlinux::CMD_PANEL_POINTER);
    SYNC("PANEL_POINTER_MOTION", windlinux::PANEL_POINTER_MOTION);
    SYNC("PANEL_POINTER_PRESS", windlinux::PANEL_POINTER_PRESS);
    SYNC("PANEL_POINTER_RELEASE", windlinux::PANEL_POINTER_RELEASE);
    SYNC("PANEL_POINTER_LEAVE", windlinux::PANEL_POINTER_LEAVE);
    SYNC("PANEL_POINTER_WHEEL", windlinux::PANEL_POINTER_WHEEL);
    SYNC("CMD_MENU_POINTER", windlinux::CMD_MENU_POINTER);
    SYNC("MENU_POINTER_MOTION", windlinux::MENU_POINTER_MOTION);
    SYNC("MENU_POINTER_PRESS", windlinux::MENU_POINTER_PRESS);
    SYNC("OVERLAY_PLACE_ABSOLUTE", windlinux::OVERLAY_PLACE_ABSOLUTE);
    SYNC("OVERLAY_PLACE_FLIP", windlinux::OVERLAY_PLACE_FLIP);
    SYNC("OVERLAY_PLACE_FOLLOW_CANDIDATE", windlinux::OVERLAY_PLACE_FOLLOW_CANDIDATE);
    SYNC("OVERLAY_PLACE_ANCHOR", windlinux::OVERLAY_PLACE_ANCHOR);
    SYNC("OVERLAY_ANCHOR_CENTER", windlinux::OVERLAY_ANCHOR_CENTER);
    SYNC("OVERLAY_ANCHOR_TOP_LEFT", windlinux::OVERLAY_ANCHOR_TOP_LEFT);
    SYNC("OVERLAY_ANCHOR_TOP_RIGHT", windlinux::OVERLAY_ANCHOR_TOP_RIGHT);
    SYNC("OVERLAY_ANCHOR_BOTTOM_LEFT", windlinux::OVERLAY_ANCHOR_BOTTOM_LEFT);
    SYNC("OVERLAY_ANCHOR_BOTTOM_RIGHT", windlinux::OVERLAY_ANCHOR_BOTTOM_RIGHT);
    SYNC("OVERLAY_ANCHOR_TOP_CENTER", windlinux::OVERLAY_ANCHOR_TOP_CENTER);
    SYNC("OVERLAY_ANCHOR_BOTTOM_CENTER", windlinux::OVERLAY_ANCHOR_BOTTOM_CENTER);
    SYNC("FLAG_SOFTWARE_SHADOW", windlinux::FRAME_FLAG_SOFTWARE_SHADOW);
    SYNC("FLAG_ABSOLUTE_POS", windlinux::FRAME_FLAG_ABSOLUTE_POS);
    SYNC("FLAG_ANIM_FRAME", windlinux::FRAME_FLAG_ANIM_FRAME);
    SYNC("CMD_CANDIDATE_SKIN", windlinux::CMD_CANDIDATE_SKIN);
    SYNC("SKIN_FLAG_ACTIVE", windlinux::SKIN_FLAG_ACTIVE);
    SYNC("SKIN_FLAG_MASK", windlinux::SKIN_FLAG_MASK);
    SYNC("SKIN_HIT_DRAG", windlinux::SKIN_HIT_DRAG);
    SYNC("SKIN_HIT_BLOCK", windlinux::SKIN_HIT_BLOCK);
    SYNC("SKIN_HIT_PASS", windlinux::SKIN_HIT_PASS);
    SYNC("SKIN_DECOR_UNDER", windlinux::SKIN_DECOR_UNDER);
    SYNC("SKIN_DECOR_SPLIT", windlinux::SKIN_DECOR_SPLIT);
    SYNC("OVERLAY_KIND_DECOR", windlinux::OVERLAY_KIND_DECOR);

    CASE("addon 用到的 BinaryProtocol.h 常量也与 protocol.rs 一致（那份头同样是手抄镜像）");
    SYNC("CMD_KEY_EVENT", CMD_KEY_EVENT);
    SYNC("CMD_FOCUS_GAINED", CMD_FOCUS_GAINED);
    SYNC("CMD_FOCUS_LOST", CMD_FOCUS_LOST);
    SYNC("CMD_IME_ACTIVATED", CMD_IME_ACTIVATED);
    SYNC("CMD_IME_DEACTIVATED", CMD_IME_DEACTIVATED);
    SYNC("CMD_COMPOSITION_TERMINATED", CMD_COMPOSITION_TERMINATED);
    SYNC("CMD_CARET_UPDATE", CMD_CARET_UPDATE);
    SYNC("CMD_CANDIDATE_SELECT", CMD_CANDIDATE_SELECT);
    SYNC("CMD_CANDIDATE_HOVER", CMD_CANDIDATE_HOVER);
    SYNC("CMD_CANDIDATE_SCROLL", CMD_CANDIDATE_SCROLL);
    SYNC("CMD_EXT", CMD_EXT);
    SYNC("CMD_ACK", CMD_ACK);
    SYNC("CMD_PASS_THROUGH", CMD_PASS_THROUGH);
    SYNC("CMD_COMMIT_TEXT", CMD_COMMIT_TEXT);
    SYNC("CMD_UPDATE_COMPOSITION", CMD_UPDATE_COMPOSITION);
    SYNC("CMD_CLEAR_COMPOSITION", CMD_CLEAR_COMPOSITION);
    SYNC("CMD_CLEAR_THEN_PASS_THROUGH", CMD_CLEAR_THEN_PASS_THROUGH);
    SYNC("CMD_COMMIT_TEXT_WITH_CURSOR", CMD_COMMIT_TEXT_WITH_CURSOR);
    SYNC("CMD_MOVE_CURSOR", CMD_MOVE_CURSOR);
    SYNC("CMD_DELETE_PAIR", CMD_DELETE_PAIR);
    SYNC("CMD_REPLACE_BACKWARD", CMD_REPLACE_BACKWARD);
    SYNC("CMD_HOLD_COMPOSITION", CMD_HOLD_COMPOSITION);
    SYNC("CMD_COMMIT_AND_HOLD", CMD_COMMIT_AND_HOLD);
    SYNC("CMD_COMMIT_THEN_DEFER", CMD_COMMIT_THEN_DEFER);
    SYNC("CMD_CONSUMED", CMD_CONSUMED);
    SYNC("CMD_SERVICE_READY", CMD_SERVICE_READY);
    SYNC("CMD_STATUS_UPDATE", CMD_STATUS_UPDATE);
    SYNC("COMMIT_FLAG_REPLACING_HELD", COMMIT_FLAG_REPLACING_HELD);

    CASE("下行扩展信封的 kind 字符串与 protocol.rs `ext_kind` 一致");
    CHECK_EQ(rustStr("SETTINGS_OPEN"), std::string(windlinux::EXT_KIND_SETTINGS_OPEN));
    CHECK_EQ(rustStr("MENU_OPEN"), std::string(windlinux::EXT_KIND_MENU_OPEN));
    CHECK_EQ(rustStr("MENU_DISMISS"), std::string(windlinux::EXT_KIND_MENU_DISMISS));
    CHECK_EQ(rustStr("POS_STATUS_TIP"), std::string(windlinux::EXT_KIND_POS_STATUS_TIP));
    CHECK_EQ(rustStr("POS_TOOLBAR"), std::string(windlinux::EXT_KIND_POS_TOOLBAR));
    CHECK_EQ(rustStr("POS_SOFTKEYBOARD"), std::string(windlinux::EXT_KIND_POS_SOFTKEYBOARD));
    CHECK_EQ(rustStr("HOST_FULLSCREEN"), std::string(windlinux::EXT_KIND_HOST_FULLSCREEN));
    CHECK_EQ(rustStr("HOST_DISPLAY"), std::string(windlinux::EXT_KIND_HOST_DISPLAY));
    CHECK_EQ(rustStr("HOST_REDUCE_MOTION"), std::string(windlinux::EXT_KIND_HOST_REDUCE_MOTION));
    CHECK_EQ(rustStr("SKIN_DECOR_CLICK"), std::string(windlinux::EXT_KIND_SKIN_DECOR_CLICK));
    CHECK_EQ(rustStr("POS_STATUS_TIP_QUERY"),
             std::string(windlinux::EXT_KIND_POS_STATUS_TIP_QUERY));
    CHECK_EQ(rustStr("POS_CANDIDATE"), std::string(windlinux::EXT_KIND_POS_CANDIDATE));
    CHECK_EQ(rustStr("POS_CANDIDATE_QUERY"),
             std::string(windlinux::EXT_KIND_POS_CANDIDATE_QUERY));

    CASE("menu.open 的非候选 target（有符号）与 protocol.rs `menu_target` 一致");
    SYNC("MENU_TARGET_MAIN", windlinux::MENU_TARGET_MAIN);
    SYNC("MENU_TARGET_STATUS", windlinux::MENU_TARGET_STATUS);
    SYNC("MENU_TARGET_TOOLTIP", windlinux::MENU_TARGET_TOOLTIP);

    TEST_MAIN_END();
}
