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
// 写入新版 PhraseRecord.Text 字段并删除多余字段。
//
// 幂等性: 已经是 $AA( 开头的 Text 字段跳过, 多次启动安全。
// 调用时机: store.Open 后、dict manager LoadFromStore 前 (manager.go::OpenStore)。
//
// 实现: 用 legacyPhraseRecord 读旧字段 (新 PhraseRecord 已删 Texts/Name/Type),
// 重组完成后写新 PhraseRecord (只含 Code/Text/Weight/Position/Enabled/IsSystem)。
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
			var legacy legacyPhraseRecord
			if uerr := json.Unmarshal(v, &legacy); uerr != nil {
				continue
			}
			// 已是 $AA marker, 幂等跳过
			if strings.HasPrefix(strings.TrimSpace(legacy.Text), "$AA(") {
				continue
			}
			// 没有 Texts/Name 的不是旧字符组, 跳过 (普通 / dynamic / $SS / $CC 等)
			if legacy.Texts == "" {
				continue
			}
			// 重组为 $AA marker
			markerText := fmt.Sprintf("$AA(%s, %s)",
				strconv.Quote(legacy.Name), strconv.Quote(legacy.Texts))
			code, _ := parsePhraseKey(k)
			rec := PhraseRecord{
				Code:     code,
				Text:     markerText,
				Weight:   legacy.Weight,
				Position: legacy.Position,
				Enabled:  legacy.Enabled,
				IsSystem: legacy.IsSystem,
			}

			newData, mErr := json.Marshal(rec)
			if mErr != nil {
				return fmt.Errorf("migrate phrase: marshal: %w", mErr)
			}
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
