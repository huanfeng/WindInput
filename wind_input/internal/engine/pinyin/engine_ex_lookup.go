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

// lookupSubPhrasesEx 查找从首音节开始的子词组（含模糊变体）
// parsed 用于精确计算 ConsumedLength（基于原始输入中的音节位置，含分隔符）。
// totalSyllableCount 用于 Rime 评分的 coverage 计算。
//
// 只生成 start=0 的子词组：因为 ConsumedLength 从位置 0 开始计算，
// 非首位子词组（start>0）的 ConsumedLength 会包含其之前的音节，
// 选中后前面的音节被静默丢弃（如 hejiele 中 "接了" 会吃掉 "he"）。
// 非首位词组由 Viterbi 组句提供（如 "和解"+"了"），不需要独立候选。
func (e *Engine) lookupSubPhrasesEx(syllables []string, parsed *ParseResult, totalSyllableCount int, candidatesMap map[string]*candidate.Candidate) {
	n := len(syllables)
	for length := n; length >= 2; length-- {
		subSyllables := syllables[0:length]
		subKey := strings.Join(subSyllables, "")
		results := e.lookupWithFuzzy(subKey, subSyllables)
		for _, cand := range results {
			if _, exists := candidatesMap[cand.Text]; exists {
				continue
			}
			c := cand
			charCount := len([]rune(c.Text))
			coverage := float64(length) / float64(totalSyllableCount)
			c.Weight = e.rimeScore(c.Text, float64(c.Weight), 3.0, coverage, charCount)
			c.ConsumedLength = parsed.ConsumedBytesForCompletedN(length)
			candidatesMap[c.Text] = &c
		}
	}
}

// normalizeLookupCode 将"u 形式"的 ü 韵母（nue/lue）归一化为词库使用的"v 形式"（nve/lve），
// 用于查询词库前的编码归一化。
//
// 词库中 ü 韵母统一用 v 表示（虐=nve、略=lve），但主流拼音输入法约定 nüe/lüe 既可输入
// nve/lve 也可输入 nue/lue（二者无歧义：nu+e、lu+e 不构成合法连续音节）。因此查询前把
// nue/lue 归一化到 nve/lve 以命中词库。
//
// 仅 n/l 声母需要此处理：j/q/x/y 后的 ü 在词库中本就写作 u（jue/que/xue/yue），不归一化。
// code 级子串替换是安全的——合法拼音编码中 "nue"/"lue" 子串只可能来自 nüe/lüe 音节。
func normalizeLookupCode(code string) string {
	if strings.Contains(code, "nue") {
		code = strings.ReplaceAll(code, "nue", "nve")
	}
	if strings.Contains(code, "lue") {
		code = strings.ReplaceAll(code, "lue", "lve")
	}
	return code
}

// dictLookup 归一化编码后查询词库（统一处理 nue/lue → nve/lve）。
func (e *Engine) dictLookup(code string) []candidate.Candidate {
	return e.dict.Lookup(normalizeLookupCode(code))
}

// dictLookupPrefix 归一化编码后前缀查询词库。
func (e *Engine) dictLookupPrefix(prefix string, limit int) []candidate.Candidate {
	return e.dict.LookupPrefix(normalizeLookupCode(prefix), limit)
}

// lookupWithFuzzy 带模糊拼音的词库查找
// syllables 为已切分的音节列表（用于生成模糊变体），可为 nil 表示不做模糊扩展
func (e *Engine) lookupWithFuzzy(code string, syllables []string) []candidate.Candidate {
	results := e.dictLookup(code)

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
			for _, c := range e.dictLookup(v) {
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
		for _, c := range e.dictLookup(altCode) {
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
