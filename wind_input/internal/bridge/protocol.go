// Package bridge handles IPC communication with C++ TSF Bridge
package bridge

import "encoding/json"

// RequestType defines the type of request from C++
type RequestType string

const (
	RequestTypeKeyEvent    RequestType = "key_event"
	RequestTypeCaretUpdate RequestType = "caret_update"
	RequestTypeFocusLost   RequestType = "focus_lost"
	RequestTypeToggleMode  RequestType = "toggle_mode"
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
}

// CaretData contains caret position information
type CaretData struct {
	X      int `json:"x"`
	Y      int `json:"y"`
	Height int `json:"height"`
}

// ResponseType defines the type of response to C++
type ResponseType string

const (
	ResponseTypeInsertText        ResponseType = "insert_text"
	ResponseTypeUpdateComposition ResponseType = "update_composition"
	ResponseTypeClearComposition  ResponseType = "clear_composition"
	ResponseTypeAck               ResponseType = "ack"
	ResponseTypeModeChanged       ResponseType = "mode_changed"
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
