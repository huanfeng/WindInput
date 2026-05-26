//go:build darwin

package ui

import (
	"image/color"
	"io"
	"log/slog"
	"testing"
	"time"

	"github.com/huanfeng/wind_input/internal/uicmd"
	"github.com/huanfeng/wind_input/pkg/config"
)

// manager_darwin_test.go 验证 darwin Manager stub 的命令投递正确性。
//
// 关键不变量: 凡是 setter / show / hide 调用都必须把对应 uicmd.Command 投递到
// cmdCh, 否则未来 macOS forwarder 无法订阅到完整下行命令流。

func newDarwinTestManager() *Manager {
	m := NewManager(slog.New(slog.NewTextHandler(io.Discard, nil)))
	// 显式 ready 让 ShowCandidates 等 ready 守卫的方法能投递
	m.mu.Lock()
	m.ready = true
	m.mu.Unlock()
	return m
}

// expectCmd 从 cmdCh 拉取下一个命令, 超时则 fail。
func expectCmd(t *testing.T, m *Manager) uicmd.Command {
	t.Helper()
	select {
	case item := <-m.cmdCh:
		return item.Cmd
	case <-time.After(50 * time.Millisecond):
		t.Fatal("no command on cmdCh within timeout")
		return uicmd.Command{}
	}
}

func TestDarwin_ShowCandidates(t *testing.T) {
	m := newDarwinTestManager()
	cands := []Candidate{{Text: "你好", Code: "nh", Index: 1}}
	if err := m.ShowCandidates(cands, "nh", 2, 100, 200, 24, 1, 5, 25, 5, 0); err != nil {
		t.Fatalf("ShowCandidates returned err: %v", err)
	}
	cmd := expectCmd(t, m)
	if cmd.Type != uicmd.CmdCandidatesShow {
		t.Fatalf("type = %s, want CmdCandidatesShow", cmd.Type)
	}
	p := cmd.Payload.(uicmd.CandidatesShowPayload)
	if len(p.Candidates) != 1 || p.Candidates[0].Text != "你好" {
		t.Errorf("candidates not propagated: %+v", p.Candidates)
	}
	if p.Input != "nh" || p.CursorPos != 2 || p.CaretX != 100 || p.CaretY != 200 {
		t.Errorf("anchor/input fields wrong: %+v", p)
	}
}

func TestDarwin_Hide(t *testing.T) {
	m := newDarwinTestManager()
	m.Hide()
	cmd := expectCmd(t, m)
	if cmd.Type != uicmd.CmdCandidatesHide {
		t.Errorf("type = %s, want CmdCandidatesHide", cmd.Type)
	}
}

func TestDarwin_UpdatePosition(t *testing.T) {
	m := newDarwinTestManager()
	m.UpdatePosition(300, 400)
	cmd := expectCmd(t, m)
	p := cmd.Payload.(uicmd.CandidatesPositionPayload)
	if p.X != 300 || p.Y != 400 {
		t.Errorf("position wrong: %+v", p)
	}
}

func TestDarwin_SetPinyinModeEmitsMarkers(t *testing.T) {
	m := newDarwinTestManager()
	m.modeLabel = "临时拼音"
	m.SetPinyinMode(true)
	cmd := expectCmd(t, m)
	if cmd.Type != uicmd.CmdCandidatesMarkers {
		t.Fatalf("type = %s, want CmdCandidatesMarkers", cmd.Type)
	}
	p := cmd.Payload.(uicmd.CandidatesMarkersPayload)
	if !p.IsPinyinMode {
		t.Error("IsPinyinMode not propagated")
	}
	if p.ModeLabel != "临时拼音" {
		t.Errorf("ModeLabel preserved snapshot = %q, want 临时拼音", p.ModeLabel)
	}
}

func TestDarwin_SetModeAccentColor(t *testing.T) {
	m := newDarwinTestManager()
	m.SetModeAccentColor(color.RGBA{R: 200, G: 50, B: 50, A: 255})
	cmd := expectCmd(t, m)
	p := cmd.Payload.(uicmd.CandidatesMarkersPayload)
	if p.AccentColor == nil {
		t.Fatal("AccentColor not propagated")
	}
	if *p.AccentColor != (uicmd.Color{R: 200, G: 50, B: 50, A: 255}) {
		t.Errorf("AccentColor = %+v", *p.AccentColor)
	}
}

func TestDarwin_ConfigSettersEmitConfigSnapshot(t *testing.T) {
	// 多个 config setter 各自投递一次 CmdCandidatesConfig 全量快照
	m := newDarwinTestManager()

	m.SetHideCandidateWindow(true)
	cmd := expectCmd(t, m)
	if cmd.Type != uicmd.CmdCandidatesConfig {
		t.Errorf("SetHideCandidateWindow type = %s", cmd.Type)
	}

	m.SetPreeditMode(config.PreeditMode("top"))
	cmd = expectCmd(t, m)
	p := cmd.Payload.(uicmd.CandidatesConfigPayload)
	if string(p.PreeditMode) != "top" {
		t.Errorf("PreeditMode snapshot = %q", p.PreeditMode)
	}
	if !p.HideCandidateWindow {
		t.Error("previous SetHideCandidateWindow state not preserved in subsequent snapshot")
	}

	m.SetMaxCandidateChars(10)
	cmd = expectCmd(t, m)
	p = cmd.Payload.(uicmd.CandidatesConfigPayload)
	if p.MaxCandidateChars != 10 {
		t.Errorf("MaxCandidateChars = %d", p.MaxCandidateChars)
	}
}

func TestDarwin_SetActiveAppPinState(t *testing.T) {
	m := newDarwinTestManager()
	positions := map[string][2]int{"\\\\.\\DISPLAY1": {100, 200}}
	m.SetActiveAppPinState(true, positions)
	cmd := expectCmd(t, m)
	if cmd.Type != uicmd.CmdCandidatesPinState {
		t.Fatalf("type = %s, want CmdCandidatesPinState", cmd.Type)
	}
	p := cmd.Payload.(uicmd.CandidatesPinStatePayload)
	if !p.Enabled {
		t.Error("Enabled not propagated")
	}
	if p.PositionsByMonitor["\\\\.\\DISPLAY1"] != [2]int{100, 200} {
		t.Errorf("positions not propagated: %+v", p.PositionsByMonitor)
	}
}

func TestDarwin_Toolbar(t *testing.T) {
	m := newDarwinTestManager()
	state := ToolbarState{ChineseMode: true, ModeLabel: "拼", EffectiveMode: 0}

	m.ShowToolbarWithState(10, 20, state)
	cmd := expectCmd(t, m)
	if cmd.Type != uicmd.CmdToolbarShow {
		t.Errorf("type = %s", cmd.Type)
	}
	p := cmd.Payload.(uicmd.ToolbarShowPayload)
	if p.X != 10 || p.Y != 20 {
		t.Errorf("position wrong: %+v", p)
	}
	if !p.State.ChineseMode || p.State.ModeLabel != "拼" {
		t.Errorf("state wrong: %+v", p.State)
	}

	m.UpdateToolbarState(state)
	if expectCmd(t, m).Type != uicmd.CmdToolbarUpdate {
		t.Error("UpdateToolbarState wrong cmd type")
	}

	m.SetToolbarVisible(false)
	if expectCmd(t, m).Type != uicmd.CmdToolbarHide {
		t.Error("SetToolbarVisible(false) wrong cmd type")
	}
}

func TestDarwin_StatusIndicator(t *testing.T) {
	m := newDarwinTestManager()
	m.ShowStatusIndicator(StatusState{ModeLabel: "中", PunctLabel: "，"}, 500, 600)
	cmd := expectCmd(t, m)
	p := cmd.Payload.(uicmd.StatusShowPayload)
	if p.State.ModeLabel != "中" || p.State.PunctLabel != "，" {
		t.Errorf("status state wrong: %+v", p.State)
	}
	if p.X != 500 || p.Y != 600 {
		t.Errorf("position wrong: %+v", p)
	}

	m.HideStatusIndicator()
	if expectCmd(t, m).Type != uicmd.CmdStatusHide {
		t.Error("HideStatusIndicator wrong cmd type")
	}
}

func TestDarwin_Toast(t *testing.T) {
	m := newDarwinTestManager()
	m.ShowToast(ToastOptions{
		Title:    "test",
		Message:  "hello",
		Level:    ToastSuccess,
		Position: ToastBottomRight,
		Duration: 3000,
	})
	cmd := expectCmd(t, m)
	if cmd.Type != uicmd.CmdToastShow {
		t.Fatalf("type = %s", cmd.Type)
	}
	p := cmd.Payload.(uicmd.ToastShowPayload)
	if p.Title != "test" || p.Message != "hello" {
		t.Errorf("toast text wrong: %+v", p)
	}
	if p.Level != uicmd.ToastSuccess {
		t.Errorf("level mapping wrong: got %q", p.Level)
	}
	if p.Position != uicmd.ToastBottomRight {
		t.Errorf("position mapping wrong: got %q", p.Position)
	}
	if p.Duration != 3000 {
		t.Errorf("duration wrong: %d", p.Duration)
	}

	m.HideToast()
	if expectCmd(t, m).Type != uicmd.CmdToastHide {
		t.Error("HideToast wrong cmd type")
	}
}

func TestDarwin_Tooltip(t *testing.T) {
	m := newDarwinTestManager()
	m.ShowTooltipText("hint", 100, 200, 150)
	cmd := expectCmd(t, m)
	p := cmd.Payload.(uicmd.TooltipShowPayload)
	if p.Text != "hint" || p.CenterX != 100 || p.BelowY != 200 || p.AboveY != 150 {
		t.Errorf("tooltip fields wrong: %+v", p)
	}

	m.HideTooltip()
	if expectCmd(t, m).Type != uicmd.CmdTooltipHide {
		t.Error("HideTooltip wrong cmd type")
	}
}

func TestDarwin_TooltipEmptyTextSkipped(t *testing.T) {
	// 空文本不应投递命令 (与 Win 行为一致)
	m := newDarwinTestManager()
	m.ShowTooltipText("", 0, 0, 0)
	select {
	case <-m.cmdCh:
		t.Error("empty tooltip should not emit command")
	case <-time.After(20 * time.Millisecond):
		// expected
	}
}

func TestDarwin_Hotkeys(t *testing.T) {
	m := newDarwinTestManager()
	entries := []GlobalHotkeyEntry{
		{ID: 1, Modifiers: 0x02, VK: 0x20, Command: "toggle_mode"},
	}
	m.RegisterGlobalHotkeys(entries)
	cmd := expectCmd(t, m)
	if cmd.Type != uicmd.CmdHotkeysRegister {
		t.Fatalf("type = %s", cmd.Type)
	}
	p := cmd.Payload.(uicmd.HotkeysRegisterPayload)
	if len(p.Entries) != 1 {
		t.Fatalf("entries len = %d", len(p.Entries))
	}
	if p.Entries[0].Command != "toggle_mode" || p.Entries[0].Mods != 0x02 || p.Entries[0].KeyCode != 0x20 {
		t.Errorf("entry wrong: %+v", p.Entries[0])
	}

	m.UnregisterGlobalHotkeys()
	if expectCmd(t, m).Type != uicmd.CmdHotkeysUnregister {
		t.Error("Unregister wrong cmd type")
	}
}

func TestDarwin_SettingsOpen(t *testing.T) {
	m := newDarwinTestManager()
	m.OpenSettingsWithPage("about")
	cmd := expectCmd(t, m)
	if cmd.Type != uicmd.CmdSettingsOpen {
		t.Fatalf("type = %s", cmd.Type)
	}
	if p := cmd.Payload.(uicmd.SettingsOpenPayload); p.Page != "about" {
		t.Errorf("page = %q, want about", p.Page)
	}
}

func TestDarwin_UnifiedMenuPropagatesSidebandFields(t *testing.T) {
	// 验证 MenuState 与 Callback 通过旁路传递 (跨进程序列化无法承载, Win/darwin 同协议)
	m := newDarwinTestManager()
	called := false
	state := UnifiedMenuState{ChineseMode: true}
	m.ShowUnifiedMenu(100, 200, 180, state, func(id int) { called = true; _ = id })

	select {
	case item := <-m.cmdCh:
		if item.Cmd.Type != uicmd.CmdMenuShow {
			t.Errorf("type = %s", item.Cmd.Type)
		}
		if item.MenuState == nil || !item.MenuState.ChineseMode {
			t.Error("MenuState not propagated via sideband")
		}
		if item.Callback == nil {
			t.Fatal("Callback nil")
		}
		item.Callback(42) // 触发以验证 closure 完整
		if !called {
			t.Error("callback closure not invoked")
		}
	case <-time.After(50 * time.Millisecond):
		t.Fatal("no cmd received")
	}
}

func TestDarwin_StartReadyClose(t *testing.T) {
	m := NewManager(slog.New(slog.NewTextHandler(io.Discard, nil)))
	if m.IsReady() {
		t.Error("should not be ready before Start")
	}
	if err := m.Start(); err != nil {
		t.Fatalf("Start: %v", err)
	}
	if !m.IsReady() {
		t.Error("should be ready after Start")
	}
	// WaitReady 应立即返回 (readyCh 已关闭)
	done := make(chan struct{})
	go func() { m.WaitReady(); close(done) }()
	select {
	case <-done:
	case <-time.After(50 * time.Millisecond):
		t.Error("WaitReady blocked after Start")
	}
}

func TestDarwin_StubReturnDefaults(t *testing.T) {
	// 一些查询/Win 专有函数在 darwin 上必须返回安全默认值, 不能 panic
	m := newDarwinTestManager()
	if m.IsVisible() {
		t.Error("IsVisible should be false on darwin")
	}
	if m.IsCandidateMenuOpen() || m.IsToolbarMenuOpen() || m.IsUnifiedMenuOpen() {
		t.Error("menu-open queries should all be false on darwin")
	}
	if m.IsHostRendering() {
		t.Error("IsHostRendering should be false (no host render on darwin)")
	}
	if x, y := m.GetToolbarPosition(); x != 0 || y != 0 {
		t.Errorf("GetToolbarPosition = (%d, %d), want (0, 0)", x, y)
	}
	if GetCapsLockState() {
		t.Error("GetCapsLockState should be false on darwin stub")
	}
}

func TestDarwin_ParseHotkeyString(t *testing.T) {
	if _, ok := ParseHotkeyString("", 1, "cmd"); ok {
		t.Error("empty string should return ok=false")
	}
	if _, ok := ParseHotkeyString("none", 1, "cmd"); ok {
		t.Error("'none' should return ok=false")
	}
	entry, ok := ParseHotkeyString("ctrl+space", 5, "toggle")
	if !ok {
		t.Fatal("expected ok=true")
	}
	if entry.ID != 5 || entry.Command != "toggle" {
		t.Errorf("entry = %+v", entry)
	}
}
