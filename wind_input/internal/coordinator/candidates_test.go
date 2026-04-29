package coordinator

import (
	"image"
	"testing"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/pkg/config"
)

// mockBridgeServer 是 BridgeServer 接口的最小化测试实现
type mockBridgeServer struct {
	commitCalled   bool
	commitText     string
	updateCalled   bool
	updateText     string
	updateCaretPos int
	clearCalled    bool
}

func (m *mockBridgeServer) PushStateToAllClients(_ *bridge.StatusUpdateData) {}
func (m *mockBridgeServer) PushCommitTextToActiveClient(text string) {
	m.commitCalled = true
	m.commitText = text
}
func (m *mockBridgeServer) PushClearCompositionToActiveClient() { m.clearCalled = true }
func (m *mockBridgeServer) PushUpdateCompositionToActiveClient(text string, caretPos int) {
	m.updateCalled = true
	m.updateText = text
	m.updateCaretPos = caretPos
}
func (m *mockBridgeServer) PushEnglishPairConfigToAllClients(_ bool, _ []string) {}
func (m *mockBridgeServer) PushStatsConfigToAllClients(_ bool, _ bool)           {}
func (m *mockBridgeServer) RestartService()                                      {}
func (m *mockBridgeServer) GetActiveHostRender() (func(*image.RGBA, int, int) error, func()) {
	return nil, nil
}

// ── uiCursorPos ──────────────────────────────────────────────────────────────

func TestUiCursorPos(t *testing.T) {
	tests := []struct {
		name           string
		segments       []ConfirmedSegment
		inputBuffer    string
		inputCursorPos int
		preeditDisplay string
		want           int
	}{
		{
			name:           "no segments, no preedit",
			inputBuffer:    "nihao",
			inputCursorPos: 5,
			want:           5,
		},
		{
			// "我们" = 6 UTF-8 bytes（2字符 × 3字节），非 displayCursorPos 的 2 runes
			// 修复前：2+2=4；修复后：6+2=8
			name: "chinese prefix byte vs rune difference",
			segments: []ConfirmedSegment{
				{Text: "我们", ConsumedCode: "women"},
			},
			inputBuffer:    "db",
			inputCursorPos: 2,
			want:           6 + 2,
		},
		{
			// 核心 bug 场景：zhongguoren 中手选"中"后，剩余 guoren
			// compositionText = "中guo ren"（9字节），光标应在末尾 = 位置 10
			name: "step commit single chinese char prefix",
			segments: []ConfirmedSegment{
				{Text: "中", ConsumedCode: "zhong"},
			},
			inputBuffer:    "guoren",
			inputCursorPos: 6,
			preeditDisplay: "guo ren",
			want:           3 + 7, // "中"=3字节 + "guo ren"=7显示字符
		},
		{
			// 两个汉字前缀：选了"中"再选"国"，剩余 ren
			name: "two chinese chars prefix",
			segments: []ConfirmedSegment{
				{Text: "中", ConsumedCode: "zhong"},
				{Text: "国", ConsumedCode: "guo"},
			},
			inputBuffer:    "ren",
			inputCursorPos: 3,
			preeditDisplay: "ren",
			want:           3 + 3 + 3, // 两个中文字符 6字节 + "ren"=3字节
		},
		{
			name:           "no segments, preedit with separator",
			inputBuffer:    "nihao",
			inputCursorPos: 5,
			preeditDisplay: "ni hao",
			want:           6, // 5个ASCII字节 + 1个分隔符
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			c := &Coordinator{
				confirmedSegments: tt.segments,
				inputBuffer:       tt.inputBuffer,
				inputCursorPos:    tt.inputCursorPos,
				preeditDisplay:    tt.preeditDisplay,
			}
			got := c.uiCursorPos()
			if got != tt.want {
				t.Errorf("uiCursorPos() = %d, want %d", got, tt.want)
			}
		})
	}
}

// uiCursorPos 和 displayCursorPos 在纯 ASCII 场景下结果应相同，
// 只有中文前缀时两者才会有差异。
func TestUiCursorPosVsDisplayCursorPosAsciiOnly(t *testing.T) {
	c := &Coordinator{
		inputBuffer:    "nihao",
		inputCursorPos: 5,
		preeditDisplay: "ni hao",
	}
	if c.uiCursorPos() != c.displayCursorPos() {
		t.Errorf("should be equal for ASCII-only: uiCursorPos=%d displayCursorPos=%d",
			c.uiCursorPos(), c.displayCursorPos())
	}
}

func TestUiCursorPosDiffersFromDisplayCursorPosWithChinesePrefix(t *testing.T) {
	c := &Coordinator{
		confirmedSegments: []ConfirmedSegment{
			{Text: "中", ConsumedCode: "zhong"},
		},
		inputBuffer:    "guoren",
		inputCursorPos: 6,
		preeditDisplay: "guo ren",
	}
	ui := c.uiCursorPos()
	tsf := c.displayCursorPos()
	if ui == tsf {
		t.Errorf("uiCursorPos (%d) should differ from displayCursorPos (%d) when Chinese prefix present", ui, tsf)
	}
	// "中" = 3 bytes - 1 rune = 2 的差值
	if ui-tsf != 2 {
		t.Errorf("diff = %d, want 2 (3 bytes - 1 rune per Chinese char)", ui-tsf)
	}
}

// ── compositionUpdateResult ───────────────────────────────────────────────────

func TestCompositionUpdateResult(t *testing.T) {
	t.Run("inline preedit on includes text and caret", func(t *testing.T) {
		c := &Coordinator{
			config:         &config.Config{UI: config.UIConfig{InlinePreedit: true}},
			inputBuffer:    "nihao",
			inputCursorPos: 5,
			preeditDisplay: "ni hao",
		}
		r := c.compositionUpdateResult()
		if r == nil {
			t.Fatal("returned nil")
		}
		if r.Type != bridge.ResponseTypeUpdateComposition {
			t.Errorf("Type = %v, want UpdateComposition", r.Type)
		}
		if r.Text != "ni hao" {
			t.Errorf("Text = %q, want %q", r.Text, "ni hao")
		}
		if r.CaretPos != 6 { // 5 ASCII + 1 separator
			t.Errorf("CaretPos = %d, want 6", r.CaretPos)
		}
	})

	t.Run("inline preedit off sends empty text", func(t *testing.T) {
		c := &Coordinator{
			config:         &config.Config{UI: config.UIConfig{InlinePreedit: false}},
			inputBuffer:    "nihao",
			inputCursorPos: 5,
			preeditDisplay: "ni hao",
		}
		r := c.compositionUpdateResult()
		if r == nil {
			t.Fatal("returned nil")
		}
		if r.Type != bridge.ResponseTypeUpdateComposition {
			t.Errorf("Type = %v, want UpdateComposition", r.Type)
		}
		if r.Text != "" {
			t.Errorf("Text = %q, want empty (InlinePreedit=false)", r.Text)
		}
		if r.CaretPos != 0 {
			t.Errorf("CaretPos = %d, want 0 (InlinePreedit=false)", r.CaretPos)
		}
	})

	t.Run("nil config defaults to inline behavior", func(t *testing.T) {
		c := &Coordinator{
			config:         nil,
			inputBuffer:    "abc",
			inputCursorPos: 3,
		}
		r := c.compositionUpdateResult()
		if r.Text != "abc" {
			t.Errorf("Text = %q, want %q", r.Text, "abc")
		}
	})

	t.Run("step commit inline off sends empty", func(t *testing.T) {
		c := &Coordinator{
			config: &config.Config{UI: config.UIConfig{InlinePreedit: false}},
			confirmedSegments: []ConfirmedSegment{
				{Text: "中", ConsumedCode: "zhong"},
			},
			inputBuffer:    "guoren",
			inputCursorPos: 6,
			preeditDisplay: "guo ren",
		}
		r := c.compositionUpdateResult()
		if r.Text != "" {
			t.Errorf("Text = %q, want empty when InlinePreedit=false", r.Text)
		}
	})
}

// ── pushKeyEventResult ────────────────────────────────────────────────────────

func TestPushKeyEventResult(t *testing.T) {
	t.Run("nil result does nothing", func(t *testing.T) {
		srv := &mockBridgeServer{}
		pushKeyEventResult(srv, nil)
		if srv.commitCalled || srv.updateCalled || srv.clearCalled {
			t.Error("no method should be called for nil result")
		}
	})

	t.Run("nil server does not panic", func(t *testing.T) {
		defer func() {
			if r := recover(); r != nil {
				t.Errorf("panicked: %v", r)
			}
		}()
		pushKeyEventResult(nil, &bridge.KeyEventResult{Type: bridge.ResponseTypeInsertText, Text: "hi"})
	})

	t.Run("InsertText dispatches to PushCommitText", func(t *testing.T) {
		srv := &mockBridgeServer{}
		pushKeyEventResult(srv, &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: "你好",
		})
		if !srv.commitCalled {
			t.Error("PushCommitTextToActiveClient not called")
		}
		if srv.commitText != "你好" {
			t.Errorf("commitText = %q, want %q", srv.commitText, "你好")
		}
		if srv.updateCalled || srv.clearCalled {
			t.Error("unexpected extra method calls")
		}
	})

	t.Run("UpdateComposition dispatches to PushUpdateComposition", func(t *testing.T) {
		srv := &mockBridgeServer{}
		pushKeyEventResult(srv, &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     "ni hao",
			CaretPos: 6,
		})
		if !srv.updateCalled {
			t.Error("PushUpdateCompositionToActiveClient not called")
		}
		if srv.updateText != "ni hao" || srv.updateCaretPos != 6 {
			t.Errorf("args = (%q, %d), want (%q, 6)", srv.updateText, srv.updateCaretPos, "ni hao")
		}
		if srv.commitCalled || srv.clearCalled {
			t.Error("unexpected extra method calls")
		}
	})

	t.Run("ClearComposition dispatches to PushClearComposition", func(t *testing.T) {
		srv := &mockBridgeServer{}
		pushKeyEventResult(srv, &bridge.KeyEventResult{
			Type: bridge.ResponseTypeClearComposition,
		})
		if !srv.clearCalled {
			t.Error("PushClearCompositionToActiveClient not called")
		}
		if srv.commitCalled || srv.updateCalled {
			t.Error("unexpected extra method calls")
		}
	})
}
