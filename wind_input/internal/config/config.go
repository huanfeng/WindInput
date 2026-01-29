// Package config handles application configuration
package config

import (
	"fmt"
	"os"
	"path/filepath"

	"gopkg.in/yaml.v3"
)

const (
	AppName        = "WindInput"
	ConfigFileName = "config.yaml"
	UserDictFile   = "user_dict.txt"
	StateFileName  = "state.yaml" // 用于记忆状态
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
	RememberLastState   bool `yaml:"remember_last_state" json:"remember_last_state"`       // 记忆前次状态（优先级最高）
	DefaultChineseMode  bool `yaml:"default_chinese_mode" json:"default_chinese_mode"`     // 默认中文模式
	DefaultFullWidth    bool `yaml:"default_full_width" json:"default_full_width"`         // 默认全角
	DefaultChinesePunct bool `yaml:"default_chinese_punct" json:"default_chinese_punct"`   // 默认中文标点
}

// DictionaryConfig contains dictionary settings
type DictionaryConfig struct {
	SystemDict string `yaml:"system_dict" json:"system_dict"`
	UserDict   string `yaml:"user_dict" json:"user_dict"`
	PinyinDict string `yaml:"pinyin_dict" json:"pinyin_dict"` // 拼音词库（用于反查）
}

// EngineConfig 引擎配置
type EngineConfig struct {
	Type       string       `yaml:"type" json:"type"`               // pinyin / wubi / shuangpin / mixed
	FilterMode string       `yaml:"filter_mode" json:"filter_mode"` // general / gb18030 / smart
	Pinyin     PinyinConfig `yaml:"pinyin" json:"pinyin"`
	Wubi       WubiConfig   `yaml:"wubi" json:"wubi"`
}

// PinyinConfig 拼音引擎配置
type PinyinConfig struct {
	ShowWubiHint bool `yaml:"show_wubi_hint" json:"show_wubi_hint"` // 显示五笔编码提示（反查）
}

// WubiConfig 五笔引擎配置
type WubiConfig struct {
	AutoCommitAt4    bool `yaml:"auto_commit_at_4" json:"auto_commit_at_4"`       // 四码唯一时自动上屏
	ClearOnEmptyAt4  bool `yaml:"clear_on_empty_at_4" json:"clear_on_empty_at_4"` // 四码为空时清空
	TopCodeCommit    bool `yaml:"top_code_commit" json:"top_code_commit"`         // 五码顶字上屏
	PunctCommit      bool `yaml:"punct_commit" json:"punct_commit"`               // 标点顶字上屏
}

// HotkeyConfig contains hotkey settings
type HotkeyConfig struct {
	// 中英文切换键（多选）: lshift, rshift, lctrl, rctrl, capslock
	ToggleModeKeys   []string `yaml:"toggle_mode_keys" json:"toggle_mode_keys"`
	CommitOnSwitch   bool     `yaml:"commit_on_switch" json:"commit_on_switch"`           // 中文切换为英文时已有编码上屏
	SwitchEngine     string   `yaml:"switch_engine" json:"switch_engine"`                 // ctrl+`, ctrl+shift+e, none
	ToggleFullWidth  string   `yaml:"toggle_full_width" json:"toggle_full_width"`         // shift+space, ctrl+shift+space, none
	TogglePunct      string   `yaml:"toggle_punct" json:"toggle_punct"`                   // ctrl+., ctrl+,, none
}

// UIConfig contains UI settings
type UIConfig struct {
	FontSize          float64 `yaml:"font_size" json:"font_size"`
	CandidatesPerPage int     `yaml:"candidates_per_page" json:"candidates_per_page"`
	FontPath          string  `yaml:"font_path" json:"font_path"`
	InlinePreedit     bool    `yaml:"inline_preedit" json:"inline_preedit"` // 启用嵌入式编码行
}

// ToolbarConfig contains toolbar settings
type ToolbarConfig struct {
	Visible   bool `yaml:"visible" json:"visible"`
	PositionX int  `yaml:"position_x" json:"position_x"`
	PositionY int  `yaml:"position_y" json:"position_y"`
}

// InputConfig contains input behavior settings
type InputConfig struct {
	FullWidth          bool     `yaml:"full_width" json:"full_width"`                     // 全角模式（运行时状态）
	ChinesePunctuation bool     `yaml:"chinese_punctuation" json:"chinese_punctuation"`   // 中文标点（运行时状态）
	PunctFollowMode    bool     `yaml:"punct_follow_mode" json:"punct_follow_mode"`       // 标点随中英文切换
	// 候选选择键组（多选）: semicolon_quote, comma_period, lrshift, lrctrl
	SelectKeyGroups    []string `yaml:"select_key_groups" json:"select_key_groups"`
	// 翻页键（多选）: pageupdown, minus_equal, brackets, shift_tab
	PageKeys           []string `yaml:"page_keys" json:"page_keys"`
}

// AdvancedConfig 高级配置
type AdvancedConfig struct {
	LogLevel string `yaml:"log_level" json:"log_level"`
}

// RuntimeState 运行时状态（用于记忆前次状态）
type RuntimeState struct {
	ChineseMode  bool   `yaml:"chinese_mode" json:"chinese_mode"`
	FullWidth    bool   `yaml:"full_width" json:"full_width"`
	ChinesePunct bool   `yaml:"chinese_punct" json:"chinese_punct"`
	EngineType   string `yaml:"engine_type" json:"engine_type"`
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
			FullWidth:          false,
			ChinesePunctuation: true,
			PunctFollowMode:    false,
			SelectKeyGroups:    []string{"semicolon_quote"},
			PageKeys:           []string{"pageupdown", "minus_equal"},
		},
		Advanced: AdvancedConfig{
			LogLevel: "info",
		},
	}
}

// DefaultRuntimeState 返回默认运行时状态
func DefaultRuntimeState() *RuntimeState {
	return &RuntimeState{
		ChineseMode:  true,
		FullWidth:    false,
		ChinesePunct: true,
		EngineType:   "pinyin",
	}
}

// GetConfigDir returns the configuration directory path
// On Windows: %APPDATA%\WindInput
func GetConfigDir() (string, error) {
	configDir, err := os.UserConfigDir()
	if err != nil {
		return "", fmt.Errorf("failed to get user config dir: %w", err)
	}
	return filepath.Join(configDir, AppName), nil
}

// GetConfigPath returns the full path to the config file
func GetConfigPath() (string, error) {
	configDir, err := GetConfigDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(configDir, ConfigFileName), nil
}

// GetStatePath returns the full path to the state file
func GetStatePath() (string, error) {
	configDir, err := GetConfigDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(configDir, StateFileName), nil
}

// GetUserDictPath returns the full path to the user dictionary
func GetUserDictPath() (string, error) {
	configDir, err := GetConfigDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(configDir, UserDictFile), nil
}

// EnsureConfigDir ensures the config directory exists
func EnsureConfigDir() error {
	configDir, err := GetConfigDir()
	if err != nil {
		return err
	}
	return os.MkdirAll(configDir, 0755)
}

// Load loads the configuration from file
// If the file doesn't exist, returns default configuration
func Load() (*Config, error) {
	configPath, err := GetConfigPath()
	if err != nil {
		return DefaultConfig(), err
	}

	data, err := os.ReadFile(configPath)
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
	// 尝试解析旧格式
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
		// 迁移 general.start_in_chinese_mode -> startup.default_chinese_mode
		if oldConfig.General.StartInChineseMode {
			c.Startup.DefaultChineseMode = true
		}
		// 迁移 general.log_level -> advanced.log_level
		if oldConfig.General.LogLevel != "" {
			c.Advanced.LogLevel = oldConfig.General.LogLevel
		}
		// 迁移 hotkeys.toggle_mode -> hotkeys.toggle_mode_keys
		if oldConfig.Hotkeys.ToggleMode != "" && len(c.Hotkeys.ToggleModeKeys) == 0 {
			switch oldConfig.Hotkeys.ToggleMode {
			case "shift":
				c.Hotkeys.ToggleModeKeys = []string{"lshift", "rshift"}
			case "ctrl+space":
				// 保持为空或设置特殊标记
			}
		}
		// 迁移旧的 select_key_2/3 格式
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
	if err := EnsureConfigDir(); err != nil {
		return fmt.Errorf("failed to create config dir: %w", err)
	}

	configPath, err := GetConfigPath()
	if err != nil {
		return err
	}

	data, err := yaml.Marshal(config)
	if err != nil {
		return fmt.Errorf("failed to marshal config: %w", err)
	}

	if err := os.WriteFile(configPath, data, 0644); err != nil {
		return fmt.Errorf("failed to write config file: %w", err)
	}

	return nil
}

// LoadRuntimeState 加载运行时状态
func LoadRuntimeState() (*RuntimeState, error) {
	statePath, err := GetStatePath()
	if err != nil {
		return DefaultRuntimeState(), err
	}

	data, err := os.ReadFile(statePath)
	if err != nil {
		if os.IsNotExist(err) {
			return DefaultRuntimeState(), nil
		}
		return DefaultRuntimeState(), fmt.Errorf("failed to read state file: %w", err)
	}

	state := DefaultRuntimeState()
	if err := yaml.Unmarshal(data, state); err != nil {
		return DefaultRuntimeState(), fmt.Errorf("failed to parse state file: %w", err)
	}

	return state, nil
}

// SaveRuntimeState 保存运行时状态
func SaveRuntimeState(state *RuntimeState) error {
	if err := EnsureConfigDir(); err != nil {
		return fmt.Errorf("failed to create config dir: %w", err)
	}

	statePath, err := GetStatePath()
	if err != nil {
		return err
	}

	data, err := yaml.Marshal(state)
	if err != nil {
		return fmt.Errorf("failed to marshal state: %w", err)
	}

	if err := os.WriteFile(statePath, data, 0644); err != nil {
		return fmt.Errorf("failed to write state file: %w", err)
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

	// Update system dict based on engine type
	switch engineType {
	case "wubi":
		cfg.Dictionary.SystemDict = "dict/wubi/wubi86.txt"
	case "pinyin":
		cfg.Dictionary.SystemDict = "dict/pinyin/pinyin.txt"
	}

	return Save(cfg)
}

// GetWubiDictPath returns the path to the wubi dictionary
func GetWubiDictPath() string {
	return "dict/wubi/wubi86.txt"
}

// GetPinyinDictPath returns the path to the pinyin dictionary
func GetPinyinDictPath() string {
	return "dict/pinyin/pinyin.txt"
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

	// 检查中英切换键
	for _, key := range c.Hotkeys.ToggleModeKeys {
		if existing, ok := usedKeys[key]; ok {
			conflicts = append(conflicts, fmt.Sprintf("按键 %s 同时用于: %s 和 中英切换", key, existing))
		} else {
			usedKeys[key] = "中英切换"
		}
	}

	// 检查候选选择键组
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

	// 检查翻页键
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
