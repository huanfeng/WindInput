// Package ipc defines the binary protocol for IPC communication between Go service and C++ TSF.
package ipc

// Protocol version (major.minor: high 4 bits = major, low 12 bits = minor)
const ProtocolVersion uint16 = 0x1000 // v1.0

// Async flag (used in version field's high bit to mark async requests)
const AsyncFlag uint16 = 0x8000 // Async request flag - no response expected

// Upstream commands (C++ -> Go)
const (
	CmdKeyEvent       uint16 = 0x0101 // Key event (down/up)
	CmdFocusGained    uint16 = 0x0201 // Focus gained
	CmdFocusLost      uint16 = 0x0202 // Focus lost
	CmdIMEActivated   uint16 = 0x0203 // IME activated (user switched to this IME)
	CmdIMEDeactivated uint16 = 0x0204 // IME deactivated (user switched to another IME)
	CmdCaretUpdate    uint16 = 0x0301 // Caret position update
	CmdBatchEvents    uint16 = 0x0F01 // Batch events container
)

// Downstream commands (Go -> C++)
const (
	CmdAck               uint16 = 0x0001 // Simple acknowledgment
	CmdPassThrough       uint16 = 0x0002 // Key not handled, pass to system
	CmdCommitText        uint16 = 0x0101 // Commit text to application
	CmdUpdateComposition uint16 = 0x0102 // Update composition (preedit)
	CmdClearComposition  uint16 = 0x0103 // Clear composition
	CmdModeChanged       uint16 = 0x0201 // Mode changed
	CmdStatusUpdate      uint16 = 0x0202 // Full status update
	CmdSyncHotkeys       uint16 = 0x0301 // Sync hotkey whitelist
	CmdConsumed          uint16 = 0x0401 // Key consumed (no output)
	CmdBatchResponse     uint16 = 0x0F02 // Batch response container
)

// Key event types
const (
	KeyEventDown uint8 = 0
	KeyEventUp   uint8 = 1
)

// Modifier flags for KeyHash encoding (high 16 bits)
const (
	ModShift    uint32 = 0x0001 // Generic Shift
	ModCtrl     uint32 = 0x0002 // Generic Ctrl
	ModAlt      uint32 = 0x0004 // Alt
	ModWin      uint32 = 0x0008 // Windows key
	ModLShift   uint32 = 0x0010 // Left Shift specifically
	ModRShift   uint32 = 0x0020 // Right Shift specifically
	ModLCtrl    uint32 = 0x0040 // Left Ctrl specifically
	ModRCtrl    uint32 = 0x0080 // Right Ctrl specifically
	ModCapsLock uint32 = 0x0100 // CapsLock as toggle key marker
)

// Status flags for StatusPayload
const (
	StatusChineseMode    uint32 = 0x0001 // Chinese mode
	StatusFullWidth      uint32 = 0x0002 // Full-width mode
	StatusChinesePunct   uint32 = 0x0004 // Chinese punctuation
	StatusToolbarVisible uint32 = 0x0008 // Toolbar visible
	StatusModeChanged    uint32 = 0x0010 // Mode was just changed
	StatusCapsLock       uint32 = 0x0020 // CapsLock is on
)

// Virtual key codes (Windows VK_* constants)
const (
	VK_BACK      uint32 = 0x08
	VK_TAB       uint32 = 0x09
	VK_RETURN    uint32 = 0x0D
	VK_SHIFT     uint32 = 0x10
	VK_CONTROL   uint32 = 0x11
	VK_MENU      uint32 = 0x12 // Alt
	VK_CAPITAL   uint32 = 0x14 // CapsLock
	VK_ESCAPE    uint32 = 0x1B
	VK_SPACE     uint32 = 0x20
	VK_PRIOR     uint32 = 0x21 // PageUp
	VK_NEXT      uint32 = 0x22 // PageDown
	VK_LSHIFT    uint32 = 0xA0
	VK_RSHIFT    uint32 = 0xA1
	VK_LCONTROL  uint32 = 0xA2
	VK_RCONTROL  uint32 = 0xA3
	VK_OEM_1     uint32 = 0xBA // ;:
	VK_OEM_PLUS  uint32 = 0xBB // =+
	VK_OEM_COMMA uint32 = 0xBC // ,<
	VK_OEM_MINUS uint32 = 0xBD // -_
	VK_OEM_PERIOD uint32 = 0xBE // .>
	VK_OEM_2     uint32 = 0xBF // /?
	VK_OEM_3     uint32 = 0xC0 // `~
	VK_OEM_4     uint32 = 0xDB // [{
	VK_OEM_5     uint32 = 0xDC // \|
	VK_OEM_6     uint32 = 0xDD // ]}
	VK_OEM_7     uint32 = 0xDE // '"
)

// Header size in bytes
const HeaderSize = 8

// BatchHeader size in bytes
const BatchHeaderSize = 4

// IpcHeader represents the protocol header (8 bytes)
type IpcHeader struct {
	Version uint16 // Protocol version (high bit may be AsyncFlag)
	Command uint16 // Command type
	Length  uint32 // Payload length in bytes
}

// BatchHeader represents the batch events header (4 bytes)
type BatchHeader struct {
	EventCount uint16 // Number of events in this batch
	Reserved   uint16 // Reserved for future use
}

// KeyPayload represents a key event (16 bytes, matches C++ struct)
type KeyPayload struct {
	KeyCode   uint32 // Virtual key code
	ScanCode  uint32 // Scan code
	Modifiers uint32 // Modifier flags
	EventType uint8  // 0=KeyDown, 1=KeyUp
	Reserved  [3]uint8
}

// CaretPayload represents caret position (12 bytes, matches C++ struct)
type CaretPayload struct {
	X      int32
	Y      int32
	Height int32
}

// CompositionPayload for update_composition response
type CompositionPayload struct {
	CaretPos int32
	Text     string // UTF-8 encoded
}

// StatusPayload for status_update response
type StatusPayload struct {
	Flags        uint32   // Status flags
	KeyDownCount uint32   // Number of KeyDown hotkeys
	KeyUpCount   uint32   // Number of KeyUp hotkeys
	Hotkeys      []uint32 // KeyHash values (KeyDown first, then KeyUp)
}

// CommitTextPayload for commit_text response
type CommitTextPayload struct {
	Text           string // UTF-8 encoded, text to commit
	NewComposition string // Optional: new composition after commit (for top code)
	ModeChanged    bool   // Whether mode was changed
	ChineseMode    bool   // New mode (if ModeChanged is true)
}

// CalcKeyHash computes the key hash for hotkey matching
// Format: (modifiers << 16) | keyCode
func CalcKeyHash(modifiers, keyCode uint32) uint32 {
	return (modifiers << 16) | (keyCode & 0xFFFF)
}

// ParseKeyHash extracts modifiers and keyCode from a key hash
func ParseKeyHash(hash uint32) (modifiers, keyCode uint32) {
	return hash >> 16, hash & 0xFFFF
}
