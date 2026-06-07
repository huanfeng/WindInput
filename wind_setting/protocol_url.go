package main

import (
	"fmt"
	"net/url"
	"strings"
)

// ProtocolRequest 是 windinput:// 协议解析后的结构化请求。
type ProtocolRequest struct {
	Kind string `json:"kind"`           // theme | schema | dict | extdict
	URL  string `json:"url"`            // 待下载的 https 直链
	Name string `json:"name,omitempty"` // 可选显示名（不可信，仅作提示）
}

// validProtocolKinds 支持的导入类型集合（本期仅 theme 实现 UI，其余预留）。
var validProtocolKinds = map[string]bool{
	"theme": true, "schema": true, "dict": true, "extdict": true,
}

// ParseProtocolURL 解析 windinput://import/<kind>?url=...&name=...。
// 入口安全收紧：url 参数必须为 https。
func ParseProtocolURL(raw string) (*ProtocolRequest, error) {
	u, err := url.Parse(strings.TrimSpace(raw))
	if err != nil {
		return nil, fmt.Errorf("无法解析链接: %w", err)
	}
	if !strings.EqualFold(u.Scheme, "windinput") {
		return nil, fmt.Errorf("不支持的协议: %s", u.Scheme)
	}
	if action := strings.ToLower(u.Host); action != "import" {
		return nil, fmt.Errorf("不支持的操作: %s", action)
	}
	kind := strings.ToLower(strings.Trim(u.Path, "/"))
	if !validProtocolKinds[kind] {
		return nil, fmt.Errorf("不支持的导入类型: %s", kind)
	}
	q := u.Query()
	target := strings.TrimSpace(q.Get("url"))
	if target == "" {
		return nil, fmt.Errorf("缺少 url 参数")
	}
	if !strings.HasPrefix(strings.ToLower(target), "https://") {
		return nil, fmt.Errorf("url 参数必须是 https 链接")
	}
	return &ProtocolRequest{Kind: kind, URL: target, Name: strings.TrimSpace(q.Get("name"))}, nil
}
