package store

import (
	"encoding/json"
	"fmt"
	"strconv"
	"strings"

	bolt "go.etcd.io/bbolt"
)

// MigratePhraseRecordsToAA 将 bbolt Phrases bucket 内旧格式的字符组短语
// (Texts + Name 双字段) 一次性重写为 $AA("name", "chars") marker 形式,
// 写回到 Text 字段并清空 Texts/Name。
//
// 幂等性: 已经是 $AA( 开头的 Text 字段跳过, 因此多次启动安全。
// 调用时机: store.Open 后、dict manager LoadFromStore 前。
//
// 本轮 PhraseRecord 保留 Texts/Name 字段 (标 deprecated), 下一版才彻底删除。
// 这一保留是必要的, 因为 migration 自身需要读这两个字段。
func (s *Store) MigratePhraseRecordsToAA() (migrated int, err error) {
	err = s.db.Update(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketPhrases)
		if b == nil {
			return nil
		}
		type pendingUpdate struct {
			oldKey []byte
			newKey []byte
			value  []byte
		}
		var pending []pendingUpdate

		c := b.Cursor()
		for k, v := c.First(); k != nil; k, v = c.Next() {
			var rec PhraseRecord
			if uerr := json.Unmarshal(v, &rec); uerr != nil {
				continue
			}
			// 已是 $AA marker, 幂等跳过
			if strings.HasPrefix(strings.TrimSpace(rec.Text), "$AA(") {
				continue
			}
			// 没有 Texts/Name 的不是字符组, 跳过
			if rec.Texts == "" {
				continue
			}
			// 旧字符组: 重写 Text 为 $AA(name, chars)
			rec.Text = fmt.Sprintf("$AA(%s, %s)",
				strconv.Quote(rec.Name), strconv.Quote(rec.Texts))
			rec.Texts = ""
			rec.Name = ""
			// 类型也归一化为 array (保持下游 LoadFromStore 行为不变)
			if rec.Type == "" {
				rec.Type = "array"
			}
			// 从 key 提取 code (key 形式: code\x00\x01name)
			code, _ := parsePhraseKey(k)
			rec.Code = code

			newData, mErr := json.Marshal(rec)
			if mErr != nil {
				return fmt.Errorf("migrate phrase: marshal: %w", mErr)
			}
			// 新 key: array 类型用 code\x00text (因为 Name 已清空, phraseKey
			// 走非 array 分支会用 Text=$AA(...))
			newKey := []byte(code + "\x00" + rec.Text)

			oldKeyCopy := make([]byte, len(k))
			copy(oldKeyCopy, k)
			pending = append(pending, pendingUpdate{
				oldKey: oldKeyCopy,
				newKey: newKey,
				value:  newData,
			})
		}

		for _, p := range pending {
			if dErr := b.Delete(p.oldKey); dErr != nil {
				return fmt.Errorf("migrate phrase: delete old key: %w", dErr)
			}
			if pErr := b.Put(p.newKey, p.value); pErr != nil {
				return fmt.Errorf("migrate phrase: put new key: %w", pErr)
			}
		}
		migrated = len(pending)
		return nil
	})
	return migrated, err
}
