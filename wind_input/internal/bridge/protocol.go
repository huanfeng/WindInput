// Package bridge handles IPC communication with C++ TSF Bridge
package bridge

// ResponseType defines the type of response to C++
type ResponseType string

const (
	ResponseTypeInsertText           ResponseType = "insert_text"
	ResponseTypeUpdateComposition    ResponseType = "update_composition"
	ResponseTypeClearComposition     ResponseType = "clear_composition"
	ResponseTypeAck                  ResponseType = "ack"
	ResponseTypePassThrough          ResponseType = "pass_through" // Key not handled, pass to system
	ResponseTypeModeChanged          ResponseType = "mode_changed"
	ResponseTypeStatusUpdate         ResponseType = "status_update"
	ResponseTypeConsumed             ResponseType = "consumed"
	ResponseTypeInsertTextWithCursor ResponseType = "insert_text_with_cursor" // 插入文本并定位光标
	ResponseTypeMoveCursorRight      ResponseType = "move_cursor_right"       // 光标右移（智能跳过）
	ResponseTypeDeletePair           ResponseType = "delete_pair"             // 删除配对（智能删除）
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
	PrevChar  rune   // Character before caret from ITfTextEditSink (0 if unavailable)
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
	Type              ResponseType
	Text              string // For InsertText
	CaretPos          int    // For UpdateComposition
	ChineseMode       bool   // For ModeChanged
	ModeChanged       bool   // Whether mode was also changed (for InsertText + mode change combo)
	NewComposition    string // New composition text after commit (inline preedit: actual text; non-inline: empty)
	HasNewComposition bool   // Whether to restart composition after commit (set for both inline and non-inline when there is remaining input)
	CursorOffset      int    // For InsertTextWithCursor: 光标从文本末尾向左偏移的字符数
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
	// HandleCaretPending: C++ 通知 composition 刚启动, 真正 caret 会在 reflow 后到达。
	// Go 据此延长 pendingFirstShow 兜底超时, 避免提前用按键前坐标 show。
	HandleCaretPending()
	HandleFocusLost()
	HandleCompositionTerminated()
	HandleFocusGained(processID uint32) *StatusUpdateData
	HandleIMEDeactivated()
	HandleIMEActivated(processID uint32) *StatusUpdateData
	HandleToggleMode() (commitText string, chineseMode bool)
	HandleCapsLockState(on bool)
	HandleMenuCommand(command string) *StatusUpdateData
	HandleClientDisconnected(activeClients int)
	// Barrier mechanism for async commit
	HandleCommitRequest(data CommitRequestData) *CommitResultData
	// Mode notification from TSF (local toggle)
	HandleModeNotify(data ModeNotifyData)
	// System mode switch (Ctrl+Space): system has decided the target mode, must follow
	HandleSystemModeSwitch(chineseMode bool) (commitText string)
	// Context menu request from TSF (screen coordinates)
	HandleShowContextMenu(screenX, screenY int)
	// Selection changed outside of composition (from ITfTextEditSink::OnEndEdit)
	// prevChar: character before caret after selection change (0 if unavailable)
	HandleSelectionChanged(prevChar rune)
	// Called when host render is set up for the active client (shared memory ready)
	HandleHostRenderReady()
	// Input stats report from TSF English mode (async)
	HandleInputStats(chars, digits, puncts, spaces, elapsedMs int)
}
