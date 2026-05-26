package uicmd

// EventType 是 UI 上行事件的类型标识 (渲染后端 → Go 服务)。
type EventType uint16

// 上行事件 cmd id (0x07xx 段)
const (
	// --- 候选框交互 (0x0701 ~ 0x070F) ---
	EvtCandidateSelect      EventType = 0x0701 // 用户点选某个候选词
	EvtCandidateHover       EventType = 0x0702 // hover 切换
	EvtCandidateContextMenu EventType = 0x0703 // 候选词右键菜单某项 (移动/删除/置顶...)
	EvtPageUp               EventType = 0x0704
	EvtPageDown             EventType = 0x0705
	EvtCandidateDragEnd     EventType = 0x0706

	// --- 菜单点选 (0x0710 ~ 0x071F) ---
	EvtMenuItemSelected EventType = 0x0710

	// --- 工具栏 (0x0720 ~ 0x072F) ---
	EvtToolbarClick EventType = 0x0720

	// --- 快捷键 (0x0730 ~ 0x073F) ---
	EvtHotkeyTriggered EventType = 0x0730
)

// EventPayload 是所有事件 payload 的标记接口。
type EventPayload interface {
	isEventPayload()
	EventType() EventType
}

// Event 是上行事件信封。
type Event struct {
	Type    EventType
	Payload EventPayload
}

// NewEvent 构造一个事件并校验 payload 与 type 一致。
func NewEvent(typ EventType, payload EventPayload) Event {
	if payload != nil && payload.EventType() != typ {
		panic("uicmd: event type mismatch: " + typ.String() + " vs payload " + payload.EventType().String())
	}
	return Event{Type: typ, Payload: payload}
}

// String 返回事件类型的可读名称。
func (t EventType) String() string {
	if name, ok := eventNames[t]; ok {
		return name
	}
	return "uicmd.UnknownEvent"
}

var eventNames = map[EventType]string{
	EvtCandidateSelect:      "candidate.select",
	EvtCandidateHover:       "candidate.hover",
	EvtCandidateContextMenu: "candidate.context_menu",
	EvtPageUp:               "page.up",
	EvtPageDown:             "page.down",
	EvtCandidateDragEnd:     "candidate.drag_end",
	EvtMenuItemSelected:     "menu.item_selected",
	EvtToolbarClick:         "toolbar.click",
	EvtHotkeyTriggered:      "hotkey.triggered",
}
