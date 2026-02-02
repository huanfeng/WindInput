package config

import (
	"fmt"
	"os"

	"gopkg.in/yaml.v3"
)

// Config represents the application configuration
type Config struct {
	Startup    StartupConfig    `yaml:"startup" json:"startup"`
	Dictionary DictionaryConfig `yaml:"dictionary" json:"dictionary"`
	Engine     EngineConfig     `yaml:"engine" json:"engine"`
	Hotkeys    HotkeyConfig     `yaml:"hotkeys" json:"hotkeys"`
	UI         UIConfig         `yaml:"ui" json:"ui"`
	Toolbar    ToolbarConfig    `yaml:"toolbar" json:"toolbar"`
	Input      InputConfig      `yaml:"input" json:"input"`
	Advanced   AdvancedConfig   `yaml:"advanced" json:"advanced"`
}

// StartupConfig 启动/默认状态配置
type StartupConfig struct {
	RememberLastState   bool `yaml:"remember_last_state" json:"remember_last_state"`
	DefaultChineseMode  bool `yaml:"default_chinese_mode" json:"default_chinese_mode"`
	DefaultFullWidth    bool `yaml:"default_full_width" json:"default_full_width"`
	DefaultChinesePunct bool `yaml:"default_chinese_punct" json:"default_chinese_punct"`
}

// DictionaryConfig contains dictionary settings
type DictionaryConfig struct {
	SystemDict string `yaml:"system_dict" json:"system_dict"`
	UserDict   string `yaml:"user_dict" json:"user_dict"`
	PinyinDict string `yaml:"pinyin_dict" json:"pinyin_dict"`
}

// EngineConfig 引擎配置
type EngineConfig struct {
	Type       string       `yaml:"type" json:"type"`
	FilterMode string       `yaml:"filter_mode" json:"filter_mode"`
	Pinyin     PinyinConfig `yaml:"pinyin" json:"pinyin"`
	Wubi       WubiConfig   `yaml:"wubi" json:"wubi"`
}

// PinyinConfig 拼音引擎配置
type PinyinConfig struct {
	ShowWubiHint bool `yaml:"show_wubi_hint" json:"show_wubi_hint"`
}

// WubiConfig 五笔引擎配置
type WubiConfig struct {
	AutoCommitAt4   bool `yaml:"auto_commit_at_4" json:"auto_commit_at_4"`
	ClearOnEmptyAt4 bool `yaml:"clear_on_empty_at_4" json:"clear_on_empty_at_4"`
	TopCodeCommit   bool `yaml:"top_code_commit" json:"top_code_commit"`
	PunctCommit     bool `yaml:"punct_commit" json:"punct_commit"`
}

// HotkeyConfig contains hotkey settings
type HotkeyConfig struct {
	ToggleModeKeys  []string `yaml:"toggle_mode_keys" json:"toggle_mode_keys"`
	CommitOnSwitch  bool     `yaml:"commit_on_switch" json:"commit_on_switch"`
	SwitchEngine    string   `yaml:"switch_engine" json:"switch_engine"`
	ToggleFullWidth string   `yaml:"toggle_full_width" json:"toggle_full_width"`
	TogglePunct     string   `yaml:"toggle_punct" json:"toggle_punct"`
}

// UIConfig contains UI settings
type UIConfig struct {
	FontSize            float64 `yaml:"font_size" json:"font_size"`
	CandidatesPerPage   int     `yaml:"candidates_per_page" json:"candidates_per_page"`
	FontPath            string  `yaml:"font_path" json:"font_path"`
	InlinePreedit       bool    `yaml:"inline_preedit" json:"inline_preedit"`
	HideCandidateWindow bool    `yaml:"hide_candidate_window" json:"hide_candidate_window"`
}

// ToolbarConfig contains toolbar settings
type ToolbarConfig struct {
	Visible   bool `yaml:"visible" json:"visible"`
	PositionX int  `yaml:"position_x" json:"position_x"`
	PositionY int  `yaml:"position_y" json:"position_y"`
}

// InputConfig contains input behavior settings
type InputConfig struct {
	PunctFollowMode bool     `yaml:"punct_follow_mode" json:"punct_follow_mode"`
	SelectKeyGroups []string `yaml:"select_key_groups" json:"select_key_groups"`
	PageKeys        []string `yaml:"page_keys" json:"page_keys"`
}

// AdvancedConfig 高级配置
type AdvancedConfig struct {
	LogLevel string `yaml:"log_level" json:"log_level"`
}

// DefaultConfig returns the default configuration
func DefaultConfig() *Config {
	return &Config{
		Startup: StartupConfig{
			RememberLastState:   false,
			DefaultChineseMode:  true,
			DefaultFullWidth:    false,
			DefaultChinesePunct: true,
		},
		Dictionary: DictionaryConfig{
			SystemDict: "dict/pinyin/pinyin.txt",
			UserDict:   UserDictFile,
			PinyinDict: "dict/pinyin/pinyin.txt",
		},
		Engine: EngineConfig{
			Type:       "pinyin",
			FilterMode: "smart",
			Pinyin: PinyinConfig{
				ShowWubiHint: true,
			},
			Wubi: WubiConfig{
				AutoCommitAt4:   false,
				ClearOnEmptyAt4: false,
				TopCodeCommit:   true,
				PunctCommit:     true,
			},
		},
		Hotkeys: HotkeyConfig{
			ToggleModeKeys:  []string{"lshift", "rshift"},
			CommitOnSwitch:  true,
			SwitchEngine:    "ctrl+`",
			ToggleFullWidth: "shift+space",
			TogglePunct:     "ctrl+.",
		},
		UI: UIConfig{
			FontSize:          18,
			CandidatesPerPage: 9,
			FontPath:          "",
			InlinePreedit:     true,
		},
		Toolbar: ToolbarConfig{
			Visible:   false,
			PositionX: 0,
			PositionY: 0,
		},
		Input: InputConfig{
			PunctFollowMode: false,
			SelectKeyGroups: []string{"semicolon_quote"},
			PageKeys:        []string{"pageupdown", "minus_equal"},
		},
		Advanced: AdvancedConfig{
			LogLevel: "info",
		},
	}
}

// Load loads the configuration from file
func Load() (*Config, error) {
	return LoadFrom("")
}

// LoadFrom loads the configuration from a specific path
// If path is empty, uses the default config path
func LoadFrom(path string) (*Config, error) {
	if path == "" {
		var err error
		path, err = GetConfigPath()
		if err != nil {
			return DefaultConfig(), err
		}
	}

	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return DefaultConfig(), nil
		}
		return DefaultConfig(), fmt.Errorf("failed to read config file: %w", err)
	}

	config := DefaultConfig()
	if err := yaml.Unmarshal(data, config); err != nil {
		return DefaultConfig(), fmt.Errorf("failed to parse config file: %w", err)
	}

	// 迁移旧配置格式
	config.migrateOldConfig(data)

	return config, nil
}

// migrateOldConfig 处理旧配置格式的兼容性
func (c *Config) migrateOldConfig(data []byte) {
	var oldConfig struct {
		General struct {
			StartInChineseMode bool   `yaml:"start_in_chinese_mode"`
			LogLevel           string `yaml:"log_level"`
		} `yaml:"general"`
		Hotkeys struct {
			ToggleMode string `yaml:"toggle_mode"`
		} `yaml:"hotkeys"`
		Input struct {
			SelectKey2 string `yaml:"select_key_2"`
			SelectKey3 string `yaml:"select_key_3"`
		} `yaml:"input"`
	}

	if err := yaml.Unmarshal(data, &oldConfig); err == nil {
		if oldConfig.General.StartInChineseMode {
			c.Startup.DefaultChineseMode = true
		}
		if oldConfig.General.LogLevel != "" {
			c.Advanced.LogLevel = oldConfig.General.LogLevel
		}
		if oldConfig.Hotkeys.ToggleMode != "" && len(c.Hotkeys.ToggleModeKeys) == 0 {
			switch oldConfig.Hotkeys.ToggleMode {
			case "shift":
				c.Hotkeys.ToggleModeKeys = []string{"lshift", "rshift"}
			}
		}
		if oldConfig.Input.SelectKey2 != "" || oldConfig.Input.SelectKey3 != "" {
			groups := []string{}
			if oldConfig.Input.SelectKey2 == "semicolon" && oldConfig.Input.SelectKey3 == "quote" {
				groups = append(groups, "semicolon_quote")
			}
			if oldConfig.Input.SelectKey2 == "comma" && oldConfig.Input.SelectKey3 == "period" {
				groups = append(groups, "comma_period")
			}
			if oldConfig.Input.SelectKey2 == "lshift" && oldConfig.Input.SelectKey3 == "rshift" {
				groups = append(groups, "lrshift")
			}
			if oldConfig.Input.SelectKey2 == "lctrl" && oldConfig.Input.SelectKey3 == "rctrl" {
				groups = append(groups, "lrctrl")
			}
			if len(groups) > 0 {
				c.Input.SelectKeyGroups = groups
			}
		}
	}
}

// Save saves the configuration to file
func Save(config *Config) error {
	return SaveTo(config, "")
}

// SaveTo saves the configuration to a specific path
// If path is empty, uses the default config path
func SaveTo(config *Config, path string) error {
	if err := EnsureConfigDir(); err != nil {
		return fmt.Errorf("failed to create config dir: %w", err)
	}

	if path == "" {
		var err error
		path, err = GetConfigPath()
		if err != nil {
			return err
		}
	}

	data, err := yaml.Marshal(config)
	if err != nil {
		return fmt.Errorf("failed to marshal config: %w", err)
	}

	if err := os.WriteFile(path, data, 0644); err != nil {
		return fmt.Errorf("failed to write config file: %w", err)
	}

	return nil
}

// SaveDefault saves the default configuration to file
func SaveDefault() error {
	return Save(DefaultConfig())
}

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
		cfg.Dictionary.SystemDict = "dict/pinyin/pinyin.txt"
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

	return conflicts
}
