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

// RefreshSystemPhraseWeights 把 db 中 IsSystem=true 且 Weight=0 的内置短语
// 刷新到 yamlMap 提供的 weight 值。
//
// yamlMap 的 key 格式: "code\x00text" (与 phraseKey 一致), value 是目标 weight。
//
// 触发场景: 旧版 db 升级后 system 短语 weight 字段空缺 (UI 上显示 9999 类),
// 启动期一次性按最新 system.phrases.yaml 的 weight 字段刷新到合理值。
// 只刷新 Weight==0 的记录, 避免覆盖用户主动改过的权重。
//
// 调用时机: SeedDefaultPhrases (创建路径) 与 MigratePhraseRecordsToAA (迁移路径)
// 之后, LoadFromStore 之前。
func (s *Store) RefreshSystemPhraseWeights(yamlMap map[string]int) (refreshed int, err error) {
	if len(yamlMap) == 0 {
		return 0, nil
	}
	err = s.db.Update(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketPhrases)
		if b == nil {
			return nil
		}
		type pending struct {
			key   []byte
			value []byte
		}
		var updates []pending

		c := b.Cursor()
		for k, v := c.First(); k != nil; k, v = c.Next() {
			var rec PhraseRecord
			if uerr := json.Unmarshal(v, &rec); uerr != nil {
				continue
			}
			if !rec.IsSystem || rec.Weight != 0 {
				continue
			}
			newWeight, ok := yamlMap[string(k)]
			if !ok || newWeight == 0 {
				continue
			}
			rec.Weight = newWeight
			newData, mErr := json.Marshal(rec)
			if mErr != nil {
				return fmt.Errorf("refresh system weight: marshal: %w", mErr)
			}
			keyCopy := make([]byte, len(k))
			copy(keyCopy, k)
			updates = append(updates, pending{key: keyCopy, value: newData})
		}

		for _, u := range updates {
			if pErr := b.Put(u.key, u.value); pErr != nil {
				return fmt.Errorf("refresh system weight: put: %w", pErr)
			}
		}
		refreshed = len(updates)
		return nil
	})
	return refreshed, err
}
