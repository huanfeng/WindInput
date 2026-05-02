// Package config: enums.go 定义配置中以字符串字面量形式存在的"行为模式"枚举类型。
//
// 设计原则：
//   - 使用 `type Foo string` + const 块，而非 iota 整数枚举，因这些值会进 YAML 配置。
//   - YAML/JSON tag 保持不变，序列化值与原字符串完全一致。
//   - 每种类型提供 `Valid() bool` 方法，便于配置加载/迁移时的校验。
package config

// EnterBehavior 回车键行为
type EnterBehavior string

const (
	EnterCommit         EnterBehavior = "commit"           // 上屏编码
	EnterClear          EnterBehavior = "clear"            // 清空编码
	EnterCommitAndInput EnterBehavior = "commit_and_input" // 上屏并继续输入
	EnterIgnore         EnterBehavior = "ignore"           // 忽略
)

// Valid 校验取值是否在合法集合内
func (b EnterBehavior) Valid() bool {
	switch b {
	case EnterCommit, EnterClear, EnterCommitAndInput, EnterIgnore:
		return true
	}
	return false
}

// SpaceOnEmptyBehavior 空码时空格键行为
type SpaceOnEmptyBehavior string

const (
	SpaceOnEmptyCommit         SpaceOnEmptyBehavior = "commit"
	SpaceOnEmptyClear          SpaceOnEmptyBehavior = "clear"
	SpaceOnEmptyCommitAndInput SpaceOnEmptyBehavior = "commit_and_input"
	SpaceOnEmptyIgnore         SpaceOnEmptyBehavior = "ignore"
)

// Valid 校验取值
func (b SpaceOnEmptyBehavior) Valid() bool {
	switch b {
	case SpaceOnEmptyCommit, SpaceOnEmptyClear, SpaceOnEmptyCommitAndInput, SpaceOnEmptyIgnore:
		return true
	}
	return false
}

// OverflowBehavior 候选按键无效时的处理策略（数字键/二三候选键/以词定字键共用）
type OverflowBehavior string

const (
	OverflowIgnore         OverflowBehavior = "ignore"
	OverflowCommit         OverflowBehavior = "commit"
	OverflowCommitAndInput OverflowBehavior = "commit_and_input"
)

// Valid 校验取值
func (b OverflowBehavior) Valid() bool {
	switch b {
	case OverflowIgnore, OverflowCommit, OverflowCommitAndInput:
		return true
	}
	return false
}

// FilterMode 候选过滤模式
type FilterMode string

const (
	FilterSmart   FilterMode = "smart"   // 智能（默认）
	FilterGeneral FilterMode = "general" // 仅常用字
	FilterGB18030 FilterMode = "gb18030" // 不限制
)

// Valid 校验取值
func (m FilterMode) Valid() bool {
	switch m {
	case FilterSmart, FilterGeneral, FilterGB18030:
		return true
	}
	return false
}

// ThemeStyle 主题风格
type ThemeStyle string

const (
	ThemeStyleSystem ThemeStyle = "system" // 跟随系统
	ThemeStyleLight  ThemeStyle = "light"  // 亮色
	ThemeStyleDark   ThemeStyle = "dark"   // 暗色
)

// Valid 校验取值
func (s ThemeStyle) Valid() bool {
	switch s {
	case ThemeStyleSystem, ThemeStyleLight, ThemeStyleDark:
		return true
	}
	return false
}

// CandidateLayout 候选布局
type CandidateLayout string

const (
	LayoutHorizontal CandidateLayout = "horizontal"
	LayoutVertical   CandidateLayout = "vertical"
)

// Valid 校验取值
func (l CandidateLayout) Valid() bool {
	switch l {
	case LayoutHorizontal, LayoutVertical:
		return true
	}
	return false
}

// PreeditMode 编码显示模式
type PreeditMode string

const (
	PreeditTop      PreeditMode = "top"      // 编码在上方独立行（默认）
	PreeditEmbedded PreeditMode = "embedded" // 嵌入候选行前
)

// Valid 校验取值
func (m PreeditMode) Valid() bool {
	switch m {
	case PreeditTop, PreeditEmbedded:
		return true
	}
	return false
}

// PinyinSeparatorMode 拼音分隔符模式
type PinyinSeparatorMode string

const (
	PinyinSeparatorAuto     PinyinSeparatorMode = "auto"
	PinyinSeparatorQuote    PinyinSeparatorMode = "quote"
	PinyinSeparatorBacktick PinyinSeparatorMode = "backtick"
	PinyinSeparatorNone     PinyinSeparatorMode = "none"
)

// Valid 校验取值
func (m PinyinSeparatorMode) Valid() bool {
	switch m {
	case PinyinSeparatorAuto, PinyinSeparatorQuote, PinyinSeparatorBacktick, PinyinSeparatorNone:
		return true
	}
	return false
}

// FontEngine 文本渲染引擎（对应 UIConfig.TextRenderMode 字段）
type FontEngine string

const (
	FontEngineDirectWrite FontEngine = "directwrite" // DirectWrite + Direct2D（默认）
	FontEngineGDI         FontEngine = "gdi"         // Windows GDI 原生
	FontEngineFreetype    FontEngine = "freetype"    // FreeType
)

// Valid 校验取值
func (e FontEngine) Valid() bool {
	switch e {
	case FontEngineDirectWrite, FontEngineGDI, FontEngineFreetype:
		return true
	}
	return false
}

// PagerDisplayMode 页码显示方式（用户级覆盖，优先级高于主题配置）
type PagerDisplayMode string

const (
	PagerDisplayDefault PagerDisplayMode = ""       // 使用主题配置（默认）
	PagerDisplayNever   PagerDisplayMode = "never"  // 不显示页码文字
	PagerDisplayAuto    PagerDisplayMode = "auto"   // 仅在多页时显示
	PagerDisplayAlways  PagerDisplayMode = "always" // 总是显示
)

// Valid 校验取值
func (m PagerDisplayMode) Valid() bool {
	switch m {
	case PagerDisplayDefault, PagerDisplayNever, PagerDisplayAuto, PagerDisplayAlways:
		return true
	}
	return false
}
