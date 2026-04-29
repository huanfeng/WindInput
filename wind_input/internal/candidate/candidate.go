package candidate

// CandidateSource 候选词来源（混输模式下区分）
type CandidateSource string

const (
	SourceNone      CandidateSource = ""          // 未标记（单引擎模式）
	SourceCodetable CandidateSource = "codetable" // 来自码表引擎
	SourcePinyin    CandidateSource = "pinyin"    // 来自拼音引擎
	SourceEnglish   CandidateSource = "english"   // 来自英文词库
)

// Candidate 候选词
type Candidate struct {
	Text           string          // 候选文字
	Pinyin         string          // 拼音（兼容旧代码）
	Code           string          // 通用编码（五笔/拼音等）
	Weight         int             // 权重（用于排序）
	NaturalOrder   int             // 全局顺序（词库文件中的出现位置，跨编码递增，用于前缀匹配时保持文件原始排序）
	Comment        string          // 注释/提示信息（如反查时显示的编码）
	IsCommon       bool            // 是否为通用规范汉字
	IsPhrase       bool            // 是否为短语（PhraseLayer 提供，永远保留但不计入 hasCommon 传染）
	IsCommand      bool            // 是否为命令候选（uuid/date/time 等）
	ConsumedLength int             // 该候选消耗的输入长度（拼音部分上屏用）
	Source         CandidateSource // 候选来源（混输模式下区分五笔/拼音）
	PhraseTemplate string          // 动态短语的原始模板文本（如 "$Y-$MM-$DD"），用于定位 PhraseLayer 条目
	IsGroup        bool            // 是否为组候选（选中后展开二级列表而非上屏）
	GroupCode      string          // 组的完整编码（选中后替换 inputBuffer，如 "zzbd"）
	Index          int             // 显示序号（UI 渲染用，1-9/0）
	HasShadow      bool            // 是否存在 Shadow 层修改（UI 右键菜单"恢复默认"用）
	IndexLabel     string          // 自定义序号标签（如 "a"/"b"），非空时覆盖 Index 的数字显示
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
// 规则：权重降序；同权重按全局顺序升序（保持词库原始文件顺序，跨编码也生效）；
// 再按编码升序（兜底）；再按消耗长度降序；最后按文本升序（确保全序，消除排序不确定性）。
func Better(a, b Candidate) bool {
	if a.Weight != b.Weight {
		return a.Weight > b.Weight
	}
	if a.NaturalOrder != b.NaturalOrder {
		return a.NaturalOrder < b.NaturalOrder
	}
	if a.Code != b.Code {
		return a.Code < b.Code
	}
	if a.ConsumedLength != b.ConsumedLength {
		return a.ConsumedLength > b.ConsumedLength
	}
	return a.Text < b.Text
}

// BetterNatural 按自然顺序比较两个候选的优先级
// 规则：精确匹配（W≥0）始终优先于前缀候选（W<0，已施加降权惩罚）；
// 同 tier 内按自然顺序升序；同顺序按权重降序。
func BetterNatural(a, b Candidate) bool {
	aIsPrefix := a.Weight < 0
	bIsPrefix := b.Weight < 0
	if aIsPrefix != bIsPrefix {
		return !aIsPrefix
	}
	if a.NaturalOrder != b.NaturalOrder {
		return a.NaturalOrder < b.NaturalOrder
	}
	return Better(a, b)
}
