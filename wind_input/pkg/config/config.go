package config

import (
	"fmt"
	"os"

	"gopkg.in/yaml.v3"
)

// Config represents the application configuration
type Config struct {
	Startup  StartupConfig  `yaml:"startup" json:"startup"`
	Schema   SchemaConfig   `yaml:"schema" json:"schema"`
	Hotkeys  HotkeyConfig   `yaml:"hotkeys" json:"hotkeys"`
	UI       UIConfig       `yaml:"ui" json:"ui"`
	Toolbar  ToolbarConfig  `yaml:"toolbar" json:"toolbar"`
	Input    InputConfig    `yaml:"input" json:"input"`
	Advanced AdvancedConfig `yaml:"advanced" json:"advanced"`
}

// SchemaConfig 输入方案配置
type SchemaConfig struct {
	Active    string   `yaml:"active" json:"active"`       // 当前活跃方案 ID
	Available []string `yaml:"available" json:"available"` // 可切换方案 ID 列表（顺序决定切换顺序）
}

// StartupConfig 启动/默认状态配置
type StartupConfig struct {
	RememberLastState   bool `yaml:"remember_last_state" json:"remember_last_state"`
	DefaultChineseMode  bool `yaml:"default_chinese_mode" json:"default_chinese_mode"`
	DefaultFullWidth    bool `yaml:"default_full_width" json:"default_full_width"`
	DefaultChinesePunct bool `yaml:"default_chinese_punct" json:"default_chinese_punct"`
}

// PinyinConfig 拼音引擎配置
type PinyinConfig struct {
	ShowCodeHint    bool              `yaml:"show_code_hint" json:"show_code_hint"`
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

// HotkeyConfig contains hotkey settings
type HotkeyConfig struct {
	ToggleModeKeys  []string `yaml:"toggle_mode_keys" json:"toggle_mode_keys"`
	CommitOnSwitch  bool     `yaml:"commit_on_switch" json:"commit_on_switch"`
	SwitchEngine    string   `yaml:"switch_engine" json:"switch_engine"`
	ToggleFullWidth string   `yaml:"toggle_full_width" json:"toggle_full_width"`
	TogglePunct     string   `yaml:"toggle_punct" json:"toggle_punct"`
	DeleteCandidate string   `yaml:"delete_candidate" json:"delete_candidate"` // 删除候选词: "ctrl+shift+number", "ctrl+number", "none"
	PinCandidate    string   `yaml:"pin_candidate" json:"pin_candidate"`       // 置顶候选词: "ctrl+number", "ctrl+shift+number", "none"
	ToggleToolbar   string   `yaml:"toggle_toolbar" json:"toggle_toolbar"`     // 显示/隐藏状态栏: 通用按键组合或 "none"
	OpenSettings    string   `yaml:"open_settings" json:"open_settings"`       // 打开设置: 通用按键组合或 "none"
	AddWord         string   `yaml:"add_word" json:"add_word"`                 // 快捷加词: 通用按键组合或 "none"
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
	Theme                   string  `yaml:"theme" json:"theme"`                                         // 主题名称：default, msime 或自定义主题名
	ThemeStyle              string  `yaml:"theme_style" json:"theme_style"`                             // 主题风格：system(跟随系统), light(亮色), dark(暗色)
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
	Visible bool `yaml:"visible" json:"visible"`
}

// InputConfig contains input behavior settings
type InputConfig struct {
	PunctFollowMode      bool                   `yaml:"punct_follow_mode" json:"punct_follow_mode"`
	FilterMode           string                 `yaml:"filter_mode" json:"filter_mode"` // 候选过滤模式: "smart"(智能), "general"(仅常用字), "gb18030"(不限制)
	SelectKeyGroups      []string               `yaml:"select_key_groups" json:"select_key_groups"`
	PageKeys             []string               `yaml:"page_keys" json:"page_keys"`
	HighlightKeys        []string               `yaml:"highlight_keys" json:"highlight_keys"`                   // 移动高亮候选项: "arrows"(上/下方向键), "tab"(Tab/Shift+Tab)
	SmartPunctAfterDigit bool                   `yaml:"smart_punct_after_digit" json:"smart_punct_after_digit"` // 数字后标点智能转换：句号→点号、逗号→英文逗号（默认 true）
	PinyinSeparator      string                 `yaml:"pinyin_separator" json:"pinyin_separator"`               // 拼音分隔符: "auto", "quote", "backtick", "none"
	ShiftTempEnglish     ShiftTempEnglishConfig `yaml:"shift_temp_english" json:"shift_temp_english"`
	CapsLockBehavior     CapsLockBehaviorConfig `yaml:"capslock_behavior" json:"capslock_behavior"`
	TempPinyin           TempPinyinConfig       `yaml:"temp_pinyin" json:"temp_pinyin"`
	AutoPair             AutoPairConfig         `yaml:"auto_pair" json:"auto_pair"`
}

// TempPinyinConfig 临时拼音模式配置
type TempPinyinConfig struct {
	TriggerKeys []string `yaml:"trigger_keys" json:"trigger_keys"` // 触发键: "backtick", "semicolon"
}

// AutoPairConfig 自动标点配对配置
type AutoPairConfig struct {
	Chinese      bool     `yaml:"chinese" json:"chinese"`             // 中文标点自动配对
	English      bool     `yaml:"english" json:"english"`             // 英文标点自动配对
	Blacklist    []string `yaml:"blacklist" json:"blacklist"`         // 应用黑名单
	ChinesePairs []string `yaml:"chinese_pairs" json:"chinese_pairs"` // 中文配对表，如 ["（）", "【】"]
	EnglishPairs []string `yaml:"english_pairs" json:"english_pairs"` // 英文配对表，如 ["()", "[]"]
}

// ParsePairs 将字符串配对列表解析为左右 rune 对
// 每个字符串应恰好包含2个 rune，如 "（）"
func ParsePairs(pairs []string) [][2]rune {
	var result [][2]rune
	for _, s := range pairs {
		runes := []rune(s)
		if len(runes) == 2 {
			result = append(result, [2]rune{runes[0], runes[1]})
		}
	}
	return result
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
	// HostRenderProcesses 启用宿主进程代理渲染的进程白名单（进程名，不区分大小写）
	// 在这些进程中，候选窗口将通过 DLL 内 CreateWindowInBand 创建，以解决 z-order 问题
	HostRenderProcesses []string `yaml:"host_render_processes,omitempty" json:"host_render_processes,omitempty"`
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
		Schema: SchemaConfig{
			Active:    "wubi86",
			Available: []string{"wubi86", "pinyin"},
		},
		Hotkeys: HotkeyConfig{
			ToggleModeKeys:  []string{"lshift", "rshift"},
			CommitOnSwitch:  true,
			SwitchEngine:    "ctrl+`",
			ToggleFullWidth: "shift+space",
			TogglePunct:     "ctrl+.",
			DeleteCandidate: "ctrl+shift+number",
			PinCandidate:    "ctrl+number",
			ToggleToolbar:   "none",
			OpenSettings:    "none",
			AddWord:         "ctrl+equal",
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
			ThemeStyle:              "system",
			TextRenderMode:          "directwrite",
			GDIFontWeight:           500,
			GDIFontScale:            1.0,
			MenuFontWeight:          500,
			MenuFontSize:            12.0,
		},
		Toolbar: ToolbarConfig{
			Visible: true,
		},
		Input: InputConfig{
			SmartPunctAfterDigit: true,
			PunctFollowMode:      false,
			FilterMode:           "smart",
			SelectKeyGroups:      []string{"semicolon_quote"},
			PageKeys:             []string{"pageupdown", "minus_equal"},
			HighlightKeys:        []string{"arrows"},
			PinyinSeparator:      "auto",
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
			AutoPair: AutoPairConfig{
				Chinese:      false,
				English:      false,
				Blacklist:    []string{},
				ChinesePairs: []string{"（）", "【】", "｛｝", "《》", "〈〉"},
				EnglishPairs: []string{"()", "[]", "{}", "<>"},
			},
		},
		Advanced: AdvancedConfig{
			LogLevel:            "info",
			HostRenderProcesses: []string{"SearchHost.exe"},
		},
	}
}

// Load loads the configuration using three-layer merge:
// 1. Code defaults (DefaultConfig)
// 2. System bundled config (data/config.yaml) overlay
// 3. User config (%APPDATA%/WindInput/config.yaml) overlay
func Load() (*Config, error) {
	return LoadFrom("")
}

// LoadFrom loads the configuration from a specific user config path.
// If path is empty, uses the default user config path.
// System config (data/config.yaml) is always loaded as the middle layer.
func LoadFrom(path string) (*Config, error) {
	if path == "" {
		var err error
		path, err = GetConfigPath()
		if err != nil {
			return DefaultConfig(), err
		}
	}

	// Layer 1: 代码默认值
	cfg := DefaultConfig()

	// Layer 2: 加载系统预置配置（data/config.yaml）覆盖代码默认值
	if sysPath, err := GetSystemConfigPath(); err == nil {
		if sysData, err := os.ReadFile(sysPath); err == nil {
			// 系统配置解析失败只打印警告，不中断
			if err := yaml.Unmarshal(sysData, cfg); err != nil {
				fmt.Fprintf(os.Stderr, "[config] warning: failed to parse system config %s: %v\n", sysPath, err)
			}
		}
		// 系统配置文件不存在是正常情况，不需要报错
	}

	// Layer 3: 加载用户配置覆盖
	userData, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			// 用户配置不存在，使用前两层的结果
			return cfg, nil
		}
		return cfg, fmt.Errorf("failed to read config file: %w", err)
	}

	if err := yaml.Unmarshal(userData, cfg); err != nil {
		return DefaultConfig(), fmt.Errorf("failed to parse config file: %w", err)
	}

	// 兜底校验
	applyConfigFallbacks(cfg)

	return cfg, nil
}

// applyConfigFallbacks 对关键字段进行兜底处理
func applyConfigFallbacks(cfg *Config) {
	// 如果 available 为空，使用默认值
	if len(cfg.Schema.Available) == 0 {
		cfg.Schema.Available = []string{"wubi86", "pinyin"}
	}
	// Schema 兜底：如果 active 为空，取 available 的第一个
	if cfg.Schema.Active == "" && len(cfg.Schema.Available) > 0 {
		cfg.Schema.Active = cfg.Schema.Available[0]
	}

	// 迁移旧的 theme:"dark" 配置到新格式
	if cfg.UI.Theme == "dark" {
		cfg.UI.Theme = "default"
		cfg.UI.ThemeStyle = "dark"
	}

	// ThemeStyle 兜底
	if cfg.UI.ThemeStyle == "" {
		cfg.UI.ThemeStyle = "system"
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

// SystemDefaultConfig returns the system default configuration
// by merging code defaults (Layer 1) with bundled data/config.yaml (Layer 2).
// This is the "factory default" that excludes user customizations.
func SystemDefaultConfig() *Config {
	cfg := DefaultConfig()

	if sysPath, err := GetSystemConfigPath(); err == nil {
		if sysData, err := os.ReadFile(sysPath); err == nil {
			if err := yaml.Unmarshal(sysData, cfg); err != nil {
				fmt.Fprintf(os.Stderr, "[config] warning: failed to parse system config %s: %v\n", sysPath, err)
			}
		}
	}

	applyConfigFallbacks(cfg)
	return cfg
}

// SaveDefault saves the default configuration to file
func SaveDefault() error {
	return Save(DefaultConfig())
}
