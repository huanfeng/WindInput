package pinyin

import (
	"sort"
	"strings"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
)

// ============================================================
// Engine 扩展方法
// 使用新的 Parser → Lexicon → Ranker 流水线
// ============================================================

// ============================================================
// 权重层级常量（统一权重体系）
// ============================================================
const (
	weightViterbi       = 3000000 // Viterbi 整句候选
	weightExactMatch    = 2000000 // 精确匹配（字数=音节数）
	weightFirstChar     = 1900000 // 首音节单字（多音节输入时，紧随精确匹配）
	weightPrefixClose   = 1800000 // 前缀匹配（字数接近）
	weightPrefixMatch   = 1700000 // 前缀匹配（一般）
	weightExactOther    = 1500000 // 精确匹配（字数≠音节数）
	weightSubPhrase     = 1000000 // 子词组
	weightPartialPrefix = 800000  // 未完成音节前缀词
	weightPartialChar   = 600000  // 未完成音节单字
	weightSingleChar    = 500000  // 首音节单字
	weightPhrasePrefix  = 300000  // partial 输入时的多字词前缀匹配
)

// ConvertEx 扩展版转换方法
// 返回包含组合态的完整转换结果
func (e *Engine) ConvertEx(input string, maxCandidates int) *PinyinConvertResult {
	return e.convertCore(input, maxCandidates, false)
}

// convertCore 核心转换逻辑（统一的候选生成流水线）
// skipFilter=true 时跳过候选过滤（用于 ConvertRaw 测试场景）
func (e *Engine) convertCore(input string, maxCandidates int, skipFilter bool) *PinyinConvertResult {
	result := &PinyinConvertResult{
		Candidates: make([]candidate.Candidate, 0),
	}

	if len(input) == 0 {
		result.IsEmpty = true
		return result
	}

	input = strings.ToLower(input)

	// 1. 解析输入为音节
	parser := NewPinyinParser()
	parsed := parser.Parse(input)

	// 2. 构建组合态
	builder := NewCompositionBuilder()
	result.Composition = builder.Build(parsed)
	result.PreeditDisplay = result.Composition.PreeditText

	completedSyllables := parsed.CompletedSyllables()
	syllableCount := len(completedSyllables)
	partial := parsed.PartialSyllable()
	allSyllables := parsed.SyllableTexts()

	logDebug("[PinyinEngine] input=%q preedit=%q completed=%v partial=%q allSyllables=%v",
		input, result.PreeditDisplay, completedSyllables, partial, allSyllables)

	// 3. 收集候选词
	candidatesMap := make(map[string]*candidate.Candidate)

	// 获取候选排序模式
	candidateOrder := "char_first"
	if e.config != nil && e.config.CandidateOrder != "" {
		candidateOrder = e.config.CandidateOrder
	}

	// 3a. Viterbi 智能组句（多音节且无未完成音节时使用）
	useViterbi := e.config != nil && e.config.UseSmartCompose &&
		e.unigram != nil && syllableCount >= 2 && partial == "" &&
		len(input) >= smartComposeThreshold

	if useViterbi {
		lattice := BuildLattice(input, e.syllableTrie, e.dict, e.unigram)
		if !lattice.IsEmpty() {
			// 获取 Top-3 最优路径
			vResults := ViterbiTopK(lattice, e.bigram, 3)
			for rank, vResult := range vResults {
				if vResult == nil || len(vResult.Words) == 0 {
					continue
				}
				sentence := vResult.String()
				if _, exists := candidatesMap[sentence]; exists {
					continue
				}
				logDebug("[PinyinEngine] Viterbi[%d]: %q words=%v logprob=%.4f",
					rank, sentence, vResult.Words, vResult.LogProb)
				c := candidate.Candidate{
					Text:           sentence,
					Code:           input,
					Weight:         weightViterbi - rank,
					ConsumedLength: len(input),
				}
				candidatesMap[sentence] = &c
			}
		}
	}

	// 3b. 精确匹配完整音节序列的词组（含模糊变体）
	if syllableCount > 0 && partial == "" {
		exactResults := e.lookupWithFuzzy(input, completedSyllables)
		// 使用 Unigram 单字频率对精确匹配进行二次排序
		type scoredExact struct {
			cand  candidate.Candidate
			score float64
		}
		scored := make([]scoredExact, 0, len(exactResults))
		for _, cand := range exactResults {
			charCount := len([]rune(cand.Text))
			lmScore := float64(0)
			if e.unigram != nil {
				lmScore = e.unigram.CharBasedScore(cand.Text)
			}
			se := scoredExact{cand: cand, score: lmScore}
			// 字数匹配音节数的给予额外加成
			if charCount == syllableCount {
				se.score += 100
			}
			scored = append(scored, se)
		}
		// 按 LM 分数降序排列
		sort.Slice(scored, func(i, j int) bool {
			return scored[i].score > scored[j].score
		})
		for i, se := range scored {
			if _, exists := candidatesMap[se.cand.Text]; exists {
				continue
			}
			c := se.cand
			charCount := len([]rune(c.Text))
			if charCount == syllableCount {
				c.Weight = weightExactMatch - i
			} else {
				c.Weight = weightExactOther - i
			}
			c.ConsumedLength = len(input)
			candidatesMap[c.Text] = &c
		}
		logDebug("[PinyinEngine] exact match for %q: %d results", input, len(exactResults))
	}

	// 3c. 前缀匹配（输入 "wome" 时找到 "women"→我们）
	// 仅在有已完成音节时运行；纯 partial 输入（如 "b","zh"）由 3f 处理
	if syllableCount > 0 {
		if ps, ok := e.dict.(dict.PrefixSearchable); ok {
			prefixLimit := 50
			if maxCandidates > 0 {
				prefixLimit = maxCandidates * 2
			}
			prefixResults := ps.LookupPrefix(input, prefixLimit)
			for i, cand := range prefixResults {
				if _, exists := candidatesMap[cand.Text]; exists {
					continue
				}
				c := cand
				charCount := len([]rune(c.Text))
				if charCount == syllableCount+1 && partial == "" {
					c.Weight = weightPrefixClose - i
				} else if charCount == syllableCount {
					c.Weight = weightPrefixMatch - i
				} else {
					c.Weight = weightSubPhrase - i
				}
				c.ConsumedLength = len(input)
				candidatesMap[c.Text] = &c
			}
			logDebug("[PinyinEngine] prefix match for %q: %d results", input, len(prefixResults))
		}
	}

	// 3d. 子词组查找（如 "nihao" → 查找 "ni"+"hao" 对应的词组）
	var mainPath []string
	if syllableCount > 1 {
		dag := BuildDAG(input, e.syllableTrie)
		mainPath = dag.MaximumMatch()
		if len(mainPath) > 1 {
			joined := strings.Join(mainPath, "")
			if joined == input {
				e.lookupSubPhrasesEx(mainPath, candidatesMap)
			}
		}
	}

	// 3e. 单字候选
	if syllableCount > 0 {
		// 使用 Unigram 对首音节单字排序（含模糊变体）
		firstSyllable := completedSyllables[0]
		charResults := e.lookupWithFuzzy(firstSyllable, []string{firstSyllable})

		type scoredChar struct {
			cand  candidate.Candidate
			score float64
		}
		scoredChars := make([]scoredChar, 0, len(charResults))
		for _, cand := range charResults {
			lmScore := float64(0)
			if e.unigram != nil {
				lmScore = e.unigram.LogProb(cand.Text)
			}
			scoredChars = append(scoredChars, scoredChar{cand: cand, score: lmScore})
		}
		sort.Slice(scoredChars, func(i, j int) bool {
			return scoredChars[i].score > scoredChars[j].score
		})

		for j, sc := range scoredChars {
			if _, exists := candidatesMap[sc.cand.Text]; exists {
				continue
			}
			c := sc.cand
			// 多音节输入时提升首音节单字权重，确保它们出现在精确匹配之后、前缀匹配之前
			if syllableCount >= 2 {
				c.Weight = weightFirstChar - j
			} else {
				c.Weight = weightSingleChar - j
			}
			c.ConsumedLength = len(firstSyllable)
			candidatesMap[c.Text] = &c
		}

		// 非首音节的单字（更低权重，含模糊变体）
		for i := 1; i < syllableCount; i++ {
			syllable := completedSyllables[i]
			others := e.lookupWithFuzzy(syllable, []string{syllable})
			for j, cand := range others {
				if _, exists := candidatesMap[cand.Text]; exists {
					continue
				}
				c := cand
				c.Weight = weightSingleChar - i*10000 - j
				// 非首音节单字的 ConsumedLength 需要计算到该音节的结束位置
				consumedLen := 0
				for k := 0; k <= i; k++ {
					consumedLen += len(completedSyllables[k])
				}
				c.ConsumedLength = consumedLen
				candidatesMap[c.Text] = &c
			}
		}
	}

	// 3e2. 当有多个 partial 音节时，为首音节生成单字候选
	// 例如输入 "bzd"，所有音节 ["b","z","d"] 都是 partial，completedSyllables 为空
	// 此时应为首音节 "b" 生成单字候选（按空格先上屏首音节）
	// 注意：单 partial（如 "b"）由 3f 处理，避免权重冲突
	if syllableCount == 0 && len(allSyllables) > 1 {
		firstPartial := allSyllables[0]
		possibles := e.syllableTrie.GetPossibleSyllables(firstPartial)
		for _, syllable := range possibles {
			charResults := e.dict.Lookup(syllable)
			for j, cand := range charResults {
				if _, exists := candidatesMap[cand.Text]; exists {
					continue
				}
				c := cand
				c.Weight = weightSingleChar - j
				c.ConsumedLength = len(firstPartial)
				candidatesMap[c.Text] = &c
			}
		}
	}

	// 3f. 未完成音节的前缀查找
	if partial != "" {
		if ps, ok := e.dict.(dict.PrefixSearchable); ok {
			partialPrefix := input
			prefixResults := ps.LookupPrefix(partialPrefix, 30)
			for i, cand := range prefixResults {
				if _, exists := candidatesMap[cand.Text]; exists {
					continue
				}
				c := cand
				charCount := len([]rune(c.Text))
				if charCount == 1 {
					// 单字候选保持高权重
					c.Weight = weightPartialPrefix - i
				} else {
					// 多字词/短语降低权重，避免"版权"、UUID 等出现在单字前面
					c.Weight = weightPhrasePrefix - i
				}
				c.ConsumedLength = len(input)
				candidatesMap[c.Text] = &c
			}
		}
		// 按完整音节前缀查找单字（即使有已完成音节，也应为 partial 部分生成候选）
		st := e.syllableTrie
		possibles := st.GetPossibleSyllables(partial)
		for _, syllable := range possibles {
			charResults := e.dict.Lookup(syllable)
			for j, cand := range charResults {
				if _, exists := candidatesMap[cand.Text]; exists {
					continue
				}
				c := cand
				// 有已完成音节或多个 partial 音节时降低权重，避免与首音节单字争排名
				otherSyllableCount := len(completedSyllables)
				if otherSyllableCount == 0 && len(allSyllables) > 1 {
					otherSyllableCount = len(allSyllables) - 1
				}
				if otherSyllableCount > 0 {
					c.Weight = weightPartialChar - otherSyllableCount*10000 - j
				} else {
					c.Weight = weightPartialChar - j
				}
				c.ConsumedLength = len(input)
				candidatesMap[c.Text] = &c
			}
		}
	}

	// 3g. 简拼词组匹配（多个 partial 声母时）
	// 例如 "bzd" → allSyllables=["b","z","d"]，匹配 "不知道"(bu zhi dao) 等
	if len(allSyllables) >= 2 && syllableCount == 0 {
		abbrevCode := strings.Join(allSyllables, "")
		if as, ok := e.dict.(dict.AbbrevSearchable); ok {
			abbrevResults := as.LookupAbbrev(abbrevCode, 30)
			for i, cand := range abbrevResults {
				if _, exists := candidatesMap[cand.Text]; exists {
					continue
				}
				c := cand
				c.Weight = weightExactMatch - i
				c.ConsumedLength = len(input)
				candidatesMap[c.Text] = &c
			}
		}
	}

	// 4. 转换为列表
	result.Candidates = make([]candidate.Candidate, 0, len(candidatesMap))
	for _, cand := range candidatesMap {
		result.Candidates = append(result.Candidates, *cand)
	}

	// 5. 排序（根据排序模式）
	e.sortCandidates(result.Candidates, candidateOrder, syllableCount)

	// 6. 应用过滤
	if !skipFilter {
		filterMode := "smart"
		if e.config != nil && e.config.FilterMode != "" {
			filterMode = e.config.FilterMode
		}
		result.Candidates = candidate.FilterCandidates(result.Candidates, filterMode)
	}

	// 7. 检查是否空码
	if len(result.Candidates) == 0 {
		result.IsEmpty = true
		result.NeedRefine = result.Composition.HasPartial()
	}

	// 8. 限制数量
	if maxCandidates > 0 && len(result.Candidates) > maxCandidates {
		result.Candidates = result.Candidates[:maxCandidates]
		result.HasMore = true
	}

	// 9. 添加五笔编码提示
	e.addWubiHints(result.Candidates)

	logDebug("[PinyinEngine] final candidates=%d isEmpty=%v",
		len(result.Candidates), result.IsEmpty)

	return result
}

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
			return candidates[i].Weight > candidates[j].Weight
		})
	case "smart":
		// 智能混排：完全按权重排序，但对单字候选使用 Unigram 分数微调
		if e.unigram != nil {
			for i := range candidates {
				if len([]rune(candidates[i].Text)) == 1 && candidates[i].Weight >= weightSingleChar && candidates[i].Weight < weightPartialChar {
					// 用 Unigram 分数微调单字权重
					lmScore := e.unigram.LogProb(candidates[i].Text)
					// 将 logprob（通常 -20 到 -5）映射到 0-9999 的范围
					bonus := int((lmScore + 20) * 600)
					if bonus < 0 {
						bonus = 0
					}
					if bonus > 9999 {
						bonus = 9999
					}
					candidates[i].Weight = weightSingleChar + bonus
				}
			}
		}
		sort.Slice(candidates, func(i, j int) bool {
			return candidates[i].Weight > candidates[j].Weight
		})
	default: // "char_first" 或默认
		// 单字优先：默认按权重排序即可（权重体系已保证单字在同音节下排在词组前面的逻辑）
		sort.Slice(candidates, func(i, j int) bool {
			return candidates[i].Weight > candidates[j].Weight
		})
	}
}

// lookupSubPhrasesEx 查找子词组（含模糊变体）
func (e *Engine) lookupSubPhrasesEx(syllables []string, candidatesMap map[string]*candidate.Candidate) {
	n := len(syllables)
	// 查找所有连续子序列组成的词组
	for length := n; length >= 2; length-- {
		for start := 0; start <= n-length; start++ {
			subSyllables := syllables[start : start+length]
			subKey := strings.Join(subSyllables, "")
			results := e.lookupWithFuzzy(subKey, subSyllables)
			for i, cand := range results {
				if _, exists := candidatesMap[cand.Text]; exists {
					continue
				}
				c := cand
				charCount := len([]rune(c.Text))
				// 子词组权重：词越长越优先，匹配字数越接近音节数越好
				bonus := length * 100000
				if charCount == length {
					bonus += 50000
				}
				c.Weight = weightSubPhrase + bonus - start*10000 - i
				// 计算该子词组消耗的输入长度
				if start == 0 {
					// 从头开始的子词组：仅消耗对应音节，支持部分上屏
					consumedLen := 0
					for k := 0; k < length; k++ {
						consumedLen += len(syllables[k])
					}
					c.ConsumedLength = consumedLen
				} else {
					// 非首位子词组：消耗全部输入（避免前缀音节丢失）
					totalLen := 0
					for _, s := range syllables {
						totalLen += len(s)
					}
					c.ConsumedLength = totalLen
				}
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

	seen := make(map[string]bool)
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

// getFuzzyConfig 获取模糊拼音配置
func (e *Engine) getFuzzyConfig() *FuzzyConfig {
	if e.config != nil {
		return e.config.Fuzzy
	}
	return nil
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
	parser := NewPinyinParser()
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
