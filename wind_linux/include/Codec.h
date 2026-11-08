// 帧编解码（纯 C++17，不依赖 Fcitx5）。
//
// 线上格式：8 字节头（u16 version | u16 cmd | u32 length，全部小端）+ payload。
// 帧布局真源是 Rust `wind-ipc/src/codec.rs`；Swift 对位实现是
// wind_macos/Sources/WindInputKit/IPC/BinaryCodec.swift——本文件的每个编解码函数都在
// 注释里写明它对位的是哪一个，改布局时三处一起看。
#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <vector>

namespace windlinux {

using Bytes = std::vector<uint8_t>;

/// 屏幕 / 位图上的矩形（左上 + 宽高）。
struct Rect {
    int32_t x = 0;
    int32_t y = 0;
    int32_t w = 0;
    int32_t h = 0;
};

struct Frame {
    uint16_t cmd = 0;
    bool isAsync = false;
    Bytes payload;
};

// ── 头 ──────────────────────────────────────────────────────────────
constexpr size_t HEADER_SIZE = 8;
constexpr uint32_t MAX_PAYLOAD_SIZE = 1024 * 1024;

Bytes encodeHeader(uint16_t cmd, uint32_t payloadLen, bool async = false);

struct HeaderInfo {
    uint16_t cmd = 0;
    uint32_t length = 0;
    bool isAsync = false;
};

enum class HeaderError { None, VersionMismatch, PayloadTooLarge };

/// 解 8 字节头。版本只比 major（高 4 位，剥掉 AsyncFlag 之后），与 Swift/Rust 一致。
HeaderError decodeHeader(const uint8_t* buf, HeaderInfo& out);

// ── 上行编码（返回完整帧：头 + payload）────────────────────────────────

Bytes encodeEmptyFrame(uint16_t cmd, bool async = false);

struct KeyEvent {
    uint32_t keyCode = 0;   // Windows VK
    uint32_t scanCode = 0;  // Linux 不上报扫描码，恒 0（同 macOS）
    uint32_t modifiers = 0; // KEYMOD_* 通用位
    uint8_t eventType = 0;  // KEY_EVENT_DOWN / KEY_EVENT_UP
    uint8_t toggles = 0;    // TOGGLE_CAPSLOCK / TOGGLE_NUMLOCK
    uint16_t eventSeq = 0;
    uint16_t prevChar = 0;  // 光标前一字符（UTF-16，0 = 不可用）
    uint16_t seqChar = 0;   // 输入序列末字符代表值（0 = 不可知），见 SeqTracker
};

/// CMD_KEY_EVENT：20 字节 payload（KeyPayload）。对位 Swift `encodeKeyEventFrame`。
Bytes encodeKeyEventFrame(const KeyEvent& e);

/// CMD_FOCUS_GAINED：39 字节定长段 + `bundleIdLen u32 + bundleId` + `windowClassLen u32 +
/// windowClass`。对位 Swift `encodeFocusGainedFrame`（它只发到 bundleId 段为止）。
///
/// ⚠ **必须发满 39 字节**：Rust `FocusGainedPayload::from_bytes` 的下限是 39（旧 38），
/// 发短了解码恒失败，焦点重型段（按应用初始模式 / 密码框强制英文 / 焦点状态气泡）整段
/// 不执行——macOS 早期就栽在这里（见 wind_macos/AGENTS.md「按应用配置 / 焦点重型段」）。
///
/// caret 段全 0：服务端 `apply_focus_caret` 见 height==0 即返回，坐标另经 CMD_CARET_UPDATE。
Bytes encodeFocusGainedFrame(uint64_t clientToken, uint64_t inputScopeMask,
                             const std::string& hostName, const std::string& windowClass);

/// CMD_FOCUS_LOST：clientToken u64 + reason u8（9 字节）。同步发送、服务端回 ack。
Bytes encodeFocusLostFrame(uint64_t clientToken, uint8_t reason);

/// CMD_IME_ACTIVATED / CMD_IME_DEACTIVATED：clientToken u64。服务端对这两条**从不回响应**
/// （server.rs 的对应臂恒返回 None），故一律带 AsyncFlag 发、不读响应。
Bytes encodeClientTokenFrame(uint16_t cmd, uint64_t clientToken);

/// CMD_CARET_UPDATE：x/y/height（12 字节版，同 macOS）。坐标：屏幕左上为原点、y 向下，
/// 物理像素（X11 根窗口坐标）。同步发送，服务端回 ack。
Bytes encodeCaretUpdateFrame(int32_t x, int32_t y, int32_t height);

/// CMD_CANDIDATE_SELECT：页内下标 i32（<0 为翻页按钮：-1 上页 / -2 下页）。
Bytes encodeCandidateSelectFrame(int32_t index);
/// CMD_CANDIDATE_HOVER：页内下标 i32；传入负值（无悬停）发 INT32_MIN，见实现处说明。
Bytes encodeCandidateHoverFrame(int32_t index);
/// CMD_CANDIDATE_SCROLL：delta i32，WHEEL_DELTA(120) 倍数，正 = 上滚。
Bytes encodeCandidateScrollFrame(int32_t delta);
/// CMD_MENU_ACTION：菜单 id i32。
Bytes encodeMenuActionFrame(int32_t id);
/// CMD_EXT（上行扩展信封）：kindLen u32 + kind + bodyLen u32 + body。
Bytes encodeExtFrame(const std::string& kind, const std::string& body);
/// CMD_MENU_POINTER：event u32 + button u32 + x i32 + y i32（16 字节，根窗口坐标）。
/// 对位 Rust `server.rs` 的 CMD_MENU_POINTER 臂（Linux 专属，无 Swift 对位）。
Bytes encodeMenuPointerFrame(uint32_t event, uint32_t button, int32_t x, int32_t y);
/// 上行扩展信封 `menu.open`：body = `{"target":T,"x":X,"y":Y,"work":[左,上,右,下]}`。
/// target ≥ 0 为该候选（页内下标）的右键菜单，-1 为功能主菜单；(x, y) 为菜单左上锚点。
Bytes encodeMenuOpenFrame(int32_t target, int32_t x, int32_t y, int32_t left, int32_t top,
                          int32_t right, int32_t bottom);
/// 同上，另带 `"lx":LX,"ly":LY`：右键点在悬停提示**位图内**的坐标（target =
/// MENU_TARGET_TOOLTIP 时用；按段 / 按行的命中在服务端按这个点做）。
Bytes encodeMenuOpenFrame(int32_t target, int32_t x, int32_t y, int32_t left, int32_t top,
                          int32_t right, int32_t bottom, int32_t lx, int32_t ly);
/// 上行扩展信封 `menu.dismiss`：body = `{"reason":"…"}`（reason 只含 [a-z_]，不转义）。
Bytes encodeMenuDismissFrame(const std::string& reason);
/// `host.display`：当前焦点宿主的显示环境（见 ExtProtocol.h）。scale 非正 = 不带该字段。
Bytes encodeHostDisplayFrame(bool caretFree, bool caretTrusted, bool inlinePreedit, double scale);
/// 焦点（或候选窗）所在的输出：名字（X11 取 RandR 输出名，Wayland 取 wl_output 的 name 事件）
/// 与它的工作区（屏幕坐标）。拿不到的留空——名字空串、工作区 nullopt——上报时就不带该字段。
struct HostOutput {
    std::string name;
    std::optional<Rect> work;
};
/// 同上，另带 `"output":"…"` 与 `"work":[左,上,右,下]`（与 menu.open 的 work 同格式）。
Bytes encodeHostDisplayFrame(bool caretFree, bool caretTrusted, bool inlinePreedit, double scale,
                             const HostOutput& output);
/// 上行扩展信封 `pos.*`（如 `pos.status_tip`）：body = `{"x":X,"y":Y}`，**内容**左上的屏幕坐标
/// （与配置 `ui.status.custom_x/y` 同义）。对位 Rust `decode_ext_point`。即不带输出名的下一个。
Bytes encodePosFrame(const std::string& kind, int32_t x, int32_t y);
/// 上行扩展信封 `pos.toolbar` / `pos.softkeyboard`：body = `{"x":X,"y":Y,"output":"…"}`，内容盒
/// 左上的屏幕坐标；`output` 为空时不带该字段（与 host.display 同源，两处要么都报要么都不报）。
Bytes encodePosFrame(const std::string& kind, int32_t x, int32_t y, const std::string& output);
/// 上行扩展信封 `host.fullscreen`：body = `{"fullscreen":true|false}`。
Bytes encodeHostFullscreenFrame(bool fullscreen);
/// CMD_PANEL_POINTER：kind u32 + event u32 + button u32 + x i32 + y i32（20 字节，**位图内**坐标，
/// 含软阴影扩边）。对位 Rust `codec.rs::encode_panel_pointer`（那边只出载荷，这里带头）。
Bytes encodePanelPointer(uint32_t kind, uint32_t event, uint32_t button, int32_t x, int32_t y);
/// CMD_FRONT_CONTEXT：appLen+app + titleLen+title + selLen+sel（均 UTF-8）。
Bytes encodeFrontContextFrame(const std::string& app, const std::string& title,
                              const std::string& sel);

// ── 下行解码（只解 payload，不含头）────────────────────────────────────

struct CommitTextPayload {
    uint32_t flags = 0;
    std::string text;
    std::string newComposition;
};
/// CMD_COMMIT_TEXT：flags u32 + textLen u32 + compLen u32 + text + composition。
std::optional<CommitTextPayload> decodeCommitText(const Bytes& p);

struct UpdateCompositionPayload {
    uint32_t caretPos = 0; // 组合内光标，UTF-16 码元
    std::string text;
};
/// CMD_UPDATE_COMPOSITION：caretPos u32 + text（剩余字节）。
std::optional<UpdateCompositionPayload> decodeUpdateComposition(const Bytes& p);

struct CommitTextWithCursorPayload {
    std::string text;
    uint32_t cursorOffset = 0; // 从文本末尾向左偏移的字符数
};
/// CMD_COMMIT_TEXT_WITH_CURSOR：textLen u32 + cursorOffset u32 + text。
std::optional<CommitTextWithCursorPayload> decodeCommitTextWithCursor(const Bytes& p);

/// CMD_MOVE_CURSOR：direction u32（1 = 右）。
std::optional<uint32_t> decodeMoveCursor(const Bytes& p);

struct ReplaceBackwardPayload {
    uint32_t count = 0;
    std::string text;
};
/// CMD_REPLACE_BACKWARD：count u32 + textLen u32 + text。
std::optional<ReplaceBackwardPayload> decodeReplaceBackward(const Bytes& p);

struct DeferredCompositionPayload {
    uint32_t timeoutMs = 0;
    std::string commitText;
    std::string holdText;
};
/// CMD_HOLD_COMPOSITION：timeoutMs u32 + textLen u32 + text（commitText 恒空）。
std::optional<DeferredCompositionPayload> decodeHoldComposition(const Bytes& p);
/// CMD_COMMIT_AND_HOLD / CMD_COMMIT_THEN_DEFER：timeoutMs u32 + commitLen u32 + holdLen u32 +
/// commit + hold（两者线格式相同）。
std::optional<DeferredCompositionPayload> decodeCommitAndHold(const Bytes& p);

/// CMD_KEY_TYPE：整段 UTF-8（无长度前缀）。
std::string decodeKeyType(const Bytes& p);

/// 命令直通车按键合成的一个组合（`Ctrl+C` → key "c"、mods {"ctrl"}）。服务端
/// `handle_cmdbar_macos.rs::split_combo` 已归一：key 小写规范名，mods ⊆ {ctrl, shift, alt, win}
/// （未知修饰名原样小写透传）。
struct KeyComboPayload {
    std::string key;
    std::vector<std::string> mods;
};
/// CMD_KEY_TAP / CMD_KEY_HOLD / CMD_KEY_RELEASE：keyLen u32 + key + modCount u32 +
/// modCount × (modLen u32 + mod)。对位 Rust `codec.rs::push_key_combo`、Swift `decodeCombo`。
std::optional<KeyComboPayload> decodeKeyCombo(const Bytes& p);
/// CMD_KEY_SEQ：comboCount u32 + comboCount × combo（布局同上）。
std::optional<std::vector<KeyComboPayload>> decodeKeySeq(const Bytes& p);

struct HostRenderFramePayload {
    uint32_t seq = 0;
    int32_t x = 0; // 逻辑点（top-left），Linux 上 scale 恒 1 ⇒ 即物理像素
    int32_t y = 0;
    uint32_t width = 0; // 设备像素
    uint32_t height = 0;
    uint32_t flags = 0;
    uint32_t scale = 1;
    bool visible() const { return (flags & 0x1) != 0; }
};
/// CMD_HOST_RENDER_FRAME（push）：seq,x,y,w,h,flags 各 u32/i32（24 字节）[+ scale u32]。
std::optional<HostRenderFramePayload> decodeHostRenderFrame(const Bytes& p);

/// CMD_OVERLAY_FRAME（push）：一层浮层（状态气泡 / Toast / tooltip）有新帧或该隐藏。
/// 像素在该层自己的 SHM 段（`overlayShmName(kind)`）。68 字节基础布局（服务端现在恒在末尾多发一个 f32 scale 成 72 字节，addon 不读，按 ≥68 字节接收），逐字段对位 Rust
/// `codec.rs::encode_overlay_frame`：kind seq w h flags place（u32）、x y altX altY（i32）、
/// anchor（u32）、margin contentX contentY（i32）、contentW contentH（u32）、durationMs（i32）。
/// 坐标一律是**内容盒**左上（不含软阴影扩边）。
///
/// 工具栏 / 软键盘（kind 10 / 11）在 72 字节之后多一段拖动区：count u32 + count × (x y w h) i32，
/// 位图内坐标。只对这两个 kind 解析；尾段 count 与数据对不上整帧拒收。
struct OverlayFramePayload {
    uint32_t kind = 0;
    uint32_t seq = 0;
    uint32_t width = 0;
    uint32_t height = 0;
    uint32_t flags = 0;
    uint32_t place = 0;
    int32_t x = 0;
    int32_t y = 0;
    int32_t altX = 0;
    int32_t altY = 0;
    uint32_t anchor = 0;
    int32_t margin = 0;
    int32_t contentX = 0;
    int32_t contentY = 0;
    uint32_t contentW = 0;
    uint32_t contentH = 0;
    int32_t durationMs = 0; // >0 由本端计时自动隐藏；0 = 常驻到下一帧
    std::vector<Rect> drag; // 可拖动区域（位图内），仅 kind 10 / 11
    bool visible() const { return (flags & 0x1) != 0; }
};
std::optional<OverlayFramePayload> decodeOverlayFrame(const Bytes& p);
/// 工具栏 / 软键盘：可交互、可拖动的两层（帧尾带拖动区、指针走 CMD_PANEL_POINTER）。
bool isPanelOverlayKind(uint32_t kind);

struct CandidateHitRect {
    int32_t index = 0;
    int32_t x = 0;
    int32_t y = 0;
    int32_t w = 0;
    int32_t h = 0;
    bool contains(int32_t px, int32_t py) const
    {
        return px >= x && px < x + w && py >= y && py < y + h;
    }
};
/// CMD_CANDIDATE_RECTS（push）：count u32 + count × (index,x,y,w,h 各 i32)。
std::optional<std::vector<CandidateHitRect>> decodeCandidateRects(const Bytes& p);

struct ExtEnvelope {
    std::string kind;
    Bytes body;
};
/// CMD_EXT（下行扩展信封）。解不出返回空——调用方按「未知消息」安静忽略（版本兼容的根本）。
std::optional<ExtEnvelope> decodeExt(const Bytes& p);

// ── 工具 ────────────────────────────────────────────────────────────

/// FNV-1a 32 位散列，结果恒非 0（空串返回 0）。宿主标识用它代替 pid：
/// 服务端以 pid==0 表示「未知宿主」并跳过按应用逻辑；散列满足「同宿主恒等、异宿主相异」，
/// 且宿主重启后不变。对位 Swift `InputController.stableHash`。
uint32_t stableHash(const std::string& s);

} // namespace windlinux
