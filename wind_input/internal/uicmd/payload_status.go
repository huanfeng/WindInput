package uicmd

// StatusState 状态指示器内容 (镜像 internal/ui.StatusState)。
type StatusState struct {
	ModeLabel  string
	PunctLabel string
	WidthLabel string
}

// StatusShowPayload 显示状态指示器。
type StatusShowPayload struct {
	State StatusState
	X     int
	Y     int
}

func (StatusShowPayload) isPayload()               {}
func (StatusShowPayload) CommandType() CommandType { return CmdStatusShow }

// StatusHidePayload 隐藏状态指示器。
type StatusHidePayload struct{}

func (StatusHidePayload) isPayload()               {}
func (StatusHidePayload) CommandType() CommandType { return CmdStatusHide }

// StatusConfigPayload 状态指示器运行时配置 (镜像 internal/ui.StatusWindowConfig)。
type StatusConfigPayload struct {
	Enabled         bool
	DisplayMode     StatusDisplayMode
	Duration        int32
	SchemaNameStyle string
	ShowMode        bool
	ShowPunct       bool
	ShowFullWidth   bool
	PositionMode    StatusPositionMode
	OffsetX         int32
	OffsetY         int32
	CustomX         int32
	CustomY         int32
	FontSize        float64
	Opacity         float64
	BackgroundColor string // "#RRGGBB"
	TextColor       string
	BorderRadius    float64
}

func (StatusConfigPayload) isPayload()               {}
func (StatusConfigPayload) CommandType() CommandType { return CmdStatusConfig }

// ModeShowPayload 显示短暂模式浮窗 (cmdMode)。
type ModeShowPayload struct {
	Mode string
	X    int
	Y    int
}

func (ModeShowPayload) isPayload()               {}
func (ModeShowPayload) CommandType() CommandType { return CmdModeShow }

// ============================================================================
// marshal / unmarshal
// ============================================================================

func writeStatusState(w *binWriter, s StatusState) error {
	if err := w.writeString(s.ModeLabel); err != nil {
		return err
	}
	if err := w.writeString(s.PunctLabel); err != nil {
		return err
	}
	return w.writeString(s.WidthLabel)
}

func readStatusState(r *binReader, s *StatusState) error {
	var err error
	if s.ModeLabel, err = r.readString(); err != nil {
		return err
	}
	if s.PunctLabel, err = r.readString(); err != nil {
		return err
	}
	if s.WidthLabel, err = r.readString(); err != nil {
		return err
	}
	return nil
}

func (p StatusShowPayload) marshal(w *binWriter) error {
	if err := writeStatusState(w, p.State); err != nil {
		return err
	}
	w.writeI32(int32(p.X))
	w.writeI32(int32(p.Y))
	return nil
}

func (p *StatusShowPayload) unmarshal(r *binReader) error {
	if err := readStatusState(r, &p.State); err != nil {
		return err
	}
	x, err := r.readI32()
	if err != nil {
		return err
	}
	y, err := r.readI32()
	if err != nil {
		return err
	}
	p.X, p.Y = int(x), int(y)
	return nil
}

func (p StatusConfigPayload) marshal(w *binWriter) error {
	w.writeBool(p.Enabled)
	if err := w.writeString(string(p.DisplayMode)); err != nil {
		return err
	}
	w.writeI32(p.Duration)
	if err := w.writeString(p.SchemaNameStyle); err != nil {
		return err
	}
	w.writeBool(p.ShowMode)
	w.writeBool(p.ShowPunct)
	w.writeBool(p.ShowFullWidth)
	if err := w.writeString(string(p.PositionMode)); err != nil {
		return err
	}
	w.writeI32(p.OffsetX)
	w.writeI32(p.OffsetY)
	w.writeI32(p.CustomX)
	w.writeI32(p.CustomY)
	w.writeF64(p.FontSize)
	w.writeF64(p.Opacity)
	if err := w.writeString(p.BackgroundColor); err != nil {
		return err
	}
	if err := w.writeString(p.TextColor); err != nil {
		return err
	}
	w.writeF64(p.BorderRadius)
	return nil
}

func (p *StatusConfigPayload) unmarshal(r *binReader) error {
	var err error
	var s string
	if p.Enabled, err = r.readBool(); err != nil {
		return err
	}
	if s, err = r.readString(); err != nil {
		return err
	}
	p.DisplayMode = StatusDisplayMode(s)
	if p.Duration, err = r.readI32(); err != nil {
		return err
	}
	if p.SchemaNameStyle, err = r.readString(); err != nil {
		return err
	}
	if p.ShowMode, err = r.readBool(); err != nil {
		return err
	}
	if p.ShowPunct, err = r.readBool(); err != nil {
		return err
	}
	if p.ShowFullWidth, err = r.readBool(); err != nil {
		return err
	}
	if s, err = r.readString(); err != nil {
		return err
	}
	p.PositionMode = StatusPositionMode(s)
	if p.OffsetX, err = r.readI32(); err != nil {
		return err
	}
	if p.OffsetY, err = r.readI32(); err != nil {
		return err
	}
	if p.CustomX, err = r.readI32(); err != nil {
		return err
	}
	if p.CustomY, err = r.readI32(); err != nil {
		return err
	}
	if p.FontSize, err = r.readF64(); err != nil {
		return err
	}
	if p.Opacity, err = r.readF64(); err != nil {
		return err
	}
	if p.BackgroundColor, err = r.readString(); err != nil {
		return err
	}
	if p.TextColor, err = r.readString(); err != nil {
		return err
	}
	if p.BorderRadius, err = r.readF64(); err != nil {
		return err
	}
	return nil
}

func (p ModeShowPayload) marshal(w *binWriter) error {
	if err := w.writeString(p.Mode); err != nil {
		return err
	}
	w.writeI32(int32(p.X))
	w.writeI32(int32(p.Y))
	return nil
}

func (p *ModeShowPayload) unmarshal(r *binReader) error {
	var err error
	if p.Mode, err = r.readString(); err != nil {
		return err
	}
	x, err := r.readI32()
	if err != nil {
		return err
	}
	y, err := r.readI32()
	if err != nil {
		return err
	}
	p.X, p.Y = int(x), int(y)
	return nil
}
