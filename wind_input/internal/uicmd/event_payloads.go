package uicmd

// CandidateSelectPayload 用户点选某个候选词。
// Index 是当前页内 0-based 索引。
type CandidateSelectPayload struct {
	Index int32
}

func (CandidateSelectPayload) isEventPayload()      {}
func (CandidateSelectPayload) EventType() EventType { return EvtCandidateSelect }

// CandidateHoverPayload hover 索引变化。Index = -1 表示离开 hover。
// TooltipBelowY/AboveY 沿用旧 OnHoverChange 语义, 由渲染端按候选框命中区计算。
type CandidateHoverPayload struct {
	Index         int32
	TooltipX      int32
	TooltipBelowY int32
	TooltipAboveY int32
}

func (CandidateHoverPayload) isEventPayload()      {}
func (CandidateHoverPayload) EventType() EventType { return EvtCandidateHover }

// CandidateContextMenuAction 候选词右键菜单动作。
type CandidateContextMenuAction string

const (
	CandidateActionMoveUp       CandidateContextMenuAction = "move_up"
	CandidateActionMoveDown     CandidateContextMenuAction = "move_down"
	CandidateActionMoveTop      CandidateContextMenuAction = "move_top"
	CandidateActionDelete       CandidateContextMenuAction = "delete"
	CandidateActionResetDefault CandidateContextMenuAction = "reset_default"
	CandidateActionCopy         CandidateContextMenuAction = "copy"
)

// CandidateContextMenuPayload 用户在候选词右键菜单选了某项。
type CandidateContextMenuPayload struct {
	Index  int32
	Action CandidateContextMenuAction
}

func (CandidateContextMenuPayload) isEventPayload()      {}
func (CandidateContextMenuPayload) EventType() EventType { return EvtCandidateContextMenu }

// PageUpPayload 用户翻上一页 (按钮点击或快捷键)。
type PageUpPayload struct{}

func (PageUpPayload) isEventPayload()      {}
func (PageUpPayload) EventType() EventType { return EvtPageUp }

// PageDownPayload 用户翻下一页。
type PageDownPayload struct{}

func (PageDownPayload) isEventPayload()      {}
func (PageDownPayload) EventType() EventType { return EvtPageDown }

// CandidateDragEndPayload 用户完成候选框拖动 (x, y = 候选框左上角屏幕坐标)。
type CandidateDragEndPayload struct {
	X int32
	Y int32
}

func (CandidateDragEndPayload) isEventPayload()      {}
func (CandidateDragEndPayload) EventType() EventType { return EvtCandidateDragEnd }

// MenuItemSelectedPayload 用户在统一菜单选了某项。
// SessionID 与 MenuShowPayload.SessionID 对应, 用于回路由 callback。
type MenuItemSelectedPayload struct {
	SessionID uint64
	ItemID    int32
}

func (MenuItemSelectedPayload) isEventPayload()      {}
func (MenuItemSelectedPayload) EventType() EventType { return EvtMenuItemSelected }

// ToolbarClickAction 工具栏点击动作。
type ToolbarClickAction string

const (
	ToolbarActionToggleMode  ToolbarClickAction = "toggle_mode"
	ToolbarActionToggleWidth ToolbarClickAction = "toggle_width"
	ToolbarActionTogglePunct ToolbarClickAction = "toggle_punct"
	ToolbarActionOpenMenu    ToolbarClickAction = "open_menu" // 右键请求菜单
	ToolbarActionDragEnd     ToolbarClickAction = "drag_end"
)

// ToolbarClickPayload 工具栏点击事件。
// X/Y 对 ToolbarActionDragEnd 是新位置, 对 ToolbarActionOpenMenu 是请求菜单的屏幕坐标。
type ToolbarClickPayload struct {
	Action ToolbarClickAction
	X      int32
	Y      int32
}

func (ToolbarClickPayload) isEventPayload()      {}
func (ToolbarClickPayload) EventType() EventType { return EvtToolbarClick }

// HotkeyTriggeredPayload 全局快捷键触发。
// Command 是 HotkeyEntry.Command 字段值, Go 服务端按此分发。
type HotkeyTriggeredPayload struct {
	Command string
}

func (HotkeyTriggeredPayload) isEventPayload()      {}
func (HotkeyTriggeredPayload) EventType() EventType { return EvtHotkeyTriggered }
