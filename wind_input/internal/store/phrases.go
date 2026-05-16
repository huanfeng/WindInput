package store

import (
	"bytes"
	"encoding/json"
	"fmt"

	bolt "go.etcd.io/bbolt"
)

// PhraseRecord is the JSON-encoded value stored under a phrase key.
// Code is not stored in JSON (it's part of the bbolt key), but populated by
// read methods.
//
// 2026-05-16 简化: 删除 Type / Texts / Name 字段, marker 信息完全由 Text
// 携带 (与用户词库 (code, text, weight) 三元组一致)。旧 db 数据中的
// Texts/Name/Type 字段在 JSON unmarshal 时被丢弃; 旧 $AA 字符组记录由
// MigratePhraseRecordsToAA 在 store.Open 后自动重组为 Text 中的
// $AA("name", "chars") marker 形式 (内部用 legacyPhraseRecord 读旧字段)。
//
// 短语分类 (普通 / cmdbar / 字符组 / 字符串数组) 完全由 PhraseLayer 在
// LoadFromStore 阶段通过解析 Text 推断, 不再依赖存储字段。
type PhraseRecord struct {
	Code string `json:"-"`              // 从 key 解析，不序列化
	Text string `json:"text,omitempty"` // 短语原文 (含 $AA / $SS / $CC marker 时由 PhraseLayer 解析)
	// Weight 是显式权重 (0~10000), 优先于 Position。
	// 0 (默认零值) 表示"未设置", 由 PhraseLayer 走默认值 1000。
	Weight   int  `json:"w,omitempty"`
	Position int  `json:"pos"`
	Enabled  bool `json:"on"`
	IsSystem bool `json:"sys,omitempty"`
}

// legacyPhraseRecord 仅在 MigratePhraseRecordsToAA 内部使用, 用于反序列化
// 旧版 store 中带 Texts/Name/Type 字段的字符组记录, 将其重组为新版 PhraseRecord
// (Text 含 $AA marker)。
//
// 调用方: store/migration.go 唯一引用点; 其他包不应使用此类型。
type legacyPhraseRecord struct {
	Text     string `json:"text,omitempty"`
	Texts    string `json:"texts,omitempty"`
	Name     string `json:"name,omitempty"`
	Type     string `json:"type,omitempty"`
	Weight   int    `json:"w,omitempty"`
	Position int    `json:"pos"`
	Enabled  bool   `json:"on"`
	IsSystem bool   `json:"sys,omitempty"`
}

// phraseKey returns the composite key for a PhraseRecord.
// 简化后 (2026-05-16): 所有 PhraseRecord 统一用 "code\x00text" 作为 key。
// 旧版字符组的 "code\x00\x01name" 形式由 MigratePhraseRecordsToAA 在 store.Open
// 后转换为新格式 (Text 含 $AA marker, key 走默认路径)。
func phraseKey(rec PhraseRecord) []byte {
	return []byte(rec.Code + "\x00" + rec.Text)
}

// parsePhraseKey splits a composite key into code and identifier.
func parsePhraseKey(key []byte) (code, identifier string) {
	for i, b := range key {
		if b == '\x00' {
			return string(key[:i]), string(key[i+1:])
		}
	}
	return string(key), ""
}

// GetAllPhrases returns every phrase in the Phrases bucket.
func (s *Store) GetAllPhrases() ([]PhraseRecord, error) {
	var results []PhraseRecord
	err := s.db.View(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketPhrases)
		if b == nil {
			return nil
		}
		return b.ForEach(func(k, v []byte) error {
			var rec PhraseRecord
			if err := json.Unmarshal(v, &rec); err != nil {
				return nil
			}
			rec.Code, _ = parsePhraseKey(k)
			results = append(results, rec)
			return nil
		})
	})
	return results, err
}

// GetPhrasesByCode returns all phrases whose code matches exactly.
func (s *Store) GetPhrasesByCode(code string) ([]PhraseRecord, error) {
	var results []PhraseRecord
	err := s.db.View(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketPhrases)
		if b == nil {
			return nil
		}
		prefix := []byte(code + "\x00")
		c := b.Cursor()
		for k, v := c.Seek(prefix); k != nil && hasPrefix(k, prefix); k, v = c.Next() {
			var rec PhraseRecord
			if err := json.Unmarshal(v, &rec); err != nil {
				continue
			}
			rec.Code = code
			results = append(results, rec)
		}
		return nil
	})
	return results, err
}

// AddPhrase inserts or overwrites a phrase record.
func (s *Store) AddPhrase(rec PhraseRecord) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketPhrases)
		if b == nil {
			return fmt.Errorf("Phrases bucket not found")
		}
		data, err := json.Marshal(rec)
		if err != nil {
			return fmt.Errorf("marshal PhraseRecord: %w", err)
		}
		return b.Put(phraseKey(rec), data)
	})
}

// UpdatePhrase overwrites an existing phrase record (same semantics as AddPhrase).
func (s *Store) UpdatePhrase(rec PhraseRecord) error {
	return s.AddPhrase(rec)
}

// RemovePhrase deletes a phrase by (code, text). 2026-05-16 简化: 不再有
// name 参数, 字符组短语的 name 已嵌入 text 的 $AA marker, key 也统一为
// "code\x00text"。
//
// 兼容性: 历史上 array 短语用 "code\x00\x01name" 作为 key, 该形式由
// MigratePhraseRecordsToAA 在 store.Open 后自动改写为新格式。但若用户的
// db 由极旧版本升级、migration 未跑过, 可能仍有 legacy key 残留 —— 由
// ForEach 扫描兜底删除匹配的 rec.Text 即可。
func (s *Store) RemovePhrase(code, text string) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketPhrases)
		if b == nil {
			return nil
		}
		return removePhraseInBucket(b, code, text)
	})
}

// RemovePhrasesBatch deletes multiple phrases in a single transaction.
func (s *Store) RemovePhrasesBatch(items []PhraseRecord) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketPhrases)
		if b == nil {
			return nil
		}
		for _, rec := range items {
			if err := removePhraseInBucket(b, rec.Code, rec.Text); err != nil {
				return err
			}
		}
		return nil
	})
}

// removePhraseInBucket: 先按精确 key (code\x00text) 删, 再扫描 bucket 找
// rec.Text 等于目标 text 的记录兜底 (覆盖 legacy key 形态)。删除是低频操作,
// O(N) 扫描成本可接受。
func removePhraseInBucket(b *bolt.Bucket, code, text string) error {
	// 1) 精确 key 快速路径
	if err := b.Delete([]byte(code + "\x00" + text)); err != nil {
		return err
	}
	// 2) ForEach 扫描兜底: 收集 rec.Text 匹配的 key 删除 (legacy key 兼容)
	codePrefix := []byte(code + "\x00")
	var toDelete [][]byte
	err := b.ForEach(func(k, v []byte) error {
		if !bytes.HasPrefix(k, codePrefix) {
			return nil
		}
		var rec PhraseRecord
		if uerr := json.Unmarshal(v, &rec); uerr != nil {
			return nil
		}
		if text != "" && rec.Text == text {
			kc := make([]byte, len(k))
			copy(kc, k)
			toDelete = append(toDelete, kc)
		}
		return nil
	})
	if err != nil {
		return err
	}
	for _, k := range toDelete {
		if err := b.Delete(k); err != nil {
			return err
		}
	}
	return nil
}

// SetPhraseEnabled toggles the Enabled flag of an existing phrase, identified
// by (code, text)。
func (s *Store) SetPhraseEnabled(code, text string, enabled bool) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketPhrases)
		if b == nil {
			return fmt.Errorf("Phrases bucket not found")
		}
		key := []byte(code + "\x00" + text)
		raw := b.Get(key)
		if raw == nil {
			return fmt.Errorf("SetPhraseEnabled: entry %q not found", string(key))
		}
		var rec PhraseRecord
		if err := json.Unmarshal(raw, &rec); err != nil {
			return fmt.Errorf("SetPhraseEnabled unmarshal: %w", err)
		}
		rec.Enabled = enabled
		data, err := json.Marshal(rec)
		if err != nil {
			return fmt.Errorf("SetPhraseEnabled marshal: %w", err)
		}
		return b.Put(key, data)
	})
}

// PhraseCount returns the total number of phrase entries.
func (s *Store) PhraseCount() (int, error) {
	var count int
	err := s.db.View(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketPhrases)
		if b == nil {
			return nil
		}
		count = b.Stats().KeyN
		return nil
	})
	return count, err
}

// ClearAllPhrases removes all phrases by deleting and recreating the Phrases bucket.
func (s *Store) ClearAllPhrases() error {
	return s.db.Update(func(tx *bolt.Tx) error {
		if tx.Bucket(bucketPhrases) != nil {
			if err := tx.DeleteBucket(bucketPhrases); err != nil {
				return fmt.Errorf("delete Phrases bucket: %w", err)
			}
		}
		_, err := tx.CreateBucket(bucketPhrases)
		return err
	})
}

// SeedPhrases inserts records only when the Phrases bucket is empty.
// If phrases already exist the call is a no-op.
func (s *Store) SeedPhrases(records []PhraseRecord) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketPhrases)
		if b == nil {
			return fmt.Errorf("Phrases bucket not found")
		}
		if b.Stats().KeyN > 0 {
			return nil
		}
		for _, rec := range records {
			data, err := json.Marshal(rec)
			if err != nil {
				return fmt.Errorf("SeedPhrases marshal: %w", err)
			}
			if err := b.Put(phraseKey(rec), data); err != nil {
				return err
			}
		}
		return nil
	})
}
