package store

import (
	"encoding/json"
	"fmt"
	"sort"
	"strings"
	"time"

	bolt "go.etcd.io/bbolt"
)

var bucketTempWords = []byte("TempWords")

const tempWordMaxWeight = 10000

// LearnTempWord adds or updates a temporary word entry for the given schema.
// If the entry exists its weight is incremented by weightDelta (capped at
// tempWordMaxWeight) and its count is incremented by 1.
// If the entry is new it is created with weight=weightDelta and count=1.
func (s *Store) LearnTempWord(schemaID, code, text string, weightDelta int) error {
	code = strings.ToLower(code)
	text = strings.ToLower(text)
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketTempWords), true)
		if err != nil {
			return err
		}
		k := userWordsKey(code, text)
		var rec UserWordRecord
		if existing := b.Get(k); existing != nil {
			if err := json.Unmarshal(existing, &rec); err != nil {
				return fmt.Errorf("unmarshal TempWord: %w", err)
			}
			rec.Weight += weightDelta
			if rec.Weight > tempWordMaxWeight {
				rec.Weight = tempWordMaxWeight
			}
			rec.Count++
		} else {
			rec = UserWordRecord{
				Text:      text,
				Weight:    weightDelta,
				Count:     1,
				CreatedAt: time.Now().UnixMilli(),
			}
		}
		data, err := json.Marshal(rec)
		if err != nil {
			return fmt.Errorf("marshal TempWord: %w", err)
		}
		return b.Put(k, data)
	})
}

// GetTempWords returns all temp words for an exact code match in the given schema.
func (s *Store) GetTempWords(schemaID, code string) ([]UserWordRecord, error) {
	code = strings.ToLower(code)
	var results []UserWordRecord
	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketTempWords), false)
		if err != nil {
			return nil
		}
		prefix := []byte(code + "\x00")
		c := b.Cursor()
		for k, v := c.Seek(prefix); k != nil && hasPrefix(k, prefix); k, v = c.Next() {
			var rec UserWordRecord
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

// SearchTempWordsPrefix returns up to limit temp words whose key starts with the
// given code prefix across the schema.
func (s *Store) SearchTempWordsPrefix(schemaID, prefix string, limit int) ([]UserWordRecord, error) {
	prefix = strings.ToLower(prefix)
	var results []UserWordRecord
	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketTempWords), false)
		if err != nil {
			return nil
		}
		pfx := []byte(prefix)
		c := b.Cursor()
		for k, v := c.Seek(pfx); k != nil && hasPrefix(k, pfx); k, v = c.Next() {
			if limit > 0 && len(results) >= limit {
				break
			}
			var rec UserWordRecord
			if err := json.Unmarshal(v, &rec); err != nil {
				continue
			}
			kCode, _ := parseUserWordsKey(k)
			rec.Code = kCode
			results = append(results, rec)
		}
		return nil
	})
	return results, err
}

// PromoteTempWord moves a word from the TempWords bucket to the UserWords bucket
// atomically in a single transaction.
func (s *Store) PromoteTempWord(schemaID, code, text string) error {
	code = strings.ToLower(code)
	text = strings.ToLower(text)
	return s.db.Update(func(tx *bolt.Tx) error {
		tmpBucket, err := schemaSubBucket(tx, schemaID, string(bucketTempWords), false)
		if err != nil {
			return fmt.Errorf("TempWords bucket: %w", err)
		}
		k := userWordsKey(code, text)
		data := tmpBucket.Get(k)
		if data == nil {
			return fmt.Errorf("temp word not found: code=%q text=%q", code, text)
		}
		var rec UserWordRecord
		if err := json.Unmarshal(data, &rec); err != nil {
			return fmt.Errorf("unmarshal TempWord: %w", err)
		}

		userBucket, err := schemaSubBucket(tx, schemaID, string(bucketUserWords), true)
		if err != nil {
			return fmt.Errorf("UserWords bucket: %w", err)
		}
		// Merge with existing user word if present.
		if existing := userBucket.Get(k); existing != nil {
			var old UserWordRecord
			if err := json.Unmarshal(existing, &old); err == nil {
				rec.Weight += old.Weight
				if rec.Weight > tempWordMaxWeight {
					rec.Weight = tempWordMaxWeight
				}
				rec.Count += old.Count
				if old.CreatedAt != 0 {
					rec.CreatedAt = old.CreatedAt
				}
			}
		}
		out, err := json.Marshal(rec)
		if err != nil {
			return fmt.Errorf("marshal UserWordRecord: %w", err)
		}
		if err := userBucket.Put(k, out); err != nil {
			return err
		}
		return tmpBucket.Delete(k)
	})
}

// EvictTempWords deletes the lowest-weight entries until at most maxKeep remain.
// Returns the number of entries deleted.
func (s *Store) EvictTempWords(schemaID string, maxKeep int) (int, error) {
	type entry struct {
		key    []byte
		weight int
	}
	var all []entry

	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketTempWords), false)
		if err != nil {
			return nil
		}
		return b.ForEach(func(k, v []byte) error {
			var rec UserWordRecord
			if err := json.Unmarshal(v, &rec); err != nil {
				return nil
			}
			keyCopy := make([]byte, len(k))
			copy(keyCopy, k)
			all = append(all, entry{key: keyCopy, weight: rec.Weight})
			return nil
		})
	})
	if err != nil {
		return 0, err
	}

	if len(all) <= maxKeep {
		return 0, nil
	}

	// Sort ascending by weight so the lowest-weight entries come first.
	sort.Slice(all, func(i, j int) bool {
		return all[i].weight < all[j].weight
	})

	toDelete := all[:len(all)-maxKeep]
	deleted := 0
	err = s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketTempWords), false)
		if err != nil {
			return err
		}
		for _, e := range toDelete {
			if err := b.Delete(e.key); err != nil {
				return err
			}
			deleted++
		}
		return nil
	})
	return deleted, err
}

// ClearTempWords deletes all temp words for the given schema by deleting and
// recreating the TempWords sub-bucket. Returns the number of entries removed.
func (s *Store) ClearTempWords(schemaID string) (int, error) {
	var count int
	err := s.db.Update(func(tx *bolt.Tx) error {
		parent, err := schemaBucket(tx, schemaID, true)
		if err != nil {
			return err
		}
		if b := parent.Bucket(bucketTempWords); b != nil {
			count = b.Stats().KeyN
			if err := parent.DeleteBucket(bucketTempWords); err != nil {
				return fmt.Errorf("delete TempWords bucket: %w", err)
			}
		}
		_, err = parent.CreateBucket(bucketTempWords)
		return err
	})
	return count, err
}

// TempWordCount returns the number of temp words stored for the given schema.
// RemoveTempWord 删除临时词条
func (s *Store) RemoveTempWord(schemaID, code, text string) error {
	code = strings.ToLower(code)
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketTempWords), false)
		if err != nil || b == nil {
			return nil
		}
		return b.Delete(userWordsKey(code, text))
	})
}

func (s *Store) TempWordCount(schemaID string) (int, error) {
	var count int
	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketTempWords), false)
		if err != nil {
			return nil
		}
		count = b.Stats().KeyN
		return nil
	})
	return count, err
}
