// config_hotkey.go — 快捷键匹配与验证
package config

import (
	"fmt"

	"github.com/huanfeng/wind_input/pkg/keys"
)

// UpdateSchemaActive 更新活跃方案 ID 到配置文件
func UpdateSchemaActive(schemaID string) error {
	cfg, err := Load()
	if err != nil {
		cfg = DefaultConfig()
	}

	cfg.Schema.Active = schemaID

	return Save(cfg)
}

// IsToggleModeKey 检查按键是否为中英切换键
func (c *Config) IsToggleModeKey(key string) bool {
	for _, k := range c.Hotkeys.ToggleModeKeys {
		if k == key {
			return true
		}
	}
	return false
}

// matchPairFirst 检查 key 是否为某个 group（若属于已配置的选择/翻页/以词定字等列表）的"前键"。
func matchPairFirst(groups []keys.PairGroup, allowed map[keys.PairGroup]struct{}, key string) bool {
	k := keys.Key(key)
	for _, g := range groups {
		if _, ok := allowed[g]; !ok {
			continue
		}
		if first, _, ok := g.Keys(); ok && first == k {
			return true
		}
	}
	return false
}

// matchPairSecond 检查 key 是否为某个 group 的"后键"。
func matchPairSecond(groups []keys.PairGroup, allowed map[keys.PairGroup]struct{}, key string) bool {
	k := keys.Key(key)
	for _, g := range groups {
		if _, ok := allowed[g]; !ok {
			continue
		}
		if _, second, ok := g.Keys(); ok && second == k {
			return true
		}
	}
	return false
}

// 各 API 接受的 PairGroup 集合（保持原行为：候选选择不接受 brackets/minus_equal/...
// 翻页不接受 lrshift/lrctrl/...，与原 switch 列表精确一致）。
var (
	selectKeyAllowedGroups = map[keys.PairGroup]struct{}{
		keys.PairSemicolonQuote: {},
		keys.PairCommaPeriod:    {},
		keys.PairLRShift:        {},
		keys.PairLRCtrl:         {},
	}
	pageKeyAllowedGroups = map[keys.PairGroup]struct{}{
		keys.PairPageUpDown:  {},
		keys.PairMinusEqual:  {},
		keys.PairBrackets:    {},
		keys.PairShiftTab:    {},
		keys.PairCommaPeriod: {},
	}
	selectCharAllowedGroups = map[keys.PairGroup]struct{}{
		keys.PairCommaPeriod: {},
		keys.PairMinusEqual:  {},
		keys.PairBrackets:    {},
	}
)

// IsSelectKey2 检查按键是否为第2候选键
func (c *Config) IsSelectKey2(key string) bool {
	return matchPairFirst(c.Input.SelectKeyGroups, selectKeyAllowedGroups, key)
}

// IsSelectKey3 检查按键是否为第3候选键
func (c *Config) IsSelectKey3(key string) bool {
	return matchPairSecond(c.Input.SelectKeyGroups, selectKeyAllowedGroups, key)
}

// IsPageUpKey 检查按键是否为向上翻页键
func (c *Config) IsPageUpKey(key string) bool {
	return matchPairFirst(c.Input.PageKeys, pageKeyAllowedGroups, key)
}

// IsPageDownKey 检查按键是否为向下翻页键
func (c *Config) IsPageDownKey(key string) bool {
	return matchPairSecond(c.Input.PageKeys, pageKeyAllowedGroups, key)
}

// IsSelectCharFirstKey 检查按键是否为以词定字第1字按键
func (c *Config) IsSelectCharFirstKey(key string) bool {
	return matchPairFirst(c.Input.SelectCharKeys, selectCharAllowedGroups, key)
}

// IsSelectCharSecondKey 检查按键是否为以词定字第2字按键
func (c *Config) IsSelectCharSecondKey(key string) bool {
	return matchPairSecond(c.Input.SelectCharKeys, selectCharAllowedGroups, key)
}

// pairGroupRawKeys 返回 PairGroup 的两键字符串名（用于冲突检测里的字符串集合）。
// 仅当 group 在 allowed 集合内才返回 keys，否则返回 nil。
func pairGroupRawKeys(g keys.PairGroup, allowed map[keys.PairGroup]struct{}) []string {
	if _, ok := allowed[g]; !ok {
		return nil
	}
	first, second, ok := g.Keys()
	if !ok {
		return nil
	}
	return []string{string(first), string(second)}
}

// ValidateHotkeyConflicts 检查快捷键冲突
func (c *Config) ValidateHotkeyConflicts() []string {
	conflicts := []string{}
	usedKeys := make(map[string]string)

	for _, key := range c.Hotkeys.ToggleModeKeys {
		if existing, ok := usedKeys[key]; ok {
			conflicts = append(conflicts, fmt.Sprintf("按键 %s 同时用于: %s 和 中英切换", key, existing))
		} else {
			usedKeys[key] = "中英切换"
		}
	}

	for _, group := range c.Input.SelectKeyGroups {
		for _, key := range pairGroupRawKeys(group, selectKeyAllowedGroups) {
			if existing, ok := usedKeys[key]; ok {
				conflicts = append(conflicts, fmt.Sprintf("按键 %s 同时用于: %s 和 候选选择", key, existing))
			} else {
				usedKeys[key] = "候选选择"
			}
		}
	}

	for _, pk := range c.Input.PageKeys {
		for _, key := range pairGroupRawKeys(pk, pageKeyAllowedGroups) {
			if existing, ok := usedKeys[key]; ok {
				conflicts = append(conflicts, fmt.Sprintf("按键 %s 同时用于: %s 和 翻页", key, existing))
			} else {
				usedKeys[key] = "翻页"
			}
		}
	}

	// HighlightKeys: 仅 PairTab 进入冲突表（PairArrows 不冲突 —— 沿用原逻辑）
	for _, hk := range c.Input.HighlightKeys {
		if hk != keys.PairTab {
			continue
		}
		first, second, ok := keys.PairTab.Keys()
		if !ok {
			continue
		}
		for _, key := range []string{string(first), string(second)} {
			if existing, ok := usedKeys[key]; ok {
				conflicts = append(conflicts, fmt.Sprintf("按键 %s 同时用于: %s 和 移动高亮", key, existing))
			} else {
				usedKeys[key] = "移动高亮"
			}
		}
	}

	for _, sc := range c.Input.SelectCharKeys {
		for _, key := range pairGroupRawKeys(sc, selectCharAllowedGroups) {
			if existing, ok := usedKeys[key]; ok {
				conflicts = append(conflicts, fmt.Sprintf("按键 %s 同时用于: %s 和 以词定字", key, existing))
			} else {
				usedKeys[key] = "以词定字"
			}
		}
	}

	return conflicts
}
