package uicmd

import (
	"errors"
	"fmt"
)

// 编解码错误。
var (
	ErrUnknownCommand = errors.New("uicmd: unknown command type")
	ErrUnknownEvent   = errors.New("uicmd: unknown event type")
	ErrEmptyPayload   = errors.New("uicmd: payload is nil")
)

// wire 布局 (不含 framing, framing 由 bridge 帧负责):
//
// Command:
//   [cmdType:uint16][session:uint64][payload bytes...]
//
// Event:
//   [evtType:uint16][payload bytes...]
//
// Payload 内部布局见各 payload 文件中的 marshal/unmarshal 实现。

const commandHeaderSize = 2 + 8 // cmdType + session
const eventHeaderSize = 2       // evtType

// EncodeCommand 序列化命令为字节流。
// payload 必须非 nil; 空 payload 类型 (如 CandidatesHidePayload{}) 也是有效值。
func EncodeCommand(c Command) ([]byte, error) {
	if c.Payload == nil {
		return nil, ErrEmptyPayload
	}
	if c.Payload.CommandType() != c.Type {
		return nil, fmt.Errorf("uicmd: command type %s does not match payload %s",
			c.Type, c.Payload.CommandType())
	}
	w := newBinWriter(commandHeaderSize + 32)
	w.writeU16(uint16(c.Type))
	w.writeU64(c.Session)
	if err := marshalPayload(w, c.Payload); err != nil {
		return nil, err
	}
	return w.Bytes(), nil
}

// DecodeCommand 反序列化命令字节流。
func DecodeCommand(buf []byte) (Command, error) {
	if len(buf) < commandHeaderSize {
		return Command{}, errBufUnderflow
	}
	r := newBinReader(buf)
	typ, _ := r.readU16()
	session, _ := r.readU64()
	cmdType := CommandType(typ)
	payload, err := unmarshalPayload(r, cmdType)
	if err != nil {
		return Command{}, err
	}
	return Command{Type: cmdType, Session: session, Payload: payload}, nil
}

// EncodeEvent 序列化事件为字节流。
func EncodeEvent(e Event) ([]byte, error) {
	if e.Payload == nil {
		return nil, ErrEmptyPayload
	}
	if e.Payload.EventType() != e.Type {
		return nil, fmt.Errorf("uicmd: event type %s does not match payload %s",
			e.Type, e.Payload.EventType())
	}
	w := newBinWriter(eventHeaderSize + 16)
	w.writeU16(uint16(e.Type))
	if err := marshalEventPayload(w, e.Payload); err != nil {
		return nil, err
	}
	return w.Bytes(), nil
}

// DecodeEvent 反序列化事件字节流。
func DecodeEvent(buf []byte) (Event, error) {
	if len(buf) < eventHeaderSize {
		return Event{}, errBufUnderflow
	}
	r := newBinReader(buf)
	typ, _ := r.readU16()
	evtType := EventType(typ)
	payload, err := unmarshalEventPayload(r, evtType)
	if err != nil {
		return Event{}, err
	}
	return Event{Type: evtType, Payload: payload}, nil
}

// ============================================================================
// payload marshal/unmarshal 分发
// ============================================================================

func marshalPayload(w *binWriter, p Payload) error {
	switch v := p.(type) {
	case CandidatesShowPayload:
		return v.marshal(w)
	case CandidatesHidePayload:
		return nil
	case CandidatesPositionPayload:
		return v.marshal(w)
	case CandidatesMarkersPayload:
		return v.marshal(w)
	case CandidatesConfigPayload:
		return v.marshal(w)
	case CandidatesPinStatePayload:
		return v.marshal(w)
	case ToolbarShowPayload:
		return v.marshal(w)
	case ToolbarHidePayload:
		return nil
	case ToolbarUpdatePayload:
		return v.marshal(w)
	case StatusShowPayload:
		return v.marshal(w)
	case StatusHidePayload:
		return nil
	case StatusConfigPayload:
		return v.marshal(w)
	case ModeShowPayload:
		return v.marshal(w)
	case TooltipShowPayload:
		return v.marshal(w)
	case TooltipHidePayload:
		return nil
	case ToastShowPayload:
		return v.marshal(w)
	case ToastHidePayload:
		return nil
	case MenuShowPayload:
		return v.marshal(w)
	case MenuHidePayload:
		return nil
	case ToolbarMenuHidePayload:
		return nil
	case CandidateMenuHidePayload:
		return nil
	case ThemeApplyPayload:
		return v.marshal(w)
	case ConfigUpdatePayload:
		return v.marshal(w)
	case HotkeysRegisterPayload:
		return v.marshal(w)
	case HotkeysUnregisterPayload:
		return nil
	case SettingsOpenPayload:
		return v.marshal(w)
	case DPIChangedPayload:
		return nil
	case KeyTapPayload:
		return v.marshal(w)
	case KeyHoldPayload:
		return v.marshal(w)
	case KeyReleasePayload:
		return v.marshal(w)
	case KeySeqPayload:
		return v.marshal(w)
	case KeyTypePayload:
		return v.marshal(w)
	default:
		return fmt.Errorf("uicmd: unsupported payload type %T", p)
	}
}

func unmarshalPayload(r *binReader, typ CommandType) (Payload, error) {
	switch typ {
	case CmdCandidatesShow:
		var p CandidatesShowPayload
		return p, p.unmarshal(r)
	case CmdCandidatesHide:
		return CandidatesHidePayload{}, nil
	case CmdCandidatesPosition:
		var p CandidatesPositionPayload
		return p, p.unmarshal(r)
	case CmdCandidatesMarkers:
		var p CandidatesMarkersPayload
		return p, p.unmarshal(r)
	case CmdCandidatesConfig:
		var p CandidatesConfigPayload
		return p, p.unmarshal(r)
	case CmdCandidatesPinState:
		var p CandidatesPinStatePayload
		return p, p.unmarshal(r)
	case CmdToolbarShow:
		var p ToolbarShowPayload
		return p, p.unmarshal(r)
	case CmdToolbarHide:
		return ToolbarHidePayload{}, nil
	case CmdToolbarUpdate:
		var p ToolbarUpdatePayload
		return p, p.unmarshal(r)
	case CmdStatusShow:
		var p StatusShowPayload
		return p, p.unmarshal(r)
	case CmdStatusHide:
		return StatusHidePayload{}, nil
	case CmdStatusConfig:
		var p StatusConfigPayload
		return p, p.unmarshal(r)
	case CmdModeShow:
		var p ModeShowPayload
		return p, p.unmarshal(r)
	case CmdTooltipShow:
		var p TooltipShowPayload
		return p, p.unmarshal(r)
	case CmdTooltipHide:
		return TooltipHidePayload{}, nil
	case CmdToastShow:
		var p ToastShowPayload
		return p, p.unmarshal(r)
	case CmdToastHide:
		return ToastHidePayload{}, nil
	case CmdMenuShow:
		var p MenuShowPayload
		return p, p.unmarshal(r)
	case CmdMenuHide:
		return MenuHidePayload{}, nil
	case CmdToolbarMenuHide:
		return ToolbarMenuHidePayload{}, nil
	case CmdCandidateMenuHide:
		return CandidateMenuHidePayload{}, nil
	case CmdThemeApply:
		var p ThemeApplyPayload
		return p, p.unmarshal(r)
	case CmdConfigUpdate:
		var p ConfigUpdatePayload
		return p, p.unmarshal(r)
	case CmdHotkeysRegister:
		var p HotkeysRegisterPayload
		return p, p.unmarshal(r)
	case CmdHotkeysUnregister:
		return HotkeysUnregisterPayload{}, nil
	case CmdSettingsOpen:
		var p SettingsOpenPayload
		return p, p.unmarshal(r)
	case CmdDPIChanged:
		return DPIChangedPayload{}, nil
	case CmdKeyTap:
		var p KeyTapPayload
		return p, p.unmarshal(r)
	case CmdKeyHold:
		var p KeyHoldPayload
		return p, p.unmarshal(r)
	case CmdKeyRelease:
		var p KeyReleasePayload
		return p, p.unmarshal(r)
	case CmdKeySeq:
		var p KeySeqPayload
		return p, p.unmarshal(r)
	case CmdKeyType:
		var p KeyTypePayload
		return p, p.unmarshal(r)
	default:
		return nil, fmt.Errorf("%w: 0x%04x", ErrUnknownCommand, uint16(typ))
	}
}

func marshalEventPayload(w *binWriter, p EventPayload) error {
	switch v := p.(type) {
	case CandidateSelectPayload:
		w.writeI32(v.Index)
		return nil
	case CandidateHoverPayload:
		w.writeI32(v.Index)
		w.writeI32(v.TooltipX)
		w.writeI32(v.TooltipBelowY)
		w.writeI32(v.TooltipAboveY)
		return nil
	case CandidateContextMenuPayload:
		w.writeI32(v.Index)
		return w.writeString(string(v.Action))
	case PageUpPayload:
		return nil
	case PageDownPayload:
		return nil
	case CandidateDragEndPayload:
		w.writeI32(v.X)
		w.writeI32(v.Y)
		return nil
	case MenuItemSelectedPayload:
		w.writeU64(v.SessionID)
		w.writeI32(v.ItemID)
		return nil
	case ToolbarClickPayload:
		if err := w.writeString(string(v.Action)); err != nil {
			return err
		}
		w.writeI32(v.X)
		w.writeI32(v.Y)
		return nil
	case HotkeyTriggeredPayload:
		return w.writeString(v.Command)
	default:
		return fmt.Errorf("uicmd: unsupported event payload type %T", p)
	}
}

func unmarshalEventPayload(r *binReader, typ EventType) (EventPayload, error) {
	switch typ {
	case EvtCandidateSelect:
		idx, err := r.readI32()
		if err != nil {
			return nil, err
		}
		return CandidateSelectPayload{Index: idx}, nil
	case EvtCandidateHover:
		var p CandidateHoverPayload
		var err error
		if p.Index, err = r.readI32(); err != nil {
			return nil, err
		}
		if p.TooltipX, err = r.readI32(); err != nil {
			return nil, err
		}
		if p.TooltipBelowY, err = r.readI32(); err != nil {
			return nil, err
		}
		if p.TooltipAboveY, err = r.readI32(); err != nil {
			return nil, err
		}
		return p, nil
	case EvtCandidateContextMenu:
		var p CandidateContextMenuPayload
		var err error
		if p.Index, err = r.readI32(); err != nil {
			return nil, err
		}
		act, err := r.readString()
		if err != nil {
			return nil, err
		}
		p.Action = CandidateContextMenuAction(act)
		return p, nil
	case EvtPageUp:
		return PageUpPayload{}, nil
	case EvtPageDown:
		return PageDownPayload{}, nil
	case EvtCandidateDragEnd:
		var p CandidateDragEndPayload
		var err error
		if p.X, err = r.readI32(); err != nil {
			return nil, err
		}
		if p.Y, err = r.readI32(); err != nil {
			return nil, err
		}
		return p, nil
	case EvtMenuItemSelected:
		var p MenuItemSelectedPayload
		var err error
		if p.SessionID, err = r.readU64(); err != nil {
			return nil, err
		}
		if p.ItemID, err = r.readI32(); err != nil {
			return nil, err
		}
		return p, nil
	case EvtToolbarClick:
		var p ToolbarClickPayload
		act, err := r.readString()
		if err != nil {
			return nil, err
		}
		p.Action = ToolbarClickAction(act)
		if p.X, err = r.readI32(); err != nil {
			return nil, err
		}
		if p.Y, err = r.readI32(); err != nil {
			return nil, err
		}
		return p, nil
	case EvtHotkeyTriggered:
		s, err := r.readString()
		if err != nil {
			return nil, err
		}
		return HotkeyTriggeredPayload{Command: s}, nil
	default:
		return nil, fmt.Errorf("%w: 0x%04x", ErrUnknownEvent, uint16(typ))
	}
}
