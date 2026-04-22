package datformat

import (
	"bytes"
	"testing"
)

func TestDAT_Build_And_ExactLookup(t *testing.T) {
	b := NewDATBuilder()
	keys := []string{"shi", "shui", "si", "sha", "she", "shu"}
	for i, k := range keys {
		b.Add(k, uint32(i))
	}
	dat, err := b.Build()
	if err != nil {
		t.Fatalf("Build error: %v", err)
	}

	// 所有已添加的 key 必须能精确匹配到对应 dataIndex
	for i, k := range keys {
		idx, found := dat.ExactMatch(k)
		if !found {
			t.Errorf("key %q should be found", k)
			continue
		}
		if idx != uint32(i) {
			t.Errorf("key %q: want dataIndex %d, got %d", k, i, idx)
		}
	}

	// 不存在的 key 应返回 false
	notExist := []string{"s", "sh", "shia", "x", "", "shuix"}
	for _, k := range notExist {
		_, found := dat.ExactMatch(k)
		if found {
			t.Errorf("key %q should NOT be found", k)
		}
	}
}

func TestDAT_Build_Empty(t *testing.T) {
	b := NewDATBuilder()
	dat, err := b.Build()
	if err != nil {
		t.Fatalf("Build error: %v", err)
	}
	_, found := dat.ExactMatch("any")
	if found {
		t.Error("empty DAT should not match any key")
	}
	_, found = dat.ExactMatch("")
	if found {
		t.Error("empty DAT should not match empty key")
	}
}

func TestDAT_Build_SingleKey(t *testing.T) {
	b := NewDATBuilder()
	b.Add("hello", 42)
	dat, err := b.Build()
	if err != nil {
		t.Fatalf("Build error: %v", err)
	}

	idx, found := dat.ExactMatch("hello")
	if !found {
		t.Fatal("key 'hello' should be found")
	}
	if idx != 42 {
		t.Errorf("want dataIndex 42, got %d", idx)
	}

	_, found = dat.ExactMatch("hell")
	if found {
		t.Error("prefix 'hell' should NOT be found")
	}
	_, found = dat.ExactMatch("helloo")
	if found {
		t.Error("extended key 'helloo' should NOT be found")
	}
}

func TestDAT_PrefixCollect(t *testing.T) {
	b := NewDATBuilder()
	keys := []string{"sa", "sai", "san", "sang", "she", "shi", "shou", "si", "song", "su"}
	for i, k := range keys {
		b.Add(k, uint32(i))
	}
	dat, err := b.Build()
	if err != nil {
		t.Fatalf("Build error: %v", err)
	}

	// PrefixCollect("s", 0) 应返回全部 10 个
	results := dat.PrefixCollect("s", 0)
	if len(results) != 10 {
		t.Errorf("PrefixCollect(\"s\", 0): want 10, got %d", len(results))
	}

	// PrefixCollect("sh", 0) 应返回 3 个 (she, shi, shou)
	results = dat.PrefixCollect("sh", 0)
	if len(results) != 3 {
		t.Errorf("PrefixCollect(\"sh\", 0): want 3, got %d", len(results))
	}

	// PrefixCollect("s", 5) 应返回 5 个
	results = dat.PrefixCollect("s", 5)
	if len(results) != 5 {
		t.Errorf("PrefixCollect(\"s\", 5): want 5, got %d", len(results))
	}

	// PrefixCollect("x", 0) 应返回 0 个
	results = dat.PrefixCollect("x", 0)
	if len(results) != 0 {
		t.Errorf("PrefixCollect(\"x\", 0): want 0, got %d", len(results))
	}
}

func TestDATCursor_Incremental(t *testing.T) {
	b := NewDATBuilder()
	keys := []string{"sa", "sai", "san", "sang", "she", "shi", "shou", "si", "song", "su"}
	for i, k := range keys {
		b.Add(k, uint32(i))
	}
	dat, err := b.Build()
	if err != nil {
		t.Fatalf("Build error: %v", err)
	}

	cursor := dat.PrefixCursor("s")
	defer cursor.Close()

	var all []uint32

	r1 := cursor.Next(3)
	if len(r1) != 3 {
		t.Errorf("Next(3): want 3, got %d", len(r1))
	}
	all = append(all, r1...)

	r2 := cursor.Next(4)
	if len(r2) != 4 {
		t.Errorf("Next(4): want 4, got %d", len(r2))
	}
	all = append(all, r2...)

	r3 := cursor.Next(100)
	if len(r3) != 3 {
		t.Errorf("Next(100): want 3 (remaining), got %d", len(r3))
	}
	all = append(all, r3...)

	if cursor.HasMore() {
		t.Error("HasMore() should be false after exhausting cursor")
	}

	if len(all) != 10 {
		t.Errorf("total collected: want 10, got %d", len(all))
	}

	// 检查无重复
	seen := make(map[uint32]bool)
	for _, idx := range all {
		if seen[idx] {
			t.Errorf("duplicate dataIndex %d", idx)
		}
		seen[idx] = true
	}
}

func TestWdatWriter_Write(t *testing.T) {
	w := NewWdatWriter()
	w.AddCode("ni", []WdatEntry{{Text: "你", Weight: 100}, {Text: "尼", Weight: 50}})
	w.AddCode("nihao", []WdatEntry{{Text: "你好", Weight: 200}})
	w.AddCode("shi", []WdatEntry{{Text: "是", Weight: 300}})
	w.AddAbbrev("nh", []WdatEntry{{Text: "你好", Weight: 200}})

	var buf bytes.Buffer
	if err := w.Write(&buf); err != nil {
		t.Fatalf("Write failed: %v", err)
	}
	if buf.Len() == 0 {
		t.Fatal("output is empty")
	}
	if string(buf.Bytes()[:4]) != "WDAT" {
		t.Errorf("magic = %q, want WDAT", string(buf.Bytes()[:4]))
	}
}
