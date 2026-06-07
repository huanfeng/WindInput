package theme

// Behavior 主题行为配置 YAML schema（P6）。全部为可空指针，nil=未指定、走 defaultBehavior。
// 这是"可被用户覆盖"字段的白名单：出现在此结构的字段才允许用户 override（用户 override 层见阶段 3）。
type Behavior struct {
	FontSize         *int  `yaml:"font_size,omitempty" json:"font_size,omitempty"`
	AlwaysShowPager  *bool `yaml:"always_show_pager,omitempty" json:"always_show_pager,omitempty"`
	ShowPageNumber   *bool `yaml:"show_page_number,omitempty" json:"show_page_number,omitempty"`
	HidePager        *bool `yaml:"hide_pager,omitempty" json:"hide_pager,omitempty"`
	VerticalMaxWidth *int  `yaml:"vertical_max_width,omitempty" json:"vertical_max_width,omitempty"`
}

// ResolvedBehavior 解析后的行为配置（所有字段已填具体值）。
type ResolvedBehavior struct {
	FontSize         int
	AlwaysShowPager  bool
	ShowPageNumber   bool
	HidePager        bool
	VerticalMaxWidth int
}

// defaultBehavior 引擎内置行为基线。值与重构前 hardcode 现状一致（零回归）：
// 字号 18（= 旧 baseFontSize 默认）、单页不显翻页区、显示页码、竖排最大宽 600。
func defaultBehavior() ResolvedBehavior {
	return ResolvedBehavior{
		FontSize:         18,
		AlwaysShowPager:  false,
		ShowPageNumber:   true,
		VerticalMaxWidth: 600,
	}
}

// mergeBehavior 用主题 behavior（非 nil 字段）覆盖基线，返回解析后行为。ov 为 nil 时原样返回基线。
// 注：字段范围校验（如 font_size>0、vertical_max_width 合理上限）不在此层，留待阶段3 用户 override 接入时统一处理。
func mergeBehavior(base ResolvedBehavior, ov *Behavior) ResolvedBehavior {
	if ov == nil {
		return base
	}
	out := base
	if ov.FontSize != nil {
		out.FontSize = *ov.FontSize
	}
	if ov.AlwaysShowPager != nil {
		out.AlwaysShowPager = *ov.AlwaysShowPager
	}
	if ov.ShowPageNumber != nil {
		out.ShowPageNumber = *ov.ShowPageNumber
	}
	if ov.HidePager != nil {
		out.HidePager = *ov.HidePager
	}
	if ov.VerticalMaxWidth != nil {
		out.VerticalMaxWidth = *ov.VerticalMaxWidth
	}
	return out
}
