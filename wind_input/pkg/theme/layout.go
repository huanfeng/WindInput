package theme

import "strings"

// BuildIndexLabelsFromSlots 把序号槽位 []string 拼成 "/" 分隔串（候选窗 IndexLabels）。
// 槽位 0→候选序号 1、…、槽位 9→第 10 个候选；不足 10 或空槽回退默认数字（1..9,0）。
// 约束：单个标签不应含 '/'（渲染器以 '/' 切分槽位），此处不做转义。
func BuildIndexLabelsFromSlots(labels []string) string {
	digits := []string{"1", "2", "3", "4", "5", "6", "7", "8", "9", "0"}
	parts := make([]string, 10)
	for i := range 10 {
		if i < len(labels) && labels[i] != "" {
			parts[i] = labels[i]
		} else {
			parts[i] = digits[i]
		}
	}
	return strings.Join(parts, "/")
}

// LayoutSchema 描述主题的尺寸与布局，与 v2.5 spec §四 的 yaml 字段 1:1 对应。
// 不含任何颜色；颜色由 PaletteSchema 描述。
//
// 这是 *原始解析层*（Raw 层）：距离/圆角/间隙/边框 类字段用 `*int` 表示，
// nil 表示"用户未在 yaml 中写该字段"（回退 density 基线），非 nil（含 0）表示"用户显式给值"。
// 经 mergeWithDensityBaseline 合并后产出 plain int 的 ResolvedLayout。
type LayoutSchema struct {
	Meta    LayoutMeta `yaml:"meta" json:"meta"`
	Density string     `yaml:"density" json:"density"` // compact | cozy | comfortable
	Scale   float64    `yaml:"scale" json:"scale"`     // 整体缩放，DPI 适配后再乘

	CandidateWindow RawCandidateWindowLayout `yaml:"candidate_window" json:"candidate_window"`
	Toolbar         RawToolbarLayout         `yaml:"toolbar" json:"toolbar"`
	Status          RawStatusLayout          `yaml:"status" json:"status"`
	Tooltip         RawTooltipLayout         `yaml:"tooltip" json:"tooltip"`
	PopupMenu       RawPopupMenuLayout       `yaml:"popup_menu" json:"popup_menu"`
	Toast           RawToastLayout           `yaml:"toast" json:"toast"` // 预留，本期不消费
}

// LayoutMeta 标识一个共享 layout 零件
type LayoutMeta struct {
	Name    string `yaml:"name" json:"name"`
	Version string `yaml:"version" json:"version"`
}

// intPtr 返回 v 的指针，用于构造 Raw* 结构体字面量与测试。
func intPtr(v int) *int { return &v }

// ---- Raw 层（原始解析）：距离/圆角/间隙/边框 用 *int 支持"显式 0" ----

// RawPadding 通用四边内边距（原始解析层，nil=未写）
type RawPadding struct {
	Top    *int `yaml:"top" json:"top"`
	Right  *int `yaml:"right" json:"right"`
	Bottom *int `yaml:"bottom" json:"bottom"`
	Left   *int `yaml:"left" json:"left"`
}

// RawCandidateWindowLayout 候选窗口的所有 band 布局（原始解析层）
type RawCandidateWindowLayout struct {
	WindowPadding RawPadding `yaml:"window_padding" json:"window_padding"`
	BandGap       *int       `yaml:"band_gap" json:"band_gap"`
	BorderWidth   *int       `yaml:"border_width" json:"border_width"`
	BorderRadius  *int       `yaml:"border_radius" json:"border_radius"`

	PreeditBar    RawBandLayout          `yaml:"preedit_bar" json:"preedit_bar"`
	CandidateList RawCandidateListLayout `yaml:"candidate_list" json:"candidate_list"`
	FooterBar     RawBandLayout          `yaml:"footer_bar" json:"footer_bar"`
}

// RawBandLayout 通用 band 布局（原始解析层）
type RawBandLayout struct {
	Visible  bool       `yaml:"visible" json:"visible"`
	Padding  RawPadding `yaml:"padding" json:"padding"`
	FontSize int        `yaml:"font_size" json:"font_size"`
}

// RawCandidateListLayout 候选列表 band（原始解析层）
type RawCandidateListLayout struct {
	ItemPadding RawPadding         `yaml:"item_padding" json:"item_padding"`
	ItemGap     *int               `yaml:"item_gap" json:"item_gap"`
	ItemHeight  int                `yaml:"item_height" json:"item_height"` // 0 = 自适应
	ItemRadius  *int               `yaml:"item_radius" json:"item_radius"`
	Index       RawIndexLayout     `yaml:"index" json:"index"`
	Text        TextLayout         `yaml:"text" json:"text"`
	Comment     RawCommentLayout   `yaml:"comment" json:"comment"`
	AccentBar   RawAccentBarLayout `yaml:"accent_bar" json:"accent_bar"`
}

// RawAccentBarLayout 选中候选左侧强调条（原始解析层）
type RawAccentBarLayout struct {
	Enabled bool `yaml:"enabled" json:"enabled"`
	Width   *int `yaml:"width" json:"width"`
}

// RawIndexLayout 序号样式（原始解析层）
type RawIndexLayout struct {
	Labels   []string `yaml:"labels,omitempty" json:"labels,omitempty"` // 序号显示的唯一来源：10 个槽位字符串
	Circle   bool     `yaml:"circle" json:"circle"`                     // 是否绘制圆形背景，与 labels 内容正交
	Gap      *int     `yaml:"gap" json:"gap"`
	MinWidth *int     `yaml:"min_width" json:"min_width"`
	FontSize int      `yaml:"font_size" json:"font_size"`
}

// RawCommentLayout 候选注释（原始解析层）
type RawCommentLayout struct {
	Visible   bool   `yaml:"visible" json:"visible"`
	FontSize  int    `yaml:"font_size" json:"font_size"`
	Gap       *int   `yaml:"gap" json:"gap"`
	Placement string `yaml:"placement" json:"placement"`
}

// RawToolbarLayout 工具栏布局（原始解析层）
type RawToolbarLayout struct {
	Height  int        `yaml:"height" json:"height"`
	Padding RawPadding `yaml:"padding" json:"padding"`
	ItemGap *int       `yaml:"item_gap" json:"item_gap"`
}

// RawStatusLayout 状态提示（原始解析层）
type RawStatusLayout struct {
	BorderWidth *int       `yaml:"border_width" json:"border_width"`
	Padding     RawPadding `yaml:"padding" json:"padding"`
}

// RawTooltipLayout 候选提示（原始解析层）
type RawTooltipLayout struct {
	Padding      RawPadding `yaml:"padding" json:"padding"`
	FontSize     int        `yaml:"font_size" json:"font_size"`
	BorderRadius *int       `yaml:"border_radius" json:"border_radius"`
	MaxWidth     int        `yaml:"max_width" json:"max_width"`
}

// RawPopupMenuLayout 弹出菜单（原始解析层）
type RawPopupMenuLayout struct {
	ItemPadding     RawPadding `yaml:"item_padding" json:"item_padding"`
	ItemHeight      int        `yaml:"item_height" json:"item_height"`
	SeparatorHeight *int       `yaml:"separator_height" json:"separator_height"`
}

// RawToastLayout Toast 通知（原始解析层），本期预留不消费
type RawToastLayout struct {
	Padding      RawPadding `yaml:"padding" json:"padding"`
	FontSize     int        `yaml:"font_size" json:"font_size"`
	BorderRadius *int       `yaml:"border_radius" json:"border_radius"`
	MaxWidth     int        `yaml:"max_width" json:"max_width"`
}

// ---- Plain 层（供 Resolved 复用，标量类型保持 plain int）----

// Padding 通用四边内边距
type Padding struct {
	Top    int `yaml:"top" json:"top"`
	Right  int `yaml:"right" json:"right"`
	Bottom int `yaml:"bottom" json:"bottom"`
	Left   int `yaml:"left" json:"left"`
}

// BandLayout 通用 band（preedit / code / footer）布局
type BandLayout struct {
	Visible  bool    `yaml:"visible" json:"visible"`
	Padding  Padding `yaml:"padding" json:"padding"`
	FontSize int     `yaml:"font_size" json:"font_size"`
}

// CandidateListLayout 候选列表 band（特殊化的 BandLayout）
type CandidateListLayout struct {
	ItemPadding Padding         `yaml:"item_padding" json:"item_padding"`
	ItemGap     int             `yaml:"item_gap" json:"item_gap"`
	ItemHeight  int             `yaml:"item_height" json:"item_height"` // 0 = 自适应
	ItemRadius  int             `yaml:"item_radius" json:"item_radius"`
	Index       IndexLayout     `yaml:"index" json:"index"`
	Text        TextLayout      `yaml:"text" json:"text"`
	Comment     CommentLayout   `yaml:"comment" json:"comment"`
	AccentBar   AccentBarLayout `yaml:"accent_bar" json:"accent_bar"`
}

// AccentBarLayout 选中候选左侧强调条（msime 风格）。enabled=false 时不绘制。
type AccentBarLayout struct {
	Enabled bool `yaml:"enabled" json:"enabled"`
	Width   int  `yaml:"width" json:"width"`
}

// IndexLayout 序号样式
//
// Labels：序号显示的唯一来源（≤10 项），槽位 0→候选序号 1、…、槽位 9→第 10 个候选（index 0）。
// 不足 10 项或某槽为空串的位置回退默认数字（1..9,0）。约束：单个标签不应含 '/'（渲染器以 '/' 切分槽位）。
// Circle：是否绘制圆形背景，与 Labels 内容正交（圆里放数字 / emoji / 任意字符均可）。
type IndexLayout struct {
	Labels   []string `yaml:"labels,omitempty" json:"labels,omitempty"`
	Circle   bool     `yaml:"circle" json:"circle"`
	Gap      int      `yaml:"gap" json:"gap"`
	MinWidth int      `yaml:"min_width" json:"min_width"`
	FontSize int      `yaml:"font_size" json:"font_size"`
}

// TextLayout 候选文本
type TextLayout struct {
	FontSize int `yaml:"font_size" json:"font_size"`
}

// CommentLayout 候选注释
type CommentLayout struct {
	Visible   bool   `yaml:"visible" json:"visible"`
	FontSize  int    `yaml:"font_size" json:"font_size"`
	Gap       int    `yaml:"gap" json:"gap"`
	Placement string `yaml:"placement" json:"placement"` // inline | below
}

// ToolbarLayout 工具栏布局
type ToolbarLayout struct {
	Height  int     `yaml:"height" json:"height"`
	Padding Padding `yaml:"padding" json:"padding"`
	ItemGap int     `yaml:"item_gap" json:"item_gap"`
}

// StatusLayout 状态提示（temp_pinyin 等）
type StatusLayout struct {
	BorderWidth int     `yaml:"border_width" json:"border_width"`
	Padding     Padding `yaml:"padding" json:"padding"`
}

// TooltipLayout 候选提示
type TooltipLayout struct {
	Padding      Padding `yaml:"padding" json:"padding"`
	FontSize     int     `yaml:"font_size" json:"font_size"`
	BorderRadius int     `yaml:"border_radius" json:"border_radius"`
	MaxWidth     int     `yaml:"max_width" json:"max_width"`
}

// PopupMenuLayout 弹出菜单
type PopupMenuLayout struct {
	ItemPadding     Padding `yaml:"item_padding" json:"item_padding"`
	ItemHeight      int     `yaml:"item_height" json:"item_height"`
	SeparatorHeight int     `yaml:"separator_height" json:"separator_height"`
}

// ToastLayout Toast 通知，本期预留不消费
type ToastLayout struct {
	Padding      Padding `yaml:"padding" json:"padding"`
	FontSize     int     `yaml:"font_size" json:"font_size"`
	BorderRadius int     `yaml:"border_radius" json:"border_radius"`
	MaxWidth     int     `yaml:"max_width" json:"max_width"`
}
