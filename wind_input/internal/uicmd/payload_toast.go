package uicmd

// ToastShowPayload 显示 Toast 通知 (镜像 internal/ui.ToastOptions)。
//
// Duration 语义沿用 ToastOptions:
//   - 0       使用默认 5000ms
//   - >0      自动隐藏的毫秒数
//   - <0      不自动隐藏
type ToastShowPayload struct {
	Title    string
	Message  string
	Level    ToastLevel
	Position ToastPosition
	Duration int32
	MaxWidth int32
}

func (ToastShowPayload) isPayload()               {}
func (ToastShowPayload) CommandType() CommandType { return CmdToastShow }

// ToastHidePayload 立即隐藏当前 Toast。
type ToastHidePayload struct{}

func (ToastHidePayload) isPayload()               {}
func (ToastHidePayload) CommandType() CommandType { return CmdToastHide }

func (p ToastShowPayload) marshal(w *binWriter) error {
	if err := w.writeString(p.Title); err != nil {
		return err
	}
	if err := w.writeString(p.Message); err != nil {
		return err
	}
	if err := w.writeString(string(p.Level)); err != nil {
		return err
	}
	if err := w.writeString(string(p.Position)); err != nil {
		return err
	}
	w.writeI32(p.Duration)
	w.writeI32(p.MaxWidth)
	return nil
}

func (p *ToastShowPayload) unmarshal(r *binReader) error {
	var err error
	if p.Title, err = r.readString(); err != nil {
		return err
	}
	if p.Message, err = r.readString(); err != nil {
		return err
	}
	var s string
	if s, err = r.readString(); err != nil {
		return err
	}
	p.Level = ToastLevel(s)
	if s, err = r.readString(); err != nil {
		return err
	}
	p.Position = ToastPosition(s)
	if p.Duration, err = r.readI32(); err != nil {
		return err
	}
	if p.MaxWidth, err = r.readI32(); err != nil {
		return err
	}
	return nil
}
