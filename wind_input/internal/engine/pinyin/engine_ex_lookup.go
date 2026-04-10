// engine_ex_lookup.go — 候选排序、词组查找、输入解析
package pinyin

import (
	"sort"
	"strings"

	"github.com/huanfeng/wind_input/internal/candidate"
)

// sortCandidates 根据排序模式对候选进行排序
func (e *Engine) sortCandidates(candidates []candidate.Candidate, order string, syllableCount int) {
	switch order {
	case "phrase_first":
		// 词组优先：词组排在单字前面（在同级别内按权重排序）
		sort.SliceStable(candidates, func(i, j int) bool {
			iLen := len([]rune(candidates[i].Text))
			jLen := len([]rune(candidates[j].Text))
			iIsPhrase := iLen > 1
			jIsPhrase := jLen > 1
			if iIsPhrase != jIsPhrase {
				return iIsPhrase // 词组排前面
			}
			return candidate.Better(candidates[i], candidates[j])
		})
	case "smart":
		// 智能混排：Scorer 已统一处理 LM 分数，直接按权重排序
		sort.SliceStable(candidates, func(i, j int) bool {
			return candidate.Better(candidates[i], candidates[j])
		})
	default: // "char_first" 或默认
		// 单字优先：默认按权重排序即可（权重体系已保证单字在同音节下排在词组前面的逻辑）
		sort.SliceStable(candidates, func(i, j int) bool {
			return candidate.Better(candidates[i], candidates[j])
		})
	}
}

// lookupSubPhrasesEx 查找子词组（含模糊变体）
// parsed 用于精确计算 ConsumedLength（基于原始输入中的音节位置，含分隔符）。
// totalSyllableCount 用于 Rime 评分的 coverage 计算。
func (e *Engine) lookupSubPhrasesEx(syllables []string, parsed *ParseResult, totalSyllableCount int, candidatesMap map[string]*candidate.Candidate) {
	n := len(syllables)
	// 查找所有连续子序列组成的词组。
	// start=0（首位）：initialQuality=3.0，ConsumedLength 精确到子词组末尾。
	// start>0（非首位）：initialQuality=2.0（降级），ConsumedLength 到子词组末尾
	//   （前面的音节一并消耗——用户主动选择时知道前缀会被牺牲）。
	for length := n; length >= 2; length-- {
		for start := 0; start <= n-length; start++ {
			subSyllables := syllables[start : start+length]
			subKey := strings.Join(subSyllables, "")
			results := e.lookupWithFuzzy(subKey, subSyllables)
			for _, cand := range results {
				if _, exists := candidatesMap[cand.Text]; exists {
					continue
				}
				c := cand
				charCount := len([]rune(c.Text))
				// start=0: initialQuality=3.0, start>0: 降级为 2.0
				iq := 3.0
				if start > 0 {
					iq = 2.0
				}
				coverage := float64(length) / float64(totalSyllableCount)
				c.Weight = e.rimeScore(c.Text, float64(c.Weight), iq, coverage, charCount)
				// ConsumedLength: 消耗到子词组最后一个音节的结束位置（包含前面的音节）
				c.ConsumedLength = parsed.ConsumedBytesForCompletedN(start + length)
				candidatesMap[c.Text] = &c
			}
		}
	}
}

// lookupWithFuzzy 带模糊拼音的词库查找
// syllables 为已切分的音节列表（用于生成模糊变体），可为 nil 表示不做模糊扩展
func (e *Engine) lookupWithFuzzy(code string, syllables []string) []candidate.Candidate {
	results := e.dict.Lookup(code)

	fuzzy := e.getFuzzyConfig()
	if fuzzy == nil || !fuzzy.Enabled() {
		return results
	}

	seen := make(map[string]bool, len(results))
	for _, c := range results {
		seen[c.Text] = true
	}

	// 单音节：直接生成音节变体查找
	if len(syllables) <= 1 {
		syllable := code
		if len(syllables) == 1 {
			syllable = syllables[0]
		}
		for _, v := range fuzzy.Variants(syllable) {
			for _, c := range e.dict.Lookup(v) {
				if !seen[c.Text] {
					seen[c.Text] = true
					results = append(results, c)
				}
			}
		}
		return results
	}

	// 多音节：展开所有组合
	for _, altCode := range fuzzy.ExpandCode(syllables) {
		for _, c := range e.dict.Lookup(altCode) {
			if !seen[c.Text] {
				seen[c.Text] = true
				results = append(results, c)
			}
		}
	}

	return results
}

// getFuzzyConfig 获取模糊拼音配置（原子读取，线程安全）
func (e *Engine) getFuzzyConfig() *FuzzyConfig {
	return e.fuzzyPtr.Load()
}

// ============================================================
// 便捷方法
// ============================================================

// ParseInput 仅解析输入，不查询词库
// 用于 UI 层获取组合态显示
func (e *Engine) ParseInput(input string) *CompositionState {
	if len(input) == 0 {
		return &CompositionState{}
	}

	input = strings.ToLower(input)
	parser := NewPinyinParserWithTrie(e.syllableTrie)
	parsed := parser.Parse(input)

	builder := NewCompositionBuilder()
	return builder.Build(parsed)
}

// GetPossibleSyllables 获取以 prefix 开头的所有可能音节
// 用于 UI 显示可能的续写提示
func (e *Engine) GetPossibleSyllables(prefix string) []string {
	return e.syllableTrie.GetPossibleSyllables(strings.ToLower(prefix))
}

// IsValidSyllable 检查是否是有效的完整音节
func (e *Engine) IsValidSyllable(syllable string) bool {
	return e.syllableTrie.Contains(strings.ToLower(syllable))
}

// IsValidSyllablePrefix 检查是否是有效的音节前缀
func (e *Engine) IsValidSyllablePrefix(prefix string) bool {
	return e.syllableTrie.HasPrefix(strings.ToLower(prefix))
}
