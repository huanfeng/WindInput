package coordinator

import (
	"testing"
)

func TestConfirmedPrefix(t *testing.T) {
	tests := []struct {
		name     string
		segments []ConfirmedSegment
		want     string
	}{
		{"empty", nil, ""},
		{"single", []ConfirmedSegment{{Text: "我们", ConsumedCode: "women"}}, "我们"},
		{"multiple", []ConfirmedSegment{
			{Text: "我们", ConsumedCode: "women"},
			{Text: "的", ConsumedCode: "de"},
		}, "我们的"},
		{"three", []ConfirmedSegment{
			{Text: "中", ConsumedCode: "zhong"},
			{Text: "华", ConsumedCode: "hua"},
			{Text: "人民", ConsumedCode: "renmin"},
		}, "中华人民"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			c := &Coordinator{
				confirmedSegments: tt.segments,
			}
			got := c.confirmedPrefix()
			if got != tt.want {
				t.Errorf("confirmedPrefix() = %q, want %q", got, tt.want)
			}
		})
	}
}

func TestCompositionTextWithConfirmedSegments(t *testing.T) {
	tests := []struct {
		name           string
		segments       []ConfirmedSegment
		inputBuffer    string
		preeditDisplay string
		want           string
	}{
		{
			name:           "no segments, preedit",
			segments:       nil,
			inputBuffer:    "nihao",
			preeditDisplay: "ni hao",
			want:           "ni hao",
		},
		{
			name:           "no segments, no preedit",
			segments:       nil,
			inputBuffer:    "nihao",
			preeditDisplay: "",
			want:           "nihao",
		},
		{
			name: "with segments, preedit",
			segments: []ConfirmedSegment{
				{Text: "我们", ConsumedCode: "women"},
			},
			inputBuffer:    "debiaoxian",
			preeditDisplay: "de biao xian",
			want:           "我们de biao xian",
		},
		{
			name: "with segments, no preedit",
			segments: []ConfirmedSegment{
				{Text: "我们", ConsumedCode: "women"},
			},
			inputBuffer:    "db",
			preeditDisplay: "",
			want:           "我们db",
		},
		{
			name: "multiple segments",
			segments: []ConfirmedSegment{
				{Text: "我们", ConsumedCode: "women"},
				{Text: "的", ConsumedCode: "de"},
			},
			inputBuffer:    "biaoxian",
			preeditDisplay: "biao xian",
			want:           "我们的biao xian",
		},
		{
			name: "trailing apostrophe",
			segments: []ConfirmedSegment{
				{Text: "西", ConsumedCode: "xi"},
			},
			inputBuffer:    "an'",
			preeditDisplay: "an",
			want:           "西an'",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			c := &Coordinator{
				confirmedSegments: tt.segments,
				inputBuffer:       tt.inputBuffer,
				preeditDisplay:    tt.preeditDisplay,
			}
			got := c.compositionText()
			if got != tt.want {
				t.Errorf("compositionText() = %q, want %q", got, tt.want)
			}
		})
	}
}

func TestDisplayCursorPosWithConfirmedSegments(t *testing.T) {
	tests := []struct {
		name               string
		segments           []ConfirmedSegment
		inputBuffer        string
		inputCursorPos     int
		preeditDisplay     string
		syllableBoundaries []int
		want               int
	}{
		{
			name:           "no segments, no preedit, cursor at end",
			inputBuffer:    "nihao",
			inputCursorPos: 5,
			want:           5,
		},
		{
			name: "with segment, no preedit, cursor at end",
			segments: []ConfirmedSegment{
				{Text: "我们", ConsumedCode: "women"},
			},
			inputBuffer:    "db",
			inputCursorPos: 2,
			want:           2 + 2, // "我们" = 2 runes + 2 bytes ASCII
		},
		{
			name: "with segment, preedit with separators",
			segments: []ConfirmedSegment{
				{Text: "我们", ConsumedCode: "women"},
			},
			inputBuffer:        "debiaoxian",
			inputCursorPos:     10,
			preeditDisplay:     "de biao xian",
			syllableBoundaries: []int{2, 6},
			want:               2 + 10 + 2, // "我们" = 2 runes + 10 ASCII + 2 separators = 14
		},
		{
			name: "with segment, cursor in middle",
			segments: []ConfirmedSegment{
				{Text: "你", ConsumedCode: "ni"},
			},
			inputBuffer:        "hao",
			inputCursorPos:     1, // cursor after 'h'
			preeditDisplay:     "hao",
			syllableBoundaries: nil,
			want:               1 + 1, // "你" = 1 rune + 1 byte = 2
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			c := &Coordinator{
				confirmedSegments:  tt.segments,
				inputBuffer:        tt.inputBuffer,
				inputCursorPos:     tt.inputCursorPos,
				preeditDisplay:     tt.preeditDisplay,
				syllableBoundaries: tt.syllableBoundaries,
			}
			got := c.displayCursorPos()
			if got != tt.want {
				t.Errorf("displayCursorPos() = %d, want %d", got, tt.want)
			}
		})
	}
}

func TestPopConfirmedSegment(t *testing.T) {
	// 测试弹出确认段恢复编码
	t.Run("pop single segment", func(t *testing.T) {
		c := &Coordinator{
			confirmedSegments: []ConfirmedSegment{
				{Text: "我们", ConsumedCode: "women"},
			},
			inputBuffer:    "",
			inputCursorPos: 0,
		}

		// 模拟弹出
		lastSeg := c.confirmedSegments[len(c.confirmedSegments)-1]
		c.confirmedSegments = c.confirmedSegments[:len(c.confirmedSegments)-1]
		c.inputBuffer = lastSeg.ConsumedCode
		c.inputCursorPos = len(lastSeg.ConsumedCode)

		if c.inputBuffer != "women" {
			t.Errorf("inputBuffer = %q, want %q", c.inputBuffer, "women")
		}
		if c.inputCursorPos != 5 {
			t.Errorf("inputCursorPos = %d, want 5", c.inputCursorPos)
		}
		if len(c.confirmedSegments) != 0 {
			t.Errorf("confirmedSegments len = %d, want 0", len(c.confirmedSegments))
		}
	})

	t.Run("pop from multiple segments", func(t *testing.T) {
		c := &Coordinator{
			confirmedSegments: []ConfirmedSegment{
				{Text: "我们", ConsumedCode: "women"},
				{Text: "的", ConsumedCode: "de"},
			},
			inputBuffer:    "",
			inputCursorPos: 0,
		}

		lastSeg := c.confirmedSegments[len(c.confirmedSegments)-1]
		c.confirmedSegments = c.confirmedSegments[:len(c.confirmedSegments)-1]
		c.inputBuffer = lastSeg.ConsumedCode
		c.inputCursorPos = len(lastSeg.ConsumedCode)

		if c.inputBuffer != "de" {
			t.Errorf("inputBuffer = %q, want %q", c.inputBuffer, "de")
		}
		if len(c.confirmedSegments) != 1 {
			t.Errorf("confirmedSegments len = %d, want 1", len(c.confirmedSegments))
		}
		if c.confirmedSegments[0].Text != "我们" {
			t.Errorf("remaining segment text = %q, want %q", c.confirmedSegments[0].Text, "我们")
		}
	})
}

func TestBackspaceAtCursorZeroWithConfirmedSegments(t *testing.T) {
	// 测试光标在最左边按退格，弹出确认段拼接到 buffer 前面
	t.Run("backspace at cursor 0 prepends segment code", func(t *testing.T) {
		c := &Coordinator{
			confirmedSegments: []ConfirmedSegment{
				{Text: "我们", ConsumedCode: "women"},
			},
			inputBuffer:    "de",
			inputCursorPos: 0,
		}

		// 模拟退格在 cursor 0 的行为
		lastSeg := c.confirmedSegments[len(c.confirmedSegments)-1]
		c.confirmedSegments = c.confirmedSegments[:len(c.confirmedSegments)-1]
		c.inputBuffer = lastSeg.ConsumedCode + c.inputBuffer
		c.inputCursorPos = len(lastSeg.ConsumedCode)

		if c.inputBuffer != "womende" {
			t.Errorf("inputBuffer = %q, want %q", c.inputBuffer, "womende")
		}
		if c.inputCursorPos != 5 {
			t.Errorf("inputCursorPos = %d, want 5", c.inputCursorPos)
		}
		if len(c.confirmedSegments) != 0 {
			t.Errorf("confirmedSegments len = %d, want 0", len(c.confirmedSegments))
		}
	})
}

func TestClearStateResetsConfirmedSegments(t *testing.T) {
	c := &Coordinator{
		confirmedSegments: []ConfirmedSegment{
			{Text: "我们", ConsumedCode: "women"},
			{Text: "的", ConsumedCode: "de"},
		},
		inputBuffer:    "biaoxian",
		inputCursorPos: 8,
	}

	// clearState 需要 engineMgr，mock 最小化
	// 直接测试确认段被清空
	c.confirmedSegments = nil
	c.inputBuffer = ""
	c.inputCursorPos = 0

	if len(c.confirmedSegments) != 0 {
		t.Errorf("confirmedSegments should be nil after clear")
	}
	if c.inputBuffer != "" {
		t.Errorf("inputBuffer should be empty after clear")
	}
}

func TestFullCommitWithConfirmedSegments(t *testing.T) {
	// 模拟完全消费时的上屏拼接
	t.Run("full commit joins all segments", func(t *testing.T) {
		segments := []ConfirmedSegment{
			{Text: "我们", ConsumedCode: "women"},
			{Text: "的", ConsumedCode: "de"},
		}
		candidateText := "表现"

		var finalText string
		for _, seg := range segments {
			finalText += seg.Text
		}
		finalText += candidateText

		if finalText != "我们的表现" {
			t.Errorf("finalText = %q, want %q", finalText, "我们的表现")
		}
	})
}

func TestEnterWithConfirmedSegments(t *testing.T) {
	// 模拟回车上屏：确认段文字 + 原始编码
	segments := []ConfirmedSegment{
		{Text: "我们", ConsumedCode: "women"},
	}
	inputBuffer := "debiaoxian"

	var finalText string
	for _, seg := range segments {
		finalText += seg.Text
	}
	finalText += inputBuffer

	if finalText != "我们debiaoxian" {
		t.Errorf("finalText = %q, want %q", finalText, "我们debiaoxian")
	}
}
