package pinyin

import "github.com/huanfeng/wind_input/internal/candidate"

// ============================================================
// 音节类型与解析结果
// ============================================================

// SyllableType 音节类型
type SyllableType int

const (
	// SyllableExact 完整音节，如 "ni", "hao", "zhong"
	SyllableExact SyllableType = iota
	// SyllablePartial 未完成音节（前缀），如 "zh", "sh", "n"
	SyllablePartial
)

// String 返回音节类型的字符串表示
func (t SyllableType) String() string {
	switch t {
	case SyllableExact:
		return "exact"
	case SyllablePartial:
		return "partial"
	default:
		return "unknown"
	}
}

// ParsedSyllable 解析后的单个音节
type ParsedSyllable struct {
	Text     string       // 音节文本，如 "ni", "zh"
	Type     SyllableType // 音节类型
	Start    int          // 在原始输入中的起始位置
	End      int          // 结束位置（不包含）
	Possible []string     // 如果是 Partial 类型，可能的完整音节后缀
}

// IsExact 是否为完整音节
func (s *ParsedSyllable) IsExact() bool {
	return s.Type == SyllableExact
}

// IsPartial 是否为未完成音节
func (s *ParsedSyllable) IsPartial() bool {
	return s.Type == SyllablePartial
}

// ParseResult 音节解析结果
type ParseResult struct {
	Input     string           // 原始输入，如 "nihaozh"
	Syllables []ParsedSyllable // 解析出的音节列表
}

// CompletedSyllables 返回所有已完成的音节文本
func (r *ParseResult) CompletedSyllables() []string {
	var result []string
	for _, s := range r.Syllables {
		if s.IsExact() {
			result = append(result, s.Text)
		}
	}
	return result
}

// HasPartial 是否包含未完成的音节
func (r *ParseResult) HasPartial() bool {
	if len(r.Syllables) == 0 {
		return false
	}
	return r.Syllables[len(r.Syllables)-1].IsPartial()
}

// PartialSyllable 返回未完成的音节（如果有）
func (r *ParseResult) PartialSyllable() string {
	if !r.HasPartial() {
		return ""
	}
	return r.Syllables[len(r.Syllables)-1].Text
}

// LastSyllable 返回最后一个音节
func (r *ParseResult) LastSyllable() *ParsedSyllable {
	if len(r.Syllables) == 0 {
		return nil
	}
	return &r.Syllables[len(r.Syllables)-1]
}

// IsComplete 输入是否被完全解析为完整音节
func (r *ParseResult) IsComplete() bool {
	if len(r.Syllables) == 0 {
		return false
	}
	// 检查所有音节是否都是完整的
	for _, s := range r.Syllables {
		if s.IsPartial() {
			return false
		}
	}
	// 检查是否覆盖了整个输入
	if len(r.Syllables) > 0 {
		last := r.Syllables[len(r.Syllables)-1]
		return last.End == len(r.Input)
	}
	return false
}

// SyllableTexts 返回所有音节的文本（包括未完成的）
func (r *ParseResult) SyllableTexts() []string {
	result := make([]string, len(r.Syllables))
	for i, s := range r.Syllables {
		result[i] = s.Text
	}
	return result
}

// ============================================================
// 词库条目
// ============================================================

// EntrySource 词库条目来源
type EntrySource int

const (
	// SourceSystem 系统词库
	SourceSystem EntrySource = iota
	// SourceUser 用户词库
	SourceUser
	// SourcePhrase 短语词库
	SourcePhrase
)

// String 返回来源的字符串表示
func (s EntrySource) String() string {
	switch s {
	case SourceSystem:
		return "system"
	case SourceUser:
		return "user"
	case SourcePhrase:
		return "phrase"
	default:
		return "unknown"
	}
}

// LexiconEntry 词库条目
type LexiconEntry struct {
	Text      string      // 文字，如 "你好"
	Syllables []string    // 音节序列，如 ["ni", "hao"]
	Freq      int         // 词频
	Source    EntrySource // 来源
}

// ============================================================
// 匹配类型
// ============================================================

// MatchType 候选匹配类型
type MatchType int

const (
	// MatchExact 精确匹配（音节完全对应）
	MatchExact MatchType = iota
	// MatchPartial 前缀匹配（最后一个音节为前缀）
	MatchPartial
	// MatchFuzzy 模糊匹配
	MatchFuzzy
)

// String 返回匹配类型的字符串表示
func (t MatchType) String() string {
	switch t {
	case MatchExact:
		return "exact"
	case MatchPartial:
		return "partial"
	case MatchFuzzy:
		return "fuzzy"
	default:
		return "unknown"
	}
}

// ============================================================
// 候选评分特征
// ============================================================

// CandidateFeatures 候选评分特征
//
// MatchType vs IsFuzzy 语义区分：
//   - MatchType 描述「结构匹配质量」：Exact（音节完全对应）、Partial（最后音节为前缀）、Fuzzy（仅通过模糊音规则命中）
//   - IsFuzzy 是一个「来源标记」：该候选是否经由模糊拼音扩展（zh↔z 等）召回
//   - 两者可以组合：MatchExact + IsFuzzy=true 表示「通过模糊音找到，但音节数完全对齐」
//   - Scorer 中 MatchType 决定基础分层，IsFuzzy 施加额外惩罚
type CandidateFeatures struct {
	MatchType     MatchType // 结构匹配质量：Exact/Partial/Fuzzy
	SyllableMatch bool      // 字数是否等于音节数
	CharCount     int       // 字符数
	SyllableCount int       // 对应音节数
	IsUserWord    bool      // 是否用户词
	IsFuzzy       bool      // 是否经模糊拼音扩展召回
	IsPartial     bool      // 是否 partial 匹配（末尾音节未完成）
	IsAbbrev      bool      // 是否简拼匹配
	IsViterbi     bool      // 是否 Viterbi 组句结果
	IsCommand     bool      // 是否命令
	LMScore       float64   // 语言模型分数 (log prob)
	BigramScore   float64   // Bigram 上下文分数（Phase 3 接通后生效）
	FreqScore     float64   // 词频分数
	SegmentRank   int       // 切分路径排名（0=主路径）
}

// ============================================================
// 转换结果
// ============================================================

// PinyinConvertResult 拼音转换结果
type PinyinConvertResult struct {
	// 候选词列表
	Candidates []candidate.Candidate

	// 组合状态（输入态信息）
	Composition *CompositionState

	// 预编辑区显示文本，如 "ni'hao'zh"
	PreeditDisplay string

	// 已确定的文本（可自动上屏的部分，拼音通常为空）
	CompletedText string

	// 状态标记
	HasMore    bool // 是否还有更多候选
	IsEmpty    bool // 是否空码（无候选）
	NeedRefine bool // 是否需要用户继续输入以细化
}
