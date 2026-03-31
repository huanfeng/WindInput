package coordinator

import (
	"testing"
)

func TestInputHistory_RecordAndGet(t *testing.T) {
	h := NewInputHistory(20)
	h.Record("风", "feng", "pinyin", 1)
	h.Record("力", "li", "pinyin", 1)

	records := h.GetRecentRecords(10, 1)
	if len(records) != 2 {
		t.Fatalf("expected 2 records, got %d", len(records))
	}
	// newest first
	if records[0].Text != "力" {
		t.Errorf("records[0].Text = %q, want %q", records[0].Text, "力")
	}
	if records[1].Text != "风" {
		t.Errorf("records[1].Text = %q, want %q", records[1].Text, "风")
	}
}

func TestInputHistory_MaxCharsLimit(t *testing.T) {
	h := NewInputHistory(4)
	// 记录 5 个单字，最早的应被淘汰
	for _, ch := range []string{"一", "二", "三", "四", "五"} {
		h.Record(ch, ch, "pinyin", 1)
	}

	records := h.GetRecentRecords(10, 1)
	// 最多保留 4 个字符
	if h.CharCount(1) > 4 {
		t.Errorf("CharCount = %d, want <= 4", h.CharCount(1))
	}
	// 最早的"一"应被淘汰
	for _, r := range records {
		if r.Text == "一" {
			t.Errorf("earliest record %q should have been trimmed", "一")
		}
	}
}

func TestInputHistory_ClientIsolation(t *testing.T) {
	h := NewInputHistory(20)
	h.Record("甲", "jia", "pinyin", 1)
	h.Record("乙", "yi", "pinyin", 2)

	r1 := h.GetRecentRecords(10, 1)
	r2 := h.GetRecentRecords(10, 2)

	if len(r1) != 1 || r1[0].Text != "甲" {
		t.Errorf("client 1 records wrong: %v", r1)
	}
	if len(r2) != 1 || r2[0].Text != "乙" {
		t.Errorf("client 2 records wrong: %v", r2)
	}
}

func TestInputHistory_GetRecentChars(t *testing.T) {
	h := NewInputHistory(20)
	for _, ch := range []string{"风", "力", "发", "电"} {
		h.Record(ch, ch, "pinyin", 1)
	}

	// 取 2 个：最近的 2 个，从早到晚
	chars2 := h.GetRecentChars(2, 1)
	if string(chars2) != "发电" {
		t.Errorf("GetRecentChars(2) = %q, want %q", string(chars2), "发电")
	}

	// 取 4 个：全部，从早到晚
	chars4 := h.GetRecentChars(4, 1)
	if string(chars4) != "风力发电" {
		t.Errorf("GetRecentChars(4) = %q, want %q", string(chars4), "风力发电")
	}

	// 取 10 个：不足时返回全部
	chars10 := h.GetRecentChars(10, 1)
	if string(chars10) != "风力发电" {
		t.Errorf("GetRecentChars(10) = %q, want %q", string(chars10), "风力发电")
	}
}

func TestInputHistory_MultiCharRecords(t *testing.T) {
	h := NewInputHistory(20)
	h.Record("你好", "nihao", "pinyin", 1)
	h.Record("世界", "shijie", "pinyin", 1)

	// 取 4 个字符：你好世界
	chars4 := h.GetRecentChars(4, 1)
	if string(chars4) != "你好世界" {
		t.Errorf("GetRecentChars(4) = %q, want %q", string(chars4), "你好世界")
	}

	// 取 3 个字符：好世界
	chars3 := h.GetRecentChars(3, 1)
	if string(chars3) != "好世界" {
		t.Errorf("GetRecentChars(3) = %q, want %q", string(chars3), "好世界")
	}
}

func TestInputHistory_ClearClient(t *testing.T) {
	h := NewInputHistory(20)
	h.Record("风", "feng", "pinyin", 1)
	h.ClearClient(1)

	records := h.GetRecentRecords(10, 1)
	if len(records) != 0 {
		t.Errorf("expected 0 records after clear, got %d", len(records))
	}
	if h.CharCount(1) != 0 {
		t.Errorf("expected CharCount 0 after clear, got %d", h.CharCount(1))
	}
}

func TestInputHistory_Empty(t *testing.T) {
	h := NewInputHistory(20)

	records := h.GetRecentRecords(10, 1)
	if len(records) != 0 {
		t.Errorf("expected 0 records on empty, got %d", len(records))
	}

	chars := h.GetRecentChars(5, 1)
	if len(chars) != 0 {
		t.Errorf("expected 0 chars on empty, got %d", len(chars))
	}

	if h.CharCount(1) != 0 {
		t.Errorf("expected CharCount 0 on empty, got %d", h.CharCount(1))
	}
}
