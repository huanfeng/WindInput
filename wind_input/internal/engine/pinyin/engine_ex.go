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
// 权重层级常量（5 级权重体系）
// ============================================================
const (
	weightCommand       = 4000000 // L0: 特殊命令精确匹配（uuid, date 等）
	weightViterbi       = 3000000 // L1: Viterbi 整句候选
	weightExactMatch    = 2000000 // L2: 精确匹配 + 混合简拼（字数=音节数）
	weightFirstSyllable = 1500000 // L3: 首音节单字 + 前缀接近匹配
	weightSupplement    = 500000  // L4: 子词组、partial 前缀、非首音节单字
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

	// 注意：以下变量来自 parsed（原始解析结果），而非 composition。
	// - completedSyllables: 仅包含 Exact 音节（如 "ni","hao"），不含 partial
	// - allSyllables: 包含所有音节文本（Exact + Partial），用于简拼匹配
	// composition.CompletedSyllables 会把非末尾 partial 提升为 completed（仅用于 UI 显示）
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

	// ── 步骤 0：特殊命令精确匹配（仅查命令，不查普通词条） ──
	// 通过 CommandSearchable 接口仅查询 PhraseLayer 中的命令（uuid, date 等），
	// 不会把普通拼音词条提升到命令权重。对所有输入无条件执行。
	if cs, ok := e.dict.(dict.CommandSearchable); ok {
		cmdResults := cs.LookupCommand(input)
		for i, cand := range cmdResults {
			c := cand
			c.Weight = weightCommand + 1000 - i
			c.ConsumedLength = len(input)
			candidatesMap[c.Text] = &c
		}
		if len(cmdResults) > 0 {
			logDebug("[PinyinEngine] command match for %q: %d results", input, len(cmdResults))
		}
	}

	// ── 3a. Viterbi 智能组句（多音节且无未完成音节时使用） ──
	useViterbi := e.config != nil && e.config.UseSmartCompose &&
		e.unigram != nil && syllableCount >= 2 && partial == "" &&
		len(input) >= smartComposeThreshold

	if useViterbi {
		lattice := BuildLattice(input, e.syllableTrie, e.dict, e.unigram)
		if !lattice.IsEmpty() {
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

	// ── 3b. 精确匹配完整音节序列的词组（含模糊变体） ──
	if syllableCount > 0 && partial == "" {
		exactResults := e.lookupWithFuzzy(input, completedSyllables)
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
			if charCount == syllableCount {
				se.score += 100
			}
			scored = append(scored, se)
		}
		sort.Slice(scored, func(i, j int) bool {
			if scored[i].score != scored[j].score {
				return scored[i].score > scored[j].score
			}
			if scored[i].cand.Text != scored[j].cand.Text {
				return scored[i].cand.Text < scored[j].cand.Text
			}
			return scored[i].cand.Code < scored[j].cand.Code
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
				c.Weight = weightExactMatch - 500000 - i // 字数不匹配：降级
			}
			c.ConsumedLength = len(input)
			candidatesMap[c.Text] = &c
		}
		logDebug("[PinyinEngine] exact match for %q: %d results", input, len(exactResults))
	}

	// ── 3c. 前缀匹配（输入 "wome" 时找到 "women"→我们） ──
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
					c.Weight = weightFirstSyllable + 300000 - i // 字数接近：L3 上段
				} else if charCount == syllableCount {
					c.Weight = weightFirstSyllable + 200000 - i // 字数一致：L3 中段
				} else {
					c.Weight = weightSupplement - i // 其他：L4
				}
				c.ConsumedLength = len(input)
				candidatesMap[c.Text] = &c
			}
			logDebug("[PinyinEngine] prefix match for %q: %d results", input, len(prefixResults))
		}
	}

	// ── 3d. 子词组查找（如 "nihao" → 查找 "ni"+"hao" 对应的词组） ──
	// 对于有 partial 后缀的输入（如 "nihaozh"），DAG 只能覆盖到 "nihao"，
	// 此时 joined 是 input 的前缀而非完全相等，也应执行子词组查找。
	var mainPath []string
	if syllableCount > 1 {
		dag := BuildDAG(input, e.syllableTrie)
		mainPath = dag.MaximumMatch()
		if len(mainPath) > 1 {
			joined := strings.Join(mainPath, "")
			if joined == input || strings.HasPrefix(input, joined) {
				e.lookupSubPhrasesEx(mainPath, candidatesMap)
			}
		}
	}

	// ── 3e. 单字候选 ──
	if syllableCount > 0 {
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
			if scoredChars[i].score != scoredChars[j].score {
				return scoredChars[i].score > scoredChars[j].score
			}
			if scoredChars[i].cand.Text != scoredChars[j].cand.Text {
				return scoredChars[i].cand.Text < scoredChars[j].cand.Text
			}
			return scoredChars[i].cand.Code < scoredChars[j].cand.Code
		})

		for j, sc := range scoredChars {
			if _, exists := candidatesMap[sc.cand.Text]; exists {
				continue
			}
			c := sc.cand
			if syllableCount >= 2 {
				c.Weight = weightFirstSyllable - j // L3：多音节输入的首音节单字
			} else {
				c.Weight = weightSupplement - j // L4：单音节输入的单字
			}
			c.ConsumedLength = len(firstSyllable)
			candidatesMap[c.Text] = &c
		}

		// 非首音节的单字（L4 权重）
		for i := 1; i < syllableCount; i++ {
			syllable := completedSyllables[i]
			others := e.lookupWithFuzzy(syllable, []string{syllable})
			for j, cand := range others {
				if _, exists := candidatesMap[cand.Text]; exists {
					continue
				}
				c := cand
				c.Weight = weightSupplement - i*10000 - j
				consumedLen := 0
				for k := 0; k <= i; k++ {
					consumedLen += len(completedSyllables[k])
				}
				c.ConsumedLength = consumedLen
				candidatesMap[c.Text] = &c
			}
		}
	}

	// ── 3e2. 多 partial 音节时的首音节单字候选 ──
	// 例如 "bzd" → ["b","z","d"] 都是 partial，为首音节 "b" 生成单字候选
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
				c.Weight = weightSupplement - j
				c.ConsumedLength = len(firstPartial)
				candidatesMap[c.Text] = &c
			}
		}
	}

	// ── 3f. 未完成音节的前缀查找 ──
	if partial != "" {
		if ps, ok := e.dict.(dict.PrefixSearchable); ok {
			prefixResults := ps.LookupPrefix(input, 30)
			for i, cand := range prefixResults {
				if _, exists := candidatesMap[cand.Text]; exists {
					continue
				}
				c := cand
				charCount := len([]rune(c.Text))
				if charCount == 1 {
					c.Weight = weightSupplement + 300000 - i // 单字候选：L4 上段
				} else {
					c.Weight = weightSupplement - 200000 - i // 多字词：L4 下段
				}
				c.ConsumedLength = len(input)
				candidatesMap[c.Text] = &c
			}
		}
		// 按完整音节前缀查找单字
		possibles := e.syllableTrie.GetPossibleSyllables(partial)
		for _, syllable := range possibles {
			charResults := e.dict.Lookup(syllable)
			for j, cand := range charResults {
				if _, exists := candidatesMap[cand.Text]; exists {
					continue
				}
				c := cand
				otherSyllableCount := len(completedSyllables)
				if otherSyllableCount == 0 && len(allSyllables) > 1 {
					otherSyllableCount = len(allSyllables) - 1
				}
				if otherSyllableCount > 0 {
					// 有其他音节时降低权重，避免末尾 partial 的单字排在首音节单字前面
					// 例如 "bzd" 时，"d" 的单字（到、的）应排在 "b" 的单字（不、白）之后
					c.Weight = weightSupplement - 100000 - otherSyllableCount*10000 - j
				} else {
					// 单 partial 输入（如 "b"）：保持较高权重
					c.Weight = weightSupplement + 100000 - j
				}
				c.ConsumedLength = len(input)
				candidatesMap[c.Text] = &c
			}
		}
	}

	// ── 3g. 简拼/混合简拼词组匹配 ──
	// 纯简拼：bzd → allSyllables=["b","z","d"] → abbrev="bzd"
	// 混合简拼：nizm → allSyllables=["ni","z","m"] → abbrev="nzm"
	if len(allSyllables) >= 2 {
		var abbrevBuilder strings.Builder
		for _, s := range allSyllables {
			abbrevBuilder.WriteByte(s[0])
		}
		abbrevCode := abbrevBuilder.String()

		if as, ok := e.dict.(dict.AbbrevSearchable); ok {
			abbrevResults := as.LookupAbbrev(abbrevCode, 30)
			for i, cand := range abbrevResults {
				c := cand
				charCount := len([]rune(c.Text))
				if charCount == len(allSyllables) {
					c.Weight = weightExactMatch - 100 - i // 字数匹配音节数：L2
				} else {
					c.Weight = weightSupplement + 400000 - i // 字数不匹配：L4 高段
				}
				c.ConsumedLength = len(input)
				if existing, exists := candidatesMap[c.Text]; exists {
					// 重复文本时保留更高优先级候选，避免前缀低权重覆盖简拼高权重。
					if candidate.Better(c, *existing) {
						candidatesMap[c.Text] = &c
					}
				} else {
					candidatesMap[c.Text] = &c
				}
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
			return candidate.Better(candidates[i], candidates[j])
		})
	case "smart":
		// 智能混排：完全按权重排序，但对 L4 层单字候选使用 Unigram 分数微调
		if e.unigram != nil {
			for i := range candidates {
				w := candidates[i].Weight
				if len([]rune(candidates[i].Text)) == 1 && w >= weightSupplement && w < weightFirstSyllable {
					lmScore := e.unigram.LogProb(candidates[i].Text)
					bonus := int((lmScore + 20) * 600)
					if bonus < 0 {
						bonus = 0
					}
					if bonus > 9999 {
						bonus = 9999
					}
					candidates[i].Weight = weightSupplement + bonus
				}
			}
		}
		sort.Slice(candidates, func(i, j int) bool {
			return candidate.Better(candidates[i], candidates[j])
		})
	default: // "char_first" 或默认
		// 单字优先：默认按权重排序即可（权重体系已保证单字在同音节下排在词组前面的逻辑）
		sort.Slice(candidates, func(i, j int) bool {
			return candidate.Better(candidates[i], candidates[j])
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
				c.Weight = weightSupplement + bonus - start*10000 - i
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
