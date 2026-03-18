package pinyin

import (
	"sort"
	"strings"
	"time"

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
	convertStart := time.Now()

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

	// 预计算关键长度（基于 Parser 的音节位置信息，含分隔符）
	// allCompletedEnd: 所有已完成音节在原始输入中的结束位置
	allCompletedEnd := parsed.ConsumedBytesForCompletedN(syllableCount)

	logDebug("[PinyinEngine] input=%q preedit=%q completed=%v partial=%q allSyllables=%v parseElapsed=%v",
		input, result.PreeditDisplay, completedSyllables, partial, allSyllables, time.Since(convertStart))

	// 检查首个 completed syllable 是否也是输入的第一个段
	// 例如 sdem → allSyllables=["s","de","m"]，completedSyllables=["de"]
	// "de" 不是第一段，不应获得首音节优先权
	firstCompletedIsLeading := syllableCount > 0 && len(allSyllables) > 0 &&
		allSyllables[0] == completedSyllables[0]

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
	{
		cmdResults := e.dict.LookupCommand(queryInput)
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

	// Viterbi 智能组句已移除：与分步确认（方案二）的逐词上屏模式不兼容。
	// 分步确认依赖精确的 ConsumedLength 来驱动状态机，Viterbi 的全局整句结果
	// 无法与逐词消费机制协调。词组匹配由步骤 1（精确匹配）和步骤 2（子词组）覆盖。

	completedCode := strings.Join(completedSyllables, "")

	// ── 步骤 1：精确匹配完整音节序列的词组（含模糊变体） ──
	// 当有 partial 后缀时，仍对已完成音节部分执行精确匹配，
	// 这样 "wobuzhidaog" 中的 "wobuzhidao" 仍能精确匹配 "我不知道"。
	hasExplicitSep := strings.Contains(input, "'")
	if syllableCount > 0 {
		exactInput := completedCode
		if partial == "" {
			exactInput = queryInput // 无 partial 时用完整输入
		}
		exactResults := e.lookupWithFuzzy(exactInput, completedSyllables)
		for _, cand := range exactResults {
			if _, exists := candidatesMap[cand.Text]; exists {
				continue
			}
			c := cand
			charCount := len([]rune(c.Text))
			// 当输入含显式分隔符时，字数不匹配音节数的候选降级为 MatchPartial
			// 例如 xi'an (2 音节)：西安(2字)→MatchExact，见(1字)→MatchPartial
			// 当首段是 partial 时（如 sdem），completed 音节匹配整体降级
			matchType := MatchExact
			if hasExplicitSep && charCount != syllableCount {
				matchType = MatchPartial
			}
			if !firstCompletedIsLeading {
				matchType = MatchPartial
			}
			f := e.buildFeatures(c.Text, float64(c.Weight), matchType, syllableCount, charCount, featureOpts{isPartial: !firstCompletedIsLeading})
			c.Weight = e.scorerWeight(f)
			c.ConsumedLength = allCompletedEnd // 基于 Parser 音节位置精确计算
			candidatesMap[c.Text] = &c
		}
		logDebug("[PinyinEngine] exact match for %q: %d results (partial=%q)", exactInput, len(exactResults), partial)
	}

	// ── 步骤 1b：多切分并行打分 ──
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
				altMatchType := MatchExact
				if !firstCompletedIsLeading {
					altMatchType = MatchPartial
				}
				f := e.buildFeatures(c.Text, float64(c.Weight), altMatchType, len(altSyllables), charCount, featureOpts{segmentRank: 1, isPartial: !firstCompletedIsLeading})
				c.Weight = e.scorerWeight(f)
				// alt 路径的 ConsumedLength 基于其音节覆盖长度，不含 partial 后缀
				c.ConsumedLength = len(altCode)
				if c.ConsumedLength > len(input) {
					c.ConsumedLength = len(input)
				}
				candidatesMap[c.Text] = &c
			}
		}
	}

	// ── 步骤 3：前缀匹配（输入 "wome" 时找到 "women"→我们） ──
	if syllableCount > 0 {
		{
			prefixLimit := 50
			if maxCandidates > 0 {
				prefixLimit = maxCandidates * 2
			}
			prefixResults := e.dict.LookupPrefix(queryInput, prefixLimit)
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

	// ── 步骤 2：子词组查找（如 "nihaoshijie" → 查找 "你好"、"世界" 等子词组） ──
	// 直接使用 Parser 已解析的 completedSyllables，不再冗余重建 DAG。
	// 枚举所有从首位开始的连续子序列，支持部分上屏。
	if syllableCount > 1 {
		e.lookupSubPhrasesEx(completedSyllables, parsed, candidatesMap)
	}

	// ── 步骤 4：单字候选 ──

	// ── 4a. 首段 partial 音节的单字候选 ──
	// 当首个 completed 不是输入首段时（如 sdem → "s" 在 "de" 前），
	// 为首段 partial 音节生成候选，权重高于首 completed 音节的候选
	if syllableCount > 0 && !firstCompletedIsLeading {
		leadingPartial := allSyllables[0]
		possibles := e.syllableTrie.GetPossibleSyllables(leadingPartial)
		const maxLeadingPerSyllable = 5
		for _, syllable := range possibles {
			charResults := e.dict.Lookup(syllable)
			added := 0
			for _, cand := range charResults {
				if added >= maxLeadingPerSyllable {
					break
				}
				if _, exists := candidatesMap[cand.Text]; exists {
					continue
				}
				c := cand
				charCount := len([]rune(c.Text))
				// 首段 partial 使用 MatchExact：用户首先输入的段理应获得最高优先级
				// 这确保 "s" 的候选（世、是等）排在 "de"（的、得等）之前
				f := e.buildFeatures(c.Text, float64(c.Weight), MatchExact, 1, charCount, featureOpts{})
				c.Weight = e.scorerWeight(f)
				c.ConsumedLength = len(leadingPartial)
				candidatesMap[c.Text] = &c
				added++
			}
		}
	}

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
			// 如果首个 completed 不是输入首段（前面有 partial），降级为 MatchPartial
			matchType := MatchExact
			if syllableCount >= 2 || !firstCompletedIsLeading {
				matchType = MatchPartial
			}
			isPartial := !firstCompletedIsLeading
			f := e.buildFeatures(c.Text, float64(c.Weight), matchType, 1, charCount, featureOpts{isPartial: isPartial})
			c.Weight = e.scorerWeight(f)
			// 基于 Parser 位置：消耗到第 1 个已完成音节的结束位置（自动包含前置 partial 段）
			c.ConsumedLength = parsed.ConsumedBytesForCompletedN(1)
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
				// 基于 Parser 位置精确计算：消耗到第 i+1 个已完成音节的结束位置
				c.ConsumedLength = parsed.ConsumedBytesForCompletedN(i + 1)
				candidatesMap[c.Text] = &c
			}
		}
	}

	// ── 4b. 多 partial 音节时的首音节单字候选 ──
	// 例如 "bzd" → ["b","z","d"] 都是 partial，为首音节 "b" 生成单字候选
	if syllableCount == 0 && len(allSyllables) > 1 {
		firstPartial := allSyllables[0]
		possibles := e.syllableTrie.GetPossibleSyllables(firstPartial)
		const maxMultiPartialPerSyllable = 5
		for _, syllable := range possibles {
			charResults := e.dict.Lookup(syllable)
			added := 0
			for _, cand := range charResults {
				if added >= maxMultiPartialPerSyllable {
					break
				}
				if _, exists := candidatesMap[cand.Text]; exists {
					continue
				}
				c := cand
				charCount := len([]rune(c.Text))
				f := e.buildFeatures(c.Text, float64(c.Weight), MatchPartial, 1, charCount, featureOpts{isPartial: true})
				c.Weight = e.scorerWeight(f)
				c.ConsumedLength = len(firstPartial)
				candidatesMap[c.Text] = &c
				added++
			}
		}
	}

	// ── 步骤 5：未完成音节的前缀查找 ──
	if partial != "" {
		{
			prefixResults := e.dict.LookupPrefix(queryInput, 30)
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
		// 每个音节限制候选数量，避免单字符输入（如 "s"）展开过多候选导致超时
		// 每音节取 top 5（按词频降序，dict.Lookup 已排序），确保各音节高频字都能入选
		const maxPerSyllable = 5
		possibles := e.syllableTrie.GetPossibleSyllables(partial)
		for _, syllable := range possibles {
			charResults := e.dict.Lookup(syllable)
			added := 0
			for _, cand := range charResults {
				if added >= maxPerSyllable {
					break
				}
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
				added++
			}
		}
	}

	// ── 步骤 6：简拼/混合简拼词组匹配 ──
	// 纯简拼：bzd → allSyllables=["b","z","d"] → abbrev="bzd"
	// 混合简拼：nizm → allSyllables=["ni","z","m"] → abbrev="nzm"
	if len(allSyllables) >= 2 {
		var abbrevBuilder strings.Builder
		for _, s := range allSyllables {
			abbrevBuilder.WriteByte(s[0])
		}
		abbrevCode := abbrevBuilder.String()

		{
			abbrevResults := e.dict.LookupAbbrev(abbrevCode, 30)
			for _, cand := range abbrevResults {
				c := cand
				charCount := len([]rune(c.Text))
				// 简拼匹配的权重策略：
				// - 纯缩写（如 sfg/bzd，syllableCount=0）：MatchExact（用户明确输入的是首字母）
				// - 有完整音节时（如 dazhongwu → dzw）：MatchPartial（简拼不应压过子词组/精确匹配）
				// 这确保 "dazhongwu" 的子词组 "大众" 不会被简拼 "对自我"(dzw) 压过，
				// 但纯简拼 "sfg" → "司法官" 仍保持高权重。
				abbrevMatchType := MatchExact
				if syllableCount > 0 {
					abbrevMatchType = MatchPartial
				}
				f := e.buildFeatures(c.Text, float64(c.Weight), abbrevMatchType, len(allSyllables), charCount, featureOpts{isAbbrev: true})
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
	wubiStart := time.Now()
	e.addWubiHints(result.Candidates)
	logDebug("[PinyinEngine] wubiHints elapsed=%v", time.Since(wubiStart))

	logDebug("[PinyinEngine] final candidates=%d isEmpty=%v elapsed=%v",
		len(result.Candidates), result.IsEmpty, time.Since(convertStart))

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
