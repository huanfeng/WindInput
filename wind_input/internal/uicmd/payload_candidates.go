package uicmd

// CandidatesShowPayload 显示候选框的完整数据。
type CandidatesShowPayload struct {
	Candidates          []Candidate
	Input               string // 拼音/编码原文
	CursorPos           int    // Input 内光标位置 (按 rune)
	CaretX              int    // 屏幕坐标 (光标点)
	CaretY              int
	CaretHeight         int
	Page                int
	TotalPages          int
	TotalCandidateCount int
	CandidatesPerPage   int
	SelectedIndex       int // 当前页内选中的候选索引 (0-based)
}

func (CandidatesShowPayload) isPayload()               {}
func (CandidatesShowPayload) CommandType() CommandType { return CmdCandidatesShow }

// CandidatesHidePayload 隐藏候选框 (空 payload, 用 struct 维持类型对称)。
type CandidatesHidePayload struct{}

func (CandidatesHidePayload) isPayload()               {}
func (CandidatesHidePayload) CommandType() CommandType { return CmdCandidatesHide }

// CandidatesPositionPayload 仅更新候选框位置 (光标移动跟随)。
type CandidatesPositionPayload struct {
	X int
	Y int
}

func (CandidatesPositionPayload) isPayload()               {}
func (CandidatesPositionPayload) CommandType() CommandType { return CmdCandidatesPosition }

// CandidatesMarkersPayload 候选框小标记 (模式标签 / accent 边框色)。
// AccentColor 为 nil 表示不显示发光边框。
type CandidatesMarkersPayload struct {
	IsPinyinMode     bool
	IsQuickInputMode bool
	ModeLabel        string
	AccentColor      *Color
}

func (CandidatesMarkersPayload) isPayload()               {}
func (CandidatesMarkersPayload) CommandType() CommandType { return CmdCandidatesMarkers }

// CandidatesConfigPayload 候选框布局/可见性配置。
// 合并自旧 SetCandidateLayout / SetHideCandidateWindow / SetHidePreedit /
// SetPreeditMode / SetPagerDisplayMode / SetCmdbarCandidatePrefix /
// SetMaxCandidateChars / UpdateConfig 等同步 setter。
type CandidatesConfigPayload struct {
	Layout              CandidateLayout
	HideCandidateWindow bool
	HidePreedit         bool
	PreeditMode         PreeditMode
	PagerDisplayMode    PagerDisplayMode
	CmdbarPrefix        string
	MaxCandidateChars   int
	FontSize            float64
	FontFamily          string
}

func (CandidatesConfigPayload) isPayload()               {}
func (CandidatesConfigPayload) CommandType() CommandType { return CmdCandidatesConfig }

// CandidatesPinStatePayload 当前应用的"固定候选位置"记忆。
// PositionsByMonitor: monitor 标识 → (x, y) 像素坐标。
type CandidatesPinStatePayload struct {
	Enabled            bool
	PositionsByMonitor map[string][2]int
}

func (CandidatesPinStatePayload) isPayload()               {}
func (CandidatesPinStatePayload) CommandType() CommandType { return CmdCandidatesPinState }

// ============================================================================
// marshal / unmarshal
// ============================================================================

func (p CandidatesShowPayload) marshal(w *binWriter) error {
	if len(p.Candidates) > 0xFFFFFFFF {
		return errSliceTooLong
	}
	w.writeU32(uint32(len(p.Candidates)))
	for _, c := range p.Candidates {
		if err := marshalCandidate(w, c); err != nil {
			return err
		}
	}
	if err := w.writeString(p.Input); err != nil {
		return err
	}
	w.writeI32(int32(p.CursorPos))
	w.writeI32(int32(p.CaretX))
	w.writeI32(int32(p.CaretY))
	w.writeI32(int32(p.CaretHeight))
	w.writeI32(int32(p.Page))
	w.writeI32(int32(p.TotalPages))
	w.writeI32(int32(p.TotalCandidateCount))
	w.writeI32(int32(p.CandidatesPerPage))
	w.writeI32(int32(p.SelectedIndex))
	return nil
}

func (p *CandidatesShowPayload) unmarshal(r *binReader) error {
	n, err := r.readU32()
	if err != nil {
		return err
	}
	if n > 0 {
		p.Candidates = make([]Candidate, n)
		for i := range p.Candidates {
			if err := unmarshalCandidate(r, &p.Candidates[i]); err != nil {
				return err
			}
		}
	}
	if p.Input, err = r.readString(); err != nil {
		return err
	}
	var v int32
	for _, dst := range []*int{&p.CursorPos, &p.CaretX, &p.CaretY, &p.CaretHeight,
		&p.Page, &p.TotalPages, &p.TotalCandidateCount, &p.CandidatesPerPage, &p.SelectedIndex} {
		if v, err = r.readI32(); err != nil {
			return err
		}
		*dst = int(v)
	}
	return nil
}

func (p CandidatesPositionPayload) marshal(w *binWriter) error {
	w.writeI32(int32(p.X))
	w.writeI32(int32(p.Y))
	return nil
}

func (p *CandidatesPositionPayload) unmarshal(r *binReader) error {
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

func (p CandidatesMarkersPayload) marshal(w *binWriter) error {
	w.writeBool(p.IsPinyinMode)
	w.writeBool(p.IsQuickInputMode)
	if err := w.writeString(p.ModeLabel); err != nil {
		return err
	}
	w.writeOptColor(p.AccentColor)
	return nil
}

func (p *CandidatesMarkersPayload) unmarshal(r *binReader) error {
	var err error
	if p.IsPinyinMode, err = r.readBool(); err != nil {
		return err
	}
	if p.IsQuickInputMode, err = r.readBool(); err != nil {
		return err
	}
	if p.ModeLabel, err = r.readString(); err != nil {
		return err
	}
	if p.AccentColor, err = r.readOptColor(); err != nil {
		return err
	}
	return nil
}

func (p CandidatesConfigPayload) marshal(w *binWriter) error {
	if err := w.writeString(string(p.Layout)); err != nil {
		return err
	}
	w.writeBool(p.HideCandidateWindow)
	w.writeBool(p.HidePreedit)
	if err := w.writeString(string(p.PreeditMode)); err != nil {
		return err
	}
	if err := w.writeString(string(p.PagerDisplayMode)); err != nil {
		return err
	}
	if err := w.writeString(p.CmdbarPrefix); err != nil {
		return err
	}
	w.writeI32(int32(p.MaxCandidateChars))
	w.writeF64(p.FontSize)
	return w.writeString(p.FontFamily)
}

func (p *CandidatesConfigPayload) unmarshal(r *binReader) error {
	var err error
	var s string
	if s, err = r.readString(); err != nil {
		return err
	}
	p.Layout = CandidateLayout(s)
	if p.HideCandidateWindow, err = r.readBool(); err != nil {
		return err
	}
	if p.HidePreedit, err = r.readBool(); err != nil {
		return err
	}
	if s, err = r.readString(); err != nil {
		return err
	}
	p.PreeditMode = PreeditMode(s)
	if s, err = r.readString(); err != nil {
		return err
	}
	p.PagerDisplayMode = PagerDisplayMode(s)
	if p.CmdbarPrefix, err = r.readString(); err != nil {
		return err
	}
	v, err := r.readI32()
	if err != nil {
		return err
	}
	p.MaxCandidateChars = int(v)
	if p.FontSize, err = r.readF64(); err != nil {
		return err
	}
	if p.FontFamily, err = r.readString(); err != nil {
		return err
	}
	return nil
}

func (p CandidatesPinStatePayload) marshal(w *binWriter) error {
	w.writeBool(p.Enabled)
	w.writeU32(uint32(len(p.PositionsByMonitor)))
	for k, v := range p.PositionsByMonitor {
		if err := w.writeString(k); err != nil {
			return err
		}
		w.writeI32(int32(v[0]))
		w.writeI32(int32(v[1]))
	}
	return nil
}

func (p *CandidatesPinStatePayload) unmarshal(r *binReader) error {
	var err error
	if p.Enabled, err = r.readBool(); err != nil {
		return err
	}
	n, err := r.readU32()
	if err != nil {
		return err
	}
	if n > 0 {
		p.PositionsByMonitor = make(map[string][2]int, n)
		for i := uint32(0); i < n; i++ {
			k, err := r.readString()
			if err != nil {
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
			p.PositionsByMonitor[k] = [2]int{int(x), int(y)}
		}
	}
	return nil
}

// marshalCandidate / unmarshalCandidate 是 Candidate 的 wire 编解码。
func marshalCandidate(w *binWriter, c Candidate) error {
	if err := w.writeString(c.Text); err != nil {
		return err
	}
	if err := w.writeString(c.Code); err != nil {
		return err
	}
	if err := w.writeString(c.Comment); err != nil {
		return err
	}
	w.writeI32(int32(c.Index))
	if err := w.writeString(c.IndexLabel); err != nil {
		return err
	}
	if err := w.writeString(c.Source); err != nil {
		return err
	}
	// Flags 打包到一个 uint8 中, 节省字节并方便扩展。
	var flags uint8
	if c.IsCommon {
		flags |= 1 << 0
	}
	if c.IsPhrase {
		flags |= 1 << 1
	}
	if c.IsCommand {
		flags |= 1 << 2
	}
	if c.IsGroup {
		flags |= 1 << 3
	}
	if c.IsGroupMember {
		flags |= 1 << 4
	}
	if c.HasShadow {
		flags |= 1 << 5
	}
	w.writeU8(flags)
	return nil
}

func unmarshalCandidate(r *binReader, c *Candidate) error {
	var err error
	if c.Text, err = r.readString(); err != nil {
		return err
	}
	if c.Code, err = r.readString(); err != nil {
		return err
	}
	if c.Comment, err = r.readString(); err != nil {
		return err
	}
	idx, err := r.readI32()
	if err != nil {
		return err
	}
	c.Index = int(idx)
	if c.IndexLabel, err = r.readString(); err != nil {
		return err
	}
	if c.Source, err = r.readString(); err != nil {
		return err
	}
	flags, err := r.readU8()
	if err != nil {
		return err
	}
	c.IsCommon = flags&(1<<0) != 0
	c.IsPhrase = flags&(1<<1) != 0
	c.IsCommand = flags&(1<<2) != 0
	c.IsGroup = flags&(1<<3) != 0
	c.IsGroupMember = flags&(1<<4) != 0
	c.HasShadow = flags&(1<<5) != 0
	return nil
}
