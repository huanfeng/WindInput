package uicmd

// ToolbarState 工具栏状态 (镜像 internal/ui.ToolbarState 的 wire 形态)。
type ToolbarState struct {
	ChineseMode   bool
	CapsLock      bool
	FullWidth     bool
	ChinesePunct  bool
	EffectiveMode int32  // 0=Chinese, 1=EnglishLower, 2=EnglishUpper
	ModeLabel     string // Schema icon_label (如 "拼", "五", "双", "混")
}

// ToolbarShowPayload 显示工具栏。
type ToolbarShowPayload struct {
	X     int
	Y     int
	State ToolbarState
}

func (ToolbarShowPayload) isPayload()               {}
func (ToolbarShowPayload) CommandType() CommandType { return CmdToolbarShow }

// ToolbarHidePayload 隐藏工具栏。
type ToolbarHidePayload struct{}

func (ToolbarHidePayload) isPayload()               {}
func (ToolbarHidePayload) CommandType() CommandType { return CmdToolbarHide }

// ToolbarUpdatePayload 更新工具栏状态 (不改可见性)。
type ToolbarUpdatePayload struct {
	State ToolbarState
}

func (ToolbarUpdatePayload) isPayload()               {}
func (ToolbarUpdatePayload) CommandType() CommandType { return CmdToolbarUpdate }

// ============================================================================
// marshal / unmarshal
// ============================================================================

func writeToolbarState(w *binWriter, s ToolbarState) error {
	w.writeBool(s.ChineseMode)
	w.writeBool(s.CapsLock)
	w.writeBool(s.FullWidth)
	w.writeBool(s.ChinesePunct)
	w.writeI32(s.EffectiveMode)
	return w.writeString(s.ModeLabel)
}

func readToolbarState(r *binReader, s *ToolbarState) error {
	var err error
	if s.ChineseMode, err = r.readBool(); err != nil {
		return err
	}
	if s.CapsLock, err = r.readBool(); err != nil {
		return err
	}
	if s.FullWidth, err = r.readBool(); err != nil {
		return err
	}
	if s.ChinesePunct, err = r.readBool(); err != nil {
		return err
	}
	if s.EffectiveMode, err = r.readI32(); err != nil {
		return err
	}
	if s.ModeLabel, err = r.readString(); err != nil {
		return err
	}
	return nil
}

func (p ToolbarShowPayload) marshal(w *binWriter) error {
	w.writeI32(int32(p.X))
	w.writeI32(int32(p.Y))
	return writeToolbarState(w, p.State)
}

func (p *ToolbarShowPayload) unmarshal(r *binReader) error {
	x, err := r.readI32()
	if err != nil {
		return err
	}
	y, err := r.readI32()
	if err != nil {
		return err
	}
	p.X, p.Y = int(x), int(y)
	return readToolbarState(r, &p.State)
}

func (p ToolbarUpdatePayload) marshal(w *binWriter) error {
	return writeToolbarState(w, p.State)
}

func (p *ToolbarUpdatePayload) unmarshal(r *binReader) error {
	return readToolbarState(r, &p.State)
}
