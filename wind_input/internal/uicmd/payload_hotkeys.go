package uicmd

// HotkeysRegisterPayload 注册全局快捷键列表 (替换式: 旧注册先全部撤销)。
type HotkeysRegisterPayload struct {
	Entries []HotkeyEntry
}

func (HotkeysRegisterPayload) isPayload()               {}
func (HotkeysRegisterPayload) CommandType() CommandType { return CmdHotkeysRegister }

// HotkeysUnregisterPayload 撤销所有已注册的全局快捷键。
type HotkeysUnregisterPayload struct{}

func (HotkeysUnregisterPayload) isPayload()               {}
func (HotkeysUnregisterPayload) CommandType() CommandType { return CmdHotkeysUnregister }

func (p HotkeysRegisterPayload) marshal(w *binWriter) error {
	w.writeU32(uint32(len(p.Entries)))
	for _, e := range p.Entries {
		w.writeI32(e.ID)
		w.writeU32(e.Mods)
		w.writeU32(e.KeyCode)
		if err := w.writeString(e.Command); err != nil {
			return err
		}
	}
	return nil
}

func (p *HotkeysRegisterPayload) unmarshal(r *binReader) error {
	n, err := r.readU32()
	if err != nil {
		return err
	}
	if n == 0 {
		return nil
	}
	p.Entries = make([]HotkeyEntry, n)
	for i := range p.Entries {
		if p.Entries[i].ID, err = r.readI32(); err != nil {
			return err
		}
		if p.Entries[i].Mods, err = r.readU32(); err != nil {
			return err
		}
		if p.Entries[i].KeyCode, err = r.readU32(); err != nil {
			return err
		}
		if p.Entries[i].Command, err = r.readString(); err != nil {
			return err
		}
	}
	return nil
}
