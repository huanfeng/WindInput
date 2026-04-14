package dict

// FreqScorer 词频评分器接口
// 在 CompositeDict 排序前调用，为候选词附加词频加成
type FreqScorer interface {
	// FreqBoost 返回指定候选词的词频加成分数
	// 返回 0 表示无加成
	FreqBoost(code, text string) int
}
