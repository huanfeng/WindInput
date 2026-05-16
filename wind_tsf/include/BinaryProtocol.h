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
constexpr uint16_t CMD_COMPOSITION_TERMINATED = 0x0209; // Composition unexpectedly terminated (e.g., user clicked in input field)
constexpr uint16_t CMD_CARET_UPDATE     = 0x0301; // Caret position update
constexpr uint16_t CMD_SELECTION_CHANGED = 0x0302; // Selection/caret changed without composition (from ITfTextEditSink)
constexpr uint16_t CMD_CARET_PENDING    = 0x0303; // First-show handshake: composition just started, real caret coming after reflow
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
constexpr uint16_t CMD_COMMIT_RESULT      = 0x0105; // Commit result (response to COMMIT_REQUEST)
constexpr uint16_t CMD_MODE_CHANGED       = 0x0201; // Mode changed
constexpr uint16_t CMD_STATUS_UPDATE      = 0x0202; // Full status update
constexpr uint16_t CMD_STATE_PUSH         = 0x0206; // State push (broadcast to all clients)
constexpr uint16_t CMD_SERVICE_READY      = 0x0207; // Go service connected push pipe, TSF should sync state
constexpr uint16_t CMD_SYNC_HOTKEYS       = 0x0301; // Sync hotkey whitelist
constexpr uint16_t CMD_SYNC_CONFIG        = 0x0303; // Sync config key/value (generic)
constexpr uint16_t CMD_CONSUMED           = 0x0401; // Key consumed
constexpr uint16_t CMD_COMMIT_TEXT_WITH_CURSOR = 0x0106; // Commit text with cursor offset
constexpr uint16_t CMD_MOVE_CURSOR             = 0x0107; // Move cursor (smart skip)
constexpr uint16_t CMD_DELETE_PAIR             = 0x0108; // Delete pair (smart backspace)
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
    uint32_t direction; // 1=right
};
static_assert(sizeof(MoveCursorPayload) == 4, "MoveCursorPayload must be 4 bytes");

// Commit text flags
constexpr uint32_t COMMIT_FLAG_MODE_CHANGED       = 0x0001;
constexpr uint32_t COMMIT_FLAG_HAS_NEW_COMPOSITION = 0x0002;
constexpr uint32_t COMMIT_FLAG_CHINESE_MODE       = 0x0004;

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

// ============================================================================
// Host render shared memory structures
// ============================================================================

// Shared memory magic and version
constexpr uint32_t SHARED_RENDER_MAGIC   = 0x57494E44; // 'WIND'
constexpr uint32_t SHARED_RENDER_VERSION = 1;

// Shared memory flags
constexpr uint32_t SHARED_FLAG_VISIBLE       = 0x0001; // Window should be visible
constexpr uint32_t SHARED_FLAG_CONTENT_READY = 0x0002; // New content is ready to render

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
    uint32_t reserved[6];// Padding to 64 bytes
};
static_assert(sizeof(SharedRenderHeader) == 64, "SharedRenderHeader must be 64 bytes");

// Host render setup payload (from Go, response to CMD_HOST_RENDER_REQUEST)
// Wire format: maxBufferSize(4) + shmNameLen(4) + eventNameLen(4) + shmName + eventName
struct HostRenderSetupHeader
{
    uint32_t maxBufferSize;  // Maximum shared memory size
    uint32_t shmNameLen;     // Length of shared memory name (UTF-8)
    uint32_t eventNameLen;   // Length of event name (UTF-8)
    // Followed by: shmName (shmNameLen bytes) + eventName (eventNameLen bytes)
};
static_assert(sizeof(HostRenderSetupHeader) == 12, "HostRenderSetupHeader must be 12 bytes");

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

// CMD_FOCUS_GAINED extended payload (28 bytes = CaretPayload + clientToken)
struct FocusGainedPayload
{
    CaretPayload caret;       // 20 bytes: caret position
    uint64_t     clientToken; // 8 bytes: per-instance token
};
static_assert(sizeof(FocusGainedPayload) == 28, "FocusGainedPayload must be 28 bytes");

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
constexpr const char* CONFIG_KEY_STATS = "stats";

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
    ModeChanged,
    StatusUpdate,
    SyncHotkeys,
    Consumed,
    InsertTextWithCursor, // Insert text and position cursor
    MoveCursorRight,      // Move cursor right (smart skip)
    DeletePair,           // Delete left + right char (smart delete)
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

    // For StatusUpdate / ModeChanged
    uint32_t statusFlags = 0;

    // Icon label for taskbar display (from Go service, e.g., "中", "英", "A", "拼", "五")
    std::wstring iconLabel;

    // For SyncHotkeys / StatusUpdate
    std::vector<uint32_t> keyDownHotkeys;
    std::vector<uint32_t> keyUpHotkeys;

    // Helper methods
    bool IsChineseMode() const { return (statusFlags & STATUS_CHINESE_MODE) != 0; }
    bool IsFullWidth() const { return (statusFlags & STATUS_FULL_WIDTH) != 0; }
    bool IsChinesePunct() const { return (statusFlags & STATUS_CHINESE_PUNCT) != 0; }
    bool IsToolbarVisible() const { return (statusFlags & STATUS_TOOLBAR_VISIBLE) != 0; }
    bool IsCapsLock() const { return (statusFlags & STATUS_CAPS_LOCK) != 0; }
};
