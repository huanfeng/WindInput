package candidate

// CandidateSource 候选词来源（混输模式下区分）
type CandidateSource string

const (
	SourceNone   CandidateSource = ""       // 未标记（单引擎模式）
	SourceWubi   CandidateSource = "wubi"   // 来自五笔引擎
	SourcePinyin CandidateSource = "pinyin" // 来自拼音引擎
)

// Candidate 候选词
type Candidate struct {
	Text           string          // 候选文字
	Pinyin         string          // 拼音（兼容旧代码）
	Code           string          // 通用编码（五笔/拼音等）
	Weight         int             // 权重（用于排序）
	NaturalOrder   int             // 自然顺序（词库中同一编码下的原始位置，0-based）
	Hint           string          // 提示信息（如反查时显示的编码）
	IsCommon       bool            // 是否为通用规范汉字
	IsCommand      bool            // 是否为命令候选（uuid/date/time 等）
	ConsumedLength int             // 该候选消耗的输入长度（拼音部分上屏用）
	Source         CandidateSource // 候选来源（混输模式下区分五笔/拼音）
	PhraseTemplate string          // 动态短语的原始模板文本（如 "$Y-$MM-$DD"），用于定位 PhraseLayer 条目
}

// CandidateList 候选词列表
type CandidateList []Candidate

// Len 返回候选词数量
func (c CandidateList) Len() int {
	return len(c)
}

// Less 比较候选词（按权重降序）
func (c CandidateList) Less(i, j int) bool {
	return Better(c[i], c[j])
}

// Swap 交换候选词
func (c CandidateList) Swap(i, j int) {
	c[i], c[j] = c[j], c[i]
}

// CandidateSortMode 候选排序模式
type CandidateSortMode string

const (
	SortByFrequency CandidateSortMode = "frequency" // 按词频排序（默认）
	SortByNatural   CandidateSortMode = "natural"   // 按词库自然顺序排序
)

// Better 比较两个候选的优先级（返回 a 是否应排在 b 前）
// 规则：权重降序；同权重按编码升序（保持词库自然顺序）；再按文本升序；最后按消耗长度降序。
func Better(a, b Candidate) bool {
	if a.Weight != b.Weight {
		return a.Weight > b.Weight
	}
	if a.Code != b.Code {
		return a.Code < b.Code
	}
	if a.Text != b.Text {
		return a.Text < b.Text
	}
	return a.ConsumedLength > b.ConsumedLength
}

// BetterNatural 按自然顺序比较两个候选的优先级
// 规则：自然顺序升序（靠前的排前面）；同顺序按权重降序。
func BetterNatural(a, b Candidate) bool {
	if a.NaturalOrder != b.NaturalOrder {
		return a.NaturalOrder < b.NaturalOrder
	}
	return Better(a, b)
}
