// config_hotkey.go — 快捷键匹配与验证
package config

import "fmt"

// UpdateEngineType updates the engine type in config and saves
func UpdateEngineType(engineType string) error {
	cfg, err := Load()
	if err != nil {
		cfg = DefaultConfig()
	}

	cfg.Engine.Type = engineType

	switch engineType {
	case "wubi":
		cfg.Dictionary.SystemDict = "dict/wubi/wubi86.txt"
	case "pinyin":
		cfg.Dictionary.SystemDict = "dict/pinyin"
	}

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

// IsSelectKey2 检查按键是否为第2候选键
func (c *Config) IsSelectKey2(key string) bool {
	for _, group := range c.Input.SelectKeyGroups {
		switch group {
		case "semicolon_quote":
			if key == "semicolon" {
				return true
			}
		case "comma_period":
			if key == "comma" {
				return true
			}
		case "lrshift":
			if key == "lshift" {
				return true
			}
		case "lrctrl":
			if key == "lctrl" {
				return true
			}
		}
	}
	return false
}

// IsSelectKey3 检查按键是否为第3候选键
func (c *Config) IsSelectKey3(key string) bool {
	for _, group := range c.Input.SelectKeyGroups {
		switch group {
		case "semicolon_quote":
			if key == "quote" {
				return true
			}
		case "comma_period":
			if key == "period" {
				return true
			}
		case "lrshift":
			if key == "rshift" {
				return true
			}
		case "lrctrl":
			if key == "rctrl" {
				return true
			}
		}
	}
	return false
}

// IsPageUpKey 检查按键是否为向上翻页键
func (c *Config) IsPageUpKey(key string) bool {
	for _, pk := range c.Input.PageKeys {
		switch pk {
		case "pageupdown":
			if key == "pageup" {
				return true
			}
		case "minus_equal":
			if key == "minus" {
				return true
			}
		case "brackets":
			if key == "lbracket" {
				return true
			}
		case "shift_tab":
			if key == "shift_tab" {
				return true
			}
		}
	}
	return false
}

// IsPageDownKey 检查按键是否为向下翻页键
func (c *Config) IsPageDownKey(key string) bool {
	for _, pk := range c.Input.PageKeys {
		switch pk {
		case "pageupdown":
			if key == "pagedown" {
				return true
			}
		case "minus_equal":
			if key == "equal" {
				return true
			}
		case "brackets":
			if key == "rbracket" {
				return true
			}
		case "shift_tab":
			if key == "tab" {
				return true
			}
		}
	}
	return false
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
		var keys []string
		switch group {
		case "semicolon_quote":
			keys = []string{"semicolon", "quote"}
		case "comma_period":
			keys = []string{"comma", "period"}
		case "lrshift":
			keys = []string{"lshift", "rshift"}
		case "lrctrl":
			keys = []string{"lctrl", "rctrl"}
		}
		for _, key := range keys {
			if existing, ok := usedKeys[key]; ok {
				conflicts = append(conflicts, fmt.Sprintf("按键 %s 同时用于: %s 和 候选选择", key, existing))
			} else {
				usedKeys[key] = "候选选择"
			}
		}
	}

	for _, pk := range c.Input.PageKeys {
		var keys []string
		switch pk {
		case "pageupdown":
			keys = []string{"pageup", "pagedown"}
		case "minus_equal":
			keys = []string{"minus", "equal"}
		case "brackets":
			keys = []string{"lbracket", "rbracket"}
		case "shift_tab":
			keys = []string{"shift_tab", "tab"}
		}
		for _, key := range keys {
			if existing, ok := usedKeys[key]; ok {
				conflicts = append(conflicts, fmt.Sprintf("按键 %s 同时用于: %s 和 翻页", key, existing))
			} else {
				usedKeys[key] = "翻页"
			}
		}
	}

	for _, hk := range c.Input.HighlightKeys {
		var keys []string
		switch hk {
		case "tab":
			keys = []string{"shift_tab", "tab"}
		}
		for _, key := range keys {
			if existing, ok := usedKeys[key]; ok {
				conflicts = append(conflicts, fmt.Sprintf("按键 %s 同时用于: %s 和 移动高亮", key, existing))
			} else {
				usedKeys[key] = "移动高亮"
			}
		}
	}

	return conflicts
}
