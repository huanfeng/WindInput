package store

import (
	"encoding/json"
	"fmt"

	bolt "go.etcd.io/bbolt"
)

// PhraseRecord is the JSON-encoded value stored under a phrase key.
// Code is not stored in JSON (it's part of the bbolt key), but populated by
// read methods.
type PhraseRecord struct {
	Code     string `json:"-"`              // 从 key 解析，不序列化
	Text     string `json:"text,omitempty"` // 普通/动态短语的文本
	Texts    string `json:"texts,omitempty"`
	Name     string `json:"name,omitempty"`
	Type     string `json:"type"`
	Position int    `json:"pos"`
	Enabled  bool   `json:"on"`
	IsSystem bool   `json:"sys,omitempty"`
}

// phraseKey returns the composite key for a PhraseRecord.
// Array phrases use "code\x00\x01name"; others use "code\x00text".
func phraseKey(rec PhraseRecord) []byte {
	if rec.Type == "array" {
		return []byte(rec.Code + "\x00\x01" + rec.Name)
	}
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

// RemovePhrase deletes a phrase by its code and either text (regular/dynamic)
// or name (array). If name is non-empty, the array key format is used.
func (s *Store) RemovePhrase(code, text, name string) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketPhrases)
		if b == nil {
			return nil
		}
		var key []byte
		if name != "" {
			key = []byte(code + "\x00\x01" + name)
		} else {
			key = []byte(code + "\x00" + text)
		}
		return b.Delete(key)
	})
}

// SetPhraseEnabled toggles the Enabled flag of an existing phrase.
func (s *Store) SetPhraseEnabled(code, text, name string, enabled bool) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketPhrases)
		if b == nil {
			return fmt.Errorf("Phrases bucket not found")
		}
		var key []byte
		if name != "" {
			key = []byte(code + "\x00\x01" + name)
		} else {
			key = []byte(code + "\x00" + text)
		}
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
