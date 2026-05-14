package store

import (
	"strings"
	"testing"
)

func TestMigratePhraseRecordsToAA(t *testing.T) {
	s := openTestStore(t)

	// 旧格式记录: Texts + Name 非空
	old := PhraseRecord{
		Code:     "zzbd",
		Name:     "标点",
		Texts:    "、。·",
		Type:     "array",
		Position: 1,
		Enabled:  true,
		IsSystem: true,
	}
	if err := s.AddPhrase(old); err != nil {
		t.Fatal(err)
	}
	// 普通短语: 不该被改
	normal := PhraseRecord{Code: "rq", Text: "日期", Type: "static", Enabled: true}
	if err := s.AddPhrase(normal); err != nil {
		t.Fatal(err)
	}
	// 已是 $AA 形式: 跳过 (幂等)
	already := PhraseRecord{
		Code:    "zzsz",
		Text:    `$AA("数字", "①②")`,
		Type:    "array",
		Enabled: true,
	}
	if err := s.AddPhrase(already); err != nil {
		t.Fatal(err)
	}

	n, err := s.MigratePhraseRecordsToAA()
	if err != nil {
		t.Fatalf("MigratePhraseRecordsToAA: %v", err)
	}
	if n != 1 {
		t.Fatalf("expected 1 migrated record, got %d", n)
	}

	// 校验旧字符组被改写
	recs, err := s.GetPhrasesByCode("zzbd")
	if err != nil {
		t.Fatal(err)
	}
	if len(recs) != 1 {
		t.Fatalf("expected 1 record for zzbd, got %d", len(recs))
	}
	got := recs[0]
	if got.Texts != "" {
		t.Errorf("Texts should be cleared, got %q", got.Texts)
	}
	if got.Name != "" {
		t.Errorf("Name should be cleared, got %q", got.Name)
	}
	if !strings.HasPrefix(got.Text, "$AA(") {
		t.Errorf("Text should start with $AA(, got %q", got.Text)
	}
	if !strings.Contains(got.Text, `"标点"`) || !strings.Contains(got.Text, `"、。·"`) {
		t.Errorf("Text should embed name and chars, got %q", got.Text)
	}

	// 再跑一次, 应该是 no-op (幂等)
	n2, err := s.MigratePhraseRecordsToAA()
	if err != nil {
		t.Fatal(err)
	}
	if n2 != 0 {
		t.Fatalf("second migration should be no-op, got %d", n2)
	}

	// 普通短语未受影响
	rqRecs, _ := s.GetPhrasesByCode("rq")
	if len(rqRecs) != 1 || rqRecs[0].Text != "日期" {
		t.Errorf("normal phrase corrupted: %+v", rqRecs)
	}

	// 已是 $AA 的未被重写
	szRecs, _ := s.GetPhrasesByCode("zzsz")
	if len(szRecs) != 1 || szRecs[0].Text != `$AA("数字", "①②")` {
		t.Errorf("already-migrated phrase mutated: %+v", szRecs)
	}
}
