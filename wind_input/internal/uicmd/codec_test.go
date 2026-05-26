package uicmd

import (
	"errors"
	"reflect"
	"testing"
)

// 每个命令/事件至少一个 roundtrip 测试: encode -> decode 后值应相等。
//
// 测试策略:
//   - 用包含非零字段的"非平凡"值确保编码覆盖所有分支
//   - 字符串含中文 / 空字符串 / 长字符串(>256)
//   - 切片含 0/1/N 项, 至少一个测试用 nil map
//   - 命令类型与 payload 类型不匹配应在 NewCommand/EncodeCommand 报错

func roundtripCommand(t *testing.T, c Command) {
	t.Helper()
	buf, err := EncodeCommand(c)
	if err != nil {
		t.Fatalf("encode %s: %v", c.Type, err)
	}
	got, err := DecodeCommand(buf)
	if err != nil {
		t.Fatalf("decode %s: %v", c.Type, err)
	}
	if got.Type != c.Type {
		t.Errorf("type mismatch: want %s, got %s", c.Type, got.Type)
	}
	if got.Session != c.Session {
		t.Errorf("session mismatch: want %d, got %d", c.Session, got.Session)
	}
	if !reflect.DeepEqual(got.Payload, c.Payload) {
		t.Errorf("payload mismatch for %s:\nwant %#v\ngot  %#v", c.Type, c.Payload, got.Payload)
	}
}

func roundtripEvent(t *testing.T, e Event) {
	t.Helper()
	buf, err := EncodeEvent(e)
	if err != nil {
		t.Fatalf("encode %s: %v", e.Type, err)
	}
	got, err := DecodeEvent(buf)
	if err != nil {
		t.Fatalf("decode %s: %v", e.Type, err)
	}
	if got.Type != e.Type {
		t.Errorf("type mismatch: want %s, got %s", e.Type, got.Type)
	}
	if !reflect.DeepEqual(got.Payload, e.Payload) {
		t.Errorf("payload mismatch for %s:\nwant %#v\ngot  %#v", e.Type, e.Payload, got.Payload)
	}
}

func TestRoundtripCandidatesShow(t *testing.T) {
	red := Color{R: 200, G: 50, B: 50, A: 255}
	cmd := NewCommand(CmdCandidatesShow, 42, CandidatesShowPayload{
		Candidates: []Candidate{
			{Text: "你好", Code: "nh", Comment: "encoding", Index: 1, IsCommon: true},
			{Text: "测试", Code: "cs", Index: 2, IsGroup: true, HasShadow: true},
			{Text: "", Code: "", Index: 0},
		},
		Input:               "nhcs",
		CursorPos:           3,
		CaretX:              100,
		CaretY:              200,
		CaretHeight:         24,
		Page:                1,
		TotalPages:          5,
		TotalCandidateCount: 25,
		CandidatesPerPage:   5,
		SelectedIndex:       2,
	})
	roundtripCommand(t, cmd)
	_ = red // 占位避免未使用
}

func TestRoundtripCandidatesHide(t *testing.T) {
	roundtripCommand(t, NewCommand(CmdCandidatesHide, 1, CandidatesHidePayload{}))
}

func TestRoundtripCandidatesPosition(t *testing.T) {
	roundtripCommand(t, NewCommand(CmdCandidatesPosition, 7, CandidatesPositionPayload{X: -100, Y: 50}))
}

func TestRoundtripCandidatesMarkers(t *testing.T) {
	red := Color{R: 200, G: 50, B: 50, A: 255}
	// 有 AccentColor
	roundtripCommand(t, NewCommand(CmdCandidatesMarkers, 0, CandidatesMarkersPayload{
		IsPinyinMode: true,
		ModeLabel:    "临时拼音",
		AccentColor:  &red,
	}))
	// 无 AccentColor (nil)
	roundtripCommand(t, NewCommand(CmdCandidatesMarkers, 0, CandidatesMarkersPayload{
		IsQuickInputMode: true,
		ModeLabel:        "",
	}))
}

func TestRoundtripCandidatesConfig(t *testing.T) {
	roundtripCommand(t, NewCommand(CmdCandidatesConfig, 0, CandidatesConfigPayload{
		Layout:              CandidateLayoutVertical,
		HideCandidateWindow: true,
		HidePreedit:         false,
		PreeditMode:         "inline",
		PagerDisplayMode:    "always",
		CmdbarPrefix:        "/",
		MaxCandidateChars:   8,
		FontSize:            14.5,
		FontFamily:          "Microsoft YaHei",
	}))
}

func TestRoundtripCandidatesPinState(t *testing.T) {
	roundtripCommand(t, NewCommand(CmdCandidatesPinState, 0, CandidatesPinStatePayload{
		Enabled: true,
		PositionsByMonitor: map[string][2]int{
			"\\\\.\\DISPLAY1": {100, 200},
			"\\\\.\\DISPLAY2": {-50, 75},
		},
	}))
	// 空 map
	roundtripCommand(t, NewCommand(CmdCandidatesPinState, 0, CandidatesPinStatePayload{
		Enabled: false,
	}))
}

func TestRoundtripToolbar(t *testing.T) {
	state := ToolbarState{
		ChineseMode:   true,
		FullWidth:     true,
		EffectiveMode: 0,
		ModeLabel:     "拼",
	}
	roundtripCommand(t, NewCommand(CmdToolbarShow, 0, ToolbarShowPayload{X: 10, Y: 20, State: state}))
	roundtripCommand(t, NewCommand(CmdToolbarHide, 0, ToolbarHidePayload{}))
	roundtripCommand(t, NewCommand(CmdToolbarUpdate, 0, ToolbarUpdatePayload{State: state}))
}

func TestRoundtripStatus(t *testing.T) {
	st := StatusState{ModeLabel: "中", PunctLabel: "，", WidthLabel: "半"}
	roundtripCommand(t, NewCommand(CmdStatusShow, 0, StatusShowPayload{State: st, X: 500, Y: 600}))
	roundtripCommand(t, NewCommand(CmdStatusHide, 0, StatusHidePayload{}))
	roundtripCommand(t, NewCommand(CmdStatusConfig, 0, StatusConfigPayload{
		Enabled:         true,
		DisplayMode:     "temporary",
		Duration:        1500,
		SchemaNameStyle: "short",
		ShowMode:        true,
		ShowPunct:       false,
		ShowFullWidth:   true,
		PositionMode:    "follow_cursor",
		OffsetX:         5,
		OffsetY:         -3,
		FontSize:        12.0,
		Opacity:         0.85,
		BackgroundColor: "#1E1E1E",
		TextColor:       "#FFFFFF",
		BorderRadius:    4.0,
	}))
	roundtripCommand(t, NewCommand(CmdModeShow, 0, ModeShowPayload{Mode: "中", X: 100, Y: 100}))
}

func TestRoundtripTooltip(t *testing.T) {
	roundtripCommand(t, NewCommand(CmdTooltipShow, 0, TooltipShowPayload{
		Text: "编码: ni\n反查: 你", CenterX: 300, BelowY: 400, AboveY: 280,
	}))
	roundtripCommand(t, NewCommand(CmdTooltipHide, 0, TooltipHidePayload{}))
}

func TestRoundtripToast(t *testing.T) {
	roundtripCommand(t, NewCommand(CmdToastShow, 0, ToastShowPayload{
		Title:    "词库就绪",
		Message:  "用户词库加载完成\n共 12345 条",
		Level:    ToastSuccess,
		Position: ToastBottomRight,
		Duration: 3000,
		MaxWidth: 320,
	}))
	roundtripCommand(t, NewCommand(CmdToastHide, 0, ToastHidePayload{}))
	// Duration <0 表示不自动隐藏, MaxWidth=0 表示用默认
	roundtripCommand(t, NewCommand(CmdToastShow, 0, ToastShowPayload{
		Message: "持续提示", Level: ToastError, Duration: -1, MaxWidth: 0,
	}))
}

func TestRoundtripMenu(t *testing.T) {
	items := []MenuItem{
		{ID: 101, Label: "中英切换", Type: "normal"},
		{ID: 102, Label: "分隔", Type: "separator"},
		{ID: 200, Label: "主题", Type: "normal", Children: []MenuItem{
			{ID: 201, Label: "默认", Type: "radio", Checked: true},
			{ID: 202, Label: "深色", Type: "radio"},
		}},
		{ID: 299, Label: "禁用项", Type: "normal", Disabled: true},
	}
	roundtripCommand(t, NewCommand(CmdMenuShow, 0, MenuShowPayload{
		SessionID: 0xDEADBEEFCAFEBABE,
		ScreenX:   500, ScreenY: 400, FlipRefY: 360,
		Items: items,
	}))
	roundtripCommand(t, NewCommand(CmdMenuHide, 0, MenuHidePayload{}))
	roundtripCommand(t, NewCommand(CmdToolbarMenuHide, 0, ToolbarMenuHidePayload{}))
	roundtripCommand(t, NewCommand(CmdCandidateMenuHide, 0, CandidateMenuHidePayload{}))
}

func TestRoundtripTheme(t *testing.T) {
	roundtripCommand(t, NewCommand(CmdThemeApply, 0, ThemeApplyPayload{
		ThemeID: "default",
		Style:   ThemeStyleDark,
		Colors: ThemeColors{
			Background:    Color{R: 30, G: 30, B: 30, A: 255},
			Border:        Color{R: 100, G: 100, B: 100, A: 255},
			Text:          Color{R: 240, G: 240, B: 240, A: 255},
			TextSelected:  Color{R: 255, G: 255, B: 255, A: 255},
			HighlightBg:   Color{R: 60, G: 100, B: 200, A: 255},
			IndexNormal:   Color{R: 180, G: 180, B: 180, A: 255},
			IndexSelected: Color{R: 255, G: 200, B: 0, A: 255},
		},
		Fonts: ThemeFonts{
			Family: "Microsoft YaHei", Size: 14.0,
			CommentSize: 12.0, IndexSize: 10.0, MenuSize: 13.0, StatusSize: 11.0, ToastSize: 13.0,
		},
		Geometry: ThemeGeometry{
			BorderRadius: 6.0, BorderWidth: 1.0, PaddingX: 8.0, PaddingY: 6.0,
			ItemSpacing: 2.0, ShadowRadius: 12.0, ShadowOffset: 2.0, Opacity: 0.95,
		},
		WindowsHints: WindowsRenderHints{
			TextRenderMode: "directwrite", GDIFontWeight: 400, GDIFontScale: 1.0,
			MenuFontWeight: 400, MenuFontScale: 1.0, MenuFontSize: 13.0,
		},
	}))
	roundtripCommand(t, NewCommand(CmdConfigUpdate, 0, ConfigUpdatePayload{
		FontSize: 14.0, FontFamily: "Microsoft YaHei", TooltipDelay: 500, DarkMode: false,
	}))
}

func TestRoundtripHotkeys(t *testing.T) {
	roundtripCommand(t, NewCommand(CmdHotkeysRegister, 0, HotkeysRegisterPayload{
		Entries: []HotkeyEntry{
			{ID: 1, Mods: 0x0002 /*ctrl*/, KeyCode: 0x20 /*space*/, Command: "toggle_mode"},
			{ID: 2, Mods: 0x0008 /*win*/, KeyCode: 0x49 /*I*/, Command: "open_settings"},
		},
	}))
	roundtripCommand(t, NewCommand(CmdHotkeysUnregister, 0, HotkeysUnregisterPayload{}))
}

// ============================================================================
// 事件 roundtrip
// ============================================================================

func TestRoundtripEvents(t *testing.T) {
	roundtripEvent(t, NewEvent(EvtCandidateSelect, CandidateSelectPayload{Index: 3}))
	roundtripEvent(t, NewEvent(EvtCandidateHover, CandidateHoverPayload{
		Index: 2, TooltipX: 150, TooltipBelowY: 200, TooltipAboveY: 100,
	}))
	roundtripEvent(t, NewEvent(EvtCandidateHover, CandidateHoverPayload{Index: -1}))
	roundtripEvent(t, NewEvent(EvtCandidateContextMenu, CandidateContextMenuPayload{
		Index: 1, Action: CandidateActionDelete,
	}))
	roundtripEvent(t, NewEvent(EvtPageUp, PageUpPayload{}))
	roundtripEvent(t, NewEvent(EvtPageDown, PageDownPayload{}))
	roundtripEvent(t, NewEvent(EvtCandidateDragEnd, CandidateDragEndPayload{X: 300, Y: 400}))
	roundtripEvent(t, NewEvent(EvtMenuItemSelected, MenuItemSelectedPayload{
		SessionID: 0xCAFEBABE, ItemID: 101,
	}))
	roundtripEvent(t, NewEvent(EvtToolbarClick, ToolbarClickPayload{
		Action: ToolbarActionToggleMode, X: 0, Y: 0,
	}))
	roundtripEvent(t, NewEvent(EvtHotkeyTriggered, HotkeyTriggeredPayload{Command: "toggle_mode"}))
}

// ============================================================================
// 边角错误测试
// ============================================================================

func TestEncodeNilPayload(t *testing.T) {
	if _, err := EncodeCommand(Command{Type: CmdCandidatesHide, Payload: nil}); !errors.Is(err, ErrEmptyPayload) {
		t.Errorf("want ErrEmptyPayload, got %v", err)
	}
	if _, err := EncodeEvent(Event{Type: EvtPageUp, Payload: nil}); !errors.Is(err, ErrEmptyPayload) {
		t.Errorf("want ErrEmptyPayload, got %v", err)
	}
}

func TestEncodeTypeMismatch(t *testing.T) {
	// Type = CmdCandidatesShow 但 payload 是 ToolbarShowPayload, Encode 应报错
	_, err := EncodeCommand(Command{
		Type:    CmdCandidatesShow,
		Payload: ToolbarShowPayload{},
	})
	if err == nil {
		t.Errorf("want type mismatch error, got nil")
	}
}

func TestNewCommandTypeMismatchPanic(t *testing.T) {
	defer func() {
		if r := recover(); r == nil {
			t.Errorf("want panic on type mismatch, got none")
		}
	}()
	_ = NewCommand(CmdCandidatesShow, 0, ToolbarShowPayload{})
}

func TestDecodeUnknownCommand(t *testing.T) {
	// 构造一个 cmdType=0xFFFF 的最小帧
	buf := []byte{0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0}
	if _, err := DecodeCommand(buf); !errors.Is(err, ErrUnknownCommand) {
		t.Errorf("want ErrUnknownCommand, got %v", err)
	}
}

func TestDecodeUnderflow(t *testing.T) {
	// 短于 header 的帧
	if _, err := DecodeCommand([]byte{0x01, 0x06}); err == nil {
		t.Errorf("want underflow error, got nil")
	}
	if _, err := DecodeEvent([]byte{}); err == nil {
		t.Errorf("want underflow error, got nil")
	}
}

func TestCommandTypeString(t *testing.T) {
	if got := CmdCandidatesShow.String(); got != "candidates.show" {
		t.Errorf("CmdCandidatesShow.String() = %q, want candidates.show", got)
	}
	if got := CommandType(0xFFFF).String(); got != "uicmd.Unknown" {
		t.Errorf("unknown.String() = %q, want uicmd.Unknown", got)
	}
}

func TestEventTypeString(t *testing.T) {
	if got := EvtCandidateSelect.String(); got != "candidate.select" {
		t.Errorf("EvtCandidateSelect.String() = %q", got)
	}
}
