// Package bridge handles IPC communication with C++ TSF Bridge
package bridge

// ResponseType defines the type of response to C++
type ResponseType string

const (
	ResponseTypeInsertText        ResponseType = "insert_text"
	ResponseTypeUpdateComposition ResponseType = "update_composition"
	ResponseTypeClearComposition  ResponseType = "clear_composition"
	ResponseTypeAck               ResponseType = "ack"
	ResponseTypeModeChanged       ResponseType = "mode_changed"
	ResponseTypeStatusUpdate      ResponseType = "status_update"
	ResponseTypeConsumed          ResponseType = "consumed"
)

// KeyEventData contains key event information (parsed from binary)
type KeyEventData struct {
	Key       string // Key name (derived from keycode for backwards compatibility)
	KeyCode   int    // Virtual key code
	Modifiers int    // Modifier flags
	Event     string // "down" or "up"
	// Caret position (optional, sent with key events)
	Caret *CaretData
}

// CaretData contains caret position information
type CaretData struct {
	X      int
	Y      int
	Height int
}

// StatusUpdateData for status update response
type StatusUpdateData struct {
	ChineseMode        bool
	FullWidth          bool
	ChinesePunctuation bool
	ToolbarVisible     bool
	CapsLock           bool
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

// MessageHandler handles messages from C++ Bridge
type MessageHandler interface {
	HandleKeyEvent(data KeyEventData) *KeyEventResult
	HandleCaretUpdate(data CaretData) error
	HandleFocusLost()
	HandleFocusGained() *StatusUpdateData
	HandleIMEDeactivated()
	HandleIMEActivated() *StatusUpdateData
	HandleToggleMode() (commitText string, chineseMode bool)
	HandleCapsLockState(on bool)
	HandleMenuCommand(command string) *StatusUpdateData
	HandleClientDisconnected(activeClients int)
}
