package store

import (
	"bytes"
	"encoding/json"
	"fmt"

	bolt "go.etcd.io/bbolt"
)

// UserWordBulkEntry 用于批量导出/导入用户词条。
type UserWordBulkEntry struct {
	Code      string
	Text      string
	Weight    int
	Count     int
	CreatedAt int64
}

// FreqBulkEntry 用于批量导出/导入词频数据。
type FreqBulkEntry struct {
	Code     string
	Text     string
	Count    uint32
	LastUsed int64
	Streak   uint8
}

// ShadowBulkEntry 用于批量导出/导入 Shadow 规则（保留原始序列化值）。
type ShadowBulkEntry struct {
	Code     string
	RawValue []byte
}

// PhraseBulkEntry 用于批量导出/导入短语（保留原始序列化值）。
type PhraseBulkEntry struct {
	Code     string // 从 key 解析的 code（供上层读取用）
	RawKey   []byte // 原始 bbolt key（字节级 round-trip）
	RawValue []byte
}

// DailyStatBulkEntry 用于批量导出/导入每日统计（保留原始 JSON）。
type DailyStatBulkEntry struct {
	Date     string
	RawValue []byte
}

// AllUserWords 导出指定方案的所有用户词条。
func (s *Store) AllUserWords(schemaID string) ([]UserWordBulkEntry, error) {
	return allUserWordEntries(s, schemaID, string(bucketUserWords))
}

// AllTempWords 导出指定方案的所有临时词条。
func (s *Store) AllTempWords(schemaID string) ([]UserWordBulkEntry, error) {
	return allUserWordEntries(s, schemaID, string(bucketTempWords))
}

// allUserWordEntries 从指定方案的指定子 bucket 中读取所有词条。
func allUserWordEntries(s *Store, schemaID, subBucket string) ([]UserWordBulkEntry, error) {
	var results []UserWordBulkEntry
	err := s.db.View(func(tx *bolt.Tx) error {
		schemas := tx.Bucket(bucketSchemas)
		if schemas == nil {
			return nil
		}
		schemaB := schemas.Bucket([]byte(schemaID))
		if schemaB == nil {
			return nil
		}
		b := schemaB.Bucket([]byte(subBucket))
		if b == nil {
			return nil
		}
		return b.ForEach(func(k, v []byte) error {
			code, _ := parseUserWordsKey(k)
			var rec UserWordRecord
			if err := json.Unmarshal(v, &rec); err != nil {
				return fmt.Errorf("allUserWordEntries unmarshal key %q: %w", k, err)
			}
			results = append(results, UserWordBulkEntry{
				Code:      code,
				Text:      rec.Text,
				Weight:    rec.Weight,
				Count:     rec.Count,
				CreatedAt: rec.CreatedAt,
			})
			return nil
		})
	})
	return results, err
}

// BulkPutUserWords 批量写入用户词条（追加，不清空已有数据）。
func (s *Store) BulkPutUserWords(schemaID string, entries []UserWordBulkEntry) error {
	return bulkPutUserWordEntries(s, schemaID, string(bucketUserWords), entries)
}

// BulkPutTempWords 批量写入临时词条（追加，不清空已有数据）。
func (s *Store) BulkPutTempWords(schemaID string, entries []UserWordBulkEntry) error {
	return bulkPutUserWordEntries(s, schemaID, string(bucketTempWords), entries)
}

// bulkPutUserWordEntries 将词条写入指定方案的指定子 bucket。
func bulkPutUserWordEntries(s *Store, schemaID, subBucket string, entries []UserWordBulkEntry) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, subBucket, true)
		if err != nil {
			return fmt.Errorf("bulkPutUserWordEntries: %w", err)
		}
		for _, e := range entries {
			rec := UserWordRecord{
				Text:      e.Text,
				Weight:    e.Weight,
				Count:     e.Count,
				CreatedAt: e.CreatedAt,
			}
			data, err := json.Marshal(rec)
			if err != nil {
				return fmt.Errorf("bulkPutUserWordEntries marshal %q: %w", e.Text, err)
			}
			if err := b.Put(userWordsKey(e.Code, e.Text), data); err != nil {
				return err
			}
		}
		return nil
	})
}

// AllFreq 导出指定方案的所有词频数据。
func (s *Store) AllFreq(schemaID string) ([]FreqBulkEntry, error) {
	var results []FreqBulkEntry
	err := s.db.View(func(tx *bolt.Tx) error {
		schemas := tx.Bucket(bucketSchemas)
		if schemas == nil {
			return nil
		}
		schemaB := schemas.Bucket([]byte(schemaID))
		if schemaB == nil {
			return nil
		}
		b := schemaB.Bucket(bucketFreq)
		if b == nil {
			return nil
		}
		return b.ForEach(func(k, v []byte) error {
			// key 格式为 "code:text"
			codeB, textB, ok := bytes.Cut(k, []byte(":"))
			if !ok {
				return nil
			}
			code := string(codeB)
			text := string(textB)
			var rec FreqRecord
			if err := json.Unmarshal(v, &rec); err != nil {
				return fmt.Errorf("AllFreq unmarshal key %q: %w", k, err)
			}
			results = append(results, FreqBulkEntry{
				Code:     code,
				Text:     text,
				Count:    rec.Count,
				LastUsed: rec.LastUsed,
				Streak:   rec.Streak,
			})
			return nil
		})
	})
	return results, err
}

// BulkPutFreq 批量写入词频数据（追加，不清空已有数据）。
func (s *Store) BulkPutFreq(schemaID string, entries []FreqBulkEntry) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketFreq), true)
		if err != nil {
			return fmt.Errorf("BulkPutFreq: %w", err)
		}
		for _, e := range entries {
			rec := FreqRecord{
				Count:    e.Count,
				LastUsed: e.LastUsed,
				Streak:   e.Streak,
			}
			data, err := json.Marshal(rec)
			if err != nil {
				return fmt.Errorf("BulkPutFreq marshal %q/%q: %w", e.Code, e.Text, err)
			}
			if err := b.Put([]byte(freqKey(e.Code, e.Text)), data); err != nil {
				return err
			}
		}
		return nil
	})
}

// AllShadow 导出指定方案的所有 Shadow 规则（保留原始字节）。
func (s *Store) AllShadow(schemaID string) ([]ShadowBulkEntry, error) {
	var results []ShadowBulkEntry
	err := s.db.View(func(tx *bolt.Tx) error {
		schemas := tx.Bucket(bucketSchemas)
		if schemas == nil {
			return nil
		}
		schemaB := schemas.Bucket([]byte(schemaID))
		if schemaB == nil {
			return nil
		}
		b := schemaB.Bucket(bucketShadow)
		if b == nil {
			return nil
		}
		return b.ForEach(func(k, v []byte) error {
			val := make([]byte, len(v))
			copy(val, v)
			results = append(results, ShadowBulkEntry{
				Code:     string(k),
				RawValue: val,
			})
			return nil
		})
	})
	return results, err
}

// BulkPutShadow 批量写入 Shadow 规则（追加，不清空已有数据）。
func (s *Store) BulkPutShadow(schemaID string, entries []ShadowBulkEntry) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketShadow), true)
		if err != nil {
			return fmt.Errorf("BulkPutShadow: %w", err)
		}
		for _, e := range entries {
			if err := b.Put([]byte(e.Code), e.RawValue); err != nil {
				return err
			}
		}
		return nil
	})
}

// AllSchemaPhrases 暂不支持（当前无 per-schema phrases bucket）。
// 始终返回空切片，保留接口兼容性。
func (s *Store) AllSchemaPhrases(_ string) ([]PhraseBulkEntry, error) {
	return nil, nil
}

// BulkPutSchemaPhrases 暂不支持（当前无 per-schema phrases bucket），为空操作。
func (s *Store) BulkPutSchemaPhrases(_ string, _ []PhraseBulkEntry) error {
	return nil
}

// AllGlobalPhrases 导出全局短语（保留原始字节）。
func (s *Store) AllGlobalPhrases() ([]PhraseBulkEntry, error) {
	var results []PhraseBulkEntry
	err := s.db.View(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketPhrases)
		if b == nil {
			return nil
		}
		return b.ForEach(func(k, v []byte) error {
			code, _ := parsePhraseKey(k)
			rawKey := make([]byte, len(k))
			copy(rawKey, k)
			val := make([]byte, len(v))
			copy(val, v)
			results = append(results, PhraseBulkEntry{
				Code:     code,
				RawKey:   rawKey,
				RawValue: val,
			})
			return nil
		})
	})
	return results, err
}

// BulkPutGlobalPhrases 批量写入全局短语（追加，不清空已有数据）。
// 使用 RawKey 实现字节级 round-trip。
func (s *Store) BulkPutGlobalPhrases(entries []PhraseBulkEntry) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketPhrases)
		if b == nil {
			return fmt.Errorf("Phrases bucket not found")
		}
		for _, e := range entries {
			if err := b.Put(e.RawKey, e.RawValue); err != nil {
				return err
			}
		}
		return nil
	})
}

// AllStats 导出所有每日统计（保留原始 JSON）。
func (s *Store) AllStats() ([]DailyStatBulkEntry, error) {
	var results []DailyStatBulkEntry
	err := s.db.View(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketStats)
		if b == nil {
			return nil
		}
		daily := b.Bucket(bucketStatsDay)
		if daily == nil {
			return nil
		}
		return daily.ForEach(func(k, v []byte) error {
			val := make([]byte, len(v))
			copy(val, v)
			results = append(results, DailyStatBulkEntry{
				Date:     string(k),
				RawValue: val,
			})
			return nil
		})
	})
	return results, err
}

// BulkPutStats 批量写入每日统计（追加，不清空已有数据）。
func (s *Store) BulkPutStats(entries []DailyStatBulkEntry) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := tx.CreateBucketIfNotExists(bucketStats)
		if err != nil {
			return fmt.Errorf("BulkPutStats create Stats: %w", err)
		}
		daily, err := b.CreateBucketIfNotExists(bucketStatsDay)
		if err != nil {
			return fmt.Errorf("BulkPutStats create Stats/Daily: %w", err)
		}
		for _, e := range entries {
			if err := daily.Put([]byte(e.Date), e.RawValue); err != nil {
				return err
			}
		}
		return nil
	})
}
