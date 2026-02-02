// Package control 提供控制管道通信协议
package control

import (
	"encoding/json"
	"fmt"
	"strings"
)

// 管道名称
const PipeName = `\\.\pipe\wind_input_control`

// 命令类型
const (
	CmdPing          = "PING"
	CmdReloadConfig  = "RELOAD_CONFIG"
	CmdReloadPhrases = "RELOAD_PHRASES"
	CmdReloadShadow  = "RELOAD_SHADOW"
	CmdReloadUserDict = "RELOAD_USERDICT"
	CmdReloadAll     = "RELOAD_ALL"
	CmdGetStatus     = "GET_STATUS"
)

// 响应状态
const (
	StatusOK    = "OK"
	StatusError = "ERROR"
	StatusData  = "DATA"
)

// Request 请求结构
type Request struct {
	Command string          `json:"command"`
	Args    json.RawMessage `json:"args,omitempty"`
}

// Response 响应结构
type Response struct {
	Status  string          `json:"status"`
	Message string          `json:"message,omitempty"`
	Data    json.RawMessage `json:"data,omitempty"`
}

// ServiceStatus 服务状态
type ServiceStatus struct {
	Running       bool   `json:"running"`
	EngineType    string `json:"engine_type"`
	ChineseMode   bool   `json:"chinese_mode"`
	FullWidth     bool   `json:"full_width"`
	ChinesePunct  bool   `json:"chinese_punct"`
	DictEntries   int    `json:"dict_entries"`
	UserDictCount int    `json:"user_dict_count"`
	PhraseCount   int    `json:"phrase_count"`
	ShadowCount   int    `json:"shadow_count"`
}

// ParseRequest 解析请求
// 格式: COMMAND [JSON_ARGS]
func ParseRequest(line string) (*Request, error) {
	line = strings.TrimSpace(line)
	if line == "" {
		return nil, fmt.Errorf("empty request")
	}

	parts := strings.SplitN(line, " ", 2)
	cmd := strings.ToUpper(parts[0])

	req := &Request{Command: cmd}

	if len(parts) > 1 {
		args := strings.TrimSpace(parts[1])
		if args != "" {
			req.Args = json.RawMessage(args)
		}
	}

	return req, nil
}

// FormatRequest 格式化请求
func FormatRequest(cmd string, args interface{}) (string, error) {
	if args == nil {
		return cmd, nil
	}

	argsJson, err := json.Marshal(args)
	if err != nil {
		return "", fmt.Errorf("failed to marshal args: %w", err)
	}

	return fmt.Sprintf("%s %s", cmd, string(argsJson)), nil
}

// ParseResponse 解析响应
// 格式: STATUS [JSON_DATA/MESSAGE]
func ParseResponse(line string) (*Response, error) {
	line = strings.TrimSpace(line)
	if line == "" {
		return nil, fmt.Errorf("empty response")
	}

	parts := strings.SplitN(line, " ", 2)
	status := strings.ToUpper(parts[0])

	resp := &Response{Status: status}

	if len(parts) > 1 {
		rest := strings.TrimSpace(parts[1])
		if status == StatusData {
			resp.Data = json.RawMessage(rest)
		} else {
			resp.Message = rest
		}
	}

	return resp, nil
}

// FormatResponse 格式化响应
func FormatResponse(status string, data interface{}) (string, error) {
	if status == StatusOK && data == nil {
		return StatusOK, nil
	}

	if status == StatusError {
		if msg, ok := data.(string); ok {
			return fmt.Sprintf("%s %s", StatusError, msg), nil
		}
		if err, ok := data.(error); ok {
			return fmt.Sprintf("%s %s", StatusError, err.Error()), nil
		}
	}

	if status == StatusData {
		dataJson, err := json.Marshal(data)
		if err != nil {
			return "", fmt.Errorf("failed to marshal data: %w", err)
		}
		return fmt.Sprintf("%s %s", StatusData, string(dataJson)), nil
	}

	return status, nil
}

// OkResponse 返回成功响应
func OkResponse() string {
	return StatusOK
}

// ErrorResponse 返回错误响应
func ErrorResponse(err error) string {
	return fmt.Sprintf("%s %s", StatusError, err.Error())
}

// DataResponse 返回数据响应
func DataResponse(data interface{}) (string, error) {
	return FormatResponse(StatusData, data)
}

// IsOk 检查响应是否成功
func (r *Response) IsOk() bool {
	return r.Status == StatusOK || r.Status == StatusData
}

// GetData 解析响应数据
func (r *Response) GetData(v interface{}) error {
	if r.Data == nil {
		return fmt.Errorf("no data in response")
	}
	return json.Unmarshal(r.Data, v)
}
