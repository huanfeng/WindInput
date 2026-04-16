package store

import (
	"bytes"
	"encoding/json"
	"fmt"
	"math"
	"strings"
	"time"

	bolt "go.etcd.io/bbolt"
)

const FreqBoostMax = 2000

var bucketFreq = []byte("Freq")

// FreqRecord holds per-candidate frequency data for a given (code, text) pair.
type FreqRecord struct {
	Count    uint32 `json:"c"`
	LastUsed int64  `json:"t"`
	Streak   uint8  `json:"s,omitempty"`
}

// freqKey returns the composite bucket key for a (code, text) pair.
func freqKey(code, text string) string {
	return code + ":" + text
}

// GetFreq reads the FreqRecord for (code, text) under the given schema.
// Returns a zero FreqRecord (Count==0) if the key does not exist yet.
func (s *Store) GetFreq(schemaID, code, text string) (FreqRecord, error) {
	var rec FreqRecord
	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketFreq), false)
		if err != nil {
			// Bucket not yet created → treat as empty.
			return nil
		}
		v := b.Get([]byte(freqKey(code, text)))
		if v == nil {
			return nil
		}
		return json.Unmarshal(v, &rec)
	})
	return rec, err
}

// IncrementFreq increments Count by 1, updates LastUsed to now (Unix seconds),
// and increments Streak (capped at 255) for the given (code, text) pair.
func (s *Store) IncrementFreq(schemaID, code, text string) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketFreq), true)
		if err != nil {
			return fmt.Errorf("IncrementFreq: %w", err)
		}
		key := []byte(freqKey(code, text))
		var rec FreqRecord
		if v := b.Get(key); v != nil {
			if err := json.Unmarshal(v, &rec); err != nil {
				return fmt.Errorf("IncrementFreq unmarshal: %w", err)
			}
		}
		rec.Count++
		rec.LastUsed = time.Now().Unix()
		if rec.Streak < 255 {
			rec.Streak++
		}
		data, err := json.Marshal(rec)
		if err != nil {
			return fmt.Errorf("IncrementFreq marshal: %w", err)
		}
		return b.Put(key, data)
	})
}

// ResetStreak sets Streak to 0 for the given (code, text) pair.
// If the record does not exist, this is a no-op.
func (s *Store) ResetStreak(schemaID, code, text string) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketFreq), false)
		if err != nil {
			// Bucket not yet created → nothing to reset.
			return nil
		}
		key := []byte(freqKey(code, text))
		v := b.Get(key)
		if v == nil {
			return nil
		}
		var rec FreqRecord
		if err := json.Unmarshal(v, &rec); err != nil {
			return fmt.Errorf("ResetStreak unmarshal: %w", err)
		}
		if rec.Streak == 0 {
			return nil
		}
		rec.Streak = 0
		data, err := json.Marshal(rec)
		if err != nil {
			return fmt.Errorf("ResetStreak marshal: %w", err)
		}
		return b.Put(key, data)
	})
}

// FreqEntry holds a frequency record with its parsed code and text.
type FreqEntry struct {
	Code   string
	Text   string
	Record FreqRecord
}

// SearchFreqPrefix returns freq entries whose key starts with the given prefix.
// If prefix is empty, returns all entries. Results are limited by limit (0 = unlimited).
func (s *Store) SearchFreqPrefix(schemaID, prefix string, limit int) ([]FreqEntry, error) {
	var results []FreqEntry
	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketFreq), false)
		if err != nil {
			return nil
		}
		c := b.Cursor()
		var k, v []byte
		if prefix == "" {
			k, v = c.First()
		} else {
			k, v = c.Seek([]byte(prefix))
		}
		pfx := []byte(prefix)
		for ; k != nil; k, v = c.Next() {
			if prefix != "" && !bytes.HasPrefix(k, pfx) {
				break
			}
			parts := strings.SplitN(string(k), ":", 2)
			if len(parts) != 2 {
				continue
			}
			var rec FreqRecord
			if err := json.Unmarshal(v, &rec); err != nil {
				return fmt.Errorf("SearchFreqPrefix unmarshal key %q: %w", k, err)
			}
			results = append(results, FreqEntry{
				Code:   parts[0],
				Text:   parts[1],
				Record: rec,
			})
			if limit > 0 && len(results) >= limit {
				break
			}
		}
		return nil
	})
	return results, err
}

// PutFreq sets a FreqRecord directly for the given (code, text) pair.
func (s *Store) PutFreq(schemaID, code, text string, rec FreqRecord) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketFreq), true)
		if err != nil {
			return fmt.Errorf("PutFreq: %w", err)
		}
		data, err := json.Marshal(rec)
		if err != nil {
			return fmt.Errorf("PutFreq marshal: %w", err)
		}
		return b.Put([]byte(freqKey(code, text)), data)
	})
}

// DeleteFreq removes a single frequency record.
func (s *Store) DeleteFreq(schemaID, code, text string) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketFreq), false)
		if err != nil {
			return nil
		}
		return b.Delete([]byte(freqKey(code, text)))
	})
}

// ClearAllFreq removes all frequency data for a schema by deleting and
// recreating the Freq sub-bucket. Returns the number of entries removed.
func (s *Store) ClearAllFreq(schemaID string) (int, error) {
	var count int
	err := s.db.Update(func(tx *bolt.Tx) error {
		parent, err := schemaBucket(tx, schemaID, true)
		if err != nil {
			return fmt.Errorf("ClearAllFreq: %w", err)
		}
		fb := parent.Bucket(bucketFreq)
		if fb == nil {
			return nil
		}
		count = fb.Stats().KeyN
		if err := parent.DeleteBucket(bucketFreq); err != nil {
			return fmt.Errorf("ClearAllFreq delete: %w", err)
		}
		if _, err := parent.CreateBucket(bucketFreq); err != nil {
			return fmt.Errorf("ClearAllFreq recreate: %w", err)
		}
		return nil
	})
	return count, err
}

// GetAllFreq returns all FreqRecords for the given schema, keyed by "code:text".
func (s *Store) GetAllFreq(schemaID string) (map[string]FreqRecord, error) {
	result := make(map[string]FreqRecord)
	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketFreq), false)
		if err != nil {
			// Bucket not yet created → return empty map.
			return nil
		}
		return b.ForEach(func(k, v []byte) error {
			var rec FreqRecord
			if err := json.Unmarshal(v, &rec); err != nil {
				return fmt.Errorf("GetAllFreq unmarshal key %q: %w", k, err)
			}
			result[string(k)] = rec
			return nil
		})
	})
	return result, err
}

// CalcFreqBoost computes a priority boost score for the given FreqRecord.
//
// Scoring:
//   - base    = log2(count+1) * 100
//   - recency: <1h=200, <1day=100, <1week=50, else 0
//   - streak:  min(streak*50, 250)
//   - total capped at FreqBoostMax (2000)
//   - returns 0 if Count == 0
func CalcFreqBoost(rec FreqRecord, now int64) int {
	if rec.Count == 0 {
		return 0
	}

	base := int(math.Log2(float64(rec.Count)+1) * 100)

	age := now - rec.LastUsed
	var recency int
	switch {
	case age < 3600:
		recency = 200
	case age < 86400:
		recency = 100
	case age < 7*86400:
		recency = 50
	default:
		recency = 0
	}

	streak := int(rec.Streak) * 50
	if streak > 250 {
		streak = 250
	}

	total := base + recency + streak
	if total > FreqBoostMax {
		total = FreqBoostMax
	}
	return total
}

// BatchIncrementFreq enqueues a freq update via the WriteBuffer.
// The caller supplies the current rec (e.g. from GetFreq) so we can
// pre-compute the next state without a separate read transaction.
func BatchIncrementFreq(wb *WriteBuffer, schemaID, code, text string, rec FreqRecord) {
	rec.Count++
	rec.LastUsed = time.Now().Unix()
	if rec.Streak < 255 {
		rec.Streak++
	}
	data, err := json.Marshal(rec)
	if err != nil {
		return
	}
	wb.Enqueue(WriteOp{
		Bucket: [][]byte{bucketSchemas, []byte(schemaID), bucketFreq},
		Key:    freqKey(code, text),
		Value:  data,
	})
}
