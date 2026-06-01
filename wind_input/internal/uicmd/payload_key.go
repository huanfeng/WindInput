package uicmd

// payload_key.go — 命令直通车按键合成命令的 payload (仅 darwin 通路使用)。
//
// Key/Modifiers 为 internal/keyinject.Parse 解析后的规范形态:
//   - Key:       规范键名, 如 "a" / "enter" / "left" / "home" / "vk:0x5D"
//   - Modifiers: {"ctrl","shift","alt","win"} 子集 (mac 上 win→Command)
//
// `.app` 据键名查 CGKeyCode 表、据修饰键查 CGEventFlags 合成事件。Go 服务侧
// 不直接合成 (无 GUI 事件上下文), 见 cmd/service/forwarder_darwin.go。

// KeyTapPayload — 单次按键组合 (key.tap)。
type KeyTapPayload struct {
	Key       string
	Modifiers []string
}

func (KeyTapPayload) isPayload()               {}
func (KeyTapPayload) CommandType() CommandType { return CmdKeyTap }

func (p KeyTapPayload) marshal(w *binWriter) error { return marshalKeyCombo(w, p.Key, p.Modifiers) }
func (p *KeyTapPayload) unmarshal(r *binReader) error {
	var err error
	p.Key, p.Modifiers, err = unmarshalKeyCombo(r)
	return err
}

// KeyHoldPayload — 按下并保持 (key.hold)。
type KeyHoldPayload struct {
	Key       string
	Modifiers []string
}

func (KeyHoldPayload) isPayload()               {}
func (KeyHoldPayload) CommandType() CommandType { return CmdKeyHold }

func (p KeyHoldPayload) marshal(w *binWriter) error { return marshalKeyCombo(w, p.Key, p.Modifiers) }
func (p *KeyHoldPayload) unmarshal(r *binReader) error {
	var err error
	p.Key, p.Modifiers, err = unmarshalKeyCombo(r)
	return err
}

// KeyReleasePayload — 抬起之前 hold 的组合 (key.release)。
type KeyReleasePayload struct {
	Key       string
	Modifiers []string
}

func (KeyReleasePayload) isPayload()               {}
func (KeyReleasePayload) CommandType() CommandType { return CmdKeyRelease }

func (p KeyReleasePayload) marshal(w *binWriter) error { return marshalKeyCombo(w, p.Key, p.Modifiers) }
func (p *KeyReleasePayload) unmarshal(r *binReader) error {
	var err error
	p.Key, p.Modifiers, err = unmarshalKeyCombo(r)
	return err
}

// KeyCombo 是 KeySeqPayload 内的单个组合。
type KeyCombo struct {
	Key       string
	Modifiers []string
}

// KeySeqPayload — 顺序多个按键组合 (key.seq)。
type KeySeqPayload struct {
	Combos []KeyCombo
}

func (KeySeqPayload) isPayload()               {}
func (KeySeqPayload) CommandType() CommandType { return CmdKeySeq }

func (p KeySeqPayload) marshal(w *binWriter) error {
	w.writeU32(uint32(len(p.Combos)))
	for _, c := range p.Combos {
		if err := marshalKeyCombo(w, c.Key, c.Modifiers); err != nil {
			return err
		}
	}
	return nil
}

func (p *KeySeqPayload) unmarshal(r *binReader) error {
	n, err := r.readU32()
	if err != nil {
		return err
	}
	p.Combos = make([]KeyCombo, 0, n)
	for i := uint32(0); i < n; i++ {
		var c KeyCombo
		if c.Key, c.Modifiers, err = unmarshalKeyCombo(r); err != nil {
			return err
		}
		p.Combos = append(p.Combos, c)
	}
	return nil
}

// KeyTypePayload — Unicode 文本上屏 (key.type)。
type KeyTypePayload struct {
	Text string
}

func (KeyTypePayload) isPayload()               {}
func (KeyTypePayload) CommandType() CommandType { return CmdKeyType }

func (p KeyTypePayload) marshal(w *binWriter) error { return w.writeString(p.Text) }
func (p *KeyTypePayload) unmarshal(r *binReader) error {
	var err error
	p.Text, err = r.readString()
	return err
}

// marshalKeyCombo 写 key(string) + modCount(u32) + modCount×(string)。
func marshalKeyCombo(w *binWriter, key string, mods []string) error {
	if err := w.writeString(key); err != nil {
		return err
	}
	w.writeU32(uint32(len(mods)))
	for _, m := range mods {
		if err := w.writeString(m); err != nil {
			return err
		}
	}
	return nil
}

// unmarshalKeyCombo 读 key + modifiers, 与 marshalKeyCombo 对称。
func unmarshalKeyCombo(r *binReader) (key string, mods []string, err error) {
	if key, err = r.readString(); err != nil {
		return "", nil, err
	}
	n, err := r.readU32()
	if err != nil {
		return "", nil, err
	}
	mods = make([]string, 0, n)
	for i := uint32(0); i < n; i++ {
		m, err := r.readString()
		if err != nil {
			return "", nil, err
		}
		mods = append(mods, m)
	}
	return key, mods, nil
}
