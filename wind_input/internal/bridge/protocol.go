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
	ChineseMode       bool   // New mode (used with InsertText + ModeChanged combo)
	ModeChanged       bool   // Whether mode was also changed (for InsertText + mode change combo)
	NewComposition    string // New composition text after commit (inline preedit: actual text; non-inline: empty)
	HasNewComposition bool   // Whether to restart composition after commit (set for both inline and non-inline when there is remaining input)
	CursorOffset      int    // For InsertTextWithCursor: 光标从文本末尾向左偏移的字符数

	// Status: 当 Type 为 ResponseTypeStatusUpdate 时携带完整状态（含 IconLabel）。
	// 用于模式切换响应——bridge 响应自包含 iconLabel，C++ 端的 CLangBar 立刻通过
	// UpdateFullStatus 刷新任务栏图标，不再依赖 CMD_STATE_PUSH 的稳定送达。
	Status *StatusUpdateData
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
	// HandleFocusGained: inputScopeMask 是焦点控件的 TSF InputScope bitmask
	// （bit N = 枚举值 N 存在，如 IS_PASSWORD=31）。Go 据此决策密码框强制英文等。
	// darwin 等暂未实现 InputScope 探测的平台传 0。
	HandleFocusGained(processID uint32, inputScopeMask uint64) *StatusUpdateData
	HandleIMEDeactivated()
	HandleIMEActivated(processID uint32) *StatusUpdateData
	// HandleToggleMode toggles the input mode. Returns the resulting full status
	// (含 iconLabel) so the response can be self-contained. commitText carries
	// pending input when CommitOnSwitch is enabled and we switch out of Chinese.
	HandleToggleMode() (status *StatusUpdateData, commitText string)
	HandleCapsLockState(on bool)
	HandleMenuCommand(command string) *StatusUpdateData
	HandleClientDisconnected(activeClients int)
	// Barrier mechanism for async commit
	HandleCommitRequest(data CommitRequestData) *CommitResultData
	// Mode notification from TSF (local toggle)
	HandleModeNotify(data ModeNotifyData)
	// HandleSystemModeSwitch handles a TSF-driven mode switch where the system
	// has *already decided* the target mode (e.g. Ctrl+Space). Go must follow,
	// not toggle. Returns the resulting full status; commitText set when
	// CommitOnSwitch fires.
	HandleSystemModeSwitch(chineseMode bool) (status *StatusUpdateData, commitText string)
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

// candidateSelector 是可选扩展接口 (不并入 MessageHandler 以免牵动 Win 实现)。
// darwin bridge 收到 CmdCandidateSelect (NSPanel 鼠标点选) 时类型断言调用;
// Coordinator 实现, DeferredHandler 转发。
type candidateSelector interface {
	HandleCandidateSelect(index int)
}

// candidateContextMenuHandler 是可选扩展接口: darwin bridge 收到 CmdCandidateContextMenu
// (NSPanel 右键菜单动作) 时类型断言调用。index 为页内索引, action 为动作字符串
// (move_up/move_down/move_top/delete/reset_default/copy)。Coordinator 实现, DeferredHandler 转发。
type candidateContextMenuHandler interface {
	HandleCandidateContextMenu(index int, action string)
}

// candidateHoverHandler 是可选扩展接口: darwin bridge 收到 CmdCandidateHover 时,
// 除了让 forwarder 重绘高亮, 还派发给 Coordinator 触发 tooltip 查询。index 为页内
// 索引 (-1=无悬停)。tooltip 文本经 push 通道下发, 位置由 .app 据悬停候选矩形自定。
// Coordinator 实现, DeferredHandler 转发。
type candidateHoverHandler interface {
	HandleCandidateHover(index int)
}

// hostCandidateHoverHandler 是 Windows host render 鼠标悬停的可选扩展接口。与
// candidateHoverHandler（darwin, index-only）不同：Windows host render 的 tooltip
// 由 Go 端窗口渲染，需要候选的屏幕锚点来定位（below/above 两个候选 Y 供 tooltip 子
// 系统按工作区择优）。锚点由 DLL 据 host 窗口屏幕位置 + 命中矩形算出。index<0 表示
// 离开候选区。Coordinator 实现, DeferredHandler 转发。
type hostCandidateHoverHandler interface {
	HandleCandidateHoverAt(index, tooltipX, tooltipBelowY, tooltipAboveY int)
}

// candidateScrollHandler 是 Windows host render 候选框鼠标滚轮的可选扩展接口。
// delta 为原始滚轮增量（WHEEL_DELTA=120 的整数倍，正=上滚）。Coordinator 实现统一
// 决策（默认不翻页，标准版本地候选窗也无滚轮翻页）；DeferredHandler 转发。
type candidateScrollHandler interface {
	HandleCandidateScroll(delta int)
}
