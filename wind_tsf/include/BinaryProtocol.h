#pragma once

// 本文件与 wind_input/crates/wind-ipc/src/protocol.rs（常量与结构）、codec.rs（编解码）互为镜像，
// 改命令码、Header、Payload 布局或状态位时三处同步，否则 DLL 与服务的 IPC 不兼容。

#include <cstdint>
#include <vector>
#include <string>

#include "ResponseType.h" // 不含 Win32，吃键判据可在 Linux 上单测

// Protocol version (major.minor: high 4 bits = major, low 12 bits = minor)
constexpr uint16_t PROTOCOL_VERSION = 0x1001; // v1.1 - Added barrier mechanism and state machine support

// Async flag (used in version field's high bit to mark async requests)
constexpr uint16_t ASYNC_FLAG = 0x8000; // Async request flag - no response expected

// ============================================================================
// Upstream commands (C++ -> core)
// ============================================================================
constexpr uint16_t CMD_KEY_EVENT        = 0x0101; // Key event (down/up)
constexpr uint16_t CMD_COMMIT_REQUEST   = 0x0104; // Commit request with barrier (Space/Enter/number select)
constexpr uint16_t CMD_FOCUS_GAINED     = 0x0201; // Focus gained
constexpr uint16_t CMD_FOCUS_LOST       = 0x0202; // Focus lost
constexpr uint16_t CMD_IME_ACTIVATED    = 0x0203; // IME activated
constexpr uint16_t CMD_IME_DEACTIVATED  = 0x0204; // IME deactivated
constexpr uint16_t CMD_MODE_NOTIFY      = 0x0205; // Mode changed notification (TSF local toggle, async)
constexpr uint16_t CMD_TOGGLE_MODE      = 0x0207; // Toggle mode request (from UI click)
constexpr uint16_t CMD_SYSTEM_MODE_SWITCH = 0x020B; // System mode switch (Ctrl+Space, sync, carries target mode)
constexpr uint16_t CMD_MENU_COMMAND     = 0x0208; // Menu command (toggle_mode, toggle_width, etc.)
constexpr uint16_t CMD_SHOW_CONTEXT_MENU     = 0x020A; // Request to show context menu (sends screen coordinates)
constexpr uint16_t CMD_CANDIDATE_SELECT      = 0x020D; // Host render: mouse click hit a candidate (payload: pageLocalIndex i32; <0 = page button -1 up / -2 down)
constexpr uint16_t CMD_CANDIDATE_HOVER       = 0x020E; // Host render: mouse hover (payload: index i32 + anchorX i32 + belowY i32 + aboveY i32). index: >=0 candidate, -1 nothing, -2 page-up button, -3 page-down button (differs from select/rect -1/-2 convention: hover needs a distinct "nothing")
// 服务端映射为高亮上下移（页边界跨页），见 Coordinator::handle_candidate_scroll。
constexpr uint16_t CMD_CANDIDATE_SCROLL      = 0x0211; // Host render: mouse wheel over candidate box (payload: delta i32, WHEEL_DELTA multiple, >0 = up).
constexpr uint16_t CMD_HOST_RENDER_FAILED    = 0x0212; // Host render: band window creation failed in DLL (async, payload: reason u32). core logs + notifies user the candidate fell back to its local window
constexpr uint32_t HOST_RENDER_FAIL_WINDOW_CREATE = 1; // CreateWindowInBand failed at target band and band=0
// Input state report (C++ -> core, async): compartment-driven disabled/reason change without
// a fresh focus_gained (e.g. KEYBOARD_DISABLED flips while focus stays in the same web page).
// Payload: InputStateReportPayload (14 bytes).
constexpr uint16_t CMD_INPUT_STATE_REPORT    = 0x0213;
// Observed key (C++ -> core, async): a user keydown that this DLL passed straight to the host
// (English-mode letters, idle Backspace/Enter, ...). Read-only notice for extensions, never fed
// into key handling. Only sent while core has pushed CONFIG_KEY_EXTENSION_KEY_OBSERVE on, and
// never in password fields / keyboard-disabled contexts. Payload: KeyObservedPayload (8 bytes).
constexpr uint16_t CMD_KEY_OBSERVED          = 0x021D;
// Diagnostics snapshot (C++ -> core, async): focus window chain + foreground window + TSF
// context instance ids. Payload: DiagSnapshotHeader (64 bytes) followed by three
// length-prefixed UTF-8 class names (focus / root / foreground), each `len u16 LE + bytes`.
// 刻意不并入 CMD_FOCUS_GAINED：那是宿主 UI 线程上的同步往返，首字延迟挂在它身上。
// 独立异步发送，且由 CONFIG_KEY_DIAG_SNAPSHOT 门控，HUD 关闭时不采集。
constexpr uint16_t CMD_DIAG_SNAPSHOT         = 0x0214;
// TSF UI-less（UIElement）：宿主接管候选绘制时（BeginUIElement 回 pbShow=FALSE），DLL 经
// ITfCandidateListUIElement 把服务进程里的候选交给宿主。
//   STATE  (C++ -> core, async)：报告本进程「谁画」（UiElementStatePayload）。
//   QUERY  (C++ -> core, sync) ：拉取候选快照，响应 PAGE；只在宿主接管时发。
//   ACTION (C++ -> core, async)：宿主经 ITfCandidateListUIElementBehavior 的操作回流。
// 设计见 docs/design/game-compat-tsf-uielement.md。
constexpr uint16_t CMD_UIELEMENT_STATE       = 0x0217;
constexpr uint16_t CMD_UIELEMENT_QUERY       = 0x0218;
constexpr uint16_t CMD_UIELEMENT_PAGE        = 0x0219; // core -> C++ (QUERY 的响应)
constexpr uint16_t CMD_UIELEMENT_ACTION      = 0x021A;
// 0x021B CMD_MENU_POINTER / 0x021C CMD_PANEL_POINTER 是 Linux addon 专用上行
// （wind_linux/include/ExtProtocol.h），占着码位，新增命令别撞。
constexpr uint16_t CMD_COMPOSITION_TERMINATED = 0x0209; // Composition unexpectedly terminated (e.g., user clicked in input field)
constexpr uint16_t CMD_CARET_UPDATE     = 0x0301; // Caret position update
constexpr uint16_t CMD_SELECTION_CHANGED = 0x0302; // Selection/caret changed without composition (from ITfTextEditSink)
constexpr uint16_t CMD_CARET_PENDING    = 0x0303; // First-show handshake: composition just started, real caret coming after reflow
// DLL 只采样上报不筛选：首帧 reflow 期间前几次 GetTextExt 可能仍是旧坐标，哪一帧可信由能读
// compat.toml 的服务端判定。
constexpr uint16_t CMD_CARET_PROBE      = 0x0304; // First-show probe: one pre-reflow caret sample per OnLayoutChange burst iteration.
// Generic extension envelope (same code point both directions, distinguished by
// direction like the rest of this file). Layout:
//   kindLen u32 + kind(UTF-8) + bodyLen u32 + body(opaque bytes, usually JSON)
//
// New messages: high frequency (per key / frame / mouse move) get a dedicated code point;
// low frequency ones go through this envelope as a new `kind` (rationale at CMD_EXT in
// wind-ipc protocol.rs). Unknown `kind` MUST be ignored silently so old and new peers
// interoperate. Windows only sends `diag.host_render_placed` so far (HostBandPolicy.h).
constexpr uint16_t CMD_EXT              = 0x0E01;

constexpr uint16_t CMD_BATCH_EVENTS     = 0x0F01; // Batch events container
constexpr uint16_t CMD_INPUT_STATS      = 0x0F03; // Input stats report (async, from English mode)

// ============================================================================
// Downstream commands (core -> C++)
// ============================================================================
constexpr uint16_t CMD_ACK                = 0x0001; // Simple acknowledgment
constexpr uint16_t CMD_PASS_THROUGH       = 0x0002; // Key not handled, pass to system
constexpr uint16_t CMD_COMMIT_TEXT        = 0x0101; // Commit text
constexpr uint16_t CMD_UPDATE_COMPOSITION = 0x0102; // Update composition
constexpr uint16_t CMD_CLEAR_COMPOSITION  = 0x0103; // Clear composition
// 收掉组合，并把当前这个键交还宿主（联想态回车/退格透传）。无载荷。
// 不能用「ClearComposition + pfEaten=FALSE」：OnTestKeyDown 已吃下此键，再吐出会让不补发
// WM_KEYDOWN 的宿主丢键；故吃掉原键再 SendInput 重放。
constexpr uint16_t CMD_CLEAR_THEN_PASS_THROUGH = 0x010D;
constexpr uint16_t CMD_COMMIT_RESULT      = 0x0105; // Commit result (response to COMMIT_REQUEST)
// 0x0201 (CMD_MODE_CHANGED) removed: 所有模式切换响应统一走 CMD_STATUS_UPDATE
constexpr uint16_t CMD_STATUS_UPDATE      = 0x0202; // Full status update
constexpr uint16_t CMD_STATE_PUSH         = 0x0206; // State push (broadcast to all clients, hotkeys-less)
constexpr uint16_t CMD_SERVICE_READY      = 0x0207; // Service connected push pipe, TSF should sync state
// IME_ACTIVATED / FOCUS_GAINED 的异步状态回包：服务端先回 ACK 放开宿主 UI 线程，处理完再经
// push pipe 推本命令，载荷同 CMD_STATUS_UPDATE（含 hotkeys）。AsyncReader 收到后 Post 到 TSF
// 线程做 _SyncStateFromResponse + _EnsureHostRenderSetup。CMD_STATE_PUSH 是变更广播，不带 hotkeys。
constexpr uint16_t CMD_ACTIVATION_STATUS_PUSH = 0x020C;
// FocusGained 同步路径的轻量模式预推送。载荷 4 字节 flags（STATUS_CHINESE_MODE / FULL_WIDTH /
// CHINESE_PUNCT），DLL 只原子写三个 _b* 字段，不动热键白名单。
// 标点位不可省：漏了它 _bChinesePunct 每次焦点切换被清零，中文标点态下误用英文态的透传集合。
constexpr uint16_t CMD_MODE_PUSH              = 0x020D;
// CMD_SHELL_EXEC：在 TSF 侧（前台应用进程）执行 ShellExecuteW，解决 Service 进程无前台权限问题。
// 载荷：target_len(u32 LE) + target(UTF-8) + params_len(u32 LE) + params(UTF-8)
constexpr uint16_t CMD_SHELL_EXEC             = 0x020E;
// 只让语言栏重取图标（OnUpdate(TF_LBI_ICON)），无载荷。GetIcon 是被动回调，状态推送的去重
// 又会挡掉「只有位图变了」的情形，故单列命令。刻意不带载荷：图标内容只以 SHM 为准。
constexpr uint16_t CMD_REFRESH_ICON           = 0x0216;
constexpr uint16_t CMD_SYNC_HOTKEYS       = 0x0301; // Sync hotkey whitelist
constexpr uint16_t CMD_SYNC_CONFIG        = 0x0303; // Sync config key/value (generic)
constexpr uint16_t CMD_CONSUMED           = 0x0401; // Key consumed
constexpr uint16_t CMD_COMMIT_TEXT_WITH_CURSOR = 0x0106; // Commit text with cursor offset
constexpr uint16_t CMD_MOVE_CURSOR             = 0x0107; // Move cursor (smart skip)
constexpr uint16_t CMD_DELETE_PAIR             = 0x0108; // Delete pair (smart backspace)
constexpr uint16_t CMD_REPLACE_BACKWARD        = 0x0109; // Replace preceding char(s): delete N before caret + insert text
constexpr uint16_t CMD_HOLD_COMPOSITION         = 0x010A; // Hold composition: open composition + auto-commit after timeout_ms
constexpr uint16_t CMD_COMMIT_AND_HOLD          = 0x010B; // Commit text then hold composition (punct_commit + smart symbol)
constexpr uint16_t CMD_COMMIT_THEN_DEFER         = 0x010C; // 真提交 commit 后，余码 defer 组合延迟到 keyup 才开
constexpr uint16_t CMD_HOST_RENDER_SETUP  = 0x0501; // Host render setup (shared memory + event names)
constexpr uint16_t CMD_BATCH_RESPONSE     = 0x0F02; // Batch response container

// ============================================================================
// Host render commands (C++ -> core)
// ============================================================================
constexpr uint16_t CMD_HOST_RENDER_REQUEST = 0x0501; // DLL requests host render setup (payload: caps u32)
// Capability bits sent in CMD_HOST_RENDER_REQUEST (old services ignore the payload).
constexpr uint32_t HOST_RENDER_CAP_PASS_LAYER = 0x0001; // can show the pass-through decor layer

// ============================================================================
// Key event types
// ============================================================================
constexpr uint8_t KEY_EVENT_DOWN = 0;
constexpr uint8_t KEY_EVENT_UP   = 1;
// 候选窗显示时 Ctrl 的如实上报（按下 / 松开），单击由服务端判。按下那一刻有输入会话才走这条，
// 一次按住只走一条路（见 protocol.rs 的 EVENT_MOD_DOWN）。旧 core 对未知类型放行，混搭安全。
constexpr uint8_t KEY_EVENT_MOD_DOWN = 0x02;
constexpr uint8_t KEY_EVENT_MOD_UP   = 0x03;

// ============================================================================
// Toggle key state flags (for KeyPayload.toggles)
// ============================================================================
constexpr uint8_t TOGGLE_CAPSLOCK   = 0x01; // CapsLock is on
constexpr uint8_t TOGGLE_NUMLOCK    = 0x02; // NumLock is on
constexpr uint8_t TOGGLE_SCROLLLOCK = 0x04; // ScrollLock is on

// 自上一个 keydown 送达服务端以来有键进了宿主（语义以 PassthroughNote.h 为准，宽的一侧安全）。
// 搭 toggles 空闲位是为了不动 KeyPayload 的 18 字节布局，新旧 DLL / core 混搭两向都安全。
// 用于智能符号 press2 判定：两按之间夹了服务端看不见的输入就不能当 press2，否则
// ReplaceBackward 会删掉用户刚打的字。DLL 报事实，服务端持策略。
constexpr uint8_t TOGGLE_PASSTHROUGH_KEY = 0x08;

// KEY_EVENT_MOD_UP 上：这次按住期间平台看见过服务端看不见的输入（透传的组合键、鼠标），不算单击。
constexpr uint8_t TOGGLE_MOD_NOT_TAP = 0x10;

// ============================================================================
// Modifier flags for KeyHash encoding (high 16 bits)
// Using KEYMOD_ prefix to avoid conflicts with Windows SDK MOD_* macros
// ============================================================================
constexpr uint32_t KEYMOD_SHIFT    = 0x0001; // Generic Shift
constexpr uint32_t KEYMOD_CTRL     = 0x0002; // Generic Ctrl
constexpr uint32_t KEYMOD_ALT      = 0x0004; // Alt
constexpr uint32_t KEYMOD_WIN      = 0x0008; // Windows key
constexpr uint32_t KEYMOD_LSHIFT   = 0x0010; // Left Shift specifically
constexpr uint32_t KEYMOD_RSHIFT   = 0x0020; // Right Shift specifically
constexpr uint32_t KEYMOD_LCTRL    = 0x0040; // Left Ctrl specifically
constexpr uint32_t KEYMOD_RCTRL    = 0x0080; // Right Ctrl specifically
constexpr uint32_t KEYMOD_CAPSLOCK = 0x0100; // CapsLock as toggle key marker

// ============================================================================
// Status flags for StatusPayload
// ============================================================================
constexpr uint32_t STATUS_CHINESE_MODE     = 0x0001; // Chinese mode
constexpr uint32_t STATUS_FULL_WIDTH       = 0x0002; // Full-width mode
constexpr uint32_t STATUS_CHINESE_PUNCT    = 0x0004; // Chinese punctuation
constexpr uint32_t STATUS_TOOLBAR_VISIBLE  = 0x0008; // Toolbar visible
constexpr uint32_t STATUS_MODE_CHANGED     = 0x0010; // Mode was just changed
constexpr uint32_t STATUS_CAPS_LOCK        = 0x0020; // CapsLock is on
constexpr uint32_t STATUS_HOST_RENDER_AVAIL = 0x0040; // Host render available (DLL should request setup)
// 软键盘开着：中文模式无会话时数字键本应交还宿主，软键盘的数字行需要吃下它们。
constexpr uint32_t STATUS_SOFT_KEYBOARD    = 0x0080; // Soft keyboard panel is open
// 当前是键盘面（send_keys）而非符号面：按键落回常规判定链，只有 Esc 与翻页仍归面板。
constexpr uint32_t STATUS_SOFT_KEYBOARD_KEYS = 0x0100; // Current page sends keys, not symbols
// 热键激活的模式活着，是输入会话的来源之一（见 CKeyEventSink::_HasInputSession）。
// 须由服务端告知：这些模式可以不建 composition，DLL 自己判不出会话，Enter/Esc 会被透传；
// DLL 按热键 id 段自行置位又会过宽（软键盘等也在段内）。
// 本位是 level-triggered，每次推送都带完整值、DLL 无条件镜像；改成边沿驱动漏一次清位就会卡住。
constexpr uint32_t STATUS_HOTKEY_SESSION   = 0x0200; // 热键激活的模式活着（加词/临拼/特殊/生僻字）

// ============================================================================
// 模式切换来源（仅 CMD_SYSTEM_MODE_SWITCH 的 flags 使用高 4 位，与 STATUS_* 不重叠）
//
// 服务端只用它写日志，用于区分「宿主/系统写 compartment 关 IME」与按键切换。
// ============================================================================
constexpr uint32_t MODE_SWITCH_SOURCE_SHIFT = 28;
// compartment 变化时 Ctrl 正按住，用来区分用户 Ctrl+Space 与宿主自行写 compartment。
constexpr uint32_t MODE_SWITCH_CTRL_HELD = 0x08000000;
enum class ModeSwitchSource : uint32_t
{
    CompartmentOpenClose  = 1, // 宿主/系统写了 OPENCLOSE
    CompartmentConversion = 2, // 宿主/系统写了 INPUTMODE_CONVERSION
    CtrlSpaceKey          = 3, // 系统热键失效时的按键侧兜底
};

// ============================================================================
// Protocol structures (must match wind-ipc protocol.rs exactly)
// ============================================================================
#pragma pack(push, 1)

// Protocol header (8 bytes)
struct IpcHeader
{
    uint16_t version;  // Protocol version (high bit may be ASYNC_FLAG)
    uint16_t command;  // Command type
    uint32_t length;   // Payload length in bytes
};
static_assert(sizeof(IpcHeader) == 8, "IpcHeader must be 8 bytes");

// Batch events header (4 bytes)
struct BatchHeader
{
    uint16_t eventCount;  // Number of events in this batch
    uint16_t reserved;    // Reserved for future use
};
static_assert(sizeof(BatchHeader) == 4, "BatchHeader must be 4 bytes");

// Key event payload (20 bytes)
struct KeyPayload
{
    uint32_t keyCode;     // Virtual key code
    uint32_t scanCode;    // Scan code
    uint32_t modifiers;   // Modifier flags (snapshot at event time, from state machine)
    uint8_t  eventType;   // 0=KeyDown, 1=KeyUp
    uint8_t  toggles;     // Toggle key states (CapsLock/NumLock/ScrollLock)
    uint16_t eventSeq;    // Monotonic event sequence number
    uint16_t prevChar;    // Character before caret, 0 if unavailable. ASCII digits come only from
                          // the IME's own input sequence, never from the document (SmartPunctPrevChar.h)
    uint16_t seqChar;     // Last char the IME saw go into the document (commit tail / passthrough key
                          // class representative), 0 = unknown. See InputSequenceChar.h
};
static_assert(sizeof(KeyPayload) == 20, "KeyPayload must be 20 bytes");

// Caret position payload (20 bytes)
struct CaretPayload
{
    int32_t x;
    int32_t y;
    int32_t height;
    int32_t compositionStartX; // Screen X of composition range start (0 if no composition)
    int32_t compositionStartY; // Screen Y of composition range start (0 if no composition)
};
static_assert(sizeof(CaretPayload) == 20, "CaretPayload must be 20 bytes");

// ── 坐标来源（CaretPayloadV2::source）────────────────────────────────────────
// 同一组 x/y/h 可能来自语义不同的通道（如 Win32 光标冒充 TSF 插入点）。消费端据此决定这一帧
// 能否当权威坐标、能否与组合起点比较。各来源的宿主背景见
// docs/architecture/caret-position-host-compat-notes.md。
constexpr int32_t CARET_SRC_UNKNOWN        = 0; // 旧协议 / macOS 短包，无法判定
constexpr int32_t CARET_SRC_TSF_SELECTION  = 1; // GetTextExt(selection)，最精确
constexpr int32_t CARET_SRC_TSF_COMPOSITION= 2; // selection 退化后降级用的组合起点，仍属 TSF 域
constexpr int32_t CARET_SRC_TSF_CACHED     = 3; // UpdateComposition edit session 内缓存，同属 TSF 域
constexpr int32_t CARET_SRC_GUI_CARET      = 4; // GetGUIThreadInfo/GetCaretPos 回退，跨窗口，不可作权威
constexpr int32_t CARET_SRC_CONSOLE        = 5; // 控制台窗口的估算位置
constexpr int32_t CARET_SRC_LAST_KNOWN     = 6; // 上次已知好值
// 组合刚启动时的探测（FirstShowProbe），是宿主重排前的旧坐标。不可作权威、不参与首显决策，
// 服务端只拿它刷新坐标缓存（连续快速上屏时它是唯一的位置来源，见 Rust 侧 caret_source::PRE_REFLOW）。
constexpr int32_t CARET_SRC_PRE_REFLOW     = 7;
// 宿主表示「没有插入点可给」：GetTextExt 给出高度为 0 的退化矩形，且重试窗口过完仍是它
// （如 Illustrator 画布恒为工作区右下角）。要采信而非丢弃，丢弃会跌到无关控件的 Win32 光标。
// 判据必须是「重试窗口过完」仍退化：短暂退化随后恢复的宿主提前采信会多一次闪跳，故只由
// CaretProbeKind::RetryDeadline 及同组合内被闩住后的同步路径产出。高度是合成的
// （WIND_DEFAULT_CARET_HEIGHT），否则服务端 caret_is_valid 会丢掉 h=0。
// 不属 TSF 语义域（caret_source::is_tsf 为 false），不得用于跟行、漂移校正。
constexpr int32_t CARET_SRC_TSF_DEFAULT_POS = 8;
// 宿主经 IMM32 ImmSetCandidateWindow 声明的候选窗位置（CANDIDATEFORM，已换成屏幕坐标）。
// 只在 TSF 三次查询给出同一个退化矩形（TSF_DEFAULT_POS 指纹）时才取，见 CaretEditSession.cpp
// 的「三级降级」；典型宿主是走 CUAS 的 Java AWT/Swing。属 TSF 语义域，但不随它上报组合起点
// （compStart=(0,0)）：首帧多是上次组合的旧值，报了会被锁成锚点。
constexpr int32_t CARET_SRC_IMM_CANDIDATE_FORM = 9;

// Caret position payload v2 (24 bytes) = CaretPayload + source
// 刻意不把 source 加进 CaretPayload：FocusGainedPayload 内嵌了它，改大小会让新旧混搭时焦点载荷
// 整体错位（焦点通道改在尾部追加 caretSource）。服务端按长度分支：20/12 字节视为 UNKNOWN。
struct CaretPayloadV2
{
    CaretPayload caret;
    int32_t      source;
};
static_assert(sizeof(CaretPayloadV2) == 24, "CaretPayloadV2 must be 24 bytes");

// Caret position payload v3 (40 bytes) = CaretPayloadV2 + 组合范围矩形
// 矩形是宿主对整个组合 range 的 GetTextExt 结果，可信时锚点取 (left, bottom)，单行与跨行同一公式；
// 可信判据与各宿主形态见 docs/architecture/caret-position-host-compat-notes.md「二·六」节。
// 取不到时四值全 0。服务端按长度分支，24/20/12 字节走旧路径。
struct CaretPayloadV3
{
    CaretPayload caret;
    int32_t      source;
    int32_t      compRectLeft;   // 整个组合 range 的包围矩形；四值全 0 = 未取到
    int32_t      compRectTop;
    int32_t      compRectRight;
    int32_t      compRectBottom;
};
static_assert(sizeof(CaretPayloadV3) == 40, "CaretPayloadV3 must be 40 bytes");

// Selection changed payload (4 bytes) - sent from ITfTextEditSink::OnEndEdit
// Notifies core that the caret moved outside of composition (e.g., mouse click)
struct SelectionChangedPayload
{
    uint16_t prevChar;  // Character before caret after selection change (0 if unavailable)
    uint16_t reserved;  // Reserved for future use
};
static_assert(sizeof(SelectionChangedPayload) == 4, "SelectionChangedPayload must be 4 bytes");

// Composition update header (before UTF-8 text)
struct CompositionHeader
{
    int32_t caretPos;
    // Followed by UTF-8 text (length = header.length - 4)
};
static_assert(sizeof(CompositionHeader) == 4, "CompositionHeader must be 4 bytes");

// Status update header
struct StatusHeader
{
    uint32_t flags;        // Status flags
    uint32_t keyDownCount; // Number of KeyDown hotkeys
    uint32_t keyUpCount;   // Number of KeyUp hotkeys
    // Followed by (keyDownCount + keyUpCount) uint32_t keyHash values
};
static_assert(sizeof(StatusHeader) == 12, "StatusHeader must be 12 bytes");

// Commit text header (for complex commits with mode change or new composition)
struct CommitTextHeader
{
    uint32_t flags;            // bit0: modeChanged, bit1: hasNewComposition, bit2: chineseMode
    uint32_t textLength;       // Length of commit text in bytes
    uint32_t compositionLength;// Length of new composition in bytes (0 if none)
    // Followed by UTF-8 text, then optional UTF-8 new composition
};
static_assert(sizeof(CommitTextHeader) == 12, "CommitTextHeader must be 12 bytes");

// Commit text with cursor payload
struct CommitTextWithCursorPayload
{
    uint32_t textLength;    // Length of text (UTF-8)
    uint32_t cursorOffset;  // Chars to move left from end of inserted text
    // Followed by UTF-8 text
};
static_assert(sizeof(CommitTextWithCursorPayload) == 8, "CommitTextWithCursorPayload must be 8 bytes");

// Move cursor payload
struct MoveCursorPayload
{
    // 向右移动的格数（合成几次 VK_RIGHT）。0 视同 1，兼容旧 core。
    uint32_t count;
};
static_assert(sizeof(MoveCursorPayload) == 4, "MoveCursorPayload must be 4 bytes");

// Replace backward payload (smart symbol: delete N chars before caret, then insert text)
struct ReplaceBackwardPayload
{
    uint32_t count;      // Number of chars to delete before caret
    uint32_t textLength; // Length of insert text (UTF-8)
    // Followed by UTF-8 text
};
static_assert(sizeof(ReplaceBackwardPayload) == 8, "ReplaceBackwardPayload must be 8 bytes");

// HoldComposition payload (smart symbol: open composition with text, auto-commit after timeoutMs)
struct HoldCompositionPayload
{
    uint32_t timeoutMs;  // Auto-commit timeout in milliseconds
    uint32_t textLength; // Length of composition text (UTF-8)
    // Followed by UTF-8 text
};
static_assert(sizeof(HoldCompositionPayload) == 8, "HoldCompositionPayload must be 8 bytes");

// CommitAndHold payload (punct_commit + smart symbol: commit text, then open composition with hold text)
struct CommitAndHoldPayload
{
    uint32_t timeoutMs;    // Auto-commit timeout for hold composition (milliseconds)
    uint32_t commitLength; // Length of commit text (UTF-8)
    uint32_t holdLength;   // Length of hold composition text (UTF-8)
    // Followed by commitLength bytes of UTF-8 commit_text, then holdLength bytes of UTF-8 hold_text
};
static_assert(sizeof(CommitAndHoldPayload) == 12, "CommitAndHoldPayload must be 12 bytes");

// CommitThenDefer payload (真提交 commit 后，余码 defer 组合延迟到 keyup 才开)
struct CommitThenDeferPayload
{
    uint32_t timeoutMs;    // Deferred composition auto-commit timeout (milliseconds)
    uint32_t commitLength; // Length of commit text (UTF-8)
    uint32_t deferLength;  // Length of deferred composition text (UTF-8)
    // Followed by commitLength bytes of UTF-8 commit_text, then deferLength bytes of UTF-8 defer_text
};
static_assert(sizeof(CommitThenDeferPayload) == 12, "CommitThenDeferPayload must be 12 bytes");

// Commit text flags
constexpr uint32_t COMMIT_FLAG_MODE_CHANGED       = 0x0001;
constexpr uint32_t COMMIT_FLAG_HAS_NEW_COMPOSITION = 0x0002;
constexpr uint32_t COMMIT_FLAG_CHINESE_MODE       = 0x0004;
// 本次提交要**替换**掉 HoldComposition 里待定的中文符号（智能符号 press2：「。」→「.」），
// 而非追加在它后面。只有 press2 会置位；其余提交路径一律追加（见 CTextService::CommitText）。
constexpr uint32_t COMMIT_FLAG_REPLACING_HELD     = 0x0008;
// 上屏后处于「无占位的联想会话」（assoc_placeholder = "none"）：宿主里没有组合，须在处理这条
// 应答时同步记成有输入会话，才赶得上下一次 OnTestKeyDown。清位见 KeyEventSink 的
// _EndAssocBareSession / SetCandidateSessionActive。
constexpr uint32_t COMMIT_FLAG_ASSOC_SESSION      = 0x0020;
// bit4 已废弃勿复用：它靠异步回填，赶不上 OnTestKeyDown 的同步判定；联想改用占位组合
// （见 handle_assoc.rs 的 ASSOC_COMPOSITION）。

// Commit request payload (for barrier mechanism)
// Sent from C++ to core when Space/Enter/number key is pressed during composition
struct CommitRequestPayload
{
    uint16_t barrierSeq;     // Barrier sequence number (for matching response)
    uint16_t triggerKey;     // VK code that triggered commit (VK_SPACE/VK_RETURN/0x31-0x39)
    uint32_t modifiers;      // Modifier state at trigger time
    uint32_t inputLength;    // Length of input buffer (UTF-8)
    // Followed by UTF-8 input buffer content
};
static_assert(sizeof(CommitRequestPayload) == 12, "CommitRequestPayload must be 12 bytes");

// Commit result payload (for barrier mechanism)
// Sent from core to C++ as response to COMMIT_REQUEST
struct CommitResultPayload
{
    uint16_t barrierSeq;        // Matching barrier sequence
    uint16_t flags;             // bit0: modeChanged, bit1: hasNewComposition, bit2: chineseMode
    uint32_t textLength;        // Length of commit text (UTF-8)
    uint32_t compositionLength; // Length of new composition (UTF-8, 0 if none)
    // Followed by UTF-8 commit text, then optional new composition
};
static_assert(sizeof(CommitResultPayload) == 12, "CommitResultPayload must be 12 bytes");

// Commit result flags (reuse COMMIT_FLAG_* for consistency)
// COMMIT_FLAG_MODE_CHANGED       = 0x0001
// COMMIT_FLAG_HAS_NEW_COMPOSITION = 0x0002
// COMMIT_FLAG_CHINESE_MODE       = 0x0004
// 不含 COMMIT_FLAG_REPLACING_HELD：barrier 路径是顶码提交，与智能符号 hold 预览态无交集。

// ============================================================================
// Host render shared memory structures
// ============================================================================

// Shared memory magic and version
constexpr uint32_t SHARED_RENDER_MAGIC   = 0x57494E44; // 'WIND'
constexpr uint32_t SHARED_RENDER_VERSION = 1;

// Shared memory flags
constexpr uint32_t SHARED_FLAG_VISIBLE       = 0x0001; // Window should be visible
constexpr uint32_t SHARED_FLAG_CONTENT_READY = 0x0002; // New content is ready to render
// 0x0004 = SOFTWARE_SHADOW, 0x0008 = ABSOLUTE_POS, 0x0010 = ANIM_FRAME: consumed only by the
// macOS / Linux hosts. Listed here so the bit space stays visible when adding a flag on the
// Windows side (see wind-ipc/src/protocol.rs SharedRenderHeader).
// 穿透装饰层（皮肤 hit = "pass"，见 theme-skin-animation.md §17.8）：帧在 passOffset 带第二层同尺寸
// 像素，画进 WS_EX_TRANSPARENT 的第二个窗口，点击直达宿主；BELOW = 排在候选窗下面。
// 判读见 HostPassLayerPolicy.h；只发给声明了 HOST_RENDER_CAP_PASS_LAYER 的 DLL。
constexpr uint32_t SHARED_FLAG_PASS_LAYER = 0x0020;
constexpr uint32_t SHARED_FLAG_PASS_BELOW = 0x0040;

// Max shared memory size (4MB, covers up to ~1024x1024 BGRA)
constexpr uint32_t MAX_SHARED_RENDER_SIZE = 4 * 1024 * 1024;

// Shared render header (64 bytes, at start of shared memory)
// Followed by BGRA pixel data
struct SharedRenderHeader
{
    uint32_t magic;      // SHARED_RENDER_MAGIC
    uint32_t version;    // SHARED_RENDER_VERSION
    uint32_t sequence;   // Monotonic, incremented each write by the service
    uint32_t flags;      // SHARED_FLAG_* bits
    int32_t  x;          // Screen X position
    int32_t  y;          // Screen Y position
    uint32_t width;      // Bitmap width in pixels
    uint32_t height;     // Bitmap height in pixels
    uint32_t stride;     // Bytes per row (width * 4)
    uint32_t dataSize;   // Total BGRA pixel data size in bytes
    // Mouse hit-test geometry, embedded so rects and the bitmap they describe always
    // share the same sequence (no cross-channel skew). The rect table is rectCount
    // HostRenderHitRect entries (20 bytes each) starting at byte rectsOffset from the
    // SHM base (right after the pixels). rectCount==0 ⇒ frame is non-interactive.
    uint32_t rectCount;  // Number of hit rects in the table (0 = none)
    uint32_t rectsOffset;// Byte offset from SHM base to the rect table
    // Candidate index the service highlighted in THIS frame (hover encoding: >=0 candidate,
    // -1 none, -2 page-up, -3 page-down). The host window resets its hover-dedup baseline
    // to this each frame, so re-hovering after a content change still re-highlights.
    int32_t  renderedHoverIndex;
    // The host-render client (bridge clientID) this frame is meant for. Instances in one
    // process share this SHM section but wait on their own events; a render thread renders
    // only when this matches its _instanceId, otherwise hides, so one band window shows it.
    uint32_t targetInstanceId;
    // Byte offset from SHM base to the pass-through decor layer (SHARED_FLAG_PASS_LAYER);
    // 0 = none. Same width/height/stride as the main pixels.
    uint32_t passOffset;
    uint32_t reserved;   // Padding to 64 bytes
};
static_assert(sizeof(SharedRenderHeader) == 64, "SharedRenderHeader must be 64 bytes");

// One candidate hit rect embedded after the pixels (panel-local pixel coords, mirrors
// the bitmap origin including shadow margin). index >= 0 is a page-local candidate;
// index -1 = page-up button, -2 = page-down button.
struct HostRenderHitRect
{
    int32_t index;
    int32_t x;
    int32_t y;
    int32_t w;
    int32_t h;
};
static_assert(sizeof(HostRenderHitRect) == 20, "HostRenderHitRect must be 20 bytes");

// Cap on the embedded rect table so a malformed count can never make the DLL read past
// the buffer.
constexpr uint32_t MAX_HOST_RENDER_RECTS = 256;

// ============================================================================
// Language bar icon shared memory (service pre-renders, GetIcon consumes)
// ============================================================================
//
// Mirrors wind-ipc/src/protocol.rs (IconShmHeader / IconVariant). No event or reader thread:
// GetIcon is a passive callback and reads whatever is current.
// Double-buffering + seqlock. Writer: fill the inactive slot, release fence, switch
// activeSlot, bump sequence LAST. Reader: read sequence, copy, re-read; equal = consistent.

constexpr uint32_t ICON_SHM_MAGIC   = 0x4F434957; // 'WICO' (bytes W,I,C,O)
// 与 Rust 侧同名常量逐字一致；版本不匹配时直接判失败显示「加载中」图标，不硬读错位数据。
constexpr uint32_t ICON_SHM_VERSION = 1;
constexpr uint32_t ICON_SHM_SIZE    = 128 * 1024;

// Theme tiers. The taskbar's own theme decides which one to pick; the DLL detects it
// locally (IsSystemDarkMode) rather than being told, so there is no stale window.
constexpr uint8_t ICON_THEME_LIGHT = 0;
constexpr uint8_t ICON_THEME_DARK  = 1;

// Upper bound on the variant table, purely so a corrupt count cannot make the DLL
// walk past the mapping. The real count comes from the header.
constexpr uint32_t MAX_ICON_VARIANTS = 64;

// Icon SHM header (64 bytes, at start of the mapping), followed by the variant table.
struct IconShmHeader
{
    uint32_t magic;        // ICON_SHM_MAGIC
    uint32_t version;      // ICON_SHM_VERSION
    // Bumped on every publish. 0 means "mapping created but nothing published yet" —
    // the DLL must fall back to local drawing rather than show a blank icon.
    uint32_t sequence;
    uint32_t activeSlot;   // 0 or 1
    uint32_t variantCount;
    uint32_t slotStride;   // Bytes per slot
    uint32_t slot0Offset;  // Byte offset from SHM base to slot 0
    uint32_t tableOffset;  // Byte offset from SHM base to the variant table
    uint32_t reserved[8];  // Padding to 64 bytes
};
static_assert(sizeof(IconShmHeader) == 64, "IconShmHeader must be 64 bytes");

// One pre-rendered variant. Both slots share this table; `offset` is relative to the
// start of whichever slot is active. Pixels are BGRA, NON-premultiplied (what
// CreateIconIndirect's hbmColor expects).
struct IconVariant
{
    uint16_t sizePx;   // 16 / 20 / 24 / 28 / 32
    uint8_t  theme;    // ICON_THEME_*
    uint8_t  flags;
    uint32_t offset;   // Relative to the active slot's start
    uint32_t byteLen;  // sizePx * sizePx * 4
    uint32_t reserved;
};
static_assert(sizeof(IconVariant) == 16, "IconVariant must be 16 bytes");

// Host window kind: identifies which host-rendered window an SHM channel / band window
// belongs to. Each kind has its own SHM section + per-PID event + band window, because
// candidate / tooltip / status can all be visible simultaneously.
enum HostWindowKind : uint32_t
{
    HOST_WINDOW_CANDIDATE = 0, // 候选框（含鼠标交互）
    HOST_WINDOW_TOOLTIP   = 1, // 候选悬停 tooltip（纯显示）
    HOST_WINDOW_STATUS    = 2, // 状态提示气泡（纯显示）
};
constexpr uint32_t HOST_WINDOW_KIND_COUNT = 3;

// Host render setup payload (from the service, response to CMD_HOST_RENDER_REQUEST).
// Wire format: instanceId(4) + entryCount(4) + entryCount × { HostRenderSetupEntryHeader
// + shmName + eventName }. instanceId is this connection's bridge clientID (per-instance
// identity shared by all its kinds); the DLL stamps it on every band window so the render
// thread can match it against SharedRenderHeader.targetInstanceId. One entry per active
// window kind; the DLL creates one band window per entry.
struct HostRenderSetupEntryHeader
{
    uint32_t windowKind;     // HostWindowKind
    uint32_t maxBufferSize;  // Maximum shared memory size for this channel
    uint32_t shmNameLen;     // Length of shared memory name (UTF-8)
    uint32_t eventNameLen;   // Length of event name (UTF-8)
    // Followed by: shmName (shmNameLen bytes) + eventName (eventNameLen bytes)
};
static_assert(sizeof(HostRenderSetupEntryHeader) == 16, "HostRenderSetupEntryHeader must be 16 bytes");

// Push pipe token handshake payload (client → server, 8 bytes written immediately after connecting)
// Token format: (uint64_t)GetCurrentProcessId() << 32 | per-process-instance-counter (uint32)
// 64-bit form avoids collisions when two processes share the low 16 bits of their PID
// (Windows 10/11 allocates PIDs that easily exceed 65535).
// Allows the service to build a precise token→push-handle mapping for multi-instance hosts (e.g. explorer).
struct PushTokenHandshake
{
    uint64_t clientToken;
};
static_assert(sizeof(PushTokenHandshake) == 8, "PushTokenHandshake must be 8 bytes");

// CMD_IME_ACTIVATED payload (8 bytes, carries client token)
struct IMEActivatedPayload
{
    uint64_t clientToken;
};
static_assert(sizeof(IMEActivatedPayload) == 8, "IMEActivatedPayload must be 8 bytes");

// CMD_IME_DEACTIVATED payload (8 bytes, same shape as IMEActivatedPayload).
// 带 token 让服务端只清对应实例的激活态：跨宿主切换时旧宿主的失焦晚于新宿主的
// focus_gained 到达，不区分实例会抹掉刚建立的激活。0 字节载荷按 token 0（旧行为）处理。
struct ClientTokenPayload
{
    uint64_t clientToken;
};
static_assert(sizeof(ClientTokenPayload) == 8, "ClientTokenPayload must be 8 bytes");

// ── CMD_FOCUS_LOST reason ──────────────────────────────────────────────────────
// TSF 里「失焦」是四件语义不同的事，服务端按 reason 分派三项独立后果（见 Rust FocusLostReason）：
//
//   reason            ime_active   has_edit_context   输入态
//   THREAD (0)        false        false              清       整个应用失去前台
//   DOC_CHANGED (1)   不动         不动               清       同宿主内换文档
//   CTX_LOST (2)      不动         false              不清     焦点离开可编辑控件
//   NO_EDIT_CTX (3)   不动         false              清       换到无可编辑控件的文档
//
// CTX_LOST 必须不清输入态：它来自 DocMgr 级失焦这一噪声层（会掉了又回），在那里清输入态会让
// 首字符不进编码直接上屏。NO_EDIT_CTX 的新文档无处可输，残留 buffer 必须清。
// 载荷 < 9 字节时服务端取 reason=THREAD。
constexpr uint8_t FOCUS_LOST_REASON_THREAD      = 0;
constexpr uint8_t FOCUS_LOST_REASON_DOC_CHANGED = 1;
constexpr uint8_t FOCUS_LOST_REASON_CTX_LOST    = 2;
constexpr uint8_t FOCUS_LOST_REASON_NO_EDIT_CTX = 3;

// CMD_FOCUS_LOST payload (9 bytes)。在 #pragma pack(push,1) 区内，故 uint8 紧跟 uint64。
struct FocusLostPayload
{
    uint64_t clientToken;
    uint8_t  reason;
};
static_assert(sizeof(FocusLostPayload) == 9, "FocusLostPayload must be 9 bytes");

// CMD_FOCUS_GAINED extended payload (39 bytes = CaretPayload + clientToken + inputScopeMask
// + disabled + reason + caretSource)
struct FocusGainedPayload
{
    CaretPayload caret;          // 20 bytes: caret position
    uint64_t     clientToken;    // 8 bytes: per-instance token
    // 焦点控件的 TSF InputScope 位图：bit N 表示 InputScope 枚举值 N 存在（如 IS_PASSWORD=31），
    // 值 < 0 或 >= 64 忽略；0 表示未知/默认。服务端据此做密码框强制英文等决策。
    uint64_t     inputScopeMask; // 8 bytes: InputScope bitmask
    // 焦点控件是否被判定为禁用中文及原因，随 focus_gained 上报，免得诊断 HUD 等单独的状态上报。
    uint8_t      disabled;       // 1 byte: 0/1 - GUID_COMPARTMENT_KEYBOARD_DISABLED 命中
    // reason: 0 None / 1 CompartmentDisabled / 2 InputScopePassword / 3 NumericPassword
    uint8_t      reason;         // 1 byte
    // 上面那个 caret 的来源（CARET_SRC_*）。焦点气泡锚在这个坐标上，而 OnSetFocus 里同步 edit
    // session 必被拒，回退链可能交出跨窗口的 Win32 光标，消费端要靠来源分辨。
    // 尾部追加；服务端按长度分支，< 39 字节视为 UNKNOWN。
    uint8_t      caretSource;    // 1 byte: CARET_SRC_*
    //
    // 本结构之后还有前后相接的变长段（由 SendFocusGained 手工拼接）：
    //   [本结构 39 字节][bundleIdLen:u32][bundleId][windowClassLen:u32][windowClass]
    //   [windowTitleLen:u32][windowTitle]
    // ① bundleId     macOS 专属，Windows 发 len=0 占位，让两平台共用同一条线性走法。
    // ② windowClass  焦点顶层窗口类名，用来区分 explorer.exe 的过渡型窗口（任务栏 / Alt+Tab）
    //                与停留型窗口（桌面 / 文件管理器）。
    // ③ windowTitle  同一窗口标题（截断到 256 个 UTF-16 码元），供 compat.toml 的 title 条件；
    //                只在 CONFIG_KEY_COMPAT_TITLE_MATCH 推开时才取，否则 len=0。日志只记长度，
    //                见 WindowTitlePolicy.h。
    // 新段一律接在最后并同步 Rust 侧 FocusGainedPayload 的布局图：逐段兼容的解码错位时不报错，
    // 只解出垃圾。
};
static_assert(sizeof(FocusGainedPayload) == 39, "FocusGainedPayload must be 39 bytes");

// CMD_INPUT_STATE_REPORT payload (14 bytes). Sent standalone (not tied to a focus_gained)
// when the disabled/reason state changes for the currently focused control, e.g. a
// compartment flip without a new OnSetFocus (SPA navigating into a password field).
// Field order/meaning mirrors Rust InputStateReportPayload exactly.
struct InputStateReportPayload
{
    uint32_t pid;             // 4 bytes LE, offset 0..4: GetCurrentProcessId() of the host process
    uint8_t  disabled;        // 1 byte, offset 4
    uint8_t  reason;          // 1 byte, offset 5
    uint64_t inputScopeMask;  // 8 bytes LE, offset 6..14
};
static_assert(sizeof(InputStateReportPayload) == 14, "InputStateReportPayload must be 14 bytes");

// CMD_KEY_OBSERVED payload (8 bytes). Mirrors Rust KeyObservedPayload.
struct KeyObservedPayload
{
    uint32_t keyCode;   // 4 bytes LE, offset 0..4: virtual key code
    uint32_t modifiers; // 4 bytes LE, offset 4..8: KEYMOD_* (same bits as KeyPayload)
};
static_assert(sizeof(KeyObservedPayload) == 8, "KeyObservedPayload must be 8 bytes");

// UIElement state (C++ -> core, async). flags 位定义同 Rust UIELEMENT_FLAG_*。
constexpr uint32_t UIELEMENT_FLAG_HOST_DRAWS     = 0x0001; // 宿主接管绘制（pbShow=FALSE / Show(FALSE)）
constexpr uint32_t UIELEMENT_FLAG_UI_LESS_THREAD = 0x0002; // 线程以 TF_TMAE_UIELEMENTENABLEDONLY 激活
// 宿主没声明接管（pbShow=TRUE）却实际来读候选串（GetString），已知读取者是 CUAS 的 IMM32 桥，
// 结果是屏幕上两个候选框。与声明位分开报：声明是事实、读取是推断，core 要分别记账。
constexpr uint32_t UIELEMENT_FLAG_HOST_READS     = 0x0004; // 宿主实际读取了候选串
struct UiElementStatePayload
{
    uint32_t pid;   // GetCurrentProcessId()
    uint32_t flags; // UIELEMENT_FLAG_*
};
static_assert(sizeof(UiElementStatePayload) == 8, "UiElementStatePayload must be 8 bytes");

// UIElement action (C++ -> core, async). action 取值同 Rust UIELEMENT_ACTION_*。
constexpr uint32_t UIELEMENT_ACTION_SET_SELECTION = 1; // arg = 页内下标（快照只带当页）
constexpr uint32_t UIELEMENT_ACTION_FINALIZE      = 2; // 定稿当前高亮
constexpr uint32_t UIELEMENT_ACTION_ABORT         = 3; // 放弃会话（Esc）
constexpr uint32_t UIELEMENT_ACTION_SET_PAGE      = 4; // arg = 页号（0 起）
struct UiElementActionPayload
{
    uint32_t action;
    uint32_t arg;
};
static_assert(sizeof(UiElementActionPayload) == 8, "UiElementActionPayload must be 8 bytes");

// UIElement page (core -> C++, CMD_UIELEMENT_PAGE)。线上格式（全部 LE）：
//   selected u32 + pageSize u32 + currentPage u32 + count u32 + count × { len u16 + UTF-8 }
// 定长头 16 字节；候选为当页候选，count 即宿主眼中的列表长度（GetCount），页数恒 1、selected 为页内下标
// （与 Weasel / 微软拼音同形；有的宿主会把整条列表画出来）。
constexpr size_t UIELEMENT_PAGE_HEADER_SIZE = 16;

// ── 焦点窗口句柄的来源域（DiagSnapshotHeader::focusHwndSource）────────────────
// 三条通路给出的不是同一件东西，须标来源（同 CARET_SRC_*）：FOREGROUND 域的窗口可能根本不属于
// 本进程（如任务栏搜索），拿它推 per-app 判据必然推错。
constexpr uint8_t WND_SRC_NONE       = 0; // 三条通路都没拿到
constexpr uint8_t WND_SRC_TSF_VIEW   = 1; // ITfContextView::GetWnd()——TSF 域，最准
constexpr uint8_t WND_SRC_GUI_THREAD = 2; // GetGUIThreadInfo().hwndFocus——线程域
constexpr uint8_t WND_SRC_FOREGROUND = 3; // GetForegroundWindow()——跨进程，兜底

// DiagSnapshotHeader::flags 位：本次焦点相对上一次换了 DocMgr。
// 只有 DLL 知道这件事（它持有 _pLastActiveDocMgr），core 无从推导，故必须随包上报。
constexpr uint8_t DIAG_FLAG_DOCMGR_CHANGED = 0x01;

// CMD_DIAG_SNAPSHOT 定长头（64 bytes），字段偏移必须与 Rust DiagSnapshotPayload 一致：两侧都是
// 手写序列化，只有这里的 static_assert 与 Rust 侧 diag_snapshot_head_layout_is_frozen 把关。
// 句柄一律零扩展为 uint64，32/64 位 DLL 偏移才固定；只用于展示与同一性比较，服务进程不拿它调 Win32。
struct DiagSnapshotHeader
{
    uint32_t pid;              // offset 0:  GetCurrentProcessId()
    uint32_t fgPid;            // offset 4:  前台窗口所属进程（≠pid 即"焦点与前台分属两进程"）
    uint64_t focusHwnd;        // offset 8:  焦点窗口，来源见 focusHwndSource
    uint64_t rootHwnd;         // offset 16: GetAncestor(GA_ROOT)——per-app 窗口级判据取它
    uint64_t fgHwnd;           // offset 24: GetForegroundWindow()
    uint64_t docMgrId;         // offset 32: 焦点 ITfDocumentMgr 指针（仅作实例标识）
    uint64_t contextId;        // offset 40: 焦点 ITfContext 指针（仅作实例标识）
    uint32_t focusSessionId;   // offset 48: _focusSessionId 低 32 位（与日志对齐）
    uint32_t rootBand;         // offset 52: 顶层窗口 z-band（GetWindowBand，取不到为 0）
    uint32_t hostBand;         // offset 56: host-render band 窗口当前 band（0=未建）
    uint8_t  focusHwndSource;  // offset 60: WND_SRC_*
    uint8_t  flags;            // offset 61: DIAG_FLAG_*
    uint16_t reserved;         // offset 62: 留零。加字段优先吃它，别再动偏移量
};
static_assert(sizeof(DiagSnapshotHeader) == 64, "DiagSnapshotHeader must be 64 bytes");

// Input stats payload (from C++ to core, async)
// Counts of characters typed in English mode (not intercepted by core)
struct InputStatsPayload
{
    uint32_t englishChars;    // English letter count (a-z, A-Z)
    uint32_t englishDigits;   // Digit count (0-9)
    uint32_t englishPuncts;   // Punctuation/symbol count
    uint32_t englishSpaces;   // Space count
    uint32_t elapsedMs;        // Milliseconds covered by this batch
};
static_assert(sizeof(InputStatsPayload) == 20, "InputStatsPayload must be 20 bytes");

#pragma pack(pop)

// ============================================================================
// Helper functions
// ============================================================================

// Config sync keys (must match wind-ipc protocol.rs)
constexpr const char* CONFIG_KEY_ENGLISH_PAIRS = "en_pairs";
constexpr const char* CONFIG_KEY_JUMP_OUT_KEYS = "jump_out_keys";
constexpr const char* CONFIG_KEY_STATS = "stats";
// 密码框强制英文抑制的策略开关（会话级，右键菜单「高级」可关）。DLL 需要它才能在
// OnTestKeyDown 本地判定是否放行——吃键决策发生在 IPC 之前，仅靠 core 回 PassThrough
// 已经太晚（会形成「吃了再吐」丢键）。
constexpr const char* CONFIG_KEY_PASSWORD_SUPPRESS = "password_suppress";
// Ctrl+空格 中英切换开关（keys.ctrl_space_toggle 的有效值）。格式：enabled(u8)，默认开。
// 关闭时 OnTestKeyDown 不吃 Ctrl+空格、OnKeyDown 不兜底切换。判据见 CtrlSpacePolicy.h。
constexpr const char* CONFIG_KEY_CTRL_SPACE_TOGGLE = "ctrl_space_toggle";
// 诊断快照采集开关（会话级，随输入诊断 HUD 显隐）。格式：enabled(u8)。默认关。
// 采集要查三次窗口类名 + band，属于「只有排查时才值得付」的开销。
constexpr const char* CONFIG_KEY_DIAG_SNAPSHOT = "diag_snapshot";
// 组合区空文本兜底占位字符。格式：kind(u8)，0 = 空格、1 = ZWSP（U+200B）、2 = 盲文空白（U+2800），
// 认不出的值回落空格。服务端按客户端 compat 规则 composition_placeholder 现算推送，DLL 只照做
// （三种字符都认作占位，见 CompositionPlaceholder.h）。
constexpr const char* CONFIG_KEY_COMPOSITION_PLACEHOLDER = "composition_placeholder";
// 中数间补空格（input.spacing.cjk_digit 且本应用 compat 未关）：DLL 据此把汉字后的数字吃下交给
// core 出「空格 + 数字」。值 enabled(u8)，按客户端现算，推送时机同 composition_placeholder。
constexpr const char* CONFIG_KEY_SPACE_BEFORE_DIGIT = "space_before_digit";
// 含换行的上屏是否改走剪贴板粘贴。格式：mode(u8)，0 = commit（默认）、1 = paste；认不出的值按 commit。
// 服务端按本客户端 pid 的 compat 规则 multiline_commit 现算后逐客户端推送，见 MultilineCommit.h。
constexpr const char* CONFIG_KEY_MULTILINE_COMMIT = "multiline_commit";
// 「本进程可能命中 compat 标题规则」开关（采不采窗口标题）。格式：enabled(u8)。默认关，重连从关起步，
// core 握手必推、compat 重载后广播。关着时 OnSetFocus 不取标题（标题是用户数据）。
constexpr const char* CONFIG_KEY_COMPAT_TITLE_MATCH = "compat_title_match";
// 格式：enabled(u8)。默认关，重连从关起步，core 握手必推、扩展授权变化时广播。
constexpr const char* CONFIG_KEY_EXTENSION_KEY_OBSERVE = "extension_key_observe";
// 「英文半角列有自定义标点映射」的源字符集合。英文半角下 DLL 默认透传标点键，core 收不到，
// 据此集合精确吃下这些键转发给 core。为空（默认）= 不额外吃。格式：count(u8) + [ch:u16(LE)]...
constexpr const char* CONFIG_KEY_CUSTOM_EN_PUNCT = "custom_en_punct";
// 中文模式下产物就是原样半角 ASCII、DLL 该直接透传的标点集合（与上一个键方向相反）。
// 吃下再原样吐回时，CUAS 桥接的宿主会把字符码当虚拟键码解释（如 `#`→VK_END），见
// docs/architecture/smart-symbol-compat-notes.md「产物不变的标点要透传」。
// 判据须与 core 的 wind_punct::chinese_passthrough_punct_chars 同源。
// 为空（默认）= 不透传。格式：count(u8) + [ch:u16(LE)]...（同上一个键）
constexpr const char* CONFIG_KEY_CN_PASSTHROUGH_PUNCT = "cn_passthrough_punct";
// 同上，用于英文标点态（中文输入模式 + 标点切英文），格式相同。须是独立的集合：英文标点态
// 不走中文标点表，产物原样的键更多，且两态的智能符号判据来源不同。
constexpr const char* CONFIG_KEY_EN_PASSTHROUGH_PUNCT = "en_passthrough_punct";
// 自定义标点「空格」行配了值的列（位掩码，第 i 位 = 列 i：0=中半 1=英全 2=中全 3=英半）。
// 中文模式空缓冲时据此在半角态也吃下空格，否则按 OnTestKeyDown 结论走的宿主收不到映射。
// 0（默认）= 不吃。格式：mask(u8)
constexpr const char* CONFIG_KEY_SPACE_CUSTOM_COLS = "space_custom_cols";
// 配对状态时效（秒，0=不过期）。格式：secs(u16 LE)。
// 吃键闸门（_pairPendingDepth）在 DLL，故陈旧判据也须在本地：只有 core 过期而这边照吃跳出键，
// core 再回 PassThrough 就是「吃了再吐」，会丢键。
constexpr const char* CONFIG_KEY_PAIR_STATE_TTL = "pair_state_ttl";
// 配对插入提交方式（直写 vs 按键注入）。格式：enabled(u8)。
// 开启后 InsertTextWithCursor 与空闲态 ASCII 提交走 InjectUnicodeText（VK_PACKET 注入）替代 TSF 直写，
// 因为有的宿主（AutoCAD 表格在位编辑器）丢弃直写的 ASCII 却报 S_OK，见 pair-commit-host-compat-notes.md。
// 按客户端现算（compat pair_via_comp 覆盖全局 commit_via_composition），重连从关起步。
constexpr const char* CONFIG_KEY_PAIR_VIA_COMP = "pair_via_comp";
// 语言栏按钮的悬停提示文本。格式：[ch:u16(LE)]...（UTF-16LE，无长度前缀，value 即整段）。
// 文案全在服务端，DLL 只原样返回：本地判不出「密码框」「已禁用」等成因，图标又只能表达「不可用」。
constexpr const char* CONFIG_KEY_LANGBAR_TOOLTIP = "langbar_tooltip";

// Calculate key hash for hotkey matching
// Format: (modifiers << 16) | keyCode
inline uint32_t CalcKeyHash(uint32_t modifiers, uint32_t keyCode)
{
    return (modifiers << 16) | (keyCode & 0xFFFF);
}

// Parse key hash to extract modifiers and keyCode
inline void ParseKeyHash(uint32_t hash, uint32_t& modifiers, uint32_t& keyCode)
{
    modifiers = hash >> 16;
    keyCode = hash & 0xFFFF;
}

// Get current modifier state from keyboard
inline uint32_t GetCurrentModifiers()
{
    uint32_t mods = 0;

    // Check generic modifiers
    if (GetAsyncKeyState(VK_SHIFT) < 0)   mods |= KEYMOD_SHIFT;
    if (GetAsyncKeyState(VK_CONTROL) < 0) mods |= KEYMOD_CTRL;
    if (GetAsyncKeyState(VK_MENU) < 0)    mods |= KEYMOD_ALT;
    if (GetAsyncKeyState(VK_LWIN) < 0 || GetAsyncKeyState(VK_RWIN) < 0) mods |= KEYMOD_WIN;

    // Check specific left/right modifiers
    if (GetAsyncKeyState(VK_LSHIFT) < 0)   mods |= KEYMOD_LSHIFT;
    if (GetAsyncKeyState(VK_RSHIFT) < 0)   mods |= KEYMOD_RSHIFT;
    if (GetAsyncKeyState(VK_LCONTROL) < 0) mods |= KEYMOD_LCTRL;
    if (GetAsyncKeyState(VK_RCONTROL) < 0) mods |= KEYMOD_RCTRL;

    return mods;
}

// ============================================================================
// Parsed response structures (high-level, after decoding)
// ============================================================================

struct ParsedResponse
{
    ResponseType type = ResponseType::Error;

    // For CommitText
    std::wstring commitText;
    std::wstring newComposition;
    bool modeChanged = false;
    bool chineseMode = false;

    // For UpdateComposition
    std::wstring composition;
    int caretPos = 0;
    int cursorOffset = 0;  // For InsertTextWithCursor: chars to move left from end

    // For StatusUpdate
    uint32_t statusFlags = 0;

    // Icon label for taskbar display (from the service, e.g., "中", "英", "A", "拼", "五")
    std::wstring iconLabel;

    // For SyncHotkeys / StatusUpdate
    std::vector<uint32_t> keyDownHotkeys;
    std::vector<uint32_t> keyUpHotkeys;

    // For HoldComposition
    uint32_t holdTimeoutMs = 0;

    // Helper methods
    bool IsChineseMode() const { return (statusFlags & STATUS_CHINESE_MODE) != 0; }
    bool IsFullWidth() const { return (statusFlags & STATUS_FULL_WIDTH) != 0; }
    bool IsChinesePunct() const { return (statusFlags & STATUS_CHINESE_PUNCT) != 0; }
    bool IsToolbarVisible() const { return (statusFlags & STATUS_TOOLBAR_VISIBLE) != 0; }
    bool IsCapsLock() const { return (statusFlags & STATUS_CAPS_LOCK) != 0; }
    bool IsSoftKeyboard() const { return (statusFlags & STATUS_SOFT_KEYBOARD) != 0; }
    bool IsSoftKeyboardKeys() const { return (statusFlags & STATUS_SOFT_KEYBOARD_KEYS) != 0; }
    bool IsHotkeySession() const { return (statusFlags & STATUS_HOTKEY_SESSION) != 0; }
};
