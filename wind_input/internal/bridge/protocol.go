// Package bridge handles IPC communication with C++ TSF Bridge
package bridge

import "encoding/json"

// RequestType defines the type of request from C++
type RequestType string

const (
	RequestTypeKeyEvent       RequestType = "key_event"
	RequestTypeCaretUpdate    RequestType = "caret_update"
	RequestTypeFocusLost      RequestType = "focus_lost"
	RequestTypeFocusGained    RequestType = "focus_gained"    // 输入法获取焦点
	RequestTypeIMEDeactivated RequestType = "ime_deactivated" // 输入法被切换走
	RequestTypeToggleMode     RequestType = "toggle_mode"
	RequestTypeCapsLockState  RequestType = "caps_lock_state"
	RequestTypeMenuCommand    RequestType = "menu_command"    // 菜单命令
	RequestTypeToolbarAction  RequestType = "toolbar_action"  // 工具栏动作
)

// Request from C++ TSF Bridge
type Request struct {
	Type RequestType     `json:"type"`
	Data json.RawMessage `json:"data"`
}

// KeyEventData contains key event information
type KeyEventData struct {
	Key       string `json:"key"`
	KeyCode   int    `json:"keycode"`
	Modifiers int    `json:"modifiers"`
	Event     string `json:"event"` // "down" or "up"
	// Caret position (optional, sent with key events to avoid separate caret_update)
	Caret *CaretData `json:"caret,omitempty"`
}

// CaretData contains caret position information
type CaretData struct {
	X      int `json:"x"`
	Y      int `json:"y"`
	Height int `json:"height"`
}

// CapsLockData contains Caps Lock state
type CapsLockData struct {
	CapsLockOn bool `json:"caps_lock_on"`
}

// ResponseType defines the type of response to C++
type ResponseType string

const (
	ResponseTypeInsertText        ResponseType = "insert_text"
	ResponseTypeUpdateComposition ResponseType = "update_composition"
	ResponseTypeClearComposition  ResponseType = "clear_composition"
	ResponseTypeAck               ResponseType = "ack"
	ResponseTypeModeChanged       ResponseType = "mode_changed"
	ResponseTypeStatusUpdate      ResponseType = "status_update" // 状态更新响应
	ResponseTypeConsumed          ResponseType = "consumed"      // 按键被消费，不产生输出
)

// Response to C++ TSF Bridge
type Response struct {
	Type  ResponseType `json:"type"`
	Data  interface{}  `json:"data,omitempty"`
	Error string       `json:"error,omitempty"`
}

// InsertTextData for inserting final text
type InsertTextData struct {
	Text string `json:"text"`
}

// CompositionData for updating composition text (pinyin display)
type CompositionData struct {
	Text     string `json:"text"`
	CaretPos int    `json:"caret_pos"`
}

// ModeChangedData for mode toggle response
type ModeChangedData struct {
	ChineseMode bool `json:"chinese_mode"`
}

// MenuCommandData for menu command requests
type MenuCommandData struct {
	Command string `json:"command"` // toggle_mode, toggle_width, toggle_punct, open_settings, toggle_toolbar
}

// ToolbarActionData for toolbar action requests
type ToolbarActionData struct {
	Action string `json:"action"` // click, drag_start, drag_end
	X      int    `json:"x,omitempty"`
	Y      int    `json:"y,omitempty"`
	Button string `json:"button,omitempty"` // mode, width, punct, settings
}

// StatusUpdateData for status update response
type StatusUpdateData struct {
	ChineseMode        bool `json:"chinese_mode"`
	FullWidth          bool `json:"full_width"`
	ChinesePunctuation bool `json:"chinese_punctuation"`
	ToolbarVisible     bool `json:"toolbar_visible"`
	CapsLock           bool `json:"caps_lock"`
}
