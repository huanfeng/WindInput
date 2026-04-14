package store

import (
	"encoding/json"
	"fmt"
	"math"
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
