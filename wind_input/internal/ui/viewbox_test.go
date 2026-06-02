package ui

import "testing"

// fakeMeasurer：每个 rune 宽 = fontSize*0.5，便于断言；不依赖字体后端。
type fakeMeasurer struct{}

func (fakeMeasurer) MeasureString(s string, fs float64) float64 {
	return float64(len([]rune(s))) * fs * 0.5
}

func TestViewMeasure_TextLeafAddsPadding(t *testing.T) {
	v := &View{
		Text:      "ab", // 2 runes * 10 * 0.5 = 10
		TextStyle: TextStyle{FontSize: 10},
		Padding:   Edges{Top: 2, Right: 3, Bottom: 2, Left: 3},
	}
	w, h := v.measure(fakeMeasurer{})
	if w != 16 { // 10 + 3 + 3
		t.Errorf("width want 16, got %d", w)
	}
	if h != 14 { // lineHeight(=10) + 2 + 2
		t.Errorf("height want 14, got %d", h)
	}
}

func TestViewLayout_ColumnStacksWithGapAndPadding(t *testing.T) {
	a := &View{Text: "x", TextStyle: TextStyle{FontSize: 10}}  // 5 x 10
	b := &View{Text: "yy", TextStyle: TextStyle{FontSize: 10}} // 10 x 10
	win := &View{
		Layout:   LayoutColumn,
		Gap:      4,
		Padding:  Edges{Top: 6, Right: 8, Bottom: 6, Left: 8},
		Children: []*View{a, b},
	}
	Layout(win, 0, 0, fakeMeasurer{})

	if got := win.Rect(); got.Dx() != 26 || got.Dy() != 36 {
		// w = max(5,10)+8+8 = 26 ; h = 10+4+10 +6+6 = 36
		t.Errorf("window box want 26x36, got %dx%d", got.Dx(), got.Dy())
	}
	if a.Rect().Min.X != 8 || a.Rect().Min.Y != 6 {
		t.Errorf("a origin want (8,6), got %v", a.Rect().Min)
	}
	if b.Rect().Min.X != 8 || b.Rect().Min.Y != 20 { // 6 + 10 + gap4
		t.Errorf("b origin want (8,20), got %v", b.Rect().Min)
	}
}

func TestViewLayout_RowFlowWithMarginAndGap(t *testing.T) {
	a := &View{Text: "x", TextStyle: TextStyle{FontSize: 10}}                         // 5
	b := &View{Text: "y", TextStyle: TextStyle{FontSize: 10}, Margin: Edges{Left: 2}} // 5, 左 margin 2
	row := &View{Layout: LayoutRow, Gap: 3, Children: []*View{a, b}}
	Layout(row, 100, 0, fakeMeasurer{})

	if a.Rect().Min.X != 100 {
		t.Errorf("a.x want 100, got %d", a.Rect().Min.X)
	}
	// b.x = 100 + a.w(5) + gap(3) + b.marginLeft(2) = 110
	if b.Rect().Min.X != 110 {
		t.Errorf("b.x want 110, got %d", b.Rect().Min.X)
	}
}

func TestViewLayout_RowCrossCenter(t *testing.T) {
	short := &View{Text: "x", TextStyle: TextStyle{FontSize: 10}} // h=10
	tall := &View{FixedW: 8, FixedH: 20}                          // h=20（固定）
	row := &View{Layout: LayoutRow, CrossAlign: AlignCenter, Children: []*View{short, tall}}
	Layout(row, 0, 0, fakeMeasurer{})

	// contentH = max(10,20)=20；short 居中 → y = (20-10)/2 = 5
	if short.Rect().Min.Y != 5 {
		t.Errorf("short centered y want 5, got %d", short.Rect().Min.Y)
	}
	if tall.Rect().Min.Y != 0 {
		t.Errorf("tall y want 0, got %d", tall.Rect().Min.Y)
	}
}

func TestViewMeasure_FixedSizeOverrides(t *testing.T) {
	v := &View{
		Text:      "verylongtext",
		TextStyle: TextStyle{FontSize: 10},
		FixedW:    20,
		FixedH:    12,
	}
	w, h := v.measure(fakeMeasurer{})
	if w != 20 || h != 12 {
		t.Errorf("fixed size want 20x12, got %dx%d", w, h)
	}
}
