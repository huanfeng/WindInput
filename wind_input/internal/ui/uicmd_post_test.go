//go:build windows

package ui

import (
	"image/color"
	"io"
	"log/slog"
	"reflect"
	"testing"

	"github.com/huanfeng/wind_input/internal/uicmd"
	"github.com/huanfeng/wind_input/pkg/config"
)

// newSnapshotTestManager 构造仅含 channel + logger 的最小 Manager,
// 不调用 NewManager (避免触发 CreateEvent 系统调用与子窗口构造)。
// snapshot helper 与 wrap callback 都只读 Manager 字段 / 写 channel,
// 用最小化 Manager 即可严密覆盖。
func newSnapshotTestManager() *Manager {
	return &Manager{
		logger:  slog.New(slog.NewTextHandler(io.Discard, nil)),
		cmdCh:   make(chan uicmdItem, 16),
		eventCh: make(chan uicmd.Event, 16),
	}
}

// ---------- snapshot helper ----------

func TestSnapshotCandidatesMarkers(t *testing.T) {
	m := newSnapshotTestManager()
	m.isPinyinMode = true
	m.isQuickInputMode = true
	m.modeLabel = "临时拼音"
	m.modeAccentColor = color.RGBA{R: 200, G: 50, B: 50, A: 255}

	cmd := m.snapshotCandidatesMarkers()
	if cmd.Type != uicmd.CmdCandidatesMarkers {
		t.Fatalf("cmd type = %s, want CmdCandidatesMarkers", cmd.Type)
	}
	p := cmd.Payload.(uicmd.CandidatesMarkersPayload)
	if !p.IsPinyinMode || !p.IsQuickInputMode {
		t.Errorf("modes not propagated: %+v", p)
	}
	if p.ModeLabel != "临时拼音" {
		t.Errorf("ModeLabel = %q, want 临时拼音", p.ModeLabel)
	}
	if p.AccentColor == nil {
		t.Fatal("AccentColor not propagated (nil)")
	}
	want := &uicmd.Color{R: 200, G: 50, B: 50, A: 255}
	if *p.AccentColor != *want {
		t.Errorf("AccentColor = %+v, want %+v", *p.AccentColor, *want)
	}
}

func TestSnapshotCandidatesMarkers_NilColor(t *testing.T) {
	m := newSnapshotTestManager()
	m.modeAccentColor = nil
	p := m.snapshotCandidatesMarkers().Payload.(uicmd.CandidatesMarkersPayload)
	if p.AccentColor != nil {
		t.Errorf("nil color must propagate as nil, got %+v", p.AccentColor)
	}
}

func TestSnapshotCandidatesConfig(t *testing.T) {
	m := newSnapshotTestManager()
	m.hideCandidateWindow = true
	m.hidePreedit = true
	m.preeditMode = config.PreeditMode("inline")
	m.pagerDisplayMode = config.PagerDisplayMode("always")
	m.cmdbarPrefix = "⚡"
	m.maxCandidateChars = 8

	cmd := m.snapshotCandidatesConfig()
	if cmd.Type != uicmd.CmdCandidatesConfig {
		t.Fatalf("cmd type = %s, want CmdCandidatesConfig", cmd.Type)
	}
	p := cmd.Payload.(uicmd.CandidatesConfigPayload)
	if !p.HideCandidateWindow || !p.HidePreedit {
		t.Errorf("hide flags not propagated: %+v", p)
	}
	if string(p.PreeditMode) != "inline" {
		t.Errorf("PreeditMode = %q, want inline", p.PreeditMode)
	}
	if string(p.PagerDisplayMode) != "always" {
		t.Errorf("PagerDisplayMode = %q, want always", p.PagerDisplayMode)
	}
	if p.CmdbarPrefix != "⚡" || p.MaxCandidateChars != 8 {
		t.Errorf("CmdbarPrefix/MaxCandidateChars not propagated: %+v", p)
	}
	// Layout 来自 renderer.GetLayout(); 我们没构造 renderer, 它是 nil 不会写 payload.Layout
	// 这正符合 stub 测试约定 — 仅 hidden 字段被验证, Layout 留空。
}

func TestSnapshotCandidatesPinState(t *testing.T) {
	m := newSnapshotTestManager()
	m.appPinEnabled = true
	m.appPinPositions = map[string][2]int{
		"\\\\.\\DISPLAY1": {100, 200},
		"\\\\.\\DISPLAY2": {-50, 75},
	}
	cmd := m.snapshotCandidatesPinState()
	p := cmd.Payload.(uicmd.CandidatesPinStatePayload)
	if !p.Enabled {
		t.Error("Enabled not propagated")
	}
	if !reflect.DeepEqual(p.PositionsByMonitor, m.appPinPositions) {
		t.Errorf("PositionsByMonitor mismatch:\nwant %#v\ngot  %#v", m.appPinPositions, p.PositionsByMonitor)
	}

	// 验证 deep copy: 修改源 map 不应影响已 snapshot 的 payload
	m.appPinPositions["\\\\.\\DISPLAY1"] = [2]int{999, 999}
	if p.PositionsByMonitor["\\\\.\\DISPLAY1"] == [2]int{999, 999} {
		t.Error("snapshot did not deep-copy positions map; mutation leaked")
	}
}

// ---------- wrap callback: 双流并行 ----------

func TestWrapCandidateCallbacks_DualStream(t *testing.T) {
	m := newSnapshotTestManager()

	// 记录原 callback 是否被调用 + 收到什么参数
	var origIndex int
	origCalled := false
	wrapped := m.wrapCandidateCallbacks(&CandidateCallback{
		OnSelect: func(idx int) { origCalled = true; origIndex = idx },
	})
	if wrapped == nil {
		t.Fatal("wrap returned nil for non-nil input")
	}

	// 触发包装后的 OnSelect
	wrapped.OnSelect(3)

	// 流 A: 原 callback 必须被调用 (Win 业务路径不变)
	if !origCalled {
		t.Error("original callback not invoked")
	}
	if origIndex != 3 {
		t.Errorf("original callback received index %d, want 3", origIndex)
	}

	// 流 B: Events() 通道必须收到镜像事件 (macOS forwarder 路径)
	select {
	case evt := <-m.eventCh:
		if evt.Type != uicmd.EvtCandidateSelect {
			t.Errorf("evt.Type = %s, want EvtCandidateSelect", evt.Type)
		}
		if p := evt.Payload.(uicmd.CandidateSelectPayload); p.Index != 3 {
			t.Errorf("evt payload Index = %d, want 3", p.Index)
		}
	default:
		t.Error("eventCh did not receive mirror event")
	}
}

func TestWrapCandidateCallbacks_NilOrigStillEmitsEvent(t *testing.T) {
	// 调用方未注册某个 callback (字段 nil), 包装仍应推 Event (forwarder 不依赖业务回调)。
	m := newSnapshotTestManager()
	wrapped := m.wrapCandidateCallbacks(&CandidateCallback{
		// OnSelect 故意不设
	})
	wrapped.OnSelect(7) // 不应 panic

	select {
	case evt := <-m.eventCh:
		if p := evt.Payload.(uicmd.CandidateSelectPayload); p.Index != 7 {
			t.Errorf("evt Index = %d, want 7", p.Index)
		}
	default:
		t.Error("eventCh did not receive event for nil-orig callback")
	}
}

func TestWrapCandidateCallbacks_NilInput(t *testing.T) {
	m := newSnapshotTestManager()
	if got := m.wrapCandidateCallbacks(nil); got != nil {
		t.Errorf("wrap(nil) = %v, want nil", got)
	}
}

func TestWrapCandidateCallbacks_ContextMenuActions(t *testing.T) {
	// 6 个 context menu action 共用 wrapCandidateAction 工厂, 抽查 3 个 + 验证 action 字符串正确。
	m := newSnapshotTestManager()
	var lastIdx int
	wrapped := m.wrapCandidateCallbacks(&CandidateCallback{
		OnMoveUp: func(i int) { lastIdx = i },
		OnDelete: func(i int) { lastIdx = i },
		OnCopy:   func(i int) { lastIdx = i },
	})

	cases := []struct {
		invoke func(int)
		want   uicmd.CandidateContextMenuAction
	}{
		{wrapped.OnMoveUp, uicmd.CandidateActionMoveUp},
		{wrapped.OnDelete, uicmd.CandidateActionDelete},
		{wrapped.OnCopy, uicmd.CandidateActionCopy},
	}
	for i, c := range cases {
		lastIdx = 0
		c.invoke(i + 1)
		if lastIdx != i+1 {
			t.Errorf("case %d: original callback not called or wrong arg", i)
		}
		select {
		case evt := <-m.eventCh:
			p := evt.Payload.(uicmd.CandidateContextMenuPayload)
			if p.Action != c.want {
				t.Errorf("case %d: action = %q, want %q", i, p.Action, c.want)
			}
			if int(p.Index) != i+1 {
				t.Errorf("case %d: index = %d, want %d", i, p.Index, i+1)
			}
		default:
			t.Errorf("case %d: no event emitted", i)
		}
	}
}

func TestWrapToolbarCallbacks_ContextMenuActions(t *testing.T) {
	// ToolbarContextMenuAction int → uicmd.ToolbarClickAction string 的 3-way 映射
	m := newSnapshotTestManager()
	var lastAction ToolbarContextMenuAction
	wrapped := m.wrapToolbarCallbacks(&ToolbarCallback{
		OnContextMenu: func(a ToolbarContextMenuAction) { lastAction = a },
	})

	cases := []struct {
		in   ToolbarContextMenuAction
		want uicmd.ToolbarClickAction
	}{
		{ToolbarMenuSettings, uicmd.ToolbarActionContextSettings},
		{ToolbarMenuRestartService, uicmd.ToolbarActionContextRestart},
		{ToolbarMenuAbout, uicmd.ToolbarActionContextAbout},
	}
	for _, c := range cases {
		wrapped.OnContextMenu(c.in)
		if lastAction != c.in {
			t.Errorf("orig callback action = %v, want %v", lastAction, c.in)
		}
		select {
		case evt := <-m.eventCh:
			p := evt.Payload.(uicmd.ToolbarClickPayload)
			if p.Action != c.want {
				t.Errorf("wire action = %q, want %q", p.Action, c.want)
			}
		default:
			t.Errorf("no event for action %v", c.in)
		}
	}
}

func TestWrapHotkeyCallback(t *testing.T) {
	m := newSnapshotTestManager()
	var lastCmd string
	wrapped := m.wrapHotkeyCallback(func(cmd string) { lastCmd = cmd })
	wrapped("toggle_mode")

	if lastCmd != "toggle_mode" {
		t.Errorf("orig callback not called or got %q", lastCmd)
	}
	select {
	case evt := <-m.eventCh:
		if evt.Type != uicmd.EvtHotkeyTriggered {
			t.Errorf("evt.Type = %s, want EvtHotkeyTriggered", evt.Type)
		}
		if p := evt.Payload.(uicmd.HotkeyTriggeredPayload); p.Command != "toggle_mode" {
			t.Errorf("evt cmd = %q, want toggle_mode", p.Command)
		}
	default:
		t.Error("no hotkey event emitted")
	}
}

func TestWrapHotkeyCallback_NilOrig(t *testing.T) {
	m := newSnapshotTestManager()
	wrapped := m.wrapHotkeyCallback(nil)
	wrapped("noop_cmd") // 不应 panic

	select {
	case evt := <-m.eventCh:
		if p := evt.Payload.(uicmd.HotkeyTriggeredPayload); p.Command != "noop_cmd" {
			t.Errorf("evt cmd = %q, want noop_cmd", p.Command)
		}
	default:
		t.Error("no hotkey event emitted for nil-orig")
	}
}

func TestEventChannelOverflowDoesNotBlock(t *testing.T) {
	// eventCh 容量 16 (newSnapshotTestManager); 推 100 次必须满 16 后丢弃, 不能阻塞。
	m := newSnapshotTestManager()
	wrapped := m.wrapHotkeyCallback(nil)
	for i := 0; i < 100; i++ {
		wrapped("burst")
	}
	// 排空 channel 后, 必定有 push 被丢弃 (16 < 100)
	count := 0
	for {
		select {
		case <-m.eventCh:
			count++
		default:
			goto done
		}
	}
done:
	if count == 0 {
		t.Error("no events captured")
	}
	if count >= 100 {
		t.Errorf("buffer should have caused drops, got %d events for 100 pushes", count)
	}
}
