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

// featureOpts 构建候选特征的可选参数
type featureOpts struct {
	isUser, isFuzzy, isPartial, isAbbrev, isViterbi, isCommand bool
	segmentRank                                                int
}

// buildFeatures 为候选构建特征向量
func (e *Engine) buildFeatures(text string, freqScore float64, matchType MatchType, syllableCount int, charCount int, opts featureOpts) CandidateFeatures {
	f := CandidateFeatures{
		MatchType:     matchType,
		SyllableMatch: charCount == syllableCount,
		CharCount:     charCount,
		SyllableCount: syllableCount,
		IsUserWord:    opts.isUser,
		IsFuzzy:       opts.isFuzzy,
		IsPartial:     opts.isPartial,
		IsAbbrev:      opts.isAbbrev,
		IsViterbi:     opts.isViterbi,
		IsCommand:     opts.isCommand,
		FreqScore:     freqScore,
		SegmentRank:   opts.segmentRank,
	}
	// 计算语言模型分数
	if e.unigram != nil && text != "" {
		if charCount == 1 {
			f.LMScore = e.unigram.LogProb(text)
		} else {
			f.LMScore = e.unigram.CharBasedScore(text)
		}
	}
	return f
}

// scorerWeight 使用 Scorer 计算权重，将 float64 分数映射到 int 权重空间
// 乘以 1000 使数值范围与原有硬编码权重（500000~4000000）一致
func (e *Engine) scorerWeight(f CandidateFeatures) int {
	return int(e.scorer.Score(f) * 1000)
}

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

	// 去除显式分隔符，得到纯拼音字符串用于词库查询
	queryInput := strings.ReplaceAll(input, "'", "")

	// 1. 解析输入为音节（复用引擎的 SyllableTrie，避免每次按键重建）
	parser := NewPinyinParserWithTrie(e.syllableTrie)
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

	// 3. 收集候选词（预分配容量避免多次扩容）
	candidatesMap := make(map[string]*candidate.Candidate, 64)

	// 获取候选排序模式
	candidateOrder := "char_first"
	if e.config != nil && e.config.CandidateOrder != "" {
		candidateOrder = e.config.CandidateOrder
	}

	// ── 步骤 0：特殊命令精确匹配（仅查命令，不查普通词条） ──
	// 通过 CommandSearchable 接口仅查询 PhraseLayer 中的命令（uuid, date 等），
	// 不会把普通拼音词条提升到命令权重。对所有输入无条件执行。
	if cs, ok := e.dict.(dict.CommandSearchable); ok {
		cmdResults := cs.LookupCommand(queryInput)
		for _, cand := range cmdResults {
			c := cand
			f := e.buildFeatures(c.Text, float64(c.Weight), MatchExact, 0, len([]rune(c.Text)), featureOpts{isCommand: true})
			c.Weight = e.scorerWeight(f)
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
		lattice := BuildLattice(queryInput, e.syllableTrie, e.dict, e.unigram)
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
				charCount := len([]rune(sentence))
				vf := e.buildFeatures(sentence, 0, MatchExact, charCount, charCount, featureOpts{isViterbi: true})
				vf.LMScore = vResult.LogProb
				c := candidate.Candidate{
					Text:           sentence,
					Code:           input,
					Weight:         e.scorerWeight(vf),
					ConsumedLength: len(input),
				}
				candidatesMap[sentence] = &c
			}
		}
	}

	// ── 3b. 精确匹配完整音节序列的词组（含模糊变体） ──
	if syllableCount > 0 && partial == "" {
		exactResults := e.lookupWithFuzzy(queryInput, completedSyllables)
		for _, cand := range exactResults {
			if _, exists := candidatesMap[cand.Text]; exists {
				continue
			}
			c := cand
			charCount := len([]rune(c.Text))
			f := e.buildFeatures(c.Text, float64(c.Weight), MatchExact, syllableCount, charCount, featureOpts{})
			c.Weight = e.scorerWeight(f)
			c.ConsumedLength = len(input)
			candidatesMap[c.Text] = &c
		}
		logDebug("[PinyinEngine] exact match for %q: %d results", input, len(exactResults))
	}

	// ── 3b-alt. 多切分并行打分 ──
	// 对无显式分隔符的输入，获取备选切分路径的候选
	// 即使有 partial 后缀（如 "xianr"），也对完整音节部分做多切分
	if !strings.Contains(input, "'") && syllableCount > 0 {
		detail := parser.ParseWithDetail(queryInput, 4)
		for _, alt := range detail.Alternatives {
			altSyllables := alt.CompletedSyllables()
			if len(altSyllables) == 0 {
				continue
			}
			altCode := strings.Join(altSyllables, "")
			altResults := e.lookupWithFuzzy(altCode, altSyllables)
			for _, cand := range altResults {
				if _, exists := candidatesMap[cand.Text]; exists {
					continue
				}
				c := cand
				charCount := len([]rune(c.Text))
				f := e.buildFeatures(c.Text, float64(c.Weight), MatchExact, len(altSyllables), charCount, featureOpts{segmentRank: 1})
				c.Weight = e.scorerWeight(f)
				c.ConsumedLength = len(input)
				candidatesMap[c.Text] = &c
			}
		}
	}

	// ── 3c. 前缀匹配（输入 "wome" 时找到 "women"→我们） ──
	if syllableCount > 0 {
		if ps, ok := e.dict.(dict.PrefixSearchable); ok {
			prefixLimit := 50
			if maxCandidates > 0 {
				prefixLimit = maxCandidates * 2
			}
			prefixResults := ps.LookupPrefix(queryInput, prefixLimit)
			for _, cand := range prefixResults {
				if _, exists := candidatesMap[cand.Text]; exists {
					continue
				}
				c := cand
				charCount := len([]rune(c.Text))
				f := e.buildFeatures(c.Text, float64(c.Weight), MatchPartial, syllableCount, charCount, featureOpts{})
				c.Weight = e.scorerWeight(f)
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
		dag := BuildDAG(queryInput, e.syllableTrie)
		mainPath = dag.MaximumMatch()
		if len(mainPath) > 1 {
			joined := strings.Join(mainPath, "")
			if joined == queryInput || strings.HasPrefix(queryInput, joined) {
				e.lookupSubPhrasesEx(mainPath, candidatesMap)
			}
		}
	}

	// ── 3e. 单字候选 ──
	if syllableCount > 0 {
		firstSyllable := completedSyllables[0]
		charResults := e.lookupWithFuzzy(firstSyllable, []string{firstSyllable})

		for _, cand := range charResults {
			if _, exists := candidatesMap[cand.Text]; exists {
				continue
			}
			c := cand
			charCount := len([]rune(c.Text))
			// 首音节单字：多音节输入时为 Partial（只消耗部分输入），单音节为 Exact
			matchType := MatchExact
			if syllableCount >= 2 {
				matchType = MatchPartial
			}
			f := e.buildFeatures(c.Text, float64(c.Weight), matchType, 1, charCount, featureOpts{})
			c.Weight = e.scorerWeight(f)
			c.ConsumedLength = len(firstSyllable)
			candidatesMap[c.Text] = &c
		}

		// 非首音节的单字
		for i := 1; i < syllableCount; i++ {
			syllable := completedSyllables[i]
			others := e.lookupWithFuzzy(syllable, []string{syllable})
			for _, cand := range others {
				if _, exists := candidatesMap[cand.Text]; exists {
					continue
				}
				c := cand
				charCount := len([]rune(c.Text))
				f := e.buildFeatures(c.Text, float64(c.Weight), MatchPartial, 1, charCount, featureOpts{isPartial: true})
				c.Weight = e.scorerWeight(f)
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
			for _, cand := range charResults {
				if _, exists := candidatesMap[cand.Text]; exists {
					continue
				}
				c := cand
				charCount := len([]rune(c.Text))
				f := e.buildFeatures(c.Text, float64(c.Weight), MatchPartial, 1, charCount, featureOpts{isPartial: true})
				c.Weight = e.scorerWeight(f)
				c.ConsumedLength = len(firstPartial)
				candidatesMap[c.Text] = &c
			}
		}
	}

	// ── 3f. 未完成音节的前缀查找 ──
	if partial != "" {
		if ps, ok := e.dict.(dict.PrefixSearchable); ok {
			prefixResults := ps.LookupPrefix(queryInput, 30)
			for _, cand := range prefixResults {
				if _, exists := candidatesMap[cand.Text]; exists {
					continue
				}
				c := cand
				charCount := len([]rune(c.Text))
				// 单字候选优先于多字词（partial 输入时用户更可能想要单字）
				matchType := MatchPartial
				if charCount > 1 {
					matchType = MatchFuzzy // 多字词降级到 Fuzzy 层
				}
				f := e.buildFeatures(c.Text, float64(c.Weight), matchType, syllableCount, charCount, featureOpts{isPartial: true})
				c.Weight = e.scorerWeight(f)
				c.ConsumedLength = len(input)
				candidatesMap[c.Text] = &c
			}
		}
		// 按完整音节前缀查找单字
		possibles := e.syllableTrie.GetPossibleSyllables(partial)
		for _, syllable := range possibles {
			charResults := e.dict.Lookup(syllable)
			for _, cand := range charResults {
				if _, exists := candidatesMap[cand.Text]; exists {
					continue
				}
				c := cand
				charCount := len([]rune(c.Text))
				otherSyllableCount := len(completedSyllables)
				if otherSyllableCount == 0 && len(allSyllables) > 1 {
					otherSyllableCount = len(allSyllables) - 1
				}
				// 有其他音节时标记为 isPartial 降低权重，避免末尾 partial 单字排在首音节单字前
				f := e.buildFeatures(c.Text, float64(c.Weight), MatchPartial, 1, charCount, featureOpts{isPartial: otherSyllableCount > 0})
				c.Weight = e.scorerWeight(f)
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
			for _, cand := range abbrevResults {
				c := cand
				charCount := len([]rune(c.Text))
				f := e.buildFeatures(c.Text, float64(c.Weight), MatchExact, len(allSyllables), charCount, featureOpts{isAbbrev: true})
				c.Weight = e.scorerWeight(f)
				c.ConsumedLength = len(input)
				if existing, exists := candidatesMap[c.Text]; exists {
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

	// 5.5 应用 Shadow 规则（置顶/删除/调权）
	// 必须在拼音引擎的权重分配之后执行，因为拼音引擎会覆盖 CompositeDict 设置的 Shadow 权重
	result.Candidates = e.applyShadowRules(input, result.Candidates)

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

// applyShadowRules 在拼音引擎的权重分配之后应用 Shadow 规则
// 拼音引擎会覆盖 CompositeDict 设置的权重，所以需要在最终排序后再次应用
func (e *Engine) applyShadowRules(input string, candidates []candidate.Candidate) []candidate.Candidate {
	if e.dictManager == nil {
		return candidates
	}
	shadowLayer := e.dictManager.GetShadowLayer()
	if shadowLayer == nil {
		return candidates
	}

	// 收集所有相关 code 的 Shadow 规则
	// 拼音场景：用户输入 "nihao" 但候选可能来自不同路径（精确、前缀、子词组等）
	// 需要同时查 input 和每个候选的 Code
	deleted := make(map[string]bool)
	toppedMap := make(map[string]bool)
	reweighted := make(map[string]int)

	codeSet := make(map[string]bool)
	codeSet[input] = true
	for _, c := range candidates {
		if c.Code != "" && c.Code != input {
			codeSet[c.Code] = true
		}
	}

	for code := range codeSet {
		rules := shadowLayer.GetShadowRules(code)
		for _, rule := range rules {
			switch rule.Action {
			case dict.ShadowActionDelete:
				deleted[rule.Word] = true
			case dict.ShadowActionTop:
				toppedMap[rule.Word] = true
			case dict.ShadowActionReweight:
				reweighted[rule.Word] = rule.NewWeight
			}
		}
	}

	if len(deleted) == 0 && len(toppedMap) == 0 && len(reweighted) == 0 {
		return candidates
	}

	// 应用规则：过滤删除项，标记置顶项和调权项
	needResort := false
	var results []candidate.Candidate
	for _, c := range candidates {
		if deleted[c.Text] {
			continue
		}
		if toppedMap[c.Text] {
			// 置顶权重高于拼音引擎最高权重(weightCommand=4000000)
			c.Weight = 5000000
			needResort = true
		} else if newWeight, ok := reweighted[c.Text]; ok {
			c.Weight = newWeight
			needResort = true
		}
		results = append(results, c)
	}

	// 有权重变化时重新排序
	if needResort {
		sort.SliceStable(results, func(i, j int) bool {
			return results[i].Weight > results[j].Weight
		})
	}

	return results
}
