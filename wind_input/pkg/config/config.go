package config

import (
	"errors"
	"fmt"
	"os"

	"gopkg.in/yaml.v3"

	"github.com/huanfeng/wind_input/pkg/keys"
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
	Stats    StatsConfig    `yaml:"stats" json:"stats"`
	S2T      S2TConfig      `yaml:"s2t" json:"s2t"`
}

// StatsConfig 输入统计配置
// 使用 *bool 指针类型避免 yaml.v3 反序列化时将未设置的字段归零
type StatsConfig struct {
	Enabled      *bool `yaml:"enabled,omitempty" json:"enabled"`             // 是否启用统计（nil=默认 true）
	RetainDays   int   `yaml:"retain_days" json:"retain_days"`               // 数据保留天数（0=永久，默认 0）
	TrackEnglish *bool `yaml:"track_english,omitempty" json:"track_english"` // 是否统计英文模式（nil=默认 true）
}

func boolPtr(v bool) *bool { return &v }

// IsEnabled 返回统计是否启用
func (c *StatsConfig) IsEnabled() bool {
	if c.Enabled == nil {
		return true
	}
	return *c.Enabled
}

// IsTrackEnglish 返回是否统计英文
func (c *StatsConfig) IsTrackEnglish() bool {
	if c.TrackEnglish == nil {
		return true
	}
	return *c.TrackEnglish
}

// SchemaConfig 输入方案配置
type SchemaConfig struct {
	Active    string   `yaml:"active" json:"active"`       // 当前活跃方案 ID
	Available []string `yaml:"available" json:"available"` // 可切换方案 ID 列表（顺序决定切换顺序）
	// PrimaryCodetable 主码表方案 ID。
	// 用于：拼音类方案的"反查/编码提示"统一从此方案的码表派生。
	// 留空时按 Available 顺序选第一个 codetable 方案；都没有则不显示编码提示。
	PrimaryCodetable string `yaml:"primary_codetable,omitempty" json:"primaryCodetable,omitempty"`
	// PrimaryPinyin 主拼音方案 ID。
	// 用于：码表方案的"临时拼音/快捷输入"统一指向此方案。
	// 留空时按 Available 顺序选第一个 pinyin 方案；都没有则禁用临时拼音。
	PrimaryPinyin string `yaml:"primary_pinyin,omitempty" json:"primaryPinyin,omitempty"`
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
	SkipAbbrev      bool              `yaml:"skip_abbrev" json:"skip_abbrev"` // 跳过简拼匹配（混输模式专用）
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
	ToggleS2T       string   `yaml:"toggle_s2t" json:"toggle_s2t"`             // 简入繁出开关切换: 通用按键组合或 "none"
	TakeScreenshot  string   `yaml:"take_screenshot" json:"take_screenshot"`   // UI 截图: 通用按键组合或 "none"
	GlobalHotkeys   []string `yaml:"global_hotkeys" json:"global_hotkeys"`     // 注册为全局热键的快捷键名称列表
}

// S2TConfig 简入繁出（简体->繁体）配置
type S2TConfig struct {
	Enabled bool       `yaml:"enabled" json:"enabled"` // 总开关
	Variant S2TVariant `yaml:"variant" json:"variant"` // 变体: s2t / s2tw / s2twp / s2hk
}

// TooltipConfig 候选悬停增强提示配置
type TooltipConfig struct {
	Code   TooltipCodeConfig   `yaml:"code" json:"code"`     // 编码显示
	Pinyin TooltipPinyinConfig `yaml:"pinyin" json:"pinyin"` // 拼音提示
	Chaizi TooltipChaiziConfig `yaml:"chaizi" json:"chaizi"` // 拆字提示
	Debug  TooltipDebugConfig  `yaml:"debug" json:"debug"`   // 调试信息
}

// TooltipCodeConfig 编码显示配置
type TooltipCodeConfig struct {
	Enabled *bool `yaml:"enabled,omitempty" json:"enabled,omitempty"` // nil=默认 true（向后兼容）
}

// IsEnabled 返回是否显示编码（nil 视作 true）
func (c *TooltipCodeConfig) IsEnabled() bool {
	if c.Enabled == nil {
		return true
	}
	return *c.Enabled
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

// UIConfig contains UI settings
type UIConfig struct {
	FontSize float64 `yaml:"font_size" json:"font_size"`
	// FontSizeFollowTheme=true 时候选字号跟随主题 behavior.font_size，忽略 FontSize；
	// false=用 FontSize 自定义。默认 true（新装跟随主题）。
	// 注意：**不可加 omitempty**——默认值为 true 时，omitempty 会把"显式 false"与"未设置"
	// 在 YAML 中混为一谈，破坏 diff-save/merge-on-default 的闭环（false 写不出、回读成默认 true）。
	// 走通用「缺失=继承默认」机制，无特例探针。见 renderer.recomputeBaseFont。
	FontSizeFollowTheme bool `yaml:"font_size_follow_theme" json:"font_size_follow_theme"`

	// 主题 behavior 用户覆盖层（哲学Y，见 docs/design/theme-schema-v3.md「behavior」）：
	// 每个 behavior 字段 = FollowTheme ? 主题 behavior 推荐默认 : 用户自定义值。
	// 与 FontSize/FontSizeFollowTheme 同模式；*FollowTheme 默认 true（新装跟随主题）。
	// 注意：FollowTheme 类 bool **不可加 omitempty**——默认 true 时 omitempty 会把"显式 false"
	// 与"未设置"在 YAML 中混为一谈，破坏 diff-save/merge-on-default 闭环（见 FontSizeFollowTheme 注释）。
	AlwaysShowPager             bool `yaml:"always_show_pager" json:"always_show_pager"`                             // 单页时也显示翻页区（FollowTheme=false 时生效）
	AlwaysShowPagerFollowTheme  bool `yaml:"always_show_pager_follow_theme" json:"always_show_pager_follow_theme"`   // true=跟随主题 behavior.always_show_pager
	ShowPageNumber              bool `yaml:"show_page_number" json:"show_page_number"`                               // 显示页码文字 "1/3"（FollowTheme=false 时生效）
	ShowPageNumberFollowTheme   bool `yaml:"show_page_number_follow_theme" json:"show_page_number_follow_theme"`     // true=跟随主题 behavior.show_page_number
	VerticalMaxWidth            int  `yaml:"vertical_max_width" json:"vertical_max_width"`                           // 竖排候选最大宽度（逻辑像素，FollowTheme=false 时生效）
	VerticalMaxWidthFollowTheme bool `yaml:"vertical_max_width_follow_theme" json:"vertical_max_width_follow_theme"` // true=跟随主题 behavior.vertical_max_width

	CandidatesPerPage int `yaml:"candidates_per_page" json:"candidates_per_page"`
	// CandidatesPerPageExtended 扩展档每页候选数。在临时拼音/快捷输入/短语等场景下生效，
	// 让这些场景显示更多候选；普通码表输入仍用 CandidatesPerPage（基础档）。
	// <=0 表示禁用扩展档（始终用基础档，向后兼容）；正值有效，上界 clamp 到 10
	// （候选用数字键 1-9、0 选择，每页最多 10 个可选，超出无对应数字键且标签会重复）。
	// 判定"是否启用扩展档"统一由 >0 决定，故无需下界 clamp。
	CandidatesPerPageExtended int               `yaml:"candidates_per_page_extended,omitempty" json:"candidates_per_page_extended,omitempty"`
	FontFamily                string            `yaml:"font_family" json:"font_family"`
	FontPath                  string            `yaml:"font_path" json:"font_path"`
	InlinePreedit             bool              `yaml:"inline_preedit" json:"inline_preedit"`
	HideCandidateWindow       bool              `yaml:"hide_candidate_window" json:"hide_candidate_window"`
	CandidateLayout           CandidateLayout   `yaml:"candidate_layout" json:"candidate_layout"`                   // 候选布局：horizontal 或 vertical
	StatusIndicatorDuration   int               `yaml:"status_indicator_duration" json:"status_indicator_duration"` // 状态提示显示时长（毫秒）
	StatusIndicatorOffsetX    int               `yaml:"status_indicator_offset_x" json:"status_indicator_offset_x"` // 状态提示 X 偏移量
	StatusIndicatorOffsetY    int               `yaml:"status_indicator_offset_y" json:"status_indicator_offset_y"` // 状态提示 Y 偏移量
	Theme                     string            `yaml:"theme" json:"theme"`                                         // 主题名称：default, msime 或自定义主题名
	ThemeStyle                ThemeStyle        `yaml:"theme_style" json:"theme_style"`                             // 主题风格：system(跟随系统), light(亮色), dark(暗色)
	PagerBarDisplay           PagerBarDisplay   `yaml:"pager_bar_display" json:"pager_bar_display"`                 // 翻页栏显示方式：空=主题配置, always=总是显示, auto=多页时显示, hide=隐藏
	PageNumberDisplay         PageNumberDisplay `yaml:"page_number_display" json:"page_number_display"`             // 页码显示方式：空=主题配置, show=显示, hide=隐藏
	TooltipDelay              int               `yaml:"tooltip_delay" json:"tooltip_delay"`                         // 编码提示延迟显示时间（毫秒），0 表示立即显示

	PreeditMode PreeditMode `yaml:"preedit_mode" json:"preedit_mode"` // 编码显示模式："top"（默认，编码在上方独立行）, "embedded"（嵌入候选行前）；仅 InlinePreedit=false 时生效

	// FlipLayoutWhenAbove 候选窗在光标上方时反转 bands 排列顺序，使预编辑栏保持最靠近光标。
	// true（默认，由 data/config.yaml 设置）：上方模式下 preedit 在底部，首候选紧贴光标；
	// false：保持旧行为（preedit 在顶部）。
	FlipLayoutWhenAbove bool `yaml:"flip_layout_when_above" json:"flip_layout_when_above"`

	Tooltip TooltipConfig `yaml:"tooltip" json:"tooltip"` // 候选悬停提示配置

	// 文本渲染设置
	TextRenderMode FontEngine `yaml:"text_render_mode,omitempty" json:"text_render_mode,omitempty"` // 文本渲染引擎："directwrite"（默认，DirectWrite渲染）、"gdi"（Windows原生GDI渲染）或 "freetype"（FreeType渲染）
	GDIFontWeight  int        `yaml:"gdi_font_weight,omitempty" json:"gdi_font_weight,omitempty"`   // 候选框GDI字体粗细：100~900，默认500(Medium)
	GDIFontScale   float64    `yaml:"gdi_font_scale,omitempty" json:"gdi_font_scale,omitempty"`     // GDI字体缩放：0.5~2.0，默认1.0，值越大文字越大
	MenuFontWeight int        `yaml:"menu_font_weight,omitempty" json:"menu_font_weight,omitempty"` // 菜单GDI字体粗细：100~900，默认600(SemiBold)
	MenuFontSize   float64    `yaml:"menu_font_size,omitempty" json:"menu_font_size,omitempty"`     // 菜单字体大小：默认12.0（DPI缩放前基础值）

	StatusIndicator StatusIndicatorConfig `yaml:"status_indicator" json:"status_indicator"` // 状态提示配置

	// 特殊模式内发光边框开关：nil=开启（默认），false=关闭
	ModeAccentBorder *bool `yaml:"mode_accent_border,omitempty" json:"mode_accent_border,omitempty"`
	// 特殊模式内发光边框颜色（十六进制，如 "#3C78AFD2"），空=使用内置默认色
	TempPinyinAccentColor string `yaml:"temp_pinyin_accent_color,omitempty" json:"temp_pinyin_accent_color,omitempty"`
	QuickInputAccentColor string `yaml:"quick_input_accent_color,omitempty" json:"quick_input_accent_color,omitempty"`

	// MaxCandidateChars 候选文本最大显示 rune 数，超出后截断并追加"…"。
	// 合法范围 8-64，0 或越界时回退到默认值 16。
	MaxCandidateChars int `yaml:"max_candidate_chars,omitempty" json:"max_candidate_chars,omitempty"`

	// CmdbarCandidatePrefix 副作用命令直通车候选 (Actions 含 ActionEffect) 在候选框
	// 渲染时的前缀符号, 让用户一眼分辨"会跑副作用的命令"和普通文本候选。
	// 仅 type(...) 上屏的命令视觉上与普通候选无差, 不会加前缀。
	// 用 *string 区分三态:
	//   - nil           未设置, 使用默认 "⚡"
	//   - 指向空字符串  完全不显示
	//   - 指向非空字符串 使用该符号 (如 "▶")
	CmdbarCandidatePrefix *string `yaml:"cmdbar_candidate_prefix,omitempty" json:"cmdbar_candidate_prefix,omitempty"`

	// CandidateIndexLabels 用户全局序号标签覆盖（10 槽位字符或 /-分隔模板，如 "①②③④⑤⑥⑦⑧⑨⑩" 或 "1./2./…"）。
	// 非空时覆盖主题 views.index.labels；空=用主题。优先级低于运行时 per-候选 IndexLabel。
	CandidateIndexLabels string `yaml:"candidate_index_labels,omitempty" json:"candidate_index_labels,omitempty"`

	// ThemeEditorAutoStart 打开设置界面时自动开启 Web 编辑器连接服务。
	ThemeEditorAutoStart bool `yaml:"theme_editor_auto_start,omitempty" json:"theme_editor_auto_start,omitempty"`
}

// GetCmdbarCandidatePrefix 返回 cmdbar 副作用候选的渲染前缀。
// 未设置 (nil) 时返回默认 "⚡"; 显式设为空串时返回空串 (调用方据此关闭前缀)。
func (u *UIConfig) GetCmdbarCandidatePrefix() string {
	if u.CmdbarCandidatePrefix == nil {
		return "⚡"
	}
	return *u.CmdbarCandidatePrefix
}

// ToolbarConfig contains toolbar settings
type ToolbarConfig struct {
	Visible bool `yaml:"visible" json:"visible"`
	// HideInFullscreen 控制：当前台应用处于全屏状态时是否自动隐藏工具栏。
	// 使用 *bool 区分"未设置"和"显式 false"，未设置时按默认 true 处理。
	HideInFullscreen *bool `yaml:"hide_in_fullscreen,omitempty" json:"hide_in_fullscreen,omitempty"`
}

// IsHideInFullscreen 返回是否启用「全屏时隐藏工具栏」。未设置时默认 true。
func (c *ToolbarConfig) IsHideInFullscreen() bool {
	if c.HideInFullscreen == nil {
		return true
	}
	return *c.HideInFullscreen
}

// InputConfig contains input behavior settings
type InputConfig struct {
	PunctFollowMode      bool                   `yaml:"punct_follow_mode" json:"punct_follow_mode"`
	FilterMode           FilterMode             `yaml:"filter_mode" json:"filter_mode"` // 候选过滤模式: "smart"(智能), "general"(仅常用字), "gb18030"(不限制)
	SelectKeyGroups      []keys.PairGroup       `yaml:"select_key_groups" json:"select_key_groups"`
	PageKeys             []keys.PairGroup       `yaml:"page_keys" json:"page_keys"`
	HighlightKeys        []keys.PairGroup       `yaml:"highlight_keys" json:"highlight_keys"`                   // 移动高亮候选项: PairArrows / PairTab
	SelectCharKeys       []keys.PairGroup       `yaml:"select_char_keys" json:"select_char_keys"`               // 以词定字按键: PairCommaPeriod / PairMinusEqual / PairBrackets
	SmartPunctAfterDigit bool                   `yaml:"smart_punct_after_digit" json:"smart_punct_after_digit"` // 数字后标点智能转换（默认 true）
	SmartPunctList       string                 `yaml:"smart_punct_list" json:"smart_punct_list"`               // 数字后保持英文的标点字符，如 ".,:"
	EnterBehavior        EnterBehavior          `yaml:"enter_behavior" json:"enter_behavior"`                   // 回车键行为: "commit"(上屏编码), "clear"(清空编码)
	SpaceOnEmptyBehavior SpaceOnEmptyBehavior   `yaml:"space_on_empty_behavior" json:"space_on_empty_behavior"` // 空码时空格键行为: "commit"(上屏编码), "clear"(清空编码)
	NumpadBehavior       string                 `yaml:"numpad_behavior" json:"numpad_behavior"`                 // 数字小键盘功能: "direct"(直接输入数字,默认) | "follow_main"(同主键盘区数字)
	PinyinSeparator      PinyinSeparatorMode    `yaml:"pinyin_separator" json:"pinyin_separator"`               // 拼音分隔符: "auto", "quote", "backtick", "none"
	ShiftTempEnglish     ShiftTempEnglishConfig `yaml:"shift_temp_english" json:"shift_temp_english"`
	CapsLockBehavior     CapsLockBehaviorConfig `yaml:"capslock_behavior" json:"capslock_behavior"`
	TempPinyin           TempPinyinConfig       `yaml:"temp_pinyin" json:"temp_pinyin"`
	AutoPair             AutoPairConfig         `yaml:"auto_pair" json:"auto_pair"`
	PunctCustom          PunctCustomConfig      `yaml:"punct_custom" json:"punct_custom"`
	QuickInput           QuickInputConfig       `yaml:"quick_input" json:"quick_input"`
	OverflowBehavior     OverflowBehaviorConfig `yaml:"overflow_behavior" json:"overflow_behavior"` // 候选按键无效时的处理策略
	Phrase               PhraseConfig           `yaml:"phrase" json:"phrase"`                       // 短语相关行为
	SpecialModes         []SpecialModeConfig    `yaml:"special_modes,omitempty" json:"special_modes,omitempty"`
}

// PhraseConfig 短语相关行为配置（暂无 UI，文件配置）
type PhraseConfig struct {
	// MinPrefixLength 短语前缀匹配触发的最小输入长度（默认 2）。
	// 当输入码长 < MinPrefixLength 且 < 短语自身码长时，该短语不参与前缀展开；
	// 等价规则: 短语条目仅在 len(input) >= MinPrefixLength || len(input) >= len(code) 时出现。
	// 例如默认 2 时, 单字符 "z" 不会前缀展开 zzbd / zzaa, 但用户配置的码长为 1 的短语 (input="z" code="z") 仍按精确匹配走 SearchCommand。
	MinPrefixLength int `yaml:"min_prefix_length,omitempty" json:"min_prefix_length,omitempty"`
}

// OverflowBehaviorConfig 候选按键无效时的处理策略
type OverflowBehaviorConfig struct {
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
	FixedLength   int      `yaml:"fixed_length,omitempty" json:"fixed_length,omitempty"`
	ForceVertical bool     `yaml:"force_vertical,omitempty" json:"force_vertical,omitempty"`
	AccentColor   string   `yaml:"accent_color,omitempty" json:"accent_color,omitempty"`
	// ShowAllOnEntry 刚进入模式（编码为空）时是否立即列出整张码表的全部候选。
	// false(默认)=只显示模式徽标提示，打字后才按前缀出候选；true=进入即列全部（大表慎用）。
	ShowAllOnEntry bool `yaml:"show_all_on_entry,omitempty" json:"show_all_on_entry,omitempty"`
	// —— 预留字段，MVP 不实现 ——
	CodeCharset string   `yaml:"code_charset,omitempty" json:"code_charset,omitempty"`
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
	// 兼容旧配置字段（加载后迁移到 TriggerKeys）
	Enabled    *bool  `yaml:"enabled,omitempty" json:"enabled,omitempty"`         // deprecated
	TriggerKey string `yaml:"trigger_key,omitempty" json:"trigger_key,omitempty"` // deprecated
}

// PunctCustomConfig 自定义标点映射配置
type PunctCustomConfig struct {
	Enabled  bool                `yaml:"enabled" json:"enabled"`                       // 总开关
	Mappings map[string][]string `yaml:"mappings,omitempty" json:"mappings,omitempty"` // key=源字符(引号用"1/"2/'1/'2), value=[中文半角,英文全角,中文全角], 空串=默认
}

// TempPinyinConfig 临时拼音模式配置
type TempPinyinConfig struct {
	TriggerKeys []string `yaml:"trigger_keys" json:"trigger_keys"` // 触发键: "backtick", "semicolon", "z"
	// ZIncludeOnCommit 控制 z 触发临时拼音后，按 Enter 上屏编码时是否包含触发键 z 本身。
	// 默认 true（保留 z），仅对 z 触发键生效，不影响符号触发键。暂不暴露 UI，作为内部预留开关。
	ZIncludeOnCommit *bool `yaml:"z_include_on_commit,omitempty" json:"z_include_on_commit,omitempty"`
}

// ZIncludeOnCommitEnabled 返回 z 触发临时拼音按 Enter 上屏时是否包含 z（默认 true）
func (t TempPinyinConfig) ZIncludeOnCommitEnabled() bool {
	if t.ZIncludeOnCommit == nil {
		return true
	}
	return *t.ZIncludeOnCommit
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

// CapsLockBehaviorConfig CapsLock 行为配置
type CapsLockBehaviorConfig struct {
	CancelOnModeSwitch bool `yaml:"cancel_on_mode_switch" json:"cancel_on_mode_switch"`
}

// AdvancedConfig 高级配置
type AdvancedConfig struct {
	LogLevel string `yaml:"log_level" json:"log_level"`
	// PerfSampling 启用按键链路性能采样（默认关闭）。
	// 开启后会记录每次按键的输入编码、引擎耗时等数据到内存环形缓冲区，
	// 可通过设置页导出 JSONL 文件用于性能分析。
	// 注意：采样数据包含用户输入内容，仅建议在排障或性能调优时临时开启。
	PerfSampling *bool `yaml:"perf_sampling,omitempty" json:"perf_sampling"`
	// HostRenderProcesses 启用宿主进程代理渲染的进程白名单（进程名，不区分大小写）
	// 在这些进程中，候选窗口将通过 DLL 内 CreateWindowInBand 创建，以解决 z-order 问题
	HostRenderProcesses []string `yaml:"host_render_processes,omitempty" json:"host_render_processes,omitempty"`
}

// IsPerfSampling 返回性能采样是否启用（nil 指针视为 false）
func (c *AdvancedConfig) IsPerfSampling() bool {
	if c.PerfSampling == nil {
		return false
	}
	return *c.PerfSampling
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
		},
		UI: UIConfig{
			FontSize:            18,
			FontSizeFollowTheme: true, // 新装默认跟随主题字号；老配置经 LoadFrom 探针迁移为 false（自定义）
			// 主题 behavior 用户覆盖默认跟随主题（哲学Y）；值字段为自定义模式兜底初值。
			AlwaysShowPager:             false,
			AlwaysShowPagerFollowTheme:  true,
			ShowPageNumber:              true,
			ShowPageNumberFollowTheme:   true,
			VerticalMaxWidth:            600,
			VerticalMaxWidthFollowTheme: true,
			CandidatesPerPage:           7,
			CandidatesPerPageExtended:   0, // 默认禁用扩展档，老用户行为不变；用户主动配置后才分档
			MaxCandidateChars:           16,
			FontFamily:                  "",
			FontPath:                    "",
			InlinePreedit:               true,
			PreeditMode:                 PreeditTop,
			CandidateLayout:             LayoutHorizontal,
			StatusIndicatorDuration:     800,
			StatusIndicatorOffsetX:      0,
			StatusIndicatorOffsetY:      0,
			TooltipDelay:                100,
			Theme:                       "default",
			ThemeStyle:                  ThemeStyleSystem,
			TextRenderMode:              FontEngineDirectWrite,
			GDIFontWeight:               500,
			GDIFontScale:                1.0,
			MenuFontWeight:              500,
			MenuFontSize:                12.0,
			Tooltip: TooltipConfig{
				Pinyin: TooltipPinyinConfig{Enabled: true, Heteronyms: true, MaxReadings: 0},
				Chaizi: TooltipChaiziConfig{Enabled: false},
				Debug:  TooltipDebugConfig{Enabled: false},
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
		},
		Toolbar: ToolbarConfig{
			Visible: true,
		},
		Input: InputConfig{
			SmartPunctAfterDigit: true,
			SmartPunctList:       ".,:",
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
			QuickInput: QuickInputConfig{
				TriggerKeys:   []string{"semicolon"},
				ForceVertical: true,
				DecimalPlaces: 6,
			},
			OverflowBehavior: OverflowBehaviorConfig{
				NumberKey:     OverflowIgnore,
				SelectKey:     OverflowIgnore,
				SelectCharKey: OverflowIgnore,
			},
			Phrase: PhraseConfig{
				MinPrefixLength: 2,
			},
		},
		Advanced: AdvancedConfig{
			LogLevel:            "info",
			HostRenderProcesses: []string{"SearchHost.exe"},
		},
		Stats: StatsConfig{
			Enabled:      boolPtr(true),
			RetainDays:   0,
			TrackEnglish: boolPtr(true),
		},
		S2T: S2TConfig{
			Enabled: false,
			Variant: S2TStandard,
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
		// 区分错误类型，尽量自愈，避免每次启动都解析失败而永久降级。
		var typeErr *yaml.TypeError
		if errors.As(err, &typeErr) {
			// TypeError（部分解码）：yaml.v3 已把所有可解析字段填进了 cfg，
			// 仅出错字段维持默认值，保留 cfg 现状，不重置为 DefaultConfig。
			// 隐私：只记录数量等元数据，typeErr.Errors 为字段路径描述，不含字段值内容。
			fmt.Fprintf(os.Stderr, "[config] warning: 配置部分字段不兼容，已保留其余配置并对不兼容字段使用默认值 count=%d\n", len(typeErr.Errors))
		} else {
			// 其它错误（语法损坏）：无法部分解码，回退到默认配置。
			fmt.Fprintf(os.Stderr, "[config] warning: 配置文件损坏，使用默认配置 path=%s err=%v\n", path, err)
			cfg = DefaultConfig()
		}

		// 兜底校验，确保自愈写回的配置是合法可加载状态。
		ApplyConfigFallbacks(cfg)

		// 自愈文件：先备份原始内容（best-effort），再原子回写当前有效配置。
		if bakErr := os.WriteFile(path+".bak", userData, 0o644); bakErr != nil {
			fmt.Fprintf(os.Stderr, "[config] warning: 备份损坏配置失败 path=%s err=%v\n", path+".bak", bakErr)
		}
		if saveErr := SaveTo(cfg, path); saveErr != nil {
			fmt.Fprintf(os.Stderr, "[config] warning: 自愈回写配置失败 path=%s err=%v\n", path, saveErr)
		}

		return cfg, nil
	}

	// 兜底校验
	ApplyConfigFallbacks(cfg)

	return cfg, nil
}

// ApplyConfigFallbacks 对关键字段进行兜底处理
func ApplyConfigFallbacks(cfg *Config) {
	// 如果 available 为空，使用默认值
	if len(cfg.Schema.Available) == 0 {
		cfg.Schema.Available = []string{"wubi86", "pinyin"}
	}
	// Schema 兜底：如果 active 为空，取 available 的第一个
	if cfg.Schema.Active == "" && len(cfg.Schema.Available) > 0 {
		cfg.Schema.Active = cfg.Schema.Available[0]
	}

	// MaxCandidateChars 兜底：0 或越界时回退到 16，合法范围 8-64
	if cfg.UI.MaxCandidateChars < 8 || cfg.UI.MaxCandidateChars > 64 {
		cfg.UI.MaxCandidateChars = 16
	}

	// CandidatesPerPageExtended 兜底：<=0 表示禁用扩展档（保持），上界 clamp 到 10
	// （受数字键 1-9、0 限制，每页最多 10 个可选）
	if cfg.UI.CandidatesPerPageExtended > 10 {
		cfg.UI.CandidatesPerPageExtended = 10
	}

	// Phrase.MinPrefixLength 兜底：未配置 (0) 或负值时回退到 2
	if cfg.Input.Phrase.MinPrefixLength <= 0 {
		cfg.Input.Phrase.MinPrefixLength = 2
	}

	// 迁移旧的 theme:"dark" 配置到新格式
	if cfg.UI.Theme == string(ThemeStyleDark) {
		cfg.UI.Theme = "default"
		cfg.UI.ThemeStyle = ThemeStyleDark
	}

	// ThemeStyle 兜底
	if cfg.UI.ThemeStyle == "" {
		cfg.UI.ThemeStyle = ThemeStyleSystem
	}

	// 迁移旧的快捷输入配置（enabled+trigger_key → trigger_keys）
	migrateQuickInputConfig(cfg)

	// S2T variant 兜底
	if !cfg.S2T.Variant.Valid() {
		cfg.S2T.Variant = S2TStandard
	}

	// 迁移旧的状态提示字段到新的 StatusIndicator 结构
	migrateStatusIndicatorConfig(cfg)
}

// migrateQuickInputConfig 将旧的 enabled+trigger_key 迁移到 trigger_keys
func migrateQuickInputConfig(cfg *Config) {
	qi := &cfg.Input.QuickInput
	if qi.TriggerKey != "" && len(qi.TriggerKeys) == 0 {
		// 旧格式：有 trigger_key 但没有 trigger_keys
		if qi.Enabled == nil || *qi.Enabled {
			// 启用状态（默认或显式启用）：迁移触发键到列表
			qi.TriggerKeys = []string{qi.TriggerKey}
		}
		// 禁用状态：trigger_keys 保持为空（=关闭）
		qi.TriggerKey = ""
		qi.Enabled = nil
	}
	if qi.Enabled != nil {
		// 清理旧字段
		if !*qi.Enabled {
			qi.TriggerKeys = nil
		}
		qi.Enabled = nil
	}
}

// migrateStatusIndicatorConfig 将旧的状态提示字段迁移到新的 StatusIndicatorConfig 结构
func migrateStatusIndicatorConfig(cfg *Config) {
	si := &cfg.UI.StatusIndicator
	if si.Duration == 0 && cfg.UI.StatusIndicatorDuration > 0 {
		si.Duration = cfg.UI.StatusIndicatorDuration
	}
	if si.OffsetX == 0 && cfg.UI.StatusIndicatorOffsetX != 0 {
		si.OffsetX = cfg.UI.StatusIndicatorOffsetX
	}
	if si.OffsetY == 0 && cfg.UI.StatusIndicatorOffsetY != 0 {
		si.OffsetY = cfg.UI.StatusIndicatorOffsetY
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
		// diff 失败时回退到全量保存
		fmt.Fprintf(os.Stderr, "[config] warning: diff failed, falling back to full save: %v\n", err)
		data, err := yaml.Marshal(cfg)
		if err != nil {
			return fmt.Errorf("failed to marshal config: %w", err)
		}
		return os.WriteFile(path, data, 0644)
	}

	data, err := yaml.Marshal(diff)
	if err != nil {
		return fmt.Errorf("failed to marshal config diff: %w", err)
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

	ApplyConfigFallbacks(cfg)
	return cfg
}

// SaveDefault saves the default configuration to file
func SaveDefault() error {
	return Save(DefaultConfig())
}
