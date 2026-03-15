// Package bridge handles IPC communication with C++ TSF Bridge
package bridge

// ResponseType defines the type of response to C++
type ResponseType string

const (
	ResponseTypeInsertText        ResponseType = "insert_text"
	ResponseTypeUpdateComposition ResponseType = "update_composition"
	ResponseTypeClearComposition  ResponseType = "clear_composition"
	ResponseTypeAck               ResponseType = "ack"
	ResponseTypePassThrough       ResponseType = "pass_through" // Key not handled, pass to system
	ResponseTypeModeChanged       ResponseType = "mode_changed"
	ResponseTypeStatusUpdate      ResponseType = "status_update"
	ResponseTypeConsumed          ResponseType = "consumed"
)

// Toggle key state flags (matching C++ TOGGLE_* constants)
const (
	ToggleCapsLock   uint8 = 0x01 // CapsLock is on
	ToggleNumLock    uint8 = 0x02 // NumLock is on
	ToggleScrollLock uint8 = 0x04 // ScrollLock is on
)

// KeyEventData contains key event information (parsed from binary)
type KeyEventData struct {
	Key       string // Key name (derived from keycode for backwards compatibility)
	KeyCode   int    // Virtual key code
	Modifiers int    // Modifier flags
	Event     string // "down" or "up"
	Toggles   uint8  // Toggle key states (CapsLock/NumLock/ScrollLock) from C++ side
	// Caret position (optional, sent with key events)
	Caret *CaretData
}

// IsCapsLockOn returns true if CapsLock is on (from C++ side toggle state)
func (d *KeyEventData) IsCapsLockOn() bool {
	return (d.Toggles & ToggleCapsLock) != 0
}

// CaretData contains caret position information
type CaretData struct {
	X                 int
	Y                 int
	Height            int
	CompositionStartX int // Screen X of composition range start (0 if no composition)
	CompositionStartY int // Screen Y of composition range start (0 if no composition)
}

// StatusUpdateData for status update response
type StatusUpdateData struct {
	ChineseMode        bool
	FullWidth          bool
	ChinesePunctuation bool
	ToolbarVisible     bool
	CapsLock           bool
	// Icon label for taskbar display (e.g., "中", "英", "A", "拼", "五", "双")
	// Go service determines the label; C++ TSF just renders it
	IconLabel string
	// Hotkey hashes for C++ side (compiled from config)
	KeyDownHotkeys []uint32
	KeyUpHotkeys   []uint32
}

// KeyEventResult represents the result of handling a key event
type KeyEventResult struct {
	Type           ResponseType
	Text           string // For InsertText
	CaretPos       int    // For UpdateComposition
	ChineseMode    bool   // For ModeChanged
	ModeChanged    bool   // Whether mode was also changed (for InsertText + mode change combo)
	NewComposition string // New composition after commit (for top code scenarios)
}

// CommitRequestData contains commit request information (barrier mechanism)
type CommitRequestData struct {
	BarrierSeq  uint16 // Barrier sequence number for matching response
	TriggerKey  uint16 // VK code that triggered commit (Space/Enter/1-9)
	Modifiers   uint32 // Modifier state at trigger time
	InputBuffer string // Current input buffer content
}

// CommitResultData contains commit result information (barrier mechanism)
type CommitResultData struct {
	BarrierSeq     uint16 // Matching barrier sequence
	Text           string // Text to commit
	NewComposition string // Optional new composition after commit
	ModeChanged    bool   // Whether mode was changed
	ChineseMode    bool   // New mode (if ModeChanged is true)
}

// ModeNotifyData contains mode notification from TSF (local toggle)
type ModeNotifyData struct {
	ChineseMode bool // New mode after toggle
	ClearInput  bool // Whether input buffer should be cleared
}

// MessageHandler handles messages from C++ Bridge
type MessageHandler interface {
	HandleKeyEvent(data KeyEventData) *KeyEventResult
	HandleCaretUpdate(data CaretData) error
	HandleFocusLost()
	HandleCompositionTerminated()
	HandleFocusGained() *StatusUpdateData
	HandleIMEDeactivated()
	HandleIMEActivated() *StatusUpdateData
	HandleToggleMode() (commitText string, chineseMode bool)
	HandleCapsLockState(on bool)
	HandleMenuCommand(command string) *StatusUpdateData
	HandleClientDisconnected(activeClients int)
	// Barrier mechanism for async commit
	HandleCommitRequest(data CommitRequestData) *CommitResultData
	// Mode notification from TSF (local toggle)
	HandleModeNotify(data ModeNotifyData)
	// Context menu request from TSF (screen coordinates)
	HandleShowContextMenu(screenX, screenY int)
}
