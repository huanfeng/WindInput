#pragma once

#include <cstdint>
#include <vector>
#include <string>

// Protocol version (major.minor: high 4 bits = major, low 12 bits = minor)
constexpr uint16_t PROTOCOL_VERSION = 0x1000; // v1.0

// ============================================================================
// Upstream commands (C++ -> Go)
// ============================================================================
constexpr uint16_t CMD_KEY_EVENT        = 0x0101; // Key event (down/up)
constexpr uint16_t CMD_FOCUS_GAINED     = 0x0201; // Focus gained
constexpr uint16_t CMD_FOCUS_LOST       = 0x0202; // Focus lost
constexpr uint16_t CMD_IME_ACTIVATED    = 0x0203; // IME activated
constexpr uint16_t CMD_IME_DEACTIVATED  = 0x0204; // IME deactivated
constexpr uint16_t CMD_CARET_UPDATE     = 0x0301; // Caret position update

// ============================================================================
// Downstream commands (Go -> C++)
// ============================================================================
constexpr uint16_t CMD_ACK                = 0x0001; // Simple acknowledgment
constexpr uint16_t CMD_PASS_THROUGH       = 0x0002; // Key not handled, pass to system
constexpr uint16_t CMD_COMMIT_TEXT        = 0x0101; // Commit text
constexpr uint16_t CMD_UPDATE_COMPOSITION = 0x0102; // Update composition
constexpr uint16_t CMD_CLEAR_COMPOSITION  = 0x0103; // Clear composition
constexpr uint16_t CMD_MODE_CHANGED       = 0x0201; // Mode changed
constexpr uint16_t CMD_STATUS_UPDATE      = 0x0202; // Full status update
constexpr uint16_t CMD_SYNC_HOTKEYS       = 0x0301; // Sync hotkey whitelist
constexpr uint16_t CMD_CONSUMED           = 0x0401; // Key consumed

// ============================================================================
// Key event types
// ============================================================================
constexpr uint8_t KEY_EVENT_DOWN = 0;
constexpr uint8_t KEY_EVENT_UP   = 1;

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

// ============================================================================
// Protocol structures (must match Go side exactly)
// ============================================================================
#pragma pack(push, 1)

// Protocol header (8 bytes)
struct IpcHeader
{
    uint16_t version;  // Protocol version
    uint16_t command;  // Command type
    uint32_t length;   // Payload length in bytes
};
static_assert(sizeof(IpcHeader) == 8, "IpcHeader must be 8 bytes");

// Key event payload (16 bytes)
struct KeyPayload
{
    uint32_t keyCode;     // Virtual key code
    uint32_t scanCode;    // Scan code
    uint32_t modifiers;   // Modifier flags
    uint8_t  eventType;   // 0=KeyDown, 1=KeyUp
    uint8_t  reserved[3]; // Alignment padding
};
static_assert(sizeof(KeyPayload) == 16, "KeyPayload must be 16 bytes");

// Caret position payload (12 bytes)
struct CaretPayload
{
    int32_t x;
    int32_t y;
    int32_t height;
};
static_assert(sizeof(CaretPayload) == 12, "CaretPayload must be 12 bytes");

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

// Commit text flags
constexpr uint32_t COMMIT_FLAG_MODE_CHANGED       = 0x0001;
constexpr uint32_t COMMIT_FLAG_HAS_NEW_COMPOSITION = 0x0002;
constexpr uint32_t COMMIT_FLAG_CHINESE_MODE       = 0x0004;

#pragma pack(pop)

// ============================================================================
// Helper functions
// ============================================================================

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

    // For StatusUpdate / ModeChanged
    uint32_t statusFlags = 0;

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
