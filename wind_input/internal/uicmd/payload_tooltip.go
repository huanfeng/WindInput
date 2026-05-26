package uicmd

// TooltipShowPayload 显示提示气泡 (编码反查等)。
// CenterX/BelowY/AboveY 三参语义沿用 internal/ui.ShowTooltipText:
//   - CenterX: 水平中线屏幕坐标
//   - BelowY:  候选下沿 (首选 tooltip 顶端贴此处)
//   - AboveY:  候选上沿 (下方空间不够时 tooltip 底端贴此处)
type TooltipShowPayload struct {
	Text    string
	CenterX int
	BelowY  int
	AboveY  int
}

func (TooltipShowPayload) isPayload()               {}
func (TooltipShowPayload) CommandType() CommandType { return CmdTooltipShow }

// TooltipHidePayload 隐藏提示气泡。
type TooltipHidePayload struct{}

func (TooltipHidePayload) isPayload()               {}
func (TooltipHidePayload) CommandType() CommandType { return CmdTooltipHide }

func (p TooltipShowPayload) marshal(w *binWriter) error {
	if err := w.writeString(p.Text); err != nil {
		return err
	}
	w.writeI32(int32(p.CenterX))
	w.writeI32(int32(p.BelowY))
	w.writeI32(int32(p.AboveY))
	return nil
}

func (p *TooltipShowPayload) unmarshal(r *binReader) error {
	var err error
	if p.Text, err = r.readString(); err != nil {
		return err
	}
	var v int32
	for _, dst := range []*int{&p.CenterX, &p.BelowY, &p.AboveY} {
		if v, err = r.readI32(); err != nil {
			return err
		}
		*dst = int(v)
	}
	return nil
}
