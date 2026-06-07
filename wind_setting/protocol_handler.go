package main

import (
	wailsRuntime "github.com/wailsapp/wails/v2/pkg/runtime"
)

// ProtocolImportPayload 是投递给前端的协议导入负载（含解析成功/失败）。
type ProtocolImportPayload struct {
	OK      bool             `json:"ok"`
	Error   string           `json:"error,omitempty"`
	Request *ProtocolRequest `json:"request,omitempty"`
}

// ProtocolRegStatus 协议注册状态（供设置页展示）。
type ProtocolRegStatus struct {
	Registered bool   `json:"registered"`
	Command    string `json:"command"`
	Managed    bool   `json:"managed"` // true=系统托管(macOS)，前端只读
}

func buildProtocolPayload(raw string) *ProtocolImportPayload {
	req, err := ParseProtocolURL(raw)
	if err != nil {
		return &ProtocolImportPayload{OK: false, Error: err.Error()}
	}
	return &ProtocolImportPayload{OK: true, Request: req}
}

// handleProtocolURL 解析协议链接，缓存为 pending 并（若前端就绪）emit 事件。
func (a *App) handleProtocolURL(raw string) {
	payload := buildProtocolPayload(raw)
	a.pendingMu.Lock()
	a.pendingProtocol = payload
	a.pendingMu.Unlock()
	if a.ctx != nil {
		wailsRuntime.EventsEmit(a.ctx, "protocol-import", payload)
	}
}

// ConsumePendingProtocol 前端 onMounted 主动拉取并清空缓存（Wails 导出）。
func (a *App) ConsumePendingProtocol() *ProtocolImportPayload {
	a.pendingMu.Lock()
	defer a.pendingMu.Unlock()
	p := a.pendingProtocol
	a.pendingProtocol = nil
	return p
}

// GetProtocolStatus 返回协议注册状态（Wails 导出）。
func (a *App) GetProtocolStatus() ProtocolRegStatus {
	reg, cmd := ProtocolStatus()
	return ProtocolRegStatus{Registered: reg, Command: cmd, Managed: protocolManagedBySystem}
}

// SetProtocolRegistered 注册/注销协议（Wails 导出，macOS 上为 no-op）。
func (a *App) SetProtocolRegistered(enabled bool) error {
	if enabled {
		return RegisterProtocol()
	}
	return UnregisterProtocol()
}
