#pragma once

// 跨语言协议同步（必读）：本文件与 Go 端 wind_input/internal/ipc/binary_protocol.go 互为镜像。
// 修改命令码、Header 字段、Payload 结构、状态标志位时，必须同步修改：
//   - wind_input/internal/ipc/binary_protocol.go（常量与结构体定义）
//   - wind_input/internal/ipc/binary_codec.go（编解码实现）
// 否则会破坏 C++ TSF DLL 与 Go 服务的 IPC 兼容性。

#include <cstdint>
#include <vector>
#include <string>

// Protocol version (major.minor: high 4 bits = major, low 12 bits = minor)
constexpr uint16_t PROTOCOL_VERSION = 0x1001; // v1.1 - Added barrier mechanism and state machine support

// Async flag (used in version field's high bit to mark async requests)
constexpr uint16_t ASYNC_FLAG = 0x8000; // Async request flag - no response expected

// ============================================================================
// Upstream commands (C++ -> Go)
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
constexpr uint16_t CMD_CANDIDATE_SCROLL      = 0x0211; // Host render: mouse wheel over candidate box (payload: delta i32, WHEEL_DELTA multiple, >0 = up).
// The service maps this to "move the highlight up/down" (crossing to the adjacent page at a
// page boundary) — same handler on both platforms, see Coordinator::handle_candidate_scroll.
constexpr uint16_t CMD_HOST_RENDER_FAILED    = 0x0212; // Host render: band window creation failed in DLL (async, payload: reason u32). Go logs + notifies user the candidate fell back to its local window
constexpr uint32_t HOST_RENDER_FAIL_WINDOW_CREATE = 1; // CreateWindowInBand failed at target band and band=0
// Input state report (C++ -> Go, async): compartment-driven disabled/reason change outside
// of a fresh focus_gained (e.g. GUID_COMPARTMENT_KEYBOARD_DISABLED flips while focus stays
// on the same control, such as clicking a password field inside the same web page).
// Payload: InputStateReportPayload (14 bytes). Rust side: InputStateReportPayload (Task 2).
constexpr uint16_t CMD_INPUT_STATE_REPORT    = 0x0213;
// Diagnostics snapshot (C++ -> core, async): focus window chain + foreground window + TSF
// context instance ids. Payload: DiagSnapshotHeader (64 bytes) followed by three
// length-prefixed UTF-8 class names (focus / root / foreground), each `len u16 LE + bytes`.
//
// 刻意**不并入** CMD_FOCUS_GAINED：那条路径是宿主 UI 线程上的同步 IPC 往返（见
// TextService.cpp 的 focusIpcT0 计时），首字延迟直接挂在它身上。本命令要做三次窗口类名
// 查询 + band 查询，塞进去等于给每次焦点切换加固定开销。故独立成命令、异步发送，且由
// CONFIG_KEY_DIAG_SNAPSHOT 门控——HUD 关闭时一次都不采集。
constexpr uint16_t CMD_DIAG_SNAPSHOT         = 0x0214;
constexpr uint16_t CMD_COMPOSITION_TERMINATED = 0x0209; // Composition unexpectedly terminated (e.g., user clicked in input field)
constexpr uint16_t CMD_CARET_UPDATE     = 0x0301; // Caret position update
constexpr uint16_t CMD_SELECTION_CHANGED = 0x0302; // Selection/caret changed without composition (from ITfTextEditSink)
constexpr uint16_t CMD_CARET_PENDING    = 0x0303; // First-show handshake: composition just started, real caret coming after reflow
constexpr uint16_t CMD_CARET_PROBE      = 0x0304; // First-show probe: one pre-reflow caret sample per OnLayoutChange burst iteration.
// DLL 只上报、不做判断：首帧 reflow 期间宿主可能连续多次 layout change，前几次 GetTextExt
// 仍返回旧坐标（实测 WPS 前两次是上一轮的值，EverEdit 第一次就已正确）。哪一帧可信由服务端
// 按策略判定，故这里不筛不等，纯采样上报——策略留在能读 compat.toml 的那一侧。
// Generic extension envelope (same code point both directions, distinguished by
// direction like the rest of this file). Layout:
//   kindLen u32 + kind(UTF-8) + bodyLen u32 + body(opaque bytes, usually JSON)
//
// Two-tier rule for NEW messages (see wind-ipc/src/protocol.rs CMD_EXT for the full
// rationale). The test is whether the message repeats within one continuous typing run:
//   - high frequency (per key / per frame / per mouse move) -> dedicated code point,
//     fixed or length-prefixed binary layout;
//   - low frequency (menu actions, position reports, diagnostics, deep links)
//     -> this envelope; adding a feature means adding a `kind` string, not a constant.
//
// Unknown `kind` MUST be ignored silently (log at debug, never error, never drop the
// connection) -- that is what lets old and new peers interoperate.
//
// Currently unused on the Windows side; declared here so the three copies of this
// protocol (Rust / C++ / Swift) stay in step.
constexpr uint16_t CMD_EXT              = 0x0E01;

constexpr uint16_t CMD_BATCH_EVENTS     = 0x0F01; // Batch events container
constexpr uint16_t CMD_INPUT_STATS      = 0x0F03; // Input stats report (async, from English mode)

// ============================================================================
// Downstream commands (Go -> C++)
// ============================================================================
constexpr uint16_t CMD_ACK                = 0x0001; // Simple acknowledgment
constexpr uint16_t CMD_PASS_THROUGH       = 0x0002; // Key not handled, pass to system
constexpr uint16_t CMD_COMMIT_TEXT        = 0x0101; // Commit text
constexpr uint16_t CMD_UPDATE_COMPOSITION = 0x0102; // Update composition
constexpr uint16_t CMD_CLEAR_COMPOSITION  = 0x0103; // Clear composition
// 收掉组合，并把**当前正在处理的这个键**交还宿主（联想态回车/退格透传）。无载荷。
// 不能靠「ClearComposition + pfEaten=FALSE」凑：OnTestKeyDown 已按「有会话」吃了这个键，
// 在 OnKeyDown 吐成 FALSE 就是「吃了再吐」翻转，不补发 WM_KEYDOWN 的宿主会直接丢键。
// 走 hold / 配对跳出那条已验证的路：吃掉原键 + SendInput 重放。
constexpr uint16_t CMD_CLEAR_THEN_PASS_THROUGH = 0x010D;
constexpr uint16_t CMD_COMMIT_RESULT      = 0x0105; // Commit result (response to COMMIT_REQUEST)
// 0x0201 (CMD_MODE_CHANGED) removed: 所有模式切换响应统一走 CMD_STATUS_UPDATE
constexpr uint16_t CMD_STATUS_UPDATE      = 0x0202; // Full status update
constexpr uint16_t CMD_STATE_PUSH         = 0x0206; // State push (broadcast to all clients, hotkeys-less)
constexpr uint16_t CMD_SERVICE_READY      = 0x0207; // Go service connected push pipe, TSF should sync state
// CMD_ACTIVATION_STATUS_PUSH 是 CMD_IME_ACTIVATED / CMD_FOCUS_GAINED 异步化后的「状态回包」：
// Go 端 bridge handler 立即对原同步命令回 ACK 解除宿主 UI 线程同步等待，HandleIMEActivated /
// HandleFocusGained 在 ACK 之后才执行；完成后通过 push pipe 推送本命令。载荷格式与
// CMD_STATUS_UPDATE 一致（含 hotkeys + hostRenderAvail + iconLabel）。
// AsyncReader 收到后 Post 到 TSF 线程做 _SyncStateFromResponse + _EnsureHostRenderSetup。
// 区别于 CMD_STATE_PUSH：本命令是 activation 握手的回包，必须携带完整状态；
// CMD_STATE_PUSH 是状态变更广播，hotkeys 不变所以不带。
constexpr uint16_t CMD_ACTIVATION_STATUS_PUSH = 0x020C;
// CMD_MODE_PUSH：FocusGained 同步路径的轻量模式预推送（仅 chineseMode+fullWidth）。
// 载荷：4 字节 flags（位定义同 STATUS_CHINESE_MODE/STATUS_FULL_WIDTH）。
// DLL 侧仅 InterlockedExchange _bChineseMode/_bFullWidth，不调用 _SyncStateFromResponse，不影响热键白名单。
constexpr uint16_t CMD_MODE_PUSH              = 0x020D;
// CMD_SHELL_EXEC：在 TSF 侧（前台应用进程）执行 ShellExecuteW，解决 Service 进程无前台权限问题。
// 载荷：target_len(u32 LE) + target(UTF-8) + params_len(u32 LE) + params(UTF-8)
constexpr uint16_t CMD_SHELL_EXEC             = 0x020E;
// CMD_REFRESH_ICON：只让语言栏重取图标，**无载荷**。
// GetIcon 是被动回调，服务端写完共享内存后 DLL 不会自己察觉，须由 OnUpdate(TF_LBI_ICON)
// 让系统再取一次。此前这件事寄生在状态推送上，而 UpdateFullStatus 的 needUpdate 去重会挡掉
// 「状态没变、只有位图变了」的情形（调试菜单改形状、演示动画都属此类）。
// 不带载荷是刻意的：图标内容的唯一真相在 SHM 里，载荷里再放一份就是第二条真相通路。
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
// Host render commands (C++ -> Go)
// ============================================================================
constexpr uint16_t CMD_HOST_RENDER_REQUEST = 0x0501; // DLL requests host render setup

// ============================================================================
// Key event types
// ============================================================================
constexpr uint8_t KEY_EVENT_DOWN = 0;
constexpr uint8_t KEY_EVENT_UP   = 1;

// ============================================================================
// Toggle key state flags (for KeyPayload.toggles)
// ============================================================================
constexpr uint8_t TOGGLE_CAPSLOCK   = 0x01; // CapsLock is on
constexpr uint8_t TOGGLE_NUMLOCK    = 0x02; // NumLock is on
constexpr uint8_t TOGGLE_SCROLLLOCK = 0x04; // ScrollLock is on

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
// 软键盘面板开着。吃键判定要用：中文模式无 input session 时数字键本是交还宿主的
// （session_select_or_page 那支只在有 session 时吃），软键盘的数字行需要它们被吃下来。
// 位值必须与 wind-ipc protocol.rs 的 STATUS_SOFT_KEYBOARD 一致。
constexpr uint32_t STATUS_SOFT_KEYBOARD    = 0x0080; // Soft keyboard panel is open
// 当前面是**键盘面**（send_keys），不是符号面。键盘面把按键交还输入法：字母/数字/标点
// 一律落回常规判定链，只有 Esc 与翻页仍归面板。位值必须与 protocol.rs 的
// STATUS_SOFT_KEYBOARD_KEYS 一致。
constexpr uint32_t STATUS_SOFT_KEYBOARD_KEYS = 0x0100; // Current page sends keys, not symbols

// ============================================================================
// Protocol structures (must match Go side exactly)
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

// Key event payload (18 bytes)
struct KeyPayload
{
    uint32_t keyCode;     // Virtual key code
    uint32_t scanCode;    // Scan code
    uint32_t modifiers;   // Modifier flags (snapshot at event time, from state machine)
    uint8_t  eventType;   // 0=KeyDown, 1=KeyUp
    uint8_t  toggles;     // Toggle key states (CapsLock/NumLock/ScrollLock)
    uint16_t eventSeq;    // Monotonic event sequence number
    uint16_t prevChar;    // Character before caret (from ITfTextEditSink cache, 0 if unavailable)
};
static_assert(sizeof(KeyPayload) == 18, "KeyPayload must be 18 bytes");

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
// 同一组 x/y/h 可能来自语义完全不同的通道，压进同一个字段后下游就再也分不开了。
// 曾因此让 GetGUIThreadInfo 的 Win32 光标冒充 TSF 插入点，在 Word 非正文行错位 814px、
// 在桌面输入定位到任务栏。消费端据此决定「这一帧能否当权威坐标」以及「能否与组合起点比较」。
constexpr int32_t CARET_SRC_UNKNOWN        = 0; // 旧协议 / macOS 短包，无法判定
constexpr int32_t CARET_SRC_TSF_SELECTION  = 1; // GetTextExt(selection)，最精确
constexpr int32_t CARET_SRC_TSF_COMPOSITION= 2; // selection 退化后降级用的组合起点，仍属 TSF 域
constexpr int32_t CARET_SRC_TSF_CACHED     = 3; // UpdateComposition edit session 内缓存，同属 TSF 域
constexpr int32_t CARET_SRC_GUI_CARET      = 4; // GetGUIThreadInfo/GetCaretPos 回退——**跨窗口，不可作权威**
constexpr int32_t CARET_SRC_CONSOLE        = 5; // 控制台窗口的估算位置
constexpr int32_t CARET_SRC_LAST_KNOWN     = 6; // 上次已知好值

// Caret position payload v2 (24 bytes) = CaretPayload + source
//
// ⚠ 刻意**不把 source 加进 CaretPayload 本身**：FocusGainedPayload 内嵌了 CaretPayload，
// 改它的大小会连带改变焦点载荷布局，新旧两侧混用时 focus_gained 会整体错位。
// 焦点通道改为在 FocusGainedPayload **尾部**单独追加一个字节（见 caretSource），
// 尾部追加不动任何既有字段偏移，与本结构「另起一个 v2」是同一条兼容策略的两种写法。
//
// 兼容：服务端按 payload 长度分支——20 字节按旧格式解析且 source=UNKNOWN，24 字节读 source。
// macOS .app 仍发 12 字节，同样落入 UNKNOWN。故新旧两侧可任意组合。
struct CaretPayloadV2
{
    CaretPayload caret;
    int32_t      source;
};
static_assert(sizeof(CaretPayloadV2) == 24, "CaretPayloadV2 must be 24 bytes");

// Selection changed payload (4 bytes) - sent from ITfTextEditSink::OnEndEdit
// Notifies Go that the caret moved outside of composition (e.g., mouse click)
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
    // 向右移动的格数（合成几次 VK_RIGHT）。
    // 原为 direction（恒 1 且从未被读）；直通 ime.pair 的多字符右段要越过不止一格，
    // 才需要它真的携带信息。0 视同 1——旧版 core 与新版 DLL 混搭时不该退化成「跳出没反应」。
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
// bit4 曾用于「上屏后候选窗仍有内容」（联想态保住 _hasCandidates），**已废弃勿复用**：
// 那条路要靠服务端应答异步回填标志，赢不了下一次 OnTestKeyDown 的同步判定
// （真机日志同一行里 composing=0 candidates=1 inputSession=0）。联想改为挂一个占位组合，
// 走 HasActiveComposition() 这条同步判据。见 handle_assoc.rs 的 ASSOC_COMPOSITION。

// Commit request payload (for barrier mechanism)
// Sent from C++ to Go when Space/Enter/number key is pressed during composition
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
// Sent from Go to C++ as response to COMMIT_REQUEST
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
// 0x0004 = SOFTWARE_SHADOW, 0x0008 = ABSOLUTE_POS: both are consumed only by the macOS
// `.app` host, which renders panels itself. Listed here so the bit space stays visible
// when adding a flag on the Windows side (see wind-ipc/src/protocol.rs SharedRenderHeader).

// Max shared memory size (4MB, covers up to ~1024x1024 BGRA)
constexpr uint32_t MAX_SHARED_RENDER_SIZE = 4 * 1024 * 1024;

// Shared render header (64 bytes, at start of shared memory)
// Followed by BGRA pixel data
struct SharedRenderHeader
{
    uint32_t magic;      // SHARED_RENDER_MAGIC
    uint32_t version;    // SHARED_RENDER_VERSION
    uint32_t sequence;   // Monotonic, incremented each write by Go
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
    // Candidate index Go actually highlighted in THIS frame (hover encoding: >=0 candidate,
    // -1 none, -2 page-up, -3 page-down). The host window syncs its hover-dedup baseline
    // (_lastHoverIndex) to this each frame, so re-hovering the same index after a content
    // change (typing) still re-highlights instead of being deduped against a stale value.
    int32_t  renderedHoverIndex;
    // The host-render client (bridge clientID) this frame is meant for. Multiple
    // TextService instances in one process (e.g. two Notepad windows = same PID) share
    // this one global SHM section but each waits on its own event; Go signals them all and
    // stamps the active instance here. A render thread renders only when this matches its
    // own _instanceId, otherwise it hides — so exactly one band window shows the frame.
    uint32_t targetInstanceId;
    uint32_t reserved[2];// Padding to 64 bytes
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

// Cap on the embedded rect table (matches Go ipc.MaxHostRenderRects) so a malformed
// count can never make the DLL read past the buffer.
constexpr uint32_t MAX_HOST_RENDER_RECTS = 256;

// ============================================================================
// Language bar icon shared memory (service pre-renders, GetIcon consumes)
// ============================================================================
//
// Mirrors wind-ipc/src/protocol.rs (IconShmHeader / IconVariant). Unlike host-render
// there is no event and no reader thread: GetIcon is a passive callback, so the DLL
// simply reads whatever is current at the moment it is asked.
//
// Concurrency is double-buffering + seqlock. Writer: fill the inactive slot, release
// fence, switch activeSlot, then bump sequence LAST. Reader: read sequence, copy,
// re-read sequence — equal means no publish happened mid-copy, so the copy is a
// consistent snapshot.

constexpr uint32_t ICON_SHM_MAGIC   = 0x4F434957; // 'WICO' (bytes W,I,C,O)
// 本功能尚未随任何版本发布，故开发期内改布局（尺寸档补到 7 档含 40/48、SHM 放大到
// 128 KiB）不占版本号——外面没有任何一代在跑，留着历史编号只会让人误以为有兼容包袱。
// 首个发布版本即 1；发布之后再改布局才 bump。
// 与 Rust 侧 wind-ipc/protocol.rs 的同名常量必须逐字一致；版本不匹配时本端直接判失败
// 退回本地绘制（图标还在、只是没角标），而不是硬读出一张错位的花屏。
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

// Host render setup payload (from Go, response to CMD_HOST_RENDER_REQUEST).
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
// Allows Go to build a precise token→push-handle mapping for multi-instance hosts (e.g. explorer).
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
//
// Added in v0.111.4. This used to carry an empty payload; the service then had no way to
// tell *which* instance lost focus and unconditionally cleared its single global ime_active.
// That breaks on every cross-host switch: OnKillThreadFocus fires ~100ms after DocMgr-level
// focus loss (deliberate, see TextService.cpp), so the old host's focus_lost always lands
// *after* the new host's focus_gained and wipes the activation that was just established.
//
// The service treats a 0-byte payload as token 0 and keeps the legacy behaviour, so an older
// DLL still works against a newer service.
struct ClientTokenPayload
{
    uint64_t clientToken;
};
static_assert(sizeof(ClientTokenPayload) == 8, "ClientTokenPayload must be 8 bytes");

// ── CMD_FOCUS_LOST reason（v0.111.5 起）─────────────────────────────────────────
// "失焦"在 TSF 里是四件语义不同的事，挤在一个命令里会逼服务端做错误的一刀切：过去
// 一律「清激活态 + 清输入态」，于是同宿主换文档也会把工具栏关掉，而 DocMgr 级失焦则
// 因为不敢清输入态干脆什么都不发、工具栏永不隐藏。
//
// 服务端按 reason 分派三项独立后果（见 Rust FocusLostReason）：
//
//   reason            ime_active   has_edit_context   输入态
//   THREAD      (0)     false          false           清     整个应用失去前台
//   DOC_CHANGED (1)     不动           不动            清     同宿主内换文档
//   CTX_LOST    (2)     不动           false          **不清** 焦点离开可编辑控件
//   NO_EDIT_CTX (3)     不动           false           清     换到无可编辑控件的文档
//
// CTX_LOST 之所以必须「不清输入态」：它来自 DocMgr 级失焦，那是噪声层（Excel 实测同一
// DocMgr 6ms 内掉了又回），在那里销毁输入态正是「首字符不进编码、直接上屏」的根因。
// 它只翻可见性标志，所以能安全地在噪声层调用。
// NO_EDIT_CTX 与之相反：新文档确实没有可输入的地方（QQ Ctrl+1 切会话），残留 buffer
// 无处可去，必须清。
//
// 兼容：载荷 < 9 字节时服务端取 reason=THREAD，即旧 DLL 的隐含语义，行为不变。
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
    // 焦点控件的 TSF InputScope 集合，按位图编码：bit N 置位表示 InputScope 枚举值 N 存在
    // （如 IS_PASSWORD=31 → bit 31）。枚举值 < 0 或 >= 64 的项被忽略。Go 端据此决策
    // 密码框强制英文等行为（见 coordinator 的 inputScope 常量）。0 表示未知/默认（IS_DEFAULT）。
    uint64_t     inputScopeMask; // 8 bytes: InputScope bitmask
    // 输入诊断 HUD（Task 7）：焦点控件当前是否被判定为"禁用中文"及原因，随 focus_gained
    // 一并上报，避免 HUD 首次落座需要等待单独的状态上报。字段顺序/含义与 Rust
    // FocusGainedPayload（disabled u8 + reason u8）严格一致。
    uint8_t      disabled;       // 1 byte: 0/1 - GUID_COMPARTMENT_KEYBOARD_DISABLED 命中
    // reason: 0 None / 1 CompartmentDisabled / 2 InputScopePassword / 3 NumericPassword
    uint8_t      reason;         // 1 byte
    // 上面那个 caret 的来源（CARET_SRC_* 之一，值域 0~6 故 1 字节足够）。
    //
    // ⚠ 焦点 caret 曾被当作「只更新缓存、不参与显示决策」而无需来源信息。**「焦点切换时显示
    // 状态提示气泡」推翻了这个前提**——那个气泡就锚在这个坐标上，于是它第一次直接参与定位。
    // 而 OnSetFocus 不是按键上下文，同步 edit session 必被拒（TS_E_SYNCHRONOUS），回退链会
    // 交出一个**跨窗口的** Win32 光标却仍以 TRUE 返回；消费端不知道来源就无从分辨。
    //
    // 兼容：尾部追加，既有字段偏移全不变。服务端按长度分支——<39 字节落 UNKNOWN（旧 DLL），
    // ≥39 读本字段。故新旧两侧可任意组合。
    uint8_t      caretSource;    // 1 byte: CARET_SRC_*
    //
    // ⚠ 本结构之后还有**两个前后相接的变长段**（不在结构体里，由 SendFocusGained 手工拼接）：
    //
    //   [本结构 39 字节][bundleIdLen:u32][bundleId][windowClassLen:u32][windowClass]
    //
    //   ① bundleId    macOS `.app` 专属。**Windows 发 len=0 占位**——不是冗余，是让两个
    //                 平台共用同一条线性走法，否则窗口类段的偏移会因平台而异。
    //   ② windowClass 焦点所在顶层窗口的类名（UTF-8）。服务端据此把 explorer.exe 的过渡型
    //                 窗口（任务栏 / Alt+Tab）与停留型窗口（桌面 / 文件管理器）分开——
    //                 二者进程名相同，仅凭进程名无法区分。
    //
    // ⚠ 再加新段一律接在最后，并同步 Rust 侧 `FocusGainedPayload` 的那张布局图。
    // 两处各自往尾部追加而互不知情时，字节偏移会错位，而逐段兼容的解码**不报错、只解出垃圾**。
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

// ── 焦点窗口句柄的来源域（DiagSnapshotHeader::focusHwndSource）────────────────
// ⚠ 三条通路给出的**不是同一件东西**，压进一个字段而不标来源，下游就再也分不开了
// ——与 CARET_SRC_* 给 caret 坐标分域是同一个教训。这里尤其要命：FOREGROUND 域的窗口
// 可能根本不属于本进程（Win10 任务栏搜索就是前台窗口归 SearchUI、焦点在 explorer），
// 拿它当"焦点窗口"去推 per-app 判据必然推错。
constexpr uint8_t WND_SRC_NONE       = 0; // 三条通路都没拿到
constexpr uint8_t WND_SRC_TSF_VIEW   = 1; // ITfContextView::GetWnd()——TSF 域，最准
constexpr uint8_t WND_SRC_GUI_THREAD = 2; // GetGUIThreadInfo().hwndFocus——线程域
constexpr uint8_t WND_SRC_FOREGROUND = 3; // GetForegroundWindow()——跨进程，兜底

// DiagSnapshotHeader::flags 位：本次焦点相对上一次换了 DocMgr。
// 只有 DLL 知道这件事（它持有 _pLastActiveDocMgr），core 无从推导，故必须随包上报。
constexpr uint8_t DIAG_FLAG_DOCMGR_CHANGED = 0x01;

// CMD_DIAG_SNAPSHOT 定长头（64 bytes）。字段顺序/偏移**必须**与 Rust
// DiagSnapshotPayload 完全一致——两侧都是手写序列化，没有任何编译期约束把它们绑住，
// Rust 侧 `diag_snapshot_head_layout_is_frozen` 与这里的 static_assert 是仅有的两道闸。
//
// 句柄一律按 uint64 传：DLL 可能是 32 位也可能是 64 位，HWND 宽度不同；统一零扩展后
// 两侧偏移才是一个固定值。这些值只用于展示与同一性比较（"还是不是刚才那个窗口/文档"），
// 服务进程不会拿它去调任何 Win32 API——跨进程句柄无效。
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

// Input stats payload (from C++ to Go, async)
// Counts of characters typed in English mode (not intercepted by Go)
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

// Config sync keys (must match Go side)
constexpr const char* CONFIG_KEY_ENGLISH_PAIRS = "en_pairs";
constexpr const char* CONFIG_KEY_JUMP_OUT_KEYS = "jump_out_keys";
constexpr const char* CONFIG_KEY_STATS = "stats";
// 密码框强制英文抑制的策略开关（会话级，右键菜单「高级」可关）。DLL 需要它才能在
// OnTestKeyDown 本地判定是否放行——吃键决策发生在 IPC 之前，仅靠 core 回 PassThrough
// 已经太晚（会形成「吃了再吐」丢键）。
constexpr const char* CONFIG_KEY_PASSWORD_SUPPRESS = "password_suppress";
// 诊断快照采集开关（会话级，随输入诊断 HUD 显隐）。格式：enabled(u8)。默认关。
// 采集要查三次窗口类名 + band，属于「只有排查时才值得付」的开销。
constexpr const char* CONFIG_KEY_DIAG_SNAPSHOT = "diag_snapshot";
// 「英文半角列有自定义标点映射」的源字符集合。英文模式（非全角）下本 DLL 默认直接透传标点键、
// core 收不到，用户配的「英半」列因此永远不生效；据此集合精确吃下这些键转发给 core。
// 集合为空（默认）= 行为与历史完全一致。格式：count(u8) + [ch:u16(LE)]...
constexpr const char* CONFIG_KEY_CUSTOM_EN_PUNCT = "custom_en_punct";
// 配对状态时效（秒，0=不过期）。格式：secs(u16 LE)。
// 吃键闸门（_pairPendingDepth）在本 DLL，故陈旧判据也必须在本地：若只有 core 过期而这边
// 照吃跳出键，core 回 PassThrough 已太晚（「吃了再吐」，不补发 WM_KEYDOWN 的宿主会丢键）。
constexpr const char* CONFIG_KEY_PAIR_STATE_TTL = "pair_state_ttl";
// 语言栏按钮的悬停提示文本。格式：[ch:u16(LE)]...（UTF-16LE，无长度前缀，value 即整段）。
// 文案与选择逻辑全在服务端，本 DLL 只存一份原样返回——本地只有中英态与 CapsLock 两个量，
// 判不出「密码框」「已禁用」这些成因，而图标只能表达「不可用」，说清是哪一种全靠 tooltip。
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

enum class ResponseType
{
    Ack,
    PassThrough,  // Key not handled, pass to system
    CommitText,
    UpdateComposition,
    ClearComposition,
    ClearCompositionThenPassThrough, // 收组合 + 把当前键交还宿主（重放）
    StatusUpdate,
    SyncHotkeys,
    Consumed,
    InsertTextWithCursor, // Insert text and position cursor
    MoveCursorRight,      // Move cursor right (smart skip)
    DeletePair,           // Delete left + right char (smart delete)
    ReplaceBackward,      // Delete N chars before caret + insert text (smart symbol)
    HoldComposition,      // Open composition with text + start auto-commit timer (smart symbol)
    CommitAndHold,        // Commit text then open composition with hold text + start timer
    CommitThenDefer,      // Commit text now, defer new composition (余码) to trigger-key keyup
    HostRenderSetup, // Host render setup (shared memory info)
    Error
};

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

    // Icon label for taskbar display (from Go service, e.g., "中", "英", "A", "拼", "五")
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
};
