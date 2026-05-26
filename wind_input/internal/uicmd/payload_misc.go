package uicmd

// SettingsOpenPayload 请求渲染端打开设置窗口。
// Page 为可选的指定页面 (如 "about"), 空 = 默认页。
type SettingsOpenPayload struct {
	Page string
}

func (SettingsOpenPayload) isPayload()               {}
func (SettingsOpenPayload) CommandType() CommandType { return CmdSettingsOpen }

func (p SettingsOpenPayload) marshal(w *binWriter) error {
	return w.writeString(p.Page)
}

func (p *SettingsOpenPayload) unmarshal(r *binReader) error {
	s, err := r.readString()
	if err != nil {
		return err
	}
	p.Page = s
	return nil
}

// DPIChangedPayload DPI 变更通知 (Windows 专有, 空 payload)。
// macOS 端不会收到此命令 (系统自动处理 retina 缩放), 但保留命令类型便于
// 平台无关层统一调用。
type DPIChangedPayload struct{}

func (DPIChangedPayload) isPayload()               {}
func (DPIChangedPayload) CommandType() CommandType { return CmdDPIChanged }
