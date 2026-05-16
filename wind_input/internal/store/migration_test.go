package store

import (
	"encoding/json"
	"strings"
	"testing"

	bolt "go.etcd.io/bbolt"
)

// seedLegacyPhrase 直接以 bbolt KV 写入旧版 (Texts/Name/Type) 字段格式,
// 模拟用户从旧版本升级后 db 仍有未迁移的字符组记录。
func seedLegacyPhrase(t *testing.T, s *Store, key string, legacy legacyPhraseRecord) {
	t.Helper()
	data, err := json.Marshal(legacy)
	if err != nil {
		t.Fatal(err)
	}
	if err := s.db.Update(func(tx *bolt.Tx) error {
		return tx.Bucket(bucketPhrases).Put([]byte(key), data)
	}); err != nil {
		t.Fatal(err)
	}
}

func TestMigratePhraseRecordsToAA(t *testing.T) {
	s := openTestStore(t)

	// 旧字符组记录 (Texts + Name 双字段, key 形式 code\x00\x01name)
	seedLegacyPhrase(t, s, "zzbd\x00\x01标点", legacyPhraseRecord{
		Name:     "标点",
		Texts:    "、。·",
		Type:     "array",
		Position: 1,
		Enabled:  true,
		IsSystem: true,
	})

	// 普通短语 (不应被改)
	normal := PhraseRecord{Code: "rq", Text: "日期", Enabled: true}
	if err := s.AddPhrase(normal); err != nil {
		t.Fatal(err)
	}

	// 已是 $AA 形式 (幂等, 不应被重写)
	already := PhraseRecord{Code: "zzsz", Text: `$AA("数字", "①②")`, Enabled: true}
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

	// 校验旧字符组被改写: text 变为 $AA marker
	recs, err := s.GetPhrasesByCode("zzbd")
	if err != nil {
		t.Fatal(err)
	}
	if len(recs) != 1 {
		t.Fatalf("expected 1 record for zzbd, got %d", len(recs))
	}
	got := recs[0]
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
