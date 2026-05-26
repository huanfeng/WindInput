package uicmd

// MenuItem 统一菜单的单项 (wire 形态)。
//
// 设计要点:
//   - ID 是协议层的"菜单项 ID", 与 internal/ui.UnifiedMenuToggle* 等常量一致。
//   - Label 为渲染后的文案 (含计数标签如 "拼", "五" 等), 渲染端不再二次格式化。
//   - Children 表达子菜单层级 (例如主题风格三级菜单)。
//   - Type 用 string 标记特殊样式: "separator"/"normal"/"checkable"/"radio"。
//   - Checked 与 Type=="checkable"/"radio" 联用。
//   - Disabled 用于禁用项。
type MenuItem struct {
	ID       int32
	Label    string
	Type     string
	Checked  bool
	Disabled bool
	Children []MenuItem
}

// MenuShowPayload 显示统一右键菜单。
//
// SessionID 由 Go 服务端在每次发起 ShowMenu 时分配, 上行的 EvtMenuItemSelected
// 会携带同一个 SessionID, Go 端用它路由回正确的 callback。
// 这解决了原 UICommand.MenuCallback 函数指针无法跨进程的问题。
type MenuShowPayload struct {
	SessionID uint64
	ScreenX   int
	ScreenY   int
	FlipRefY  int // 翻转参考 Y; 0 = 禁用
	Items     []MenuItem
}

func (MenuShowPayload) isPayload()               {}
func (MenuShowPayload) CommandType() CommandType { return CmdMenuShow }

// MenuHidePayload 隐藏统一菜单。
type MenuHidePayload struct{}

func (MenuHidePayload) isPayload()               {}
func (MenuHidePayload) CommandType() CommandType { return CmdMenuHide }

// ToolbarMenuHidePayload 隐藏工具栏右键菜单。
type ToolbarMenuHidePayload struct{}

func (ToolbarMenuHidePayload) isPayload()               {}
func (ToolbarMenuHidePayload) CommandType() CommandType { return CmdToolbarMenuHide }

// CandidateMenuHidePayload 隐藏候选词右键菜单。
type CandidateMenuHidePayload struct{}

func (CandidateMenuHidePayload) isPayload()               {}
func (CandidateMenuHidePayload) CommandType() CommandType { return CmdCandidateMenuHide }

// ============================================================================
// marshal / unmarshal
// ============================================================================

func writeMenuItem(w *binWriter, m MenuItem) error {
	w.writeI32(m.ID)
	if err := w.writeString(m.Label); err != nil {
		return err
	}
	if err := w.writeString(m.Type); err != nil {
		return err
	}
	w.writeBool(m.Checked)
	w.writeBool(m.Disabled)
	w.writeU32(uint32(len(m.Children)))
	for _, c := range m.Children {
		if err := writeMenuItem(w, c); err != nil {
			return err
		}
	}
	return nil
}

func readMenuItem(r *binReader, m *MenuItem) error {
	var err error
	if m.ID, err = r.readI32(); err != nil {
		return err
	}
	if m.Label, err = r.readString(); err != nil {
		return err
	}
	if m.Type, err = r.readString(); err != nil {
		return err
	}
	if m.Checked, err = r.readBool(); err != nil {
		return err
	}
	if m.Disabled, err = r.readBool(); err != nil {
		return err
	}
	n, err := r.readU32()
	if err != nil {
		return err
	}
	if n > 0 {
		m.Children = make([]MenuItem, n)
		for i := range m.Children {
			if err := readMenuItem(r, &m.Children[i]); err != nil {
				return err
			}
		}
	}
	return nil
}

func (p MenuShowPayload) marshal(w *binWriter) error {
	w.writeU64(p.SessionID)
	w.writeI32(int32(p.ScreenX))
	w.writeI32(int32(p.ScreenY))
	w.writeI32(int32(p.FlipRefY))
	w.writeU32(uint32(len(p.Items)))
	for _, it := range p.Items {
		if err := writeMenuItem(w, it); err != nil {
			return err
		}
	}
	return nil
}

func (p *MenuShowPayload) unmarshal(r *binReader) error {
	var err error
	if p.SessionID, err = r.readU64(); err != nil {
		return err
	}
	var v int32
	for _, dst := range []*int{&p.ScreenX, &p.ScreenY, &p.FlipRefY} {
		if v, err = r.readI32(); err != nil {
			return err
		}
		*dst = int(v)
	}
	n, err := r.readU32()
	if err != nil {
		return err
	}
	if n > 0 {
		p.Items = make([]MenuItem, n)
		for i := range p.Items {
			if err := readMenuItem(r, &p.Items[i]); err != nil {
				return err
			}
		}
	}
	return nil
}
