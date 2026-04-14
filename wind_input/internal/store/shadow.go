package store

import (
	"encoding/json"
	"fmt"
	"strings"

	bolt "go.etcd.io/bbolt"
)

var bucketShadow = []byte("Shadow")

// ShadowPin records a pinned word and its target position in the candidate list.
type ShadowPin struct {
	Word     string `json:"w"`
	Position int    `json:"pos"`
}

// ShadowRecord holds the pin and delete rules for a single code.
type ShadowRecord struct {
	Pinned  []ShadowPin `json:"p,omitempty"`
	Deleted []string    `json:"d,omitempty"`
}

// GetShadowRules returns the shadow rules stored for the given code (lowercased).
// Returns an empty ShadowRecord (no error) when no rules exist.
func (s *Store) GetShadowRules(schemaID, code string) (ShadowRecord, error) {
	code = strings.ToLower(code)
	var rec ShadowRecord
	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketShadow), false)
		if err != nil {
			// bucket absent means no rules
			return nil
		}
		v := b.Get([]byte(code))
		if v == nil {
			return nil
		}
		return json.Unmarshal(v, &rec)
	})
	return rec, err
}

// PinShadow inserts a pin rule for word at position under the given code.
// Behaviour:
//   - Any existing pin for the same word is removed first.
//   - The new pin is prepended to the Pinned slice (LIFO / most-recent-first).
//   - The word is removed from Deleted if present.
func (s *Store) PinShadow(schemaID, code, word string, position int) error {
	code = strings.ToLower(code)
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketShadow), true)
		if err != nil {
			return err
		}
		var rec ShadowRecord
		if v := b.Get([]byte(code)); v != nil {
			if err := json.Unmarshal(v, &rec); err != nil {
				return fmt.Errorf("shadow unmarshal: %w", err)
			}
		}

		// Remove old pin for the same word.
		filtered := rec.Pinned[:0]
		for _, p := range rec.Pinned {
			if p.Word != word {
				filtered = append(filtered, p)
			}
		}
		// Prepend new pin (LIFO).
		rec.Pinned = append([]ShadowPin{{Word: word, Position: position}}, filtered...)

		// Remove from Deleted.
		rec.Deleted = removeStr(rec.Deleted, word)

		return putShadow(b, code, &rec)
	})
}

// DeleteShadow adds word to the Deleted list for the given code (deduped)
// and removes any existing pin for that word.
func (s *Store) DeleteShadow(schemaID, code, word string) error {
	code = strings.ToLower(code)
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketShadow), true)
		if err != nil {
			return err
		}
		var rec ShadowRecord
		if v := b.Get([]byte(code)); v != nil {
			if err := json.Unmarshal(v, &rec); err != nil {
				return fmt.Errorf("shadow unmarshal: %w", err)
			}
		}

		// Remove from Pinned.
		filtered := rec.Pinned[:0]
		for _, p := range rec.Pinned {
			if p.Word != word {
				filtered = append(filtered, p)
			}
		}
		rec.Pinned = filtered

		// Add to Deleted (dedup).
		found := false
		for _, d := range rec.Deleted {
			if d == word {
				found = true
				break
			}
		}
		if !found {
			rec.Deleted = append(rec.Deleted, word)
		}

		return putShadow(b, code, &rec)
	})
}

// RemoveShadowRule removes word from both Pinned and Deleted for the given code.
// If the resulting record is empty the key is deleted entirely.
func (s *Store) RemoveShadowRule(schemaID, code, word string) error {
	code = strings.ToLower(code)
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketShadow), false)
		if err != nil {
			// Bucket absent — nothing to remove.
			return nil
		}
		key := []byte(code)
		v := b.Get(key)
		if v == nil {
			return nil
		}
		var rec ShadowRecord
		if err := json.Unmarshal(v, &rec); err != nil {
			return fmt.Errorf("shadow unmarshal: %w", err)
		}

		// Remove from Pinned.
		filtered := rec.Pinned[:0]
		for _, p := range rec.Pinned {
			if p.Word != word {
				filtered = append(filtered, p)
			}
		}
		rec.Pinned = filtered

		// Remove from Deleted.
		rec.Deleted = removeStr(rec.Deleted, word)

		// If record is empty, delete the key.
		if len(rec.Pinned) == 0 && len(rec.Deleted) == 0 {
			return b.Delete(key)
		}
		return putShadow(b, code, &rec)
	})
}

// ShadowRuleCount returns the number of codes that have at least one rule
// stored in the Shadow sub-bucket for the given schema.
func (s *Store) ShadowRuleCount(schemaID string) (int, error) {
	var count int
	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketShadow), false)
		if err != nil {
			// Bucket absent means zero rules.
			return nil
		}
		count = b.Stats().KeyN
		return nil
	})
	return count, err
}

// GetAllShadowRules returns all code→ShadowRecord entries for the given schema.
func (s *Store) GetAllShadowRules(schemaID string) (map[string]ShadowRecord, error) {
	result := make(map[string]ShadowRecord)
	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketShadow), false)
		if err != nil {
			return nil
		}
		return b.ForEach(func(k, v []byte) error {
			var rec ShadowRecord
			if err := json.Unmarshal(v, &rec); err != nil {
				return fmt.Errorf("shadow unmarshal key %q: %w", k, err)
			}
			result[string(k)] = rec
			return nil
		})
	})
	return result, err
}

// putShadow marshals rec and stores it under code in bucket b.
func putShadow(b *bolt.Bucket, code string, rec *ShadowRecord) error {
	data, err := json.Marshal(rec)
	if err != nil {
		return fmt.Errorf("shadow marshal: %w", err)
	}
	return b.Put([]byte(code), data)
}

// removeStr removes the first occurrence of s from slice and returns the result.
func removeStr(slice []string, s string) []string {
	for i, v := range slice {
		if v == s {
			return append(slice[:i], slice[i+1:]...)
		}
	}
	return slice
}
