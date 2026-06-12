package config

import (
	"errors"
	"fmt"
	"os"

	"gopkg.in/yaml.v3"

	"github.com/huanfeng/wind_input/pkg/keys"
)

// Config 应用配置（v1 结构，见 docs/design/config-restructure.md §3）。
//
// 顶层分类原则：用户心智域 = 设置页（§2.1）；input vs features 边界 =
// 按键流水线耦合 vs 可整体拔掉的自包含功能。
//
// 三态规范（§2.3）：struct 禁指针、bool 永不为 null、标量 tag 禁 omitempty
// （slice/map 豁免）；"未设置=继承默认"由磁盘键缺失 + diff-save 表达。
// 注意：Config 不含 version 字段——version 只存活于磁盘文件与 map 层。
type Config struct {
	General  GeneralConfig  `yaml:"general" json:"general"`
	Schema   SchemaConfig   `yaml:"schema" json:"schema"`
	Hotkeys  HotkeyConfig   `yaml:"hotkeys" json:"hotkeys"`
	Input    InputConfig    `yaml:"input" json:"input"`
	UI       UIConfig       `yaml:"ui" json:"ui"`
	Features FeaturesConfig `yaml:"features" json:"features"`
	Compat   CompatConfig   `yaml:"compat" json:"compat"`
	Debug    DebugConfig    `yaml:"debug" json:"debug"`
}

// GeneralConfig 启动与默认状态（原 StartupConfig）
type GeneralConfig struct {
	RememberLastState   bool `yaml:"remember_last_state" json:"remember_last_state"`
	DefaultChineseMode  bool `yaml:"default_chinese_mode" json:"default_chinese_mode"`
	DefaultFullWidth    bool `yaml:"default_full_width" json:"default_full_width"`
	DefaultChinesePunct bool `yaml:"default_chinese_punct" json:"default_chinese_punct"`
}

// SchemaConfig 输入方案配置
type SchemaConfig struct {
	Active    string   `yaml:"active" json:"active"`       // 当前活跃方案 ID
	Available []string `yaml:"available" json:"available"` // 可切换方案 ID 列表（顺序决定切换顺序）
	// PrimaryCodetable 主码表方案 ID。
	// 用于：拼音类方案的"反查/编码提示"统一从此方案的码表派生。
	// 留空时按 Available 顺序选第一个 codetable 方案；都没有则不显示编码提示。
	PrimaryCodetable string `yaml:"primary_codetable" json:"primary_codetable"`
	// PrimaryPinyin 主拼音方案 ID。
	// 用于：码表方案的"临时拼音/快捷输入"统一指向此方案。
	// 留空时按 Available 顺序选第一个 pinyin 方案；都没有则禁用临时拼音。
	PrimaryPinyin string `yaml:"primary_pinyin" json:"primary_pinyin"`
}

// HotkeyConfig contains hotkey settings
type HotkeyConfig struct {
	ToggleModeKeys   []string          `yaml:"toggle_mode_keys" json:"toggle_mode_keys"`
	CommitOnSwitch   bool              `yaml:"commit_on_switch" json:"commit_on_switch"`
	SwitchEngine     string            `yaml:"switch_engine" json:"switch_engine"`
	ToggleFullWidth  string            `yaml:"toggle_full_width" json:"toggle_full_width"`
	TogglePunct      string            `yaml:"toggle_punct" json:"toggle_punct"`
	DeleteCandidate  string            `yaml:"delete_candidate" json:"delete_candidate"`     // 删除候选词: "ctrl+shift+number", "ctrl+number", "none"
	PinCandidate     string            `yaml:"pin_candidate" json:"pin_candidate"`           // 置顶候选词: "ctrl+number", "ctrl+shift+number", "none"
	ToggleToolbar    string            `yaml:"toggle_toolbar" json:"toggle_toolbar"`         // 显示/隐藏状态栏: 通用按键组合或 "none"
	OpenSettings     string            `yaml:"open_settings" json:"open_settings"`           // 打开设置: 通用按键组合或 "none"
	AddWord          string            `yaml:"add_word" json:"add_word"`                     // 快捷加词: 通用按键组合或 "none"
	ToggleS2T        string            `yaml:"toggle_s2t" json:"toggle_s2t"`                 // 简入繁出开关切换: 通用按键组合或 "none"
	TakeScreenshot   string            `yaml:"take_screenshot" json:"take_screenshot"`       // UI 截图: 通用按键组合或 "none"
	GlobalHotkeys    []string          `yaml:"global_hotkeys" json:"global_hotkeys"`         // 注册为全局热键的快捷键名称列表
	ActivateIME      string            `yaml:"activate_ime" json:"activate_ime"`             // Windows 专用：切换到本输入法（全局热键），"none" 或空=禁用
	EnterTempPinyin  string            `yaml:"enter_temp_pinyin" json:"enter_temp_pinyin"`   // 本地热键：直接进入临时拼音模式
	EnterSpecialMode map[string]string `yaml:"enter_special_mode" json:"enter_special_mode"` // 本地热键：mode_id → 热键字符串
}

// InputConfig 按键流水线行为配置
type InputConfig struct {
	PunctFollowMode      bool                   `yaml:"punct_follow_mode" json:"punct_follow_mode"`
	FilterMode           FilterMode             `yaml:"filter_mode" json:"filter_mode"` // 候选过滤模式: "smart"(智能), "general"(仅常用字), "gb18030"(不限制)
	SelectKeyGroups      []keys.PairGroup       `yaml:"select_key_groups" json:"select_key_groups"`
	PageKeys             []keys.PairGroup       `yaml:"page_keys" json:"page_keys"`
	HighlightKeys        []keys.PairGroup       `yaml:"highlight_keys" json:"highlight_keys"`                   // 移动高亮候选项: PairArrows / PairTab
	SelectCharKeys       []keys.PairGroup       `yaml:"select_char_keys" json:"select_char_keys"`               // 以词定字按键: PairCommaPeriod / PairMinusEqual / PairBrackets
	SmartPunctAfterDigit bool                   `yaml:"smart_punct_after_digit" json:"smart_punct_after_digit"` // 数字后标点智能转换（默认 true）
	SmartPunctList       string                 `yaml:"smart_punct_list" json:"smart_punct_list"`               // 数字后保持英文的标点字符，如 ".,:"
	SmartSymbolMode      bool                   `yaml:"smart_symbol_mode" json:"smart_symbol_mode"`             // 智能符号模式：同一中文标点在时限内连按两次，删前一字符替换为英文（默认 false）
	SmartSymbolTimeoutMs int                    `yaml:"smart_symbol_timeout_ms" json:"smart_symbol_timeout_ms"` // 智能符号模式判定时限（毫秒，默认 500），内部配置
	SmartSymbolChars     string                 `yaml:"smart_symbol_chars" json:"smart_symbol_chars"`           // 参与智能符号转换的中文标点集合（默认 "。，？！：；"），内部配置，为成对符号等扩展预留
	EnterBehavior        EnterBehavior          `yaml:"enter_behavior" json:"enter_behavior"`                   // 回车键行为: "commit"(上屏编码), "clear"(清空编码)
	SpaceOnEmptyBehavior SpaceOnEmptyBehavior   `yaml:"space_on_empty_behavior" json:"space_on_empty_behavior"` // 空码时空格键行为: "commit"(上屏编码), "clear"(清空编码)
	NumpadBehavior       string                 `yaml:"numpad_behavior" json:"numpad_behavior"`                 // 数字小键盘功能: "direct"(直接输入数字,默认) | "follow_main"(同主键盘区数字)
	PinyinSeparator      PinyinSeparatorMode    `yaml:"pinyin_separator" json:"pinyin_separator"`               // 拼音分隔符: "auto", "quote", "backtick", "none"
	ShiftTempEnglish     ShiftTempEnglishConfig `yaml:"shift_temp_english" json:"shift_temp_english"`
	CapsLock             CapsLockConfig         `yaml:"capslock" json:"capslock"`
	TempPinyin           TempPinyinConfig       `yaml:"temp_pinyin" json:"temp_pinyin"`
	AutoPair             AutoPairConfig         `yaml:"auto_pair" json:"auto_pair"`
	PunctCustom          PunctCustomConfig      `yaml:"punct_custom" json:"punct_custom"`
	Overflow             OverflowConfig         `yaml:"overflow" json:"overflow"` // 候选按键无效时的处理策略
	Phrase               PhraseConfig           `yaml:"phrase" json:"phrase"`     // 短语相关行为
}

// PhraseConfig 短语相关行为配置（暂无 UI，文件配置）
type PhraseConfig struct {
	// MinPrefixLength 短语前缀匹配触发的最小输入长度（默认 2，下界 clamp 到 1）。
	// 当输入码长 < MinPrefixLength 且 < 短语自身码长时，该短语不参与前缀展开；
	// 等价规则: 短语条目仅在 len(input) >= MinPrefixLength || len(input) >= len(code) 时出现。
	MinPrefixLength int `yaml:"min_prefix_length" json:"min_prefix_length"`
}

// OverflowConfig 候选按键无效时的处理策略（原 OverflowBehaviorConfig）
type OverflowConfig struct {
	// 数字键无效时: "ignore"(不起作用) | "commit"(顶字上屏) | "commit_and_input"(顶字上屏并输入数字)
	NumberKey OverflowBehavior `yaml:"number_key" json:"number_key"`
	// 二三候选键无效时: "ignore"(不起作用) | "commit"(顶字上屏) | "commit_and_input"(顶字上屏并输入编码)
	SelectKey OverflowBehavior `yaml:"select_key" json:"select_key"`
	// 以词定字键无效时: "ignore"(不起作用) | "commit"(顶字上屏) | "commit_and_input"(顶字上屏并输入编码)
	SelectCharKey OverflowBehavior `yaml:"select_char_key" json:"select_char_key"`
}

// 特殊模式自动上屏策略
const (
	SpecialAutoCommitPrefixFree  = "prefix_free"  // 唯一候选且无更长前缀
	SpecialAutoCommitFixedLength = "fixed_length" // 达固定码长且唯一候选
	SpecialAutoCommitManual      = "manual"       // 永远手动选
)

// SpecialModeConfig 引导键特殊模式（自定义码表）单实例配置
type SpecialModeConfig struct {
	ID            string   `yaml:"id" json:"id"`
	Name          string   `yaml:"name" json:"name"`                 // 模式徽标显示名
	TriggerKeys   []string `yaml:"trigger_keys" json:"trigger_keys"` // 引导键
	Table         string   `yaml:"table" json:"table"`               // 码表文件，相对 schemas 目录
	AutoCommit    string   `yaml:"auto_commit" json:"auto_commit"`   // prefix_free|fixed_length|manual
	FixedLength   int      `yaml:"fixed_length" json:"fixed_length"`
	ForceVertical bool     `yaml:"force_vertical" json:"force_vertical"`
	AccentColor   string   `yaml:"accent_color" json:"accent_color"`
	// ShowAllOnEntry 刚进入模式（编码为空）时是否立即列出整张码表的全部候选。
	// false(默认)=只显示模式徽标提示，打字后才按前缀出候选；true=进入即列全部（大表慎用）。
	ShowAllOnEntry bool `yaml:"show_all_on_entry" json:"show_all_on_entry"`
	// —— 预留字段，MVP 不实现 ——
	CodeCharset string   `yaml:"code_charset" json:"code_charset"`
	Schemes     []string `yaml:"schemes,omitempty" json:"schemes,omitempty"`
	Engines     []string `yaml:"engines,omitempty" json:"engines,omitempty"`
}

// Validate 校验单实例配置（不校验文件是否存在，那由 registry 在加载时做）。
func (s SpecialModeConfig) Validate() error {
	if s.ID == "" {
		return fmt.Errorf("special mode: id 不能为空")
	}
	if len(s.TriggerKeys) == 0 {
		return fmt.Errorf("special mode %q: trigger_keys 不能为空", s.ID)
	}
	if s.Table == "" {
		return fmt.Errorf("special mode %q: table 不能为空", s.ID)
	}
	switch s.AutoCommit {
	case SpecialAutoCommitPrefixFree, SpecialAutoCommitManual:
	case SpecialAutoCommitFixedLength:
		if s.FixedLength <= 0 {
			return fmt.Errorf("special mode %q: auto_commit=fixed_length 时 fixed_length 必须 > 0", s.ID)
		}
	default:
		return fmt.Errorf("special mode %q: 未知 auto_commit=%q", s.ID, s.AutoCommit)
	}
	return nil
}

// QuickInputConfig 快捷输入配置
type QuickInputConfig struct {
	TriggerKeys   []string `yaml:"trigger_keys" json:"trigger_keys"`     // 触发键列表（空列表=关闭），如 ["semicolon"]
	ForceVertical bool     `yaml:"force_vertical" json:"force_vertical"` // 强制竖排显示候选（默认 true）
	DecimalPlaces int      `yaml:"decimal_places" json:"decimal_places"` // 计算结果小数保留位数（默认 6，0 表示取整）
	AccentColor   string   `yaml:"accent_color" json:"accent_color"`     // 模式内发光边框颜色（十六进制），空=内置默认色
}

// PunctCustomConfig 自定义标点映射配置
type PunctCustomConfig struct {
	Enabled  bool                `yaml:"enabled" json:"enabled"`                       // 总开关
	Mappings map[string][]string `yaml:"mappings,omitempty" json:"mappings,omitempty"` // key=源字符(引号用"1/"2/'1/'2), value=[中文半角,英文全角,中文全角,英文半角], 空串=默认
}

// TempPinyinConfig 临时拼音模式配置
type TempPinyinConfig struct {
	TriggerKeys []string `yaml:"trigger_keys" json:"trigger_keys"` // 触发键: "backtick", "semicolon", "z"
	// ZIncludeOnCommit 控制 z 触发临时拼音后，按 Enter 上屏编码时是否包含触发键 z 本身。
	// 默认 true（保留 z），仅对 z 触发键生效，不影响符号触发键。暂不暴露 UI，作为内部预留开关。
	ZIncludeOnCommit bool `yaml:"z_include_on_commit" json:"z_include_on_commit"`
	// AccentColor 模式内发光边框颜色（十六进制，如 "#3C78AFD2"），空=内置默认色
	AccentColor string `yaml:"accent_color" json:"accent_color"`
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
	// Shift+字母行为: "temp_english"(进入临时英文模式,默认), "direct_commit"(直接上屏大写字母)
	ShiftBehavior string `yaml:"shift_behavior" json:"shift_behavior"`
	// 触发键（符号键进入临时英文模式，类似临时拼音触发键）
	TriggerKeys []string `yaml:"trigger_keys" json:"trigger_keys"`
	// 允许输入符号与数字：开启后，符号/数字键不强制上屏，而是追加到 buffer，
	// buffer 中含非字母字符时进入"无候选"状态（不查词库、不显示候选列表，但保留 preedit）；
	// 数字键 1-9 仅在对应索引超出当前页可见候选数时进 buffer，否则继续作选候选用。
	AllowSymbols bool `yaml:"allow_symbols" json:"allow_symbols"`
	// 空格作为输入字符：开启后空格不再上屏，而是追加到 buffer，仅回车上屏。
	SpaceAsInput bool `yaml:"space_as_input" json:"space_as_input"`
}

// CapsLockConfig CapsLock 行为配置（原 CapsLockBehaviorConfig）
type CapsLockConfig struct {
	CancelOnModeSwitch bool `yaml:"cancel_on_mode_switch" json:"cancel_on_mode_switch"`
}

// ── UI ──────────────────────────────────────────────────────────────────────

// UIConfig 界面配置（纯容器，顶层不留散字段）
type UIConfig struct {
	Candidate       UICandidateConfig     `yaml:"candidate" json:"candidate"`
	Font            UIFontConfig          `yaml:"font" json:"font"`
	Theme           UIThemeConfig         `yaml:"theme" json:"theme"`
	StatusIndicator StatusIndicatorConfig `yaml:"status_indicator" json:"status_indicator"`
	Tooltip         TooltipConfig         `yaml:"tooltip" json:"tooltip"`
	Toolbar         ToolbarConfig         `yaml:"toolbar" json:"toolbar"`
}

// UICandidateConfig 候选窗布局与行为
type UICandidateConfig struct {
	FontSize float64 `yaml:"font_size" json:"font_size"`
	// FontSizeFollowTheme=true 时候选字号跟随主题 behavior.font_size，忽略 FontSize；
	// false=用 FontSize 自定义。默认 true（新装跟随主题），缺键=继承默认。
	FontSizeFollowTheme bool `yaml:"font_size_follow_theme" json:"font_size_follow_theme"`

	PerPage int `yaml:"per_page" json:"per_page"` // 每页候选数（基础档）
	// PerPageExtended 扩展档每页候选数。在临时拼音/快捷输入/短语等场景下生效，
	// 让这些场景显示更多候选；普通码表输入仍用 PerPage（基础档）。
	// 0=禁用扩展档（合法语义值）；正值有效，上界 clamp 到 10。
	PerPageExtended int `yaml:"per_page_extended" json:"per_page_extended"`
	// MaxChars 候选文本最大显示 rune 数，超出后截断并追加"…"。默认 16，合法范围 8-64（越界 clamp 回 16）。
	MaxChars int `yaml:"max_chars" json:"max_chars"`

	Layout        CandidateLayout `yaml:"layout" json:"layout"`                 // 候选布局：horizontal 或 vertical
	InlinePreedit bool            `yaml:"inline_preedit" json:"inline_preedit"` // 内嵌预编辑
	PreeditMode   PreeditMode     `yaml:"preedit_mode" json:"preedit_mode"`     // 编码显示模式："top"(独立行)/"embedded"(嵌入候选行前)；仅 InlinePreedit=false 时生效
	// FlipWhenAbove 候选窗在光标上方时反转 bands 排列顺序，使预编辑栏保持最靠近光标。
	FlipWhenAbove bool `yaml:"flip_when_above" json:"flip_when_above"`
	HideWindow    bool `yaml:"hide_window" json:"hide_window"` // 隐藏候选窗

	// IndexLabels 用户全局序号标签覆盖（10 槽位字符或 /-分隔模板，如 "①②③④⑤⑥⑦⑧⑨⑩"）。
	// 非空时覆盖主题 views.index.labels；空=用主题。优先级低于运行时 per-候选 IndexLabel。
	IndexLabels string `yaml:"index_labels" json:"index_labels"`
	// ModeAccentBorder 特殊模式内发光边框总开关（默认 false——v0 时代 struct 注释
	// 声称 nil=开启，但消费方 modeAccentColor 实现为 nil=关闭，以实际行为为准）
	ModeAccentBorder bool `yaml:"mode_accent_border" json:"mode_accent_border"`

	// —— 主题 behavior 用户覆盖层（哲学Y）：值字段 + bool follow 配对，
	// follow=true 跟随主题 behavior 推荐默认，false 用值字段自定义 ——
	AlwaysShowPager             bool `yaml:"always_show_pager" json:"always_show_pager"`
	AlwaysShowPagerFollowTheme  bool `yaml:"always_show_pager_follow_theme" json:"always_show_pager_follow_theme"`
	ShowPageNumber              bool `yaml:"show_page_number" json:"show_page_number"`
	ShowPageNumberFollowTheme   bool `yaml:"show_page_number_follow_theme" json:"show_page_number_follow_theme"`
	VerticalMaxWidth            int  `yaml:"vertical_max_width" json:"vertical_max_width"`
	VerticalMaxWidthFollowTheme bool `yaml:"vertical_max_width_follow_theme" json:"vertical_max_width_follow_theme"`

	// 强制覆盖枚举（空=不覆盖主题/行为层，在 follow 层之后应用）
	PagerBarDisplay   PagerBarDisplay   `yaml:"pager_bar_display" json:"pager_bar_display"`     // 空=主题配置, always, auto, hide
	PageNumberDisplay PageNumberDisplay `yaml:"page_number_display" json:"page_number_display"` // 空=主题配置, show, hide
}

// UIFontConfig 字体与文本渲染
type UIFontConfig struct {
	Family     string     `yaml:"family" json:"family"`
	Path       string     `yaml:"path" json:"path"`
	RenderMode FontEngine `yaml:"render_mode" json:"render_mode"` // "directwrite"(默认)/"gdi"/"freetype"
	GDIWeight  int        `yaml:"gdi_weight" json:"gdi_weight"`   // 候选框 GDI 字体粗细：100~900，默认 500
	GDIScale   float64    `yaml:"gdi_scale" json:"gdi_scale"`     // GDI 字体缩放：0.5~2.0，默认 1.0
	MenuWeight int        `yaml:"menu_weight" json:"menu_weight"` // 菜单 GDI 字体粗细：100~900，默认 500
	MenuSize   float64    `yaml:"menu_size" json:"menu_size"`     // 菜单字体大小：默认 12.0（DPI 缩放前基础值）
}

// UIThemeConfig 主题
type UIThemeConfig struct {
	Name            string     `yaml:"name" json:"name"`                           // 主题名称：default, msime 或自定义主题名
	Style           ThemeStyle `yaml:"style" json:"style"`                         // 主题风格：system(跟随系统), light, dark
	EditorAutoStart bool       `yaml:"editor_auto_start" json:"editor_auto_start"` // 打开设置界面时自动开启 Web 编辑器连接服务
}

// StatusIndicatorConfig 状态提示配置
type StatusIndicatorConfig struct {
	Enabled         bool    `yaml:"enabled" json:"enabled"`
	Duration        int     `yaml:"duration" json:"duration"`
	DisplayMode     string  `yaml:"display_mode" json:"display_mode"`
	SchemaNameStyle string  `yaml:"schema_name_style" json:"schema_name_style"`
	ShowMode        bool    `yaml:"show_mode" json:"show_mode"`
	ShowPunct       bool    `yaml:"show_punct" json:"show_punct"`
	ShowFullWidth   bool    `yaml:"show_full_width" json:"show_full_width"`
	PositionMode    string  `yaml:"position_mode" json:"position_mode"`
	OffsetX         int     `yaml:"offset_x" json:"offset_x"`
	OffsetY         int     `yaml:"offset_y" json:"offset_y"`
	CustomX         int     `yaml:"custom_x" json:"custom_x"`
	CustomY         int     `yaml:"custom_y" json:"custom_y"`
	FontSize        float64 `yaml:"font_size" json:"font_size"`
	Opacity         float64 `yaml:"opacity" json:"opacity"`
	BackgroundColor string  `yaml:"background_color" json:"background_color"`
	TextColor       string  `yaml:"text_color" json:"text_color"`
	BorderRadius    float64 `yaml:"border_radius" json:"border_radius"`
}

// TooltipConfig 候选悬停增强提示配置
type TooltipConfig struct {
	Delay  int                 `yaml:"delay" json:"delay"`   // 提示延迟显示时间（毫秒），0=立即显示
	Code   TooltipCodeConfig   `yaml:"code" json:"code"`     // 编码显示
	Pinyin TooltipPinyinConfig `yaml:"pinyin" json:"pinyin"` // 拼音提示
	Chaizi TooltipChaiziConfig `yaml:"chaizi" json:"chaizi"` // 拆字提示
	Debug  TooltipDebugConfig  `yaml:"debug" json:"debug"`   // 调试信息
}

// TooltipCodeConfig 编码显示配置
type TooltipCodeConfig struct {
	Enabled bool `yaml:"enabled" json:"enabled"` // 是否显示编码（默认 true）
}

// TooltipPinyinConfig 拼音提示配置
type TooltipPinyinConfig struct {
	Enabled     bool `yaml:"enabled" json:"enabled"`           // 是否显示拼音
	Heteronyms  bool `yaml:"heteronyms" json:"heteronyms"`     // 是否显示多音字所有读音
	MaxReadings int  `yaml:"max_readings" json:"max_readings"` // 每字最多显示读音数，0 表示不限
}

// TooltipChaiziConfig 拆字提示配置
type TooltipChaiziConfig struct {
	Enabled bool `yaml:"enabled" json:"enabled"` // 是否显示拆字（默认 false）
}

// TooltipDebugConfig 调试信息配置
type TooltipDebugConfig struct {
	Enabled bool `yaml:"enabled" json:"enabled"` // 是否显示调试信息（默认 false）
}

// ToolbarConfig 工具栏配置
type ToolbarConfig struct {
	Visible bool `yaml:"visible" json:"visible"`
	// HideInFullscreen 前台应用全屏时自动隐藏工具栏（默认 true）
	HideInFullscreen bool `yaml:"hide_in_fullscreen" json:"hide_in_fullscreen"`
}

// ── Features ────────────────────────────────────────────────────────────────

// FeaturesConfig 自包含可选功能（可整体拔掉而不影响基础打字）
type FeaturesConfig struct {
	Stats        StatsConfig         `yaml:"stats" json:"stats"`
	S2T          S2TConfig           `yaml:"s2t" json:"s2t"`
	QuickInput   QuickInputConfig    `yaml:"quick_input" json:"quick_input"`
	SpecialModes []SpecialModeConfig `yaml:"special_modes,omitempty" json:"special_modes,omitempty"`
	Cmdbar       CmdbarConfig        `yaml:"cmdbar" json:"cmdbar"`
}

// StatsConfig 输入统计配置
type StatsConfig struct {
	Enabled      bool `yaml:"enabled" json:"enabled"`             // 是否启用统计（默认 true）
	RetainDays   int  `yaml:"retain_days" json:"retain_days"`     // 数据保留天数（0=永久，默认 0）
	TrackEnglish bool `yaml:"track_english" json:"track_english"` // 是否统计英文模式（默认 true）
}

// S2TConfig 简入繁出（简体->繁体）配置
type S2TConfig struct {
	Enabled bool       `yaml:"enabled" json:"enabled"` // 总开关
	Variant S2TVariant `yaml:"variant" json:"variant"` // 变体: s2t / s2tw / s2twp / s2hk
}

// CmdbarConfig 命令直通车配置
type CmdbarConfig struct {
	// CandidatePrefix 副作用命令候选 (Actions 含 ActionEffect) 在候选框渲染时的
	// 前缀符号，让用户一眼分辨"会跑副作用的命令"和普通文本候选。
	// 默认 "⚡"；显式空串=完全不显示；仅 type(...) 上屏的命令不加前缀。
	CandidatePrefix string `yaml:"candidate_prefix" json:"candidate_prefix"`
}

// ── Compat / Debug ──────────────────────────────────────────────────────────

// CompatConfig 进程级兼容（与 compat.toml 概念对齐）
type CompatConfig struct {
	// HostRenderProcesses 启用宿主进程代理渲染的进程白名单（进程名，不区分大小写）。
	// 在这些进程中，候选窗口将通过 DLL 内 CreateWindowInBand 创建，以解决 z-order 问题。
	HostRenderProcesses []string `yaml:"host_render_processes,omitempty" json:"host_render_processes,omitempty"`
}

// DebugConfig 诊断配置（原 AdvancedConfig）
type DebugConfig struct {
	LogLevel string `yaml:"log_level" json:"log_level"`
	// PerfSampling 启用按键链路性能采样（默认关闭）。
	// 开启后会记录每次按键的输入编码、引擎耗时等数据到内存环形缓冲区，
	// 可通过设置页导出 JSONL 文件用于性能分析。
	// 注意：采样数据包含用户输入内容，仅建议在排障或性能调优时临时开启。
	PerfSampling bool `yaml:"perf_sampling" json:"perf_sampling"`
}

// DefaultConfig returns the default configuration (Layer 1 代码兜底值)。
// 注意：本层与 data/config.toml（Layer 2 预置层）的取值分叉是分层设计的
// 正常形态（Layer 2 为覆盖 Layer 1 而存在），勿照预置文件"修正"本层。
func DefaultConfig() *Config {
	return &Config{
		General: GeneralConfig{
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
			SwitchEngine:    "ctrl+shift+e",
			ToggleFullWidth: "shift+space",
			TogglePunct:     "ctrl+.",
			DeleteCandidate: "ctrl+shift+number",
			PinCandidate:    "ctrl+number",
			ToggleToolbar:   "none",
			OpenSettings:    "none",
			AddWord:         "ctrl+equal",
			ToggleS2T:       "ctrl+shift+j",
			TakeScreenshot:  "ctrl+shift+f11",
			GlobalHotkeys:   []string{},
			ActivateIME:     "ctrl+shift+[",
		},
		Input: InputConfig{
			SmartPunctAfterDigit: true,
			SmartPunctList:       ".,:",
			SmartSymbolMode:      false,
			SmartSymbolTimeoutMs: 500,
			SmartSymbolChars:     "。，？！：；、～￥·……——",
			EnterBehavior:        EnterCommit,
			SpaceOnEmptyBehavior: SpaceOnEmptyCommit,
			PunctFollowMode:      false,
			FilterMode:           FilterSmart,
			SelectKeyGroups:      []keys.PairGroup{keys.PairSemicolonQuote},
			PageKeys:             []keys.PairGroup{keys.PairPageUpDown, keys.PairMinusEqual},
			HighlightKeys:        []keys.PairGroup{keys.PairArrows},
			SelectCharKeys:       []keys.PairGroup{},
			PinyinSeparator:      PinyinSeparatorAuto,
			ShiftTempEnglish: ShiftTempEnglishConfig{
				Enabled:               true,
				ShowEnglishCandidates: true,
				ShiftBehavior:         "temp_english",
				TriggerKeys:           []string{},
				AllowSymbols:          false,
				SpaceAsInput:          false,
			},
			CapsLock: CapsLockConfig{
				CancelOnModeSwitch: false,
			},
			TempPinyin: TempPinyinConfig{
				TriggerKeys:      []string{"backtick"},
				ZIncludeOnCommit: true,
				AccentColor:      "",
			},
			AutoPair: AutoPairConfig{
				Chinese:      false,
				English:      false,
				Blacklist:    []string{},
				ChinesePairs: []string{"（）", "【】", "｛｝", "《》", "〈〉"},
				EnglishPairs: []string{"()", "[]", "{}", "<>"},
			},
			Overflow: OverflowConfig{
				NumberKey:     OverflowIgnore,
				SelectKey:     OverflowIgnore,
				SelectCharKey: OverflowIgnore,
			},
			Phrase: PhraseConfig{
				MinPrefixLength: 2,
			},
		},
		UI: UIConfig{
			Candidate: UICandidateConfig{
				FontSize:            18,
				FontSizeFollowTheme: true, // 新装默认跟随主题字号；缺键=继承默认
				PerPage:             7,
				PerPageExtended:     0, // 默认禁用扩展档
				MaxChars:            16,
				Layout:              LayoutHorizontal,
				InlinePreedit:       true,
				PreeditMode:         PreeditTop,
				FlipWhenAbove:       false,
				HideWindow:          false,
				IndexLabels:         "",
				ModeAccentBorder:    false,
				// behavior 覆盖层默认跟随主题（哲学Y）；值字段为自定义模式兜底初值
				AlwaysShowPager:             false,
				AlwaysShowPagerFollowTheme:  true,
				ShowPageNumber:              true,
				ShowPageNumberFollowTheme:   true,
				VerticalMaxWidth:            600,
				VerticalMaxWidthFollowTheme: true,
			},
			Font: UIFontConfig{
				Family:     "",
				Path:       "",
				RenderMode: FontEngineDirectWrite,
				GDIWeight:  500,
				GDIScale:   1.0,
				MenuWeight: 500,
				MenuSize:   12.0,
			},
			Theme: UIThemeConfig{
				Name:            "default",
				Style:           ThemeStyleSystem,
				EditorAutoStart: false,
			},
			StatusIndicator: StatusIndicatorConfig{
				Enabled:         true,
				Duration:        800,
				DisplayMode:     "temp",
				SchemaNameStyle: "full",
				ShowMode:        true,
				ShowPunct:       true,
				ShowFullWidth:   false,
				PositionMode:    "follow_caret",
				OffsetX:         0,
				OffsetY:         0,
				FontSize:        18,
				Opacity:         0.9,
				BorderRadius:    6,
			},
			Tooltip: TooltipConfig{
				Delay:  100,
				Code:   TooltipCodeConfig{Enabled: true},
				Pinyin: TooltipPinyinConfig{Enabled: true, Heteronyms: true, MaxReadings: 0},
				Chaizi: TooltipChaiziConfig{Enabled: false},
				Debug:  TooltipDebugConfig{Enabled: false},
			},
			Toolbar: ToolbarConfig{
				Visible:          true,
				HideInFullscreen: true,
			},
		},
		Features: FeaturesConfig{
			Stats: StatsConfig{
				Enabled:      true,
				RetainDays:   0,
				TrackEnglish: true,
			},
			S2T: S2TConfig{
				Enabled: false,
				Variant: S2TStandard,
			},
			QuickInput: QuickInputConfig{
				TriggerKeys:   []string{"semicolon"},
				ForceVertical: true,
				DecimalPlaces: 6,
				AccentColor:   "",
			},
			Cmdbar: CmdbarConfig{
				CandidatePrefix: "⚡",
			},
		},
		Compat: CompatConfig{
			HostRenderProcesses: []string{"SearchHost.exe"},
		},
		Debug: DebugConfig{
			LogLevel:     "info",
			PerfSampling: false,
		},
	}
}

// Load loads the configuration using three-layer merge:
// 1. Code defaults (DefaultConfig)
// 2. System bundled config (data/config.toml) overlay
// 3. User config (%APPDATA%/WindInput/config.toml) overlay
func Load() (*Config, error) {
	return LoadFrom("")
}

// LoadFrom loads the configuration from a specific user config path.
// If path is empty, uses the default user config path.
// System config (data/config.toml) is always loaded as the middle layer.
//
// 版本迁移（docs/design/config-restructure.md §4）：
//   - 旧版 .yaml（v0）：map 层执行 migrateV0toV1（结构重排）后加载，成功后写出
//     v1 TOML；旧 YAML 文件保留原地不改名（§4.4 网盘混版本共存兜底）。
//   - .toml 缺 version：按 v1 处理（手编误删保护），下次保存自动补写。
//   - version 高于当前（程序回滚）：按损坏处理，备份 .bak + 回退默认。
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

	// Layer 2: 加载系统预置配置（data/config.toml，兼容旧版 .yaml 残留）覆盖代码默认值
	if sysPath, err := GetSystemConfigPath(); err == nil {
		if sysData, err := os.ReadFile(sysPath); err == nil {
			// 系统配置解析失败只打印警告，不中断；旧版 yaml 残留走 v0 迁移防御
			if _, err := unmarshalConfigData(sysPath, sysData, cfg, !IsTOMLPath(sysPath)); err != nil {
				fmt.Fprintf(os.Stderr, "[config] warning: failed to parse system config %s: %v\n", sysPath, err)
			}
		}
		// 系统配置文件不存在是正常情况，不需要报错
	}

	// Layer 3: 加载用户配置覆盖（config.toml 优先，缺失时回退旧版 config.yaml）
	userData, readPath, migratedFrom, err := readFileWithLegacyFallback(path)
	if err != nil {
		if os.IsNotExist(err) {
			// 用户配置不存在，使用前两层的结果
			return cfg, nil
		}
		return cfg, fmt.Errorf("failed to read config file: %w", err)
	}

	// 版本边界按实际读取路径的格式判定（而非是否发生回退）：
	// 直接以 .yaml 路径调用 LoadFrom（测试/工具场景）同样按 v0 处理。
	structMigrated, err := unmarshalConfigData(readPath, userData, cfg, !IsTOMLPath(readPath))
	if err != nil {
		// 区分错误类型，尽量自愈，避免每次启动都解析失败而永久降级。
		var typeErr *yaml.TypeError
		switch {
		case errors.As(err, &typeErr):
			// TypeError（部分解码）：yaml.v3 已把所有可解析字段填进了 cfg，
			// 仅出错字段维持默认值，保留 cfg 现状，不重置为 DefaultConfig。
			// 隐私：只记录数量等元数据，typeErr.Errors 为字段路径描述，不含字段值内容。
			fmt.Fprintf(os.Stderr, "[config] warning: 配置部分字段不兼容，已保留其余配置并对不兼容字段使用默认值 count=%d\n", len(typeErr.Errors))
		case errors.Is(err, ErrFutureConfigVersion):
			// 程序被回滚到旧版：不解读未来格式，备份后回退默认（设计 §4.1 规则 4）
			fmt.Fprintf(os.Stderr, "[config] warning: 配置文件版本高于当前程序支持，已备份并回退默认 path=%s err=%v\n", readPath, err)
			cfg = DefaultConfig()
		default:
			// 其它错误（语法损坏，含 TOML 语法错误）：无法部分解码，回退到默认配置。
			fmt.Fprintf(os.Stderr, "[config] warning: 配置文件损坏，使用默认配置 path=%s err=%v\n", readPath, err)
			cfg = DefaultConfig()
		}

		// 兜底校验，确保自愈写回的配置是合法可加载状态。
		ApplyConfigFallbacks(cfg)

		// 自愈文件：先备份原始内容（best-effort），再回写当前有效配置到规范路径。
		if bakErr := os.WriteFile(readPath+".bak", userData, 0o644); bakErr != nil {
			fmt.Fprintf(os.Stderr, "[config] warning: 备份损坏配置失败 path=%s err=%v\n", readPath+".bak", bakErr)
		}
		if saveErr := SaveTo(cfg, path); saveErr != nil {
			fmt.Fprintf(os.Stderr, "[config] warning: 自愈回写配置失败 path=%s err=%v\n", path, saveErr)
		}

		return cfg, nil
	}

	// 兜底校验
	ApplyConfigFallbacks(cfg)

	// 一次性迁移写出：旧版 YAML（格式+结构）成功加载后写出 v1 TOML。
	// 旧文件保留原地不改名（§4.4）；写出失败时下次启动自动重试。
	if migratedFrom != "" || structMigrated {
		if err := SaveTo(cfg, path); err != nil {
			fmt.Fprintf(os.Stderr, "[config] warning: 配置迁移写出失败（下次启动重试） path=%s err=%v\n", path, err)
		}
	}

	return cfg, nil
}

// unmarshalConfigData 把配置文件字节解码进 cfg：先按扩展名归一化为 YAML
// （TOML 语法错误在此阶段返回，等价于文件损坏），在 map 层执行版本迁移
// （fromLegacyYAML 标记来源是否为旧版 .yaml 回退文件），再走 yaml.Unmarshal
// 保留部分覆盖与 yaml.TypeError 部分解码语义。
// 返回 migrated=true 表示发生了结构迁移（调用方应触发写出持久化）。
func unmarshalConfigData(path string, data []byte, cfg *Config, fromLegacyYAML bool) (migrated bool, err error) {
	yamlData, err := normalizeToYAML(path, data)
	if err != nil {
		return false, err
	}
	if len(yamlData) == 0 {
		return false, nil
	}

	var m map[string]any
	if err := yaml.Unmarshal(yamlData, &m); err != nil {
		return false, err
	}
	if m == nil {
		return false, nil
	}

	migrated, err = migrateConfigMap(m, fromLegacyYAML)
	if err != nil {
		return false, err // ErrFutureConfigVersion 等，调用方按损坏分支处理
	}

	remarshaled, err := yaml.Marshal(m)
	if err != nil {
		return migrated, err
	}
	return migrated, yaml.Unmarshal(remarshaled, cfg)
}

// ApplyConfigFallbacks 对关键字段进行兜底处理（仅值域 clamp 类；
// 结构性迁移已全部熔入 migrateV0toV1，见 migration_v1.go）
func ApplyConfigFallbacks(cfg *Config) {
	// 如果 available 为空，使用默认值
	if len(cfg.Schema.Available) == 0 {
		cfg.Schema.Available = []string{"wubi86", "pinyin"}
	}
	// Schema 兜底：如果 active 为空，取 available 的第一个
	if cfg.Schema.Active == "" && len(cfg.Schema.Available) > 0 {
		cfg.Schema.Active = cfg.Schema.Available[0]
	}

	// MaxChars 兜底：越界时回退到 16，合法范围 8-64
	if cfg.UI.Candidate.MaxChars < 8 || cfg.UI.Candidate.MaxChars > 64 {
		cfg.UI.Candidate.MaxChars = 16
	}

	// PerPageExtended 兜底：0=禁用扩展档（合法值），上界 clamp 到 10
	// （受数字键 1-9、0 限制，每页最多 10 个可选）
	if cfg.UI.Candidate.PerPageExtended > 10 {
		cfg.UI.Candidate.PerPageExtended = 10
	}

	// Phrase.MinPrefixLength 兜底：下界 clamp 到 1（0/负值回退默认 2）
	if cfg.Input.Phrase.MinPrefixLength <= 0 {
		cfg.Input.Phrase.MinPrefixLength = 2
	}

	// ThemeStyle 兜底
	if cfg.UI.Theme.Style == "" {
		cfg.UI.Theme.Style = ThemeStyleSystem
	}

	// S2T variant 兜底
	if !cfg.Features.S2T.Variant.Valid() {
		cfg.Features.S2T.Variant = S2TStandard
	}
}

// Save saves the configuration to file
func Save(config *Config) error {
	return SaveTo(config, "")
}

// SaveTo saves the configuration to a specific path
// If path is empty, uses the default config path
// 使用 diff 保存：只将与系统默认值不同的字段写入用户配置文件，
// 使未修改的字段能自动跟随系统默认值的更新。
func SaveTo(cfg *Config, path string) error {
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

	// 计算与系统默认配置的差异，只保存用户修改过的字段
	base := SystemDefaultConfig()
	diff, err := ComputeYAMLDiff(base, cfg)
	if err != nil {
		// diff 失败时回退到全量保存（同样注入 version 元数据）
		fmt.Fprintf(os.Stderr, "[config] warning: diff failed, falling back to full save: %v\n", err)
		full, mapErr := toYAMLMap(cfg)
		if mapErr != nil {
			return fmt.Errorf("failed to marshal config: %w", mapErr)
		}
		injectVersion(full)
		data, mErr := marshalForPath(path, full)
		if mErr != nil {
			return fmt.Errorf("failed to marshal config: %w", mErr)
		}
		return os.WriteFile(path, data, 0644)
	}

	// version 是元数据不参与 diff，在 diff 之后强制注入；
	// 空 diff 也写出 version 一行（迁移完成标记，见设计 §4.3）。
	injectVersion(diff)

	data, err := marshalForPath(path, diff)
	if err != nil {
		return fmt.Errorf("failed to marshal config diff: %w", err)
	}

	if err := os.WriteFile(path, data, 0644); err != nil {
		return fmt.Errorf("failed to write config file: %w", err)
	}

	return nil
}

// SystemDefaultConfig returns the system default configuration
// by merging code defaults (Layer 1) with bundled data/config.toml (Layer 2).
// This is the "factory default" that excludes user customizations.
func SystemDefaultConfig() *Config {
	cfg := DefaultConfig()

	if sysPath, err := GetSystemConfigPath(); err == nil {
		if sysData, err := os.ReadFile(sysPath); err == nil {
			if _, err := unmarshalConfigData(sysPath, sysData, cfg, !IsTOMLPath(sysPath)); err != nil {
				fmt.Fprintf(os.Stderr, "[config] warning: failed to parse system config %s: %v\n", sysPath, err)
			}
		}
	}

	ApplyConfigFallbacks(cfg)
	return cfg
}

// SaveDefault saves the default configuration to file
func SaveDefault() error {
	return Save(DefaultConfig())
}
