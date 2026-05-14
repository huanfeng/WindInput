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
// Deprecated 字段:
//   - Texts / Name: 字符组短语已统一改用 Text 字段携带 $AA("name", "chars")
//     marker (见 internal/dict/aa_marker.go)。MigratePhraseRecordsToAA 会把
//     旧记录改写到 Text 字段并清空这两个字段。本轮保留是为了让 migration 自己
//     还能读到旧字段; 下一轮可彻底删除。
type PhraseRecord struct {
	Code  string `json:"-"`               // 从 key 解析，不序列化
	Text  string `json:"text,omitempty"`  // 普通/动态/字符组($AA) 短语的文本
	Texts string `json:"texts,omitempty"` // Deprecated: 用 Text 的 $AA marker 替代
	Name  string `json:"name,omitempty"`  // Deprecated: 用 Text 的 $AA marker 替代
	Type  string `json:"type"`
	// Weight 是显式权重 (0~10000), 优先于 Position。
	// 0 (默认零值) 表示"未设置", 由 PhraseLayer 走 Position fallback。
	Weight   int  `json:"w,omitempty"`
	Position int  `json:"pos"`
	Enabled  bool `json:"on"`
	IsSystem bool `json:"sys,omitempty"`
}

// phraseKey returns the composite key for a PhraseRecord.
// Array phrases use "code\x00\x01name" only when Name 仍非空 (旧格式兼容);
// 字符组 marker 化后 Name 已被清空, 用 Text ($AA marker) 作为 key 主体,
// 走与普通短语相同的 "code\x00text" 路径, 避免出现孤儿 "code\x00\x01" 坏 key。
func phraseKey(rec PhraseRecord) []byte {
	if rec.Type == "array" && rec.Name != "" {
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
//
// 兼容性: 数组类条目历史上可能存在多种 key 形式:
//   - 旧种子/旧 Name 字段: "code\x00\x01name"
//   - marker 化后: "code\x00$AA(name, chars)" (Name 已清空)
//   - 其它 legacy 形态 (旧迁移残留)
//
// 删除策略:
//  1. 先尝试已知精确 key (快速路径)
//  2. 再扫描 bucket 内所有 code 前缀匹配的记录, 按身份 (Text/Name/Texts/$AA)
//     做容忍匹配兜底, 覆盖任意历史 key 形态。
func (s *Store) RemovePhrase(code, text, name string) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketPhrases)
		if b == nil {
			return nil
		}
		return removePhraseInBucket(b, code, text, name)
	})
}

// RemovePhrasesBatch deletes multiple phrases in a single transaction.
// 每项 (code,text,name) 都按 RemovePhrase 同样的兼容性策略 (精确 key + ForEach 扫描兜底)。
func (s *Store) RemovePhrasesBatch(items []PhraseRecord) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketPhrases)
		if b == nil {
			return nil
		}
		for _, rec := range items {
			if err := removePhraseInBucket(b, rec.Code, rec.Text, rec.Name); err != nil {
				return err
			}
		}
		return nil
	})
}

// removePhraseInBucket: 先按已知 key 形态精确删, 再扫描 bucket 用 matchesIdentity
// 兜底匹配所有 legacy key。删除是低频操作, O(N) 扫描成本可接受。
func removePhraseInBucket(b *bolt.Bucket, code, text, name string) error {
	// 1) 精确 key 快速路径
	for _, k := range phraseDeletionKeys(code, text, name) {
		if err := b.Delete(k); err != nil {
			return err
		}
	}
	// 2) ForEach 扫描兜底: 收集匹配 key, 在 ForEach 结束后再删除
	codePrefix := []byte(code + "\x00")
	var toDelete [][]byte
	err := b.ForEach(func(k, v []byte) error {
		if !bytes.HasPrefix(k, codePrefix) {
			return nil
		}
		var rec PhraseRecord
		if uerr := json.Unmarshal(v, &rec); uerr != nil {
			// 无法解析: 若 key 后半段恰好等于目标 identity 也删除
			_, ident := parsePhraseKey(k)
			if ident == text || (name != "" && ident == "\x01"+name) {
				kc := make([]byte, len(k))
				copy(kc, k)
				toDelete = append(toDelete, kc)
			}
			return nil
		}
		if matchesPhraseIdentity(rec, text, name) {
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

// matchesPhraseIdentity 容忍历史 Text/Name/Texts 字段任一组合,
// 判断一条 PhraseRecord 是否对应外部传入的 (text, name) 身份。
//
// 匹配规则 (任一命中即视为匹配):
//   - Text 完全相等 (普通短语 / $AA marker 化后)
//   - Name 完全相等且非空 (旧 array 残留)
//   - 旧 array: 传入 name 与 rec.Name 一致 (rec.Texts 不参与匹配, 因为 UI 侧
//     可能只传 name)
//   - 传入 text 是 $AA marker, 且其内含 name 与 rec.Name 一致 (marker 化前残留)
//   - rec.Text 是 $AA marker, 且其 name 与传入 name 一致 (反向)
func matchesPhraseIdentity(rec PhraseRecord, text, name string) bool {
	if text != "" && rec.Text == text {
		return true
	}
	if name != "" && rec.Name == name {
		return true
	}
	// 传入是 $AA marker, rec 是旧 array 残留
	if extraName, ok := extractAANameForKey(text); ok && extraName != "" {
		if rec.Name == extraName {
			return true
		}
	}
	// rec 是 $AA marker, 传入是旧 name
	if name != "" {
		if recName, ok := extractAANameForKey(rec.Text); ok && recName == name {
			return true
		}
	}
	// 兜底: 传入 text 为空 + name 为空时不匹配任何记录, 避免误删
	return false
}

// phraseDeletionKeys 返回某条短语可能对应的所有历史 key 形式,
// 删除时依次尝试以兼容 marker 化前后产生的混合数据。
func phraseDeletionKeys(code, text, name string) [][]byte {
	keys := make([][]byte, 0, 2)
	keys = append(keys, []byte(code+"\x00"+text))
	if name != "" {
		keys = append(keys, []byte(code+"\x00\x01"+name))
	}
	// 若 text 是 $AA marker, 尝试解析出旧 name 作为额外候选 key
	if extraName, ok := extractAANameForKey(text); ok && extraName != name {
		keys = append(keys, []byte(code+"\x00\x01"+extraName))
	}
	return keys
}

// extractAANameForKey 粗略提取 $AA("name", ...) 中的 name 字段,
// 用于构造兼容的旧 "code\x00\x01name" 删除 key。
// 仅做最小解析: 找首个双引号到下一个双引号之间的内容, 不处理转义。
// 设计意图: 删除路径要尽量宽容, 解析失败时返回 false 即可。
func extractAANameForKey(text string) (string, bool) {
	const prefix = "$AA("
	t := text
	// 跳过首尾空白
	for len(t) > 0 && (t[0] == ' ' || t[0] == '\t') {
		t = t[1:]
	}
	if len(t) < len(prefix)+2 || t[:len(prefix)] != prefix {
		return "", false
	}
	body := t[len(prefix):]
	// 查找第一个 "
	i := 0
	for i < len(body) && body[i] != '"' {
		i++
	}
	if i >= len(body) {
		return "", false
	}
	start := i + 1
	j := start
	for j < len(body) {
		if body[j] == '\\' && j+1 < len(body) {
			j += 2
			continue
		}
		if body[j] == '"' {
			return body[start:j], true
		}
		j++
	}
	return "", false
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
