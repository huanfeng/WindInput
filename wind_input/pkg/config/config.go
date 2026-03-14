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
	SystemDict     string `yaml:"system_dict" json:"system_dict"`
	UserDict       string `yaml:"user_dict,omitempty" json:"user_dict,omitempty"` // Deprecated: kept for backward compatibility
	PinyinUserDict string `yaml:"pinyin_user_dict" json:"pinyin_user_dict"`
	WubiUserDict   string `yaml:"wubi_user_dict" json:"wubi_user_dict"`
	PinyinDict     string `yaml:"pinyin_dict" json:"pinyin_dict"`
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
	ShowWubiHint    bool              `yaml:"show_wubi_hint" json:"show_wubi_hint"`
	UseSmartCompose bool              `yaml:"use_smart_compose" json:"use_smart_compose"`
	CandidateOrder  string            `yaml:"candidate_order" json:"candidate_order"` // 候选排序：char_first/phrase_first/smart
	Fuzzy           FuzzyPinyinConfig `yaml:"fuzzy" json:"fuzzy"`
}

// FuzzyPinyinConfig 模糊拼音配置
type FuzzyPinyinConfig struct {
	Enabled bool `yaml:"enabled" json:"enabled"`   // 总开关
	ZhZ     bool `yaml:"zh_z" json:"zh_z"`         // zh ↔ z
	ChC     bool `yaml:"ch_c" json:"ch_c"`         // ch ↔ c
	ShS     bool `yaml:"sh_s" json:"sh_s"`         // sh ↔ s
	NL      bool `yaml:"n_l" json:"n_l"`           // n ↔ l
	FH      bool `yaml:"f_h" json:"f_h"`           // f ↔ h
	RL      bool `yaml:"r_l" json:"r_l"`           // r ↔ l
	AnAng   bool `yaml:"an_ang" json:"an_ang"`     // an ↔ ang
	EnEng   bool `yaml:"en_eng" json:"en_eng"`     // en ↔ eng
	InIng   bool `yaml:"in_ing" json:"in_ing"`     // in ↔ ing
	IanIang bool `yaml:"ian_iang" json:"ian_iang"` // ian ↔ iang
	UanUang bool `yaml:"uan_uang" json:"uan_uang"` // uan ↔ uang
}

// WubiConfig 五笔引擎配置
type WubiConfig struct {
	AutoCommitAt4     bool   `yaml:"auto_commit_at_4" json:"auto_commit_at_4"`
	ClearOnEmptyAt4   bool   `yaml:"clear_on_empty_at_4" json:"clear_on_empty_at_4"`
	TopCodeCommit     bool   `yaml:"top_code_commit" json:"top_code_commit"`
	PunctCommit       bool   `yaml:"punct_commit" json:"punct_commit"`
	ShowCodeHint      bool   `yaml:"show_code_hint" json:"show_code_hint"`
	SingleCodeInput   bool   `yaml:"single_code_input" json:"single_code_input"`
	CandidateSortMode string `yaml:"candidate_sort_mode" json:"candidate_sort_mode"` // 候选排序模式：frequency（词频）、natural（自然顺序）
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
	FontSize                float64 `yaml:"font_size" json:"font_size"`
	CandidatesPerPage       int     `yaml:"candidates_per_page" json:"candidates_per_page"`
	FontPath                string  `yaml:"font_path" json:"font_path"`
	InlinePreedit           bool    `yaml:"inline_preedit" json:"inline_preedit"`
	HideCandidateWindow     bool    `yaml:"hide_candidate_window" json:"hide_candidate_window"`
	CandidateLayout         string  `yaml:"candidate_layout" json:"candidate_layout"`                   // 候选布局：horizontal 或 vertical
	StatusIndicatorDuration int     `yaml:"status_indicator_duration" json:"status_indicator_duration"` // 状态提示显示时长（毫秒）
	StatusIndicatorOffsetX  int     `yaml:"status_indicator_offset_x" json:"status_indicator_offset_x"` // 状态提示 X 偏移量
	StatusIndicatorOffsetY  int     `yaml:"status_indicator_offset_y" json:"status_indicator_offset_y"` // 状态提示 Y 偏移量
	Theme                   string  `yaml:"theme" json:"theme"`                                         // 主题名称：default, dark 或自定义主题名
	TooltipDelay            int     `yaml:"tooltip_delay" json:"tooltip_delay"`                         // 编码提示延迟显示时间（毫秒），0 表示立即显示

	// 文本渲染设置
	TextRenderMode string  `yaml:"text_render_mode,omitempty" json:"text_render_mode,omitempty"` // 文本渲染引擎："directwrite"（默认，DirectWrite渲染）、"gdi"（Windows原生GDI渲染）或 "freetype"（FreeType渲染）
	GDIFontWeight  int     `yaml:"gdi_font_weight,omitempty" json:"gdi_font_weight,omitempty"`   // 候选框GDI字体粗细：100~900，默认500(Medium)
	GDIFontScale   float64 `yaml:"gdi_font_scale,omitempty" json:"gdi_font_scale,omitempty"`     // GDI字体缩放：0.5~2.0，默认1.0，值越大文字越大
	MenuFontWeight int     `yaml:"menu_font_weight,omitempty" json:"menu_font_weight,omitempty"` // 菜单GDI字体粗细：100~900，默认600(SemiBold)
	MenuFontSize   float64 `yaml:"menu_font_size,omitempty" json:"menu_font_size,omitempty"`     // 菜单字体大小：默认12.0（DPI缩放前基础值）
}

// ToolbarConfig contains toolbar settings
type ToolbarConfig struct {
	Visible   bool `yaml:"visible" json:"visible"`
	PositionX int  `yaml:"position_x" json:"position_x"`
	PositionY int  `yaml:"position_y" json:"position_y"`
}

// InputConfig contains input behavior settings
type InputConfig struct {
	PunctFollowMode  bool                   `yaml:"punct_follow_mode" json:"punct_follow_mode"`
	SelectKeyGroups  []string               `yaml:"select_key_groups" json:"select_key_groups"`
	PageKeys         []string               `yaml:"page_keys" json:"page_keys"`
	PinyinSeparator  string                 `yaml:"pinyin_separator" json:"pinyin_separator"` // 拼音分隔符: "auto", "quote", "backtick", "none"
	ShiftTempEnglish ShiftTempEnglishConfig `yaml:"shift_temp_english" json:"shift_temp_english"`
	CapsLockBehavior CapsLockBehaviorConfig `yaml:"capslock_behavior" json:"capslock_behavior"`
	TempPinyin       TempPinyinConfig       `yaml:"temp_pinyin" json:"temp_pinyin"`
}

// TempPinyinConfig 临时拼音模式配置
type TempPinyinConfig struct {
	TriggerKeys []string `yaml:"trigger_keys" json:"trigger_keys"` // 触发键: "backtick", "semicolon"
}

// ShiftTempEnglishConfig 临时英文模式配置
type ShiftTempEnglishConfig struct {
	Enabled               bool `yaml:"enabled" json:"enabled"`
	ShowEnglishCandidates bool `yaml:"show_english_candidates" json:"show_english_candidates"`
}

// CapsLockBehaviorConfig CapsLock 行为配置
type CapsLockBehaviorConfig struct {
	CancelOnModeSwitch bool `yaml:"cancel_on_mode_switch" json:"cancel_on_mode_switch"`
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
			SystemDict:     "dict/wubi/wubi86.txt",
			PinyinUserDict: PinyinUserDictFile,
			WubiUserDict:   WubiUserDictFile,
			PinyinDict:     "dict/pinyin",
		},
		Engine: EngineConfig{
			Type:       "wubi",
			FilterMode: "smart",
			Pinyin: PinyinConfig{
				ShowWubiHint:    true,
				UseSmartCompose: true,
			},
			Wubi: WubiConfig{
				AutoCommitAt4:     false,
				ClearOnEmptyAt4:   false,
				TopCodeCommit:     false,
				PunctCommit:       true,
				ShowCodeHint:      true,
				CandidateSortMode: "frequency",
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
			FontSize:                18,
			CandidatesPerPage:       7,
			FontPath:                "",
			InlinePreedit:           true,
			CandidateLayout:         "horizontal",
			StatusIndicatorDuration: 800,
			StatusIndicatorOffsetX:  0,
			StatusIndicatorOffsetY:  0,
			TooltipDelay:            200,
			Theme:                   "default",
			TextRenderMode:          "directwrite",
			GDIFontWeight:           500,
			GDIFontScale:            1.0,
			MenuFontWeight:          600,
			MenuFontSize:            12.0,
		},
		Toolbar: ToolbarConfig{
			Visible:   true,
			PositionX: 0,
			PositionY: 0,
		},
		Input: InputConfig{
			PunctFollowMode: false,
			SelectKeyGroups: []string{"semicolon_quote"},
			PageKeys:        []string{"pageupdown", "minus_equal"},
			PinyinSeparator: "auto",
			ShiftTempEnglish: ShiftTempEnglishConfig{
				Enabled:               true,
				ShowEnglishCandidates: true,
			},
			CapsLockBehavior: CapsLockBehaviorConfig{
				CancelOnModeSwitch: false,
			},
			TempPinyin: TempPinyinConfig{
				TriggerKeys: []string{"backtick"},
			},
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
