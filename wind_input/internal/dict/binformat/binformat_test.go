package binformat

import (
	"bytes"
	"math"
	"os"
	"path/filepath"
	"testing"
)

func TestDictWriteRead(t *testing.T) {
	// 构建测试词库
	w := NewDictWriter()

	w.AddCode("nihao", []DictEntry{
		{Text: "你好", Weight: 500000},
		{Text: "拟好", Weight: 1000},
	})
	w.AddCode("ni", []DictEntry{
		{Text: "你", Weight: 17596473},
		{Text: "拟", Weight: 100000},
		{Text: "泥", Weight: 50000},
	})
	w.AddCode("hao", []DictEntry{
		{Text: "好", Weight: 8000000},
		{Text: "号", Weight: 3000000},
	})
	w.AddCode("zhongguo", []DictEntry{
		{Text: "中国", Weight: 800000},
	})

	// 添加简拼
	w.AddAbbrev("nh", []DictEntry{
		{Text: "你好", Weight: 500000},
		{Text: "拟好", Weight: 1000},
	})
	w.AddAbbrev("zg", []DictEntry{
		{Text: "中国", Weight: 800000},
	})

	// 写入临时文件
	tmpDir := t.TempDir()
	wdbPath := filepath.Join(tmpDir, "test.wdb")
	f, err := os.Create(wdbPath)
	if err != nil {
		t.Fatalf("创建文件失败: %v", err)
	}
	if err := w.Write(f); err != nil {
		f.Close()
		t.Fatalf("写入失败: %v", err)
	}
	f.Close()

	// 读取并验证
	r, err := OpenDict(wdbPath)
	if err != nil {
		t.Fatalf("打开词库失败: %v", err)
	}
	defer r.Close()

	// 测试 Lookup
	results := r.Lookup("nihao")
	if len(results) != 2 {
		t.Fatalf("Lookup nihao: 期望 2 条，实际 %d 条", len(results))
	}
	if results[0].Text != "你好" {
		t.Errorf("Lookup nihao[0]: 期望 '你好', 实际 '%s'", results[0].Text)
	}
	if results[0].Weight != 500000 {
		t.Errorf("Lookup nihao[0].Weight: 期望 500000, 实际 %d", results[0].Weight)
	}

	results = r.Lookup("ni")
	if len(results) != 3 {
		t.Fatalf("Lookup ni: 期望 3 条，实际 %d 条", len(results))
	}

	results = r.Lookup("nonexist")
	if len(results) != 0 {
		t.Errorf("Lookup nonexist: 期望 0 条，实际 %d 条", len(results))
	}

	// 测试 LookupPrefix
	results = r.LookupPrefix("ni", 10)
	if len(results) < 3 {
		t.Errorf("LookupPrefix ni: 期望至少 3 条，实际 %d 条", len(results))
	}
	// 应包含 "ni" 的候选和 "nihao" 的候选
	hasNihao := false
	for _, c := range results {
		if c.Text == "你好" {
			hasNihao = true
		}
	}
	if !hasNihao {
		t.Error("LookupPrefix ni: 缺少 '你好'")
	}

	// 测试 HasPrefix
	if !r.HasPrefix("ni") {
		t.Error("HasPrefix ni: 期望 true")
	}
	if !r.HasPrefix("zh") {
		t.Error("HasPrefix zh: 期望 true")
	}
	if r.HasPrefix("xyz") {
		t.Error("HasPrefix xyz: 期望 false")
	}

	// 测试 LookupAbbrev
	results = r.LookupAbbrev("nh", 10)
	if len(results) != 2 {
		t.Fatalf("LookupAbbrev nh: 期望 2 条，实际 %d 条", len(results))
	}
	if results[0].Text != "你好" {
		t.Errorf("LookupAbbrev nh[0]: 期望 '你好', 实际 '%s'", results[0].Text)
	}

	results = r.LookupAbbrev("zg", 10)
	if len(results) != 1 {
		t.Fatalf("LookupAbbrev zg: 期望 1 条，实际 %d 条", len(results))
	}

	results = r.LookupAbbrev("xx", 10)
	if len(results) != 0 {
		t.Errorf("LookupAbbrev xx: 期望 0 条，实际 %d 条", len(results))
	}
}

func TestDictWriterEmpty(t *testing.T) {
	w := NewDictWriter()
	var buf bytes.Buffer
	if err := w.Write(&buf); err != nil {
		t.Fatalf("写入空词库失败: %v", err)
	}
	if buf.Len() < DictFileHeaderSize {
		t.Errorf("空词库文件过小: %d bytes", buf.Len())
	}
}

func TestUnigramWriteRead(t *testing.T) {
	w := NewUnigramWriter()
	w.Add("的", -2.5)
	w.Add("是", -3.0)
	w.Add("在", -3.5)
	w.Add("中国", -5.0)
	w.Add("你好", -6.0)

	tmpDir := t.TempDir()
	wdbPath := filepath.Join(tmpDir, "test_unigram.wdb")
	f, err := os.Create(wdbPath)
	if err != nil {
		t.Fatalf("创建文件失败: %v", err)
	}
	if err := w.Write(f); err != nil {
		f.Close()
		t.Fatalf("写入失败: %v", err)
	}
	f.Close()

	r, err := OpenUnigram(wdbPath)
	if err != nil {
		t.Fatalf("打开 Unigram 失败: %v", err)
	}
	defer r.Close()

	// 测试 Size
	if r.Size() != 5 {
		t.Errorf("Size: 期望 5, 实际 %d", r.Size())
	}

	// 测试 LogProb
	logProb := r.LogProb("的")
	if math.Abs(logProb-(-2.5)) > 0.01 {
		t.Errorf("LogProb('的'): 期望 -2.5, 实际 %f", logProb)
	}

	logProb = r.LogProb("中国")
	if math.Abs(logProb-(-5.0)) > 0.01 {
		t.Errorf("LogProb('中国'): 期望 -5.0, 实际 %f", logProb)
	}

	// 测试不存在的词
	logProb = r.LogProb("不存在")
	if logProb != r.MinProb() {
		t.Errorf("LogProb('不存在'): 期望 %f, 实际 %f", r.MinProb(), logProb)
	}

	// 测试 Contains
	if !r.Contains("的") {
		t.Error("Contains('的'): 期望 true")
	}
	if r.Contains("不存在") {
		t.Error("Contains('不存在'): 期望 false")
	}

	// 测试 CharBasedScore
	score := r.CharBasedScore("的")
	if math.Abs(score-(-2.5)) > 0.01 {
		t.Errorf("CharBasedScore('的'): 期望 -2.5, 实际 %f", score)
	}
}

func TestUnigramFromFreqs(t *testing.T) {
	w := NewUnigramWriter()
	freqs := map[string]float64{
		"你": 100,
		"好": 200,
		"他": 50,
	}
	w.AddFromFreqs(freqs)

	var buf bytes.Buffer
	if err := w.Write(&buf); err != nil {
		t.Fatalf("写入失败: %v", err)
	}

	// 验证写入成功，文件大小合理
	if buf.Len() < UnigramFileHeaderSize {
		t.Errorf("文件过小: %d bytes", buf.Len())
	}
}
