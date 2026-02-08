package pinyin

import "strings"

// ============================================================
// 组合态（Composition State）
// 表示拼音输入过程中的中间状态
// ============================================================

// HighlightType 预编辑区高亮类型
type HighlightType int

const (
	// HighlightCompleted 已完成音节
	HighlightCompleted HighlightType = iota
	// HighlightPartial 未完成音节
	HighlightPartial
	// HighlightSeparator 分隔符
	HighlightSeparator
)

// String 返回高亮类型的字符串表示
func (t HighlightType) String() string {
	switch t {
	case HighlightCompleted:
		return "completed"
	case HighlightPartial:
		return "partial"
	case HighlightSeparator:
		return "separator"
	default:
		return "unknown"
	}
}

// PreeditHighlight 预编辑区高亮区域
type PreeditHighlight struct {
	Start int           // 起始位置（相对于 PreeditText）
	End   int           // 结束位置
	Type  HighlightType // 高亮类型
}

// CompositionState 输入组合状态
// 表示用户正在输入过程中的状态信息
type CompositionState struct {
	// 已完成的音节列表
	// 例如：输入 "nihaozh" 时为 ["ni", "hao"]
	CompletedSyllables []string

	// 未完成的音节（正在输入）
	// 例如：输入 "nihaozh" 时为 "zh"
	PartialSyllable string

	// 可能的续写选项（基于未完成音节）
	// 例如：输入 "zh" 时为 ["a", "ai", "an", "ang", "ao", "e", "ei", ...]
	PossibleContinues []string

	// 预编辑区显示文本
	// 例如："ni'hao'zh" 或 "ni hao zh_"
	PreeditText string

	// 光标在预编辑文本中的位置
	PreeditCursor int

	// 高亮区域列表（用于 UI 显示不同样式）
	Highlights []PreeditHighlight
}

// HasPartial 是否有未完成的音节
func (c *CompositionState) HasPartial() bool {
	return c.PartialSyllable != ""
}

// IsEmpty 组合态是否为空
func (c *CompositionState) IsEmpty() bool {
	return len(c.CompletedSyllables) == 0 && c.PartialSyllable == ""
}

// AllSyllables 返回所有音节（包括未完成的）
func (c *CompositionState) AllSyllables() []string {
	result := make([]string, 0, len(c.CompletedSyllables)+1)
	result = append(result, c.CompletedSyllables...)
	if c.PartialSyllable != "" {
		result = append(result, c.PartialSyllable)
	}
	return result
}

// TotalSyllableCount 返回音节总数
func (c *CompositionState) TotalSyllableCount() int {
	count := len(c.CompletedSyllables)
	if c.PartialSyllable != "" {
		count++
	}
	return count
}

// ============================================================
// CompositionBuilder 组合态构建器
// ============================================================

// CompositionBuilder 用于构建 CompositionState
type CompositionBuilder struct {
	separator string // 音节分隔符，默认 "'"
}

// NewCompositionBuilder 创建组合态构建器
func NewCompositionBuilder() *CompositionBuilder {
	return &CompositionBuilder{
		separator: "'",
	}
}

// SetSeparator 设置音节分隔符
func (b *CompositionBuilder) SetSeparator(sep string) *CompositionBuilder {
	b.separator = sep
	return b
}

// Build 从解析结果构建组合态
func (b *CompositionBuilder) Build(parsed *ParseResult) *CompositionState {
	if parsed == nil || len(parsed.Syllables) == 0 {
		return &CompositionState{}
	}

	comp := &CompositionState{
		CompletedSyllables: make([]string, 0),
	}

	// 分离完整音节和未完成音节
	// 非末尾的 partial 音节视为"已确认的段"加入 CompletedSyllables，
	// 仅最后一个 partial 保留为 PartialSyllable
	//
	// 注意：composition 的 CompletedSyllables 包含提升后的 partial（用于 UI 显示），
	// 而 convertCore 使用 parsed.CompletedSyllables()（仅 Exact 音节）做查询逻辑，
	// 两者用途不同，不可混淆。
	for i, syllable := range parsed.Syllables {
		if syllable.IsExact() {
			comp.CompletedSyllables = append(comp.CompletedSyllables, syllable.Text)
		} else if syllable.IsPartial() {
			if i < len(parsed.Syllables)-1 {
				// 非末尾的 partial：加入已完成列表
				comp.CompletedSyllables = append(comp.CompletedSyllables, syllable.Text)
			} else {
				// 末尾的 partial：保留为未完成音节
				comp.PartialSyllable = syllable.Text
				comp.PossibleContinues = syllable.Possible
			}
		}
	}

	// 构建预编辑文本和高亮
	comp.PreeditText, comp.Highlights = b.buildPreedit(comp)
	comp.PreeditCursor = len(comp.PreeditText)

	return comp
}

// buildPreedit 构建预编辑文本和高亮区域
func (b *CompositionBuilder) buildPreedit(comp *CompositionState) (string, []PreeditHighlight) {
	var builder strings.Builder
	var highlights []PreeditHighlight
	pos := 0

	// 添加已完成的音节
	for i, syllable := range comp.CompletedSyllables {
		if i > 0 {
			// 添加分隔符
			highlights = append(highlights, PreeditHighlight{
				Start: pos,
				End:   pos + len(b.separator),
				Type:  HighlightSeparator,
			})
			builder.WriteString(b.separator)
			pos += len(b.separator)
		}

		// 添加音节
		highlights = append(highlights, PreeditHighlight{
			Start: pos,
			End:   pos + len(syllable),
			Type:  HighlightCompleted,
		})
		builder.WriteString(syllable)
		pos += len(syllable)
	}

	// 添加未完成的音节
	if comp.PartialSyllable != "" {
		if len(comp.CompletedSyllables) > 0 {
			// 添加分隔符
			highlights = append(highlights, PreeditHighlight{
				Start: pos,
				End:   pos + len(b.separator),
				Type:  HighlightSeparator,
			})
			builder.WriteString(b.separator)
			pos += len(b.separator)
		}

		// 添加未完成音节（不同样式）
		highlights = append(highlights, PreeditHighlight{
			Start: pos,
			End:   pos + len(comp.PartialSyllable),
			Type:  HighlightPartial,
		})
		builder.WriteString(comp.PartialSyllable)
	}

	return builder.String(), highlights
}

// BuildFromSyllables 从音节列表直接构建组合态
// completedSyllables: 已完成的音节
// partialSyllable: 未完成的音节
// possibleContinues: 可能的续写
func (b *CompositionBuilder) BuildFromSyllables(
	completedSyllables []string,
	partialSyllable string,
	possibleContinues []string,
) *CompositionState {
	comp := &CompositionState{
		CompletedSyllables: completedSyllables,
		PartialSyllable:    partialSyllable,
		PossibleContinues:  possibleContinues,
	}

	comp.PreeditText, comp.Highlights = b.buildPreedit(comp)
	comp.PreeditCursor = len(comp.PreeditText)

	return comp
}
