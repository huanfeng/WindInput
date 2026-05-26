package ui

import (
	"reflect"
	"testing"

	"github.com/huanfeng/wind_input/internal/uicmd"
)

// uicmd_convert_test.go 覆盖 ui ↔ uicmd 双向映射的正确性。
//
// 这些函数承担"Win 端业务类型与 wire 镜像之间的翻译"职责, 任何字段漂移都会
// 在 macOS forwarder 接入时引发"看似工作但 IMKit 端读出垃圾值"的隐蔽问题。
// roundtrip + 字段级断言两道验证。

func TestToolbarStateRoundtrip(t *testing.T) {
	cases := []ToolbarState{
		{},
		{ChineseMode: true, FullWidth: true, ModeLabel: "拼"},
		{CapsLock: true, ChinesePunct: true, EffectiveMode: 2, ModeLabel: "五"},
	}
	for i, in := range cases {
		wire := toUIToolbarState(in)
		got := fromUIToolbarState(wire)
		if !reflect.DeepEqual(got, in) {
			t.Errorf("case %d: roundtrip mismatch\nwant %#v\ngot  %#v", i, in, got)
		}
		// 字段级断言 (防止类型扩展时漏字段)
		if wire.EffectiveMode != int32(in.EffectiveMode) {
			t.Errorf("case %d: EffectiveMode int32 cast lost data", i)
		}
		if wire.ModeLabel != in.ModeLabel {
			t.Errorf("case %d: ModeLabel mismatch", i)
		}
	}
}

func TestStatusStateConvert(t *testing.T) {
	in := StatusState{ModeLabel: "中", PunctLabel: "，", WidthLabel: "全"}
	wire := toUIStatusState(in)
	if wire.ModeLabel != in.ModeLabel || wire.PunctLabel != in.PunctLabel || wire.WidthLabel != in.WidthLabel {
		t.Errorf("StatusState conversion lost fields: %#v vs %#v", wire, in)
	}
}

func TestHotkeyEntriesRoundtrip(t *testing.T) {
	in := []GlobalHotkeyEntry{
		{ID: 1, Modifiers: 0x0002 /* ctrl */, VK: 0x20 /* space */, Command: "toggle_mode"},
		{ID: 2, Modifiers: 0x0008 /* win */, VK: 0x49 /* I */, Command: "open_settings"},
	}
	wire := toUIHotkeyEntries(in)
	got := fromUIHotkeyEntries(wire)
	if !reflect.DeepEqual(got, in) {
		t.Errorf("HotkeyEntries roundtrip mismatch\nwant %#v\ngot  %#v", in, got)
	}
	// 验证 ID/Modifiers/VK/Command 各字段没有混线
	if wire[0].ID != int32(in[0].ID) {
		t.Errorf("ID cast: want %d, got %d", in[0].ID, wire[0].ID)
	}
	if wire[0].Mods != in[0].Modifiers {
		t.Errorf("Modifiers ↔ Mods name mapping broke: want %d, got %d", in[0].Modifiers, wire[0].Mods)
	}
	if wire[0].KeyCode != in[0].VK {
		t.Errorf("VK ↔ KeyCode name mapping broke: want %d, got %d", in[0].VK, wire[0].KeyCode)
	}
}

func TestHotkeyEntriesEmpty(t *testing.T) {
	if got := toUIHotkeyEntries(nil); got != nil {
		t.Errorf("toUIHotkeyEntries(nil) = %v, want nil", got)
	}
	if got := fromUIHotkeyEntries(nil); got != nil {
		t.Errorf("fromUIHotkeyEntries(nil) = %v, want nil", got)
	}
}

func TestToastLevelRoundtrip(t *testing.T) {
	// 关键: ui.ToastLevel 是 int (iota), uicmd.ToastLevel 是 string ("info"/"success"/...);
	// 不能直接 cast, 必须走 switch 映射。这里验证全部 4 个值不漂移。
	cases := []struct {
		ui   ToastLevel
		wire uicmd.ToastLevel
	}{
		{ToastInfo, uicmd.ToastInfo},
		{ToastSuccess, uicmd.ToastSuccess},
		{ToastWarn, uicmd.ToastWarn},
		{ToastError, uicmd.ToastError},
	}
	for _, c := range cases {
		if got := toUIToastLevel(c.ui); got != c.wire {
			t.Errorf("toUIToastLevel(%v) = %q, want %q", c.ui, got, c.wire)
		}
		if got := fromUIToastLevel(c.wire); got != c.ui {
			t.Errorf("fromUIToastLevel(%q) = %v, want %v", c.wire, got, c.ui)
		}
	}
}

func TestToastLevelUnknownFallback(t *testing.T) {
	// 未识别值应回退到 Info (与 wire 兼容性设计一致)
	if got := fromUIToastLevel(uicmd.ToastLevel("garbage")); got != ToastInfo {
		t.Errorf("fromUIToastLevel(garbage) = %v, want ToastInfo", got)
	}
}

func TestToastPositionMapping(t *testing.T) {
	// ToastPosition: int iota → string wire, ToastBottomRight 是显式映射,
	// 其它 (Center/TopRight/Top) 当前统一回退到 wire 的 Center (PR-1 wire 设计简化)。
	if got := toUIToastPosition(ToastBottomRight); got != uicmd.ToastBottomRight {
		t.Errorf("ToastBottomRight wire = %q, want %q", got, uicmd.ToastBottomRight)
	}
	if got := toUIToastPosition(ToastCenter); got != uicmd.ToastCenter {
		t.Errorf("ToastCenter wire = %q, want %q", got, uicmd.ToastCenter)
	}
	// ToastTopRight/Top 当前未独立列出 wire 值, 回退到 Center 是设计预期
	if got := toUIToastPosition(ToastTopRight); got != uicmd.ToastCenter {
		t.Errorf("ToastTopRight wire = %q, want %q (fallback to Center)", got, uicmd.ToastCenter)
	}
	// 反向: BottomRight wire → BottomRight; 其它 → Center
	if got := fromUIToastPosition(uicmd.ToastBottomRight); got != ToastBottomRight {
		t.Errorf("fromUIToastPosition(BottomRight) = %v, want ToastBottomRight", got)
	}
	if got := fromUIToastPosition(uicmd.ToastPosition("center")); got != ToastCenter {
		t.Errorf("fromUIToastPosition(center) = %v, want ToastCenter", got)
	}
}
