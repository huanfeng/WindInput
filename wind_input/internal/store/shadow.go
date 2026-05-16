package store

import (
	"encoding/json"
	"fmt"
	"strings"

	bolt "go.etcd.io/bbolt"
)

var bucketShadow = []byte("Shadow")

// ShadowPin records a pinned word and its target position in the candidate list.
//
// 2026-05-17 R2: 新增 CandID 字段, 用于按候选稳定 id 精准定位 (动态短语
// 每天展开 Text 不一样, 旧 Word 匹配失效)。CandID 非空时取代 Word 做匹配,
// 否则按 Word 兼容旧行为 (含 alpha 阶段持久化的手输文本规则)。
type ShadowPin struct {
	Word     string `json:"w"`
	CandID   string `json:"id,omitempty"`
	Position int    `json:"pos"`
}

// ShadowDelete records a deleted (hidden) candidate.
//
// 2026-05-17 R2: 把原先纯 string 的 Deleted slice 升级为结构体,
// 引入 CandID 字段 (同 ShadowPin)。UnmarshalJSON 兼容旧版纯字符串
// 格式 — 旧 db 里的 `"d":["词A","词B"]` 仍能读为新结构 (Word=旧字符串,
// CandID="")。
type ShadowDelete struct {
	Word   string `json:"w"`
	CandID string `json:"id,omitempty"`
}

// UnmarshalJSON 兼容两种格式:
//   - 新版对象 {"w":"...","id":"..."}
//   - 旧版纯字符串 "..."  (此时填入 Word, CandID 留空)
func (d *ShadowDelete) UnmarshalJSON(data []byte) error {
	// 优先尝试旧版字符串 (典型 1~30 字节, 比对象短得多, 快速分支)
	if len(data) > 0 && data[0] == '"' {
		var s string
		if err := json.Unmarshal(data, &s); err == nil {
			d.Word = s
			d.CandID = ""
			return nil
		}
	}
	// 否则按新版对象解析 (避免无限递归: 用 alias 摆脱 UnmarshalJSON 方法)
	type alias ShadowDelete
	var v alias
	if err := json.Unmarshal(data, &v); err != nil {
		return err
	}
	*d = ShadowDelete(v)
	return nil
}

// ShadowRecord holds the pin and delete rules for a single code.
type ShadowRecord struct {
	Pinned  []ShadowPin    `json:"p,omitempty"`
	Deleted []ShadowDelete `json:"d,omitempty"`
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

// shadowMatch 判定一条 pin/delete 规则是否匹配目标。
// CandID 非空时按 id 精准匹配; 否则按 word 兼容旧行为。
func shadowMatchPin(p ShadowPin, word, candID string) bool {
	if candID != "" || p.CandID != "" {
		return p.CandID == candID
	}
	return p.Word == word
}

func shadowMatchDel(d ShadowDelete, word, candID string) bool {
	if candID != "" || d.CandID != "" {
		return d.CandID == candID
	}
	return d.Word == word
}

// PinShadow inserts a pin rule under the given code.
// Behaviour:
//   - Existing pin for the same (Word, CandID) target is removed first.
//   - The new pin is prepended to the Pinned slice (LIFO / most-recent-first).
//   - Matching entry in Deleted is removed (if present).
//
// candID 非空时按 id 匹配, 否则按 word; word 仍持久化以便手输规则 / UI 显示。
func (s *Store) PinShadow(schemaID, code, word, candID string, position int) error {
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

		// Remove old pin for the same target.
		filtered := rec.Pinned[:0]
		for _, p := range rec.Pinned {
			if !shadowMatchPin(p, word, candID) {
				filtered = append(filtered, p)
			}
		}
		// Prepend new pin (LIFO).
		rec.Pinned = append([]ShadowPin{{Word: word, CandID: candID, Position: position}}, filtered...)

		// Remove from Deleted.
		deleted := rec.Deleted[:0]
		for _, d := range rec.Deleted {
			if !shadowMatchDel(d, word, candID) {
				deleted = append(deleted, d)
			}
		}
		rec.Deleted = deleted

		return putShadow(b, code, &rec)
	})
}

// DeleteShadow adds a delete rule for the given target (deduped by word/candID)
// and removes any matching pin.
func (s *Store) DeleteShadow(schemaID, code, word, candID string) error {
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

		// Remove matching pins.
		filtered := rec.Pinned[:0]
		for _, p := range rec.Pinned {
			if !shadowMatchPin(p, word, candID) {
				filtered = append(filtered, p)
			}
		}
		rec.Pinned = filtered

		// Add to Deleted (dedup by target).
		found := false
		for _, d := range rec.Deleted {
			if shadowMatchDel(d, word, candID) {
				found = true
				break
			}
		}
		if !found {
			rec.Deleted = append(rec.Deleted, ShadowDelete{Word: word, CandID: candID})
		}

		return putShadow(b, code, &rec)
	})
}

// RemoveShadowRule removes both pin and delete rules for the given target.
// If the resulting record is empty the key is deleted entirely.
func (s *Store) RemoveShadowRule(schemaID, code, word, candID string) error {
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

		// Remove matching pins.
		filtered := rec.Pinned[:0]
		for _, p := range rec.Pinned {
			if !shadowMatchPin(p, word, candID) {
				filtered = append(filtered, p)
			}
		}
		rec.Pinned = filtered

		// Remove matching deletes.
		deleted := rec.Deleted[:0]
		for _, d := range rec.Deleted {
			if !shadowMatchDel(d, word, candID) {
				deleted = append(deleted, d)
			}
		}
		rec.Deleted = deleted

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
