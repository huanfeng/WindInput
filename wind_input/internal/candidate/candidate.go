package candidate

// Candidate 候选词
type Candidate struct {
	Text           string // 候选文字
	Pinyin         string // 拼音（兼容旧代码）
	Code           string // 通用编码（五笔/拼音等）
	Weight         int    // 权重（用于排序）
	Hint           string // 提示信息（如反查时显示的编码）
	IsCommon       bool   // 是否为通用规范汉字
	IsCommand      bool   // 是否为命令候选（uuid/date/time 等）
	ConsumedLength int    // 该候选消耗的输入长度（拼音部分上屏用）
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

// Better 比较两个候选的优先级（返回 a 是否应排在 b 前）
// 规则：权重降序；同权重按文本升序；再按编码升序；最后按消耗长度降序。
func Better(a, b Candidate) bool {
	if a.Weight != b.Weight {
		return a.Weight > b.Weight
	}
	if a.Text != b.Text {
		return a.Text < b.Text
	}
	if a.Code != b.Code {
		return a.Code < b.Code
	}
	return a.ConsumedLength > b.ConsumedLength
}
