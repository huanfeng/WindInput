package uicmd

// SettingsOpenPayload 请求渲染端打开设置窗口。
// Page 为可选的指定页面 (如 "about"), 空 = 默认页。
// WebMode=true 时向 wind_setting.exe 追加 --web 参数，直接打开 Web 版设置界面。
type SettingsOpenPayload struct {
	Page    string
	WebMode bool
}

func (SettingsOpenPayload) isPayload()               {}
func (SettingsOpenPayload) CommandType() CommandType { return CmdSettingsOpen }

func (p SettingsOpenPayload) marshal(w *binWriter) error {
	if err := w.writeString(p.Page); err != nil {
		return err
	}
	w.writeBool(p.WebMode)
	return nil
}

func (p *SettingsOpenPayload) unmarshal(r *binReader) error {
	s, err := r.readString()
	if err != nil {
		return err
	}
	p.Page = s
	// WebMode 是后加字段，旧消息流可能没有此字节；EOF 时默认 false。
	if !r.eof() {
		b, err := r.readBool()
		if err != nil {
			return err
		}
		p.WebMode = b
	}
	return nil
}

// DPIChangedPayload DPI 变更通知 (Windows 专有, 空 payload)。
// macOS 端不会收到此命令 (系统自动处理 retina 缩放), 但保留命令类型便于
// 平台无关层统一调用。
type DPIChangedPayload struct{}

func (DPIChangedPayload) isPayload()               {}
func (DPIChangedPayload) CommandType() CommandType { return CmdDPIChanged }
