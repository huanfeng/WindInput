// Package ipc defines the binary protocol for IPC communication between Go service and C++ TSF.
//
// 跨语言协议同步（必读）：本文件与 wind_tsf/include/BinaryProtocol.h 互为镜像。
// 修改命令码、Header 字段、Payload 结构、状态标志位时，必须同步修改：
//   - wind_tsf/include/BinaryProtocol.h
//   - wind_tsf/src/IPCClient.cpp（编解码实现）
//
// 否则会破坏 Go 服务与 C++ TSF DLL 的 IPC 兼容性。
package ipc

// Protocol version (major.minor: high 4 bits = major, low 12 bits = minor)
const ProtocolVersion uint16 = 0x1001 // v1.1 - Added barrier mechanism and state machine support

// Async flag (used in version field's high bit to mark async requests)
const AsyncFlag uint16 = 0x8000 // Async request flag - no response expected

// Upstream commands (C++ -> Go)
const (
	CmdKeyEvent              uint16 = 0x0101 // Key event (down/up)
	CmdCommitRequest         uint16 = 0x0104 // Commit request with barrier (Space/Enter/number select)
	CmdFocusGained           uint16 = 0x0201 // Focus gained
	CmdFocusLost             uint16 = 0x0202 // Focus lost
	CmdIMEActivated          uint16 = 0x0203 // IME activated (user switched to this IME)
	CmdIMEDeactivated        uint16 = 0x0204 // IME deactivated (user switched to another IME)
	CmdModeNotify            uint16 = 0x0205 // Mode changed notification (TSF local toggle, async)
	CmdToggleMode            uint16 = 0x0207 // Toggle mode request (from UI click)
	CmdSystemModeSwitch      uint16 = 0x020B // System mode switch (Ctrl+Space, sync, carries target mode)
	CmdMenuCommand           uint16 = 0x0208 // Menu command (toggle_mode, toggle_width, etc.)
	CmdCompositionTerminated uint16 = 0x0209 // Composition unexpectedly terminated (e.g., user clicked in input field)
	CmdShowContextMenu       uint16 = 0x020A // 请求显示右键菜单（TSF发送屏幕坐标）
	CmdCaretUpdate           uint16 = 0x0301 // Caret position update
	CmdSelectionChanged      uint16 = 0x0302 // Selection/caret changed without composition (from ITfTextEditSink)
	CmdCaretPending          uint16 = 0x0303 // First-show handshake: composition just started, real caret coming after reflow
	CmdBatchEvents           uint16 = 0x0F01 // Batch events container
	CmdInputStats            uint16 = 0x0F03 // Input stats report (async, from TSF English mode)
)

// Downstream commands (Go -> C++)
const (
	CmdAck                  uint16 = 0x0001 // Simple acknowledgment
	CmdPassThrough          uint16 = 0x0002 // Key not handled, pass to system
	CmdCommitText           uint16 = 0x0101 // Commit text to application
	CmdUpdateComposition    uint16 = 0x0102 // Update composition (preedit)
	CmdClearComposition     uint16 = 0x0103 // Clear composition
	CmdCommitResult         uint16 = 0x0105 // Commit result (response to COMMIT_REQUEST)
	CmdModeChanged          uint16 = 0x0201 // Mode changed
	CmdStatusUpdate         uint16 = 0x0202 // Full status update
	CmdStatePush            uint16 = 0x0206 // State push (broadcast to all clients)
	CmdServiceReady         uint16 = 0x0207 // Go service connected push pipe, TSF should sync state
	CmdSyncHotkeys          uint16 = 0x0301 // Sync hotkey whitelist
	CmdSyncConfig           uint16 = 0x0303 // Sync config key/value (generic)
	CmdCommitTextWithCursor uint16 = 0x0106 // Commit text with cursor offset
	CmdMoveCursor           uint16 = 0x0107 // Move cursor (skip over)
	CmdDeletePair           uint16 = 0x0108 // Delete pair (smart backspace)
	CmdConsumed             uint16 = 0x0401 // Key consumed (no output)
	CmdHostRenderSetup      uint16 = 0x0501 // Host render setup (shared memory + event names)
	CmdBatchResponse        uint16 = 0x0F02 // Batch response container
)

// Config sync keys (used with CmdSyncConfig)
const ConfigKeyEnglishPairs = "en_pairs"
const ConfigKeyStats = "stats"

// Host render commands (C++ -> Go)
const (
	CmdHostRenderRequest uint16 = 0x0501 // DLL requests host render setup after seeing HOST_RENDER flag
)

// Key event types
const (
	KeyEventDown uint8 = 0
	KeyEventUp   uint8 = 1
)

// Toggle key state flags (for KeyPayload.Toggles)
const (
	ToggleCapsLock   uint8 = 0x01 // CapsLock is on
	ToggleNumLock    uint8 = 0x02 // NumLock is on
	ToggleScrollLock uint8 = 0x04 // ScrollLock is on
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
	StatusChineseMode     uint32 = 0x0001 // Chinese mode
	StatusFullWidth       uint32 = 0x0002 // Full-width mode
	StatusChinesePunct    uint32 = 0x0004 // Chinese punctuation
	StatusToolbarVisible  uint32 = 0x0008 // Toolbar visible
	StatusModeChanged     uint32 = 0x0010 // Mode was just changed
	StatusCapsLock        uint32 = 0x0020 // CapsLock is on
	StatusHostRenderAvail uint32 = 0x0040 // Host render available (DLL should request setup)
)

// Virtual key codes (Windows VK_* constants)
const (
	VK_BACK       uint32 = 0x08
	VK_TAB        uint32 = 0x09
	VK_RETURN     uint32 = 0x0D
	VK_SHIFT      uint32 = 0x10
	VK_CONTROL    uint32 = 0x11
	VK_MENU       uint32 = 0x12 // Alt
	VK_CAPITAL    uint32 = 0x14 // CapsLock
	VK_ESCAPE     uint32 = 0x1B
	VK_SPACE      uint32 = 0x20
	VK_PRIOR      uint32 = 0x21 // PageUp
	VK_NEXT       uint32 = 0x22 // PageDown
	VK_END        uint32 = 0x23
	VK_HOME       uint32 = 0x24
	VK_LEFT       uint32 = 0x25
	VK_UP         uint32 = 0x26
	VK_RIGHT      uint32 = 0x27
	VK_DOWN       uint32 = 0x28
	VK_INSERT     uint32 = 0x2D // Insert
	VK_DELETE     uint32 = 0x2E // Delete
	VK_LSHIFT     uint32 = 0xA0
	VK_RSHIFT     uint32 = 0xA1
	VK_LCONTROL   uint32 = 0xA2
	VK_RCONTROL   uint32 = 0xA3
	VK_NUMPAD0    uint32 = 0x60 // Numpad 0
	VK_NUMPAD9    uint32 = 0x69 // Numpad 9
	VK_MULTIPLY   uint32 = 0x6A // Numpad *
	VK_ADD        uint32 = 0x6B // Numpad +
	VK_SUBTRACT   uint32 = 0x6D // Numpad -
	VK_DECIMAL    uint32 = 0x6E // Numpad .
	VK_DIVIDE     uint32 = 0x6F // Numpad /
	VK_OEM_1      uint32 = 0xBA // ;:
	VK_OEM_PLUS   uint32 = 0xBB // =+
	VK_OEM_COMMA  uint32 = 0xBC // ,<
	VK_OEM_MINUS  uint32 = 0xBD // -_
	VK_OEM_PERIOD uint32 = 0xBE // .>
	VK_OEM_2      uint32 = 0xBF // /?
	VK_OEM_3      uint32 = 0xC0 // `~
	VK_OEM_4      uint32 = 0xDB // [{
	VK_OEM_5      uint32 = 0xDC // \|
	VK_OEM_6      uint32 = 0xDD // ]}
	VK_OEM_7      uint32 = 0xDE // '"
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

// KeyPayload represents a key event (18 bytes, matches C++ struct)
type KeyPayload struct {
	KeyCode   uint32 // Virtual key code
	ScanCode  uint32 // Scan code
	Modifiers uint32 // Modifier flags (snapshot at event time, from state machine)
	EventType uint8  // 0=KeyDown, 1=KeyUp
	Toggles   uint8  // Toggle key states (CapsLock/NumLock/ScrollLock)
	EventSeq  uint16 // Monotonic event sequence number
	PrevChar  uint16 // Character before caret (from ITfTextEditSink cache, 0 if unavailable)
}

// CaretPayload represents caret position (20 bytes, matches C++ struct)
type CaretPayload struct {
	X                 int32
	Y                 int32
	Height            int32
	CompositionStartX int32 // Screen X of composition range start (0 if no composition)
	CompositionStartY int32 // Screen Y of composition range start (0 if no composition)
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

// CommitRequestPayload for commit_request (barrier mechanism)
// Sent from C++ to Go when Space/Enter/number key is pressed during composition
type CommitRequestPayload struct {
	BarrierSeq  uint16 // Barrier sequence number (for matching response)
	TriggerKey  uint16 // VK code that triggered commit (VK_SPACE/VK_RETURN/0x31-0x39)
	Modifiers   uint32 // Modifier state at trigger time
	InputBuffer string // Input buffer content (UTF-8)
}

// CommitResultPayload for commit_result response (barrier mechanism)
// Sent from Go to C++ as response to COMMIT_REQUEST
type CommitResultPayload struct {
	BarrierSeq     uint16 // Matching barrier sequence
	Text           string // UTF-8 encoded, text to commit
	NewComposition string // Optional: new composition after commit
	ModeChanged    bool   // Whether mode was changed
	ChineseMode    bool   // New mode (if ModeChanged is true)
}

// Commit flags (for CommitTextPayload and CommitResultPayload wire format)
const (
	CommitFlagModeChanged       uint16 = 0x0001
	CommitFlagHasNewComposition uint16 = 0x0002
	CommitFlagChineseMode       uint16 = 0x0004
)

// Shared memory constants for host render
const (
	SharedRenderMagic   uint32 = 0x57494E44 // 'WIND'
	SharedRenderVersion uint32 = 1

	// SharedRenderHeader flags
	SharedFlagVisible      uint32 = 0x0001 // Window should be visible
	SharedFlagContentReady uint32 = 0x0002 // New content is ready to render

	// SharedRenderHeaderSize is the fixed header size in shared memory (64 bytes)
	SharedRenderHeaderSize = 64

	// MaxSharedRenderSize is the max shared memory allocation (4MB, covers ~1024x1024 BGRA)
	MaxSharedRenderSize = 4 * 1024 * 1024
)

// SharedRenderHeader is the header at the start of shared memory.
// Total size: 64 bytes. Followed by BGRA pixel data.
type SharedRenderHeader struct {
	Magic    uint32 // 0x57494E44 = 'WIND'
	Version  uint32 // 1
	Sequence uint32 // Monotonic, incremented each write by Go
	Flags    uint32 // SharedFlag* bits
	X        int32  // Screen X position
	Y        int32  // Screen Y position
	Width    uint32 // Bitmap width in pixels
	Height   uint32 // Bitmap height in pixels
	Stride   uint32 // Bytes per row (width * 4)
	DataSize uint32 // Total BGRA pixel data size in bytes
	// 40 bytes used, 24 bytes reserved to reach 64
}

// HostRenderSetupPayload is sent from Go to DLL with shared memory details.
type HostRenderSetupPayload struct {
	MaxBufferSize uint32 // Maximum shared memory size
	ShmName       string // Shared memory name (e.g. "Local\\WindInput_SHM_12345")
	EventName     string // Named event name (e.g. "Local\\WindInput_EVT_12345")
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
