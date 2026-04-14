package store

import (
	"encoding/json"
	"fmt"
	"strings"
	"time"

	bolt "go.etcd.io/bbolt"
)

var bucketUserWords = []byte("UserWords")

// UserWordRecord is the JSON-encoded value stored under a user/temp word key.
type UserWordRecord struct {
	Text      string `json:"t"`
	Weight    int    `json:"w"`
	Count     int    `json:"c,omitempty"`
	CreatedAt int64  `json:"ts"`
}

// userWordsKey returns the composite key "code\x00text" used in user/temp word buckets.
func userWordsKey(code, text string) []byte {
	return []byte(code + "\x00" + text)
}

// parseUserWordsKey splits a composite key back into code and text.
func parseUserWordsKey(key []byte) (code, text string) {
	for i, b := range key {
		if b == '\x00' {
			return string(key[:i]), string(key[i+1:])
		}
	}
	return string(key), ""
}

// hasPrefix reports whether key starts with prefix.
func hasPrefix(key, prefix []byte) bool {
	if len(key) < len(prefix) {
		return false
	}
	for i, b := range prefix {
		if key[i] != b {
			return false
		}
	}
	return true
}

// AddUserWord inserts or updates a user word entry in the UserWords sub-bucket
// for the given schema. code is lowercased before storage. On duplicate, weight
// is updated to max(old, new).
func (s *Store) AddUserWord(schemaID, code, text string, weight int) error {
	code = strings.ToLower(code)
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketUserWords), true)
		if err != nil {
			return err
		}
		k := userWordsKey(code, text)
		var rec UserWordRecord
		if existing := b.Get(k); existing != nil {
			if err := json.Unmarshal(existing, &rec); err == nil {
				if weight > rec.Weight {
					rec.Weight = weight
				}
			} else {
				rec = UserWordRecord{Text: text, Weight: weight, CreatedAt: time.Now().Unix()}
			}
		} else {
			rec = UserWordRecord{Text: text, Weight: weight, CreatedAt: time.Now().Unix()}
		}
		data, err := json.Marshal(rec)
		if err != nil {
			return fmt.Errorf("marshal UserWordRecord: %w", err)
		}
		return b.Put(k, data)
	})
}

// RemoveUserWord deletes a user word entry from the given schema.
func (s *Store) RemoveUserWord(schemaID, code, text string) error {
	code = strings.ToLower(code)
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketUserWords), false)
		if err != nil {
			return fmt.Errorf("RemoveUserWord: %w", err)
		}
		return b.Delete(userWordsKey(code, text))
	})
}

// UpdateUserWordWeight sets a new weight for an existing user word.
// Returns an error if the entry does not exist.
func (s *Store) UpdateUserWordWeight(schemaID, code, text string, newWeight int) error {
	code = strings.ToLower(code)
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketUserWords), false)
		if err != nil {
			return fmt.Errorf("UpdateUserWordWeight: %w", err)
		}
		k := userWordsKey(code, text)
		raw := b.Get(k)
		if raw == nil {
			return fmt.Errorf("UpdateUserWordWeight: entry %q/%q not found", code, text)
		}
		var rec UserWordRecord
		if err := json.Unmarshal(raw, &rec); err != nil {
			return fmt.Errorf("UpdateUserWordWeight unmarshal: %w", err)
		}
		rec.Weight = newWeight
		data, err := json.Marshal(rec)
		if err != nil {
			return fmt.Errorf("UpdateUserWordWeight marshal: %w", err)
		}
		return b.Put(k, data)
	})
}

// GetUserWords returns all user words stored for the given code in the schema.
func (s *Store) GetUserWords(schemaID, code string) ([]UserWordRecord, error) {
	code = strings.ToLower(code)
	var results []UserWordRecord
	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketUserWords), false)
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
			results = append(results, rec)
		}
		return nil
	})
	return results, err
}

// SearchUserWordsPrefix returns user words whose code starts with prefix,
// up to limit results (limit <= 0 means unlimited).
func (s *Store) SearchUserWordsPrefix(schemaID, prefix string, limit int) ([]UserWordRecord, error) {
	prefix = strings.ToLower(prefix)
	var results []UserWordRecord
	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketUserWords), false)
		if err != nil {
			return nil
		}
		seek := []byte(prefix)
		c := b.Cursor()
		for k, v := c.Seek(seek); k != nil && hasPrefix(k, seek); k, v = c.Next() {
			if limit > 0 && len(results) >= limit {
				break
			}
			var rec UserWordRecord
			if err := json.Unmarshal(v, &rec); err != nil {
				continue
			}
			results = append(results, rec)
		}
		return nil
	})
	return results, err
}

// UserWordCount returns the total number of user word entries for schemaID.
func (s *Store) UserWordCount(schemaID string) (int, error) {
	var count int
	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketUserWords), false)
		if err != nil {
			return nil
		}
		count = b.Stats().KeyN
		return nil
	})
	return count, err
}

// OnWordSelected is called when the user selects a word. It increments the
// selection count atomically and boosts the weight by boostDelta once the
// count reaches a multiple of countThreshold.
func (s *Store) OnWordSelected(schemaID, code, text string, boostDelta, countThreshold int) error {
	code = strings.ToLower(code)
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketUserWords), true)
		if err != nil {
			return fmt.Errorf("OnWordSelected: %w", err)
		}
		k := userWordsKey(code, text)
		var rec UserWordRecord
		if raw := b.Get(k); raw != nil {
			if err := json.Unmarshal(raw, &rec); err != nil {
				return fmt.Errorf("OnWordSelected unmarshal: %w", err)
			}
		} else {
			rec = UserWordRecord{Text: text, CreatedAt: time.Now().Unix()}
		}
		rec.Count++
		if countThreshold > 0 && rec.Count%countThreshold == 0 {
			rec.Weight += boostDelta
		}
		data, err := json.Marshal(rec)
		if err != nil {
			return fmt.Errorf("OnWordSelected marshal: %w", err)
		}
		return b.Put(k, data)
	})
}
