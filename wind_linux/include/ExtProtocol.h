// 「外部宿主」（macOS `.app` / Linux Fcitx5 addon）专用的 cmd id —— Windows TSF 不用，
// 故不在 wind_tsf/include/BinaryProtocol.h 里。
//
// 真源：wind_input/crates/wind-ipc/src/protocol.rs（同名常量）。Swift 镜像在
// wind_macos/Sources/WindInputKit/IPC/ProtocolTypes.swift。本文件的每个值都由
// tests/protocol_sync_test.cpp 在测试时逐个对照 protocol.rs 的源码——改了那边而忘了
// 这边，单测当场红，而不是等到「消息静默丢失」。
#pragma once

#include <cstdint>

namespace windlinux {

// ── 上行（addon → 服务）──
constexpr uint16_t CMD_MENU_ACTION = 0x0210;
constexpr uint16_t CMD_FRONT_CONTEXT = 0x0215;
/// 自绘菜单打开期间的指针事件：event u32 + button u32 + x i32 + y i32（Linux 为根窗口坐标，macOS 为 wire 点）。
constexpr uint16_t CMD_MENU_POINTER = 0x021B;
constexpr uint32_t MENU_POINTER_MOTION = 1;
constexpr uint32_t MENU_POINTER_PRESS = 2;
/// 工具栏 / 软键盘上的指针事件：kind u32 + event u32 + button u32 + x i32 + y i32（20 字节，位图内坐标）。
constexpr uint16_t CMD_PANEL_POINTER = 0x021C;
constexpr uint32_t PANEL_POINTER_MOTION = 1;
constexpr uint32_t PANEL_POINTER_PRESS = 2;
constexpr uint32_t PANEL_POINTER_RELEASE = 3;
constexpr uint32_t PANEL_POINTER_LEAVE = 4;
constexpr uint32_t PANEL_POINTER_WHEEL = 5;

// ── 下行 push（服务 → addon）──
constexpr uint16_t CMD_HOST_RENDER_FRAME = 0x0502;
constexpr uint16_t CMD_CANDIDATE_RECTS = 0x0503;
constexpr uint16_t CMD_MODE_STATUS = 0x0504;
constexpr uint16_t CMD_MENU_SHOW = 0x0506;
constexpr uint16_t CMD_KEY_TAP = 0x050E;
constexpr uint16_t CMD_KEY_SEQ = 0x050F;
constexpr uint16_t CMD_KEY_HOLD = 0x0510;
constexpr uint16_t CMD_KEY_RELEASE = 0x0511;
constexpr uint16_t CMD_KEY_TYPE = 0x0512;
/// 光栅浮层帧（状态气泡 / Toast / tooltip / 菜单），macOS `.app` 与 Linux addon 共用。布局见 Codec.h `OverlayFramePayload`。
constexpr uint16_t CMD_OVERLAY_FRAME = 0x0513;
/// 候选窗异形皮肤的区域（窗体 / 装饰矩形与鼠标策略 / 输入区），每个候选帧先于 CMD_HOST_RENDER_FRAME 一条。
/// 布局见 Codec.h `CandidateSkin`。
constexpr uint16_t CMD_CANDIDATE_SKIN = 0x0514;

// ── CMD_CANDIDATE_SKIN 的枚举（protocol.rs `pub mod skin`）──
constexpr uint32_t SKIN_FLAG_ACTIVE = 0x0001;
/// 带输入区表：候选窗只在这些矩形里接鼠标（穿透装饰、透明外扩区不接）。
constexpr uint32_t SKIN_FLAG_MASK = 0x0002;
constexpr uint32_t SKIN_HIT_DRAG = 0;
constexpr uint32_t SKIN_HIT_BLOCK = 1;
constexpr uint32_t SKIN_HIT_PASS = 2;
constexpr uint32_t SKIN_DECOR_UNDER = 0x0001;
constexpr uint32_t SKIN_DECOR_SPLIT = 0x0002;

// ── CMD_OVERLAY_FRAME 的枚举（protocol.rs `pub mod overlay`）──
constexpr uint32_t OVERLAY_KIND_TOOLTIP = 1;
constexpr uint32_t OVERLAY_KIND_STATUS = 2;
constexpr uint32_t OVERLAY_KIND_TOAST = 3;
/// 自绘菜单第 0 级；第 k 级子菜单 = OVERLAY_KIND_MENU + k（k < OVERLAY_MENU_LEVELS）。
constexpr uint32_t OVERLAY_KIND_MENU = 4;
constexpr uint32_t OVERLAY_MENU_LEVELS = 6;
/// 悬浮工具栏 / 软键盘：帧尾多一段拖动区（count u32 + count × x y w h i32）。
constexpr uint32_t OVERLAY_KIND_TOOLBAR = 10;
constexpr uint32_t OVERLAY_KIND_SOFTKEYBOARD = 11;
/// 候选窗的穿透装饰层（只 macOS 用；本端收不到，列出来供同步测试核对）。
constexpr uint32_t OVERLAY_KIND_DECOR = 12;
constexpr uint32_t OVERLAY_PLACE_ABSOLUTE = 0;
constexpr uint32_t OVERLAY_PLACE_FLIP = 1;
constexpr uint32_t OVERLAY_PLACE_FOLLOW_CANDIDATE = 2;
constexpr uint32_t OVERLAY_PLACE_ANCHOR = 3;
/// 坐标已由服务端按本端报去的工作区算定：原样摆放、不夹回（命中测试在服务端按这个位置做）。
constexpr uint32_t OVERLAY_PLACE_EXACT = 4;
constexpr uint32_t OVERLAY_ANCHOR_CENTER = 1;
constexpr uint32_t OVERLAY_ANCHOR_TOP_LEFT = 2;
constexpr uint32_t OVERLAY_ANCHOR_TOP_RIGHT = 3;
constexpr uint32_t OVERLAY_ANCHOR_BOTTOM_LEFT = 4;
constexpr uint32_t OVERLAY_ANCHOR_BOTTOM_RIGHT = 5;
constexpr uint32_t OVERLAY_ANCHOR_TOP_CENTER = 6;
constexpr uint32_t OVERLAY_ANCHOR_BOTTOM_CENTER = 7;

// ── 扩展信封 kind（protocol.rs `ext_kind`）──
/// 上行：请求打开自绘菜单。body 见 `encodeMenuOpenFrame`。
inline constexpr const char* EXT_KIND_MENU_OPEN = "menu.open";
/// 上行：本端把菜单收掉了（空闲超时 / 失焦 / 服务重启…），服务端据此复位 menu_open。
inline constexpr const char* EXT_KIND_MENU_DISMISS = "menu.dismiss";
/// 上行：状态气泡拖动落点 / 应询回报的当前位置（内容左上）。落不落盘由服务端按定位方式定。
inline constexpr const char* EXT_KIND_POS_STATUS_TIP = "pos.status_tip";
/// 上行：工具栏 / 软键盘拖动落点。body `{"x":..,"y":..,"output":"DP-1"}`，内容盒左上，output 可缺省。
inline constexpr const char* EXT_KIND_POS_TOOLBAR = "pos.toolbar";
inline constexpr const char* EXT_KIND_POS_SOFTKEYBOARD = "pos.softkeyboard";
/// 上行：焦点宿主进入 / 退出全屏。body `{"fullscreen":bool}`。
inline constexpr const char* EXT_KIND_HOST_FULLSCREEN = "host.fullscreen";
/// 上行：当前焦点宿主的显示环境。body `{"caret_free":bool,"caret_trusted":bool,"scale":1.5}`，
/// 字段可缺省（= 不改现值）。caret_free：Wayland 原生应用，候选窗由合成器摆位，服务端据此跳过
/// 首显闸门里「等权威坐标」的那段（否则首键后要等 600ms 长兜底）；caret_trusted：本端报的光标
/// 可直接当权威（Linux 恒为真，来源字段却只能填 UNKNOWN）；inline_preedit：客户端声明了 Preedit
/// 能力位（否则服务端把编码画进候选窗）；scale：界面缩放因子（见 protocol.rs `HOST_DISPLAY`）；output：焦点所在输出名（可缺省）；work：该输出的工作区 [左,上,右,下]（可缺省，与 output 成对报）。
/// panels：能力声明，本 addon 能贴工具栏 / 软键盘浮层（恒报 true；旧 addon 不报 → 服务端不开软键盘）。
inline constexpr const char* EXT_KIND_HOST_DISPLAY = "host.display";
/// 上行：点了候选窗上的一块皮肤装饰（drag / block）。body `{"index":N}`，N 为 CMD_CANDIDATE_SKIN
/// 里该装饰的 index。服务端触发它的 click 事件；旧服务不认这个 kind，安静忽略。
inline constexpr const char* EXT_KIND_SKIN_DECOR_CLICK = "skin.decor_click";
/// 下行：服务端问状态气泡此刻在哪（切「固定位置」时以当前位置落盘）。气泡不在屏上不答。
inline constexpr const char* EXT_KIND_POS_STATUS_TIP_QUERY = "pos.status_tip.query";

// ── `menu.open` 的 target 里「不是候选」的几档（protocol.rs `menu_target`）──
constexpr int32_t MENU_TARGET_MAIN = -1;
constexpr int32_t MENU_TARGET_STATUS = -2;
/// 悬停提示：body 另带右键点在提示位图内的坐标 `lx`/`ly`。
constexpr int32_t MENU_TARGET_TOOLTIP = -3;

/// InputScope 的 IS_PASSWORD 位（TSF 枚举 31）。服务端据此对密码框强制英文半角直通；
/// 与 Swift `InputController.inputScopePasswordBit`、Rust 协调器的判定同值。
constexpr uint64_t INPUT_SCOPE_PASSWORD_BIT = uint64_t(1) << 31;

/// 帧 flags（SharedRenderHeader / HostRenderFrame 共用）：位图内已画软件阴影。
constexpr uint32_t FRAME_FLAG_SOFTWARE_SHADOW = 0x0004;
/// 帧 flags：`(x, y)` 是固定位置的绝对屏幕坐标，不是按光标推算的落点。
constexpr uint32_t FRAME_FLAG_ABSOLUTE_POS = 0x0008;
/// 帧 flags：换帧（内容没变，只换皮肤装饰的动画帧）——照原位贴图，不重新落位、不 raise。
constexpr uint32_t FRAME_FLAG_ANIM_FRAME = 0x0010;

} // namespace windlinux
