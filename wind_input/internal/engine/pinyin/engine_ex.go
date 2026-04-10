package pinyin

import (
	"strings"
	"time"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine/pinyin/shuangpin"
)

// rimeScore 计算 Rime 风格评分并映射到 int 权重
// text: 候选文本（用于 LM 查询）
// dictWeight: 词库原始权重
// initialQuality: 来源基础偏移（查 initialQuality 值表）
// coverage: 音节覆盖率（consumedSyllableCount / totalSyllableCount）
// charCount: 候选字符数
func (e *Engine) rimeScore(text string, dictWeight float64, initialQuality float64, coverage float64, charCount int) int {
	score := e.rimeScorer.ScoreWithLM(text, dictWeight, initialQuality, coverage, charCount)
	return int(score * 1000000)
}

// ============================================================
// Engine 扩展方法
// 使用新的 Parser → Lexicon → Ranker 流水线
// ============================================================

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

	// ── 双拼预处理：将双拼键序列转换为全拼 ──
	var spResult *shuangpinConvertResult
	originalInput := input // 保存原始双拼输入（用于 ConsumedLength 回映）
	if e.spConverter != nil {
		spResult = e.shuangpinPreprocess(input)
		input = spResult.fullPinyin // 替换为全拼继续处理
		if len(input) == 0 {
			result.IsEmpty = true
			// 如果有 partial，设置预编辑区为声母提示
			if spResult.hasPartial {
				result.PreeditDisplay = spResult.preeditDisplay
			}
			return result
		}
	}
	_ = originalInput // 在后处理中使用

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

	// totalSyllableCount 用于 coverage 计算（Rime 评分模型）
	totalSyllableCount := len(parsed.Syllables)
	if totalSyllableCount == 0 {
		totalSyllableCount = 1 // 防止除零
	}

	// 注意：以下变量来自 parsed（原始解析结果），而非 composition。
	// - completedSyllables: 仅包含 Exact 音节（如 "ni","hao"），不含 partial
	// - allSyllables: 包含所有音节文本（Exact + Partial），用于简拼匹配
	// composition.CompletedSyllables 会把非末尾 partial 提升为 completed（仅用于 UI 显示）
	completedSyllables := parsed.CompletedSyllables()
	syllableCount := len(completedSyllables)
	result.HasFullSyllable = syllableCount > 0
	partial := parsed.PartialSyllable()
	allSyllables := parsed.SyllableTexts()

	// 计算从输入起始位置连续的完成音节（无 partial 间隔）。
	// 只有连续完成音节才能安全地生成候选，否则 ConsumedLength 会跨过中间的 partial 导致输入丢失。
	// 例如："nihao" → contiguous=["ni","hao"]，"nihdao" → contiguous=["ni"]，"lwai" → contiguous=[]
	contiguousSyllables, contiguousEnd := parsed.ContiguousCompletedFromStart()
	contiguousCount := len(contiguousSyllables)

	// allCompletedEnd: 连续完成音节在原始输入中的结束位置（安全消耗范围）
	allCompletedEnd := contiguousEnd

	e.logger.Debug("convertCore", "input", input, "preedit", result.PreeditDisplay,
		"completed", completedSyllables, "contiguous", contiguousSyllables,
		"partial", partial, "allSyllables", allSyllables, "parseElapsed", time.Since(convertStart))

	// 检查首个 completed syllable 是否也是输入的第一个段
	firstCompletedIsLeading := contiguousCount > 0

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
			charCount := len([]rune(c.Text))
			c.Weight = e.rimeScore(c.Text, float64(c.Weight), 100.0, 1.0, charCount)
			c.ConsumedLength = len(input)
			candidatesMap[c.Text] = &c
		}
		if len(cmdResults) > 0 {
			e.logger.Debug("command match", "input", input, "results", len(cmdResults))
		}
	}

	// 使用连续完成音节生成候选的编码（安全范围，不跨越 partial 间隔）
	completedCode := strings.Join(contiguousSyllables, "")

	// ── 步骤 0b：动态规划造句（Poet） ──
	// 参照 Rime Poet：对已完成音节构建词网格，动态规划找最优词序列组合。
	// 触发条件：≥2 连续完成音节 + 有 unigram 模型。
	// ConsumedLength = allCompletedEnd（仅消耗连续完成音节，不跨越 partial 间隔）。
	// 造句结果作为普通候选参与排序，不享有绝对优先——Rime 中造句和精确匹配同级。
	if contiguousCount >= 2 && e.unigram != nil && len(completedCode) >= 4 {
		lattice := BuildLattice(completedCode, e.syllableTrie, e.dict, e.unigram)
		if !lattice.IsEmpty() {
			vResults := ViterbiTopK(lattice, e.bigram, 3)
			for _, vResult := range vResults {
				if vResult == nil || len(vResult.Words) == 0 {
					continue
				}
				sentence := vResult.String()
				if _, exists := candidatesMap[sentence]; exists {
					continue
				}
				charCount := len([]rune(sentence))

				// 检查是否为纯单字拼凑（每个 Word 都是单字）
				allSingleChar := true
				for _, w := range vResult.Words {
					if len([]rune(w)) > 1 {
						allSingleChar = false
						break
					}
				}
				// 短输入（≤3 音节）的纯单字拼凑直接丢弃，不作为候选。
				// 这些组合（如"前他""林歪"）不是真实词组，对用户没有价值。
				// 长句（≥4 音节）保留单字回退作为兜底，确保输入始终有结果。
				if allSingleChar && contiguousCount <= 3 {
					continue
				}
				// 长句中的纯单字拼凑降低优先级
				iq := 4.0
				if allSingleChar {
					iq = 1.0
				}
				coverage := float64(contiguousCount) / float64(totalSyllableCount)
				// 造句的 dictWeight 用 Viterbi 路径的 LogProb 反映整句质量
				// LogProb 通常在 [-30, 0] 范围，映射到 [0, rimeMaxDictWeight] 区间，
				// 与词库归一化权重同尺度，确保 NormalizeWeight 不会截断信息。
				sentenceWeight := (vResult.LogProb + 30.0) / 30.0 * rimeMaxDictWeight
				if sentenceWeight < 0 {
					sentenceWeight = 0
				}
				if sentenceWeight > rimeMaxDictWeight {
					sentenceWeight = rimeMaxDictWeight
				}
				c := candidate.Candidate{
					Text:           sentence,
					Code:           completedCode,
					Weight:         e.rimeScore(sentence, sentenceWeight, iq, coverage, charCount),
					ConsumedLength: allCompletedEnd, // 仅消耗已完成音节，partial 留在 buffer
					// Viterbi 结果来自系统词库/语言模型造句，不应被 smart/common 过滤误删。
					IsCommon: true,
				}
				candidatesMap[sentence] = &c
			}
		}
	}

	// ── 步骤 1：精确匹配完整音节序列的词组（含模糊变体） ──
	// 当有 partial 后缀时，仍对已完成音节部分执行精确匹配，
	// 这样 "wobuzhidaog" 中的 "wobuzhidao" 仍能精确匹配 "我不知道"。
	hasExplicitSep := strings.Contains(input, "'")
	if contiguousCount > 0 {
		exactInput := completedCode
		if partial == "" {
			exactInput = queryInput // 无 partial 时用完整输入
		}
		exactResults := e.lookupWithFuzzy(exactInput, contiguousSyllables)
		for _, cand := range exactResults {
			c := cand
			charCount := len([]rune(c.Text))
			iq := 4.0
			if hasExplicitSep && charCount != contiguousCount {
				iq = 2.0 // 显式分隔符下字数不匹配音节数，降级
			}
			coverage := float64(contiguousCount) / float64(totalSyllableCount)
			c.Weight = e.rimeScore(c.Text, float64(c.Weight), iq, coverage, charCount)
			c.ConsumedLength = allCompletedEnd // 基于 Parser 音节位置精确计算
			// 精确匹配的词频权重最可靠：如果候选已存在（如来自 Viterbi），保留更高权重
			if existing, exists := candidatesMap[c.Text]; exists {
				if c.Weight > existing.Weight {
					candidatesMap[c.Text] = &c
				}
				continue
			}
			candidatesMap[c.Text] = &c
		}
		e.logger.Debug("exact match", "input", exactInput, "results", len(exactResults), "partial", partial)
	}

	// ── 步骤 1b：多切分并行打分 ──
	// 对无显式分隔符的输入，获取备选切分路径的候选
	// 即使有 partial 后缀（如 "xianr"），也对完整音节部分做多切分
	if contiguousCount > 0 && !strings.Contains(input, "'") {
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
				iq := 3.5
				if !firstCompletedIsLeading {
					iq = 2.0
				}
				coverage := float64(len(altSyllables)) / float64(totalSyllableCount)
				c.Weight = e.rimeScore(c.Text, float64(c.Weight), iq, coverage, charCount)
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
	// 当有显式分隔符时跳过：queryInput 去掉了 '，会把 "xi'ande" 当作 "xiande"
	// 匹配到 "显得比"(xiandebi) 等不尊重分隔符边界的结果
	if contiguousCount > 0 && !hasExplicitSep {
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
				coverage := float64(contiguousCount) / float64(totalSyllableCount)
				c.Weight = e.rimeScore(c.Text, float64(c.Weight), 2.0, coverage, charCount)
				c.ConsumedLength = len(input)
				candidatesMap[c.Text] = &c
			}
			e.logger.Debug("prefix match", "input", input, "results", len(prefixResults))
		}
	}

	// ── 步骤 2：子词组查找（如 "nihaoshijie" → 查找 "你好"、"世界" 等子词组） ──
	// 直接使用 Parser 已解析的 completedSyllables，不再冗余重建 DAG。
	// 枚举所有从首位开始的连续子序列，支持部分上屏。
	if contiguousCount > 1 {
		e.lookupSubPhrasesEx(contiguousSyllables, parsed, totalSyllableCount, candidatesMap)
	}

	// ── 步骤 4：单字候选 ──

	// ── 4a. 首段 partial 音节的单字候选 ──
	// 当首个 completed 不是输入首段时（如 sdem → "s" 在 "de" 前），
	// 为首段 partial 音节生成候选，权重高于首 completed 音节的候选
	if contiguousCount == 0 && syllableCount > 0 {
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
				if charCount != 1 {
					continue // 步骤 4a 仅取单字，多字词的 ConsumedLength 无法正确覆盖
				}
				// 步骤 4a：首段 partial 单字，initialQuality=3.0，coverage=1/total
				coverage := 1.0 / float64(totalSyllableCount)
				c.Weight = e.rimeScore(c.Text, float64(c.Weight), 3.0, coverage, charCount)
				c.ConsumedLength = len(leadingPartial)
				candidatesMap[c.Text] = &c
				added++
			}
		}
	}

	// 首个完成音节的单字候选：仅当它是输入首段时生成。
	// 当首段是 partial 时（如 lwai 中 "l" 在 "wai" 前），首个完成音节的单字候选
	// 的 ConsumedLength 会包含前面的 partial，选中后导致 partial 被丢弃。
	// 用户应先通过步骤 4a 处理 leading partial。
	if contiguousCount > 0 {
		firstSyllable := contiguousSyllables[0]
		charResults := e.lookupWithFuzzy(firstSyllable, []string{firstSyllable})

		for _, cand := range charResults {
			if _, exists := candidatesMap[cand.Text]; exists {
				continue
			}
			c := cand
			charCount := len([]rune(c.Text))
			// 步骤 4：首音节单字 initialQuality 按场景区分
			// 单音节输入=4.0，多音节输入=2.5
			iq := 4.0
			if syllableCount >= 2 {
				iq = 2.5
			}
			coverage := 1.0 / float64(totalSyllableCount)
			c.Weight = e.rimeScore(c.Text, float64(c.Weight), iq, coverage, charCount)
			// 基于 Parser 位置：消耗到第 1 个已完成音节的结束位置
			c.ConsumedLength = parsed.ConsumedBytesForCompletedN(1)
			candidatesMap[c.Text] = &c
		}

		// 非首音节单字不再加入初始候选列表。
		// 这些候选（如 linwai 中的"外/歪/崴"）选中后会丢弃前面未确认的音节（lin），
		// 造成用户输入丢失。它们应在用户部分上屏确认首音节后自然出现。
	}

	// ── 4b. 多 partial 音节时的首音节单字候选 ──
	// 例如 "bzd" → ["b","z","d"] 都是 partial，为首音节 "b" 生成单字候选
	if contiguousCount == 0 && len(allSyllables) > 1 {
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
				if charCount != 1 {
					continue // 步骤 4b 仅取单字，多字词的 ConsumedLength 无法正确覆盖
				}
				// 步骤 4b：多 partial 首字，initialQuality=2.0
				coverage := 1.0 / float64(totalSyllableCount)
				c.Weight = e.rimeScore(c.Text, float64(c.Weight), 2.0, coverage, charCount)
				c.ConsumedLength = len(firstPartial)
				candidatesMap[c.Text] = &c
				added++
			}
		}
	}

	// ── 步骤 5：未完成音节的前缀查找 ──
	// 步骤 5 安全条件：trailing partial 紧跟在连续完成音节之后，或是单独的 partial 输入
	if partial != "" && (contiguousCount > 0 || len(allSyllables) == 1) {
		{
			prefixResults := e.dict.LookupPrefix(queryInput, 30)
			for _, cand := range prefixResults {
				if _, exists := candidatesMap[cand.Text]; exists {
					continue
				}
				c := cand
				charCount := len([]rune(c.Text))
				// 步骤 5：partial 前缀词组
				// 单字 initialQuality=1.5，多字词 initialQuality=1.0
				iq := 1.5
				if charCount > 1 {
					iq = 1.0
				}
				coverage := float64(syllableCount) / float64(totalSyllableCount)
				c.Weight = e.rimeScore(c.Text, float64(c.Weight), iq, coverage, charCount)
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
				// 步骤 5：partial 展开单字，initialQuality=0.0（coverage 也清零，是最低优先级候选）
				// 有其他完整音节时 coverage=0 额外压制高频字跨层级
				iq := 0.0
				coverageVal := 0.0
				if otherSyllableCount == 0 {
					// 纯 partial 输入时给予少量 coverage
					coverageVal = 1.0 / float64(totalSyllableCount)
				}
				c.Weight = e.rimeScore(c.Text, float64(c.Weight), iq, coverageVal, charCount)
				c.ConsumedLength = len(input)
				candidatesMap[c.Text] = &c
				added++
			}
		}
	}

	// ── 步骤 6：简拼/混合简拼词组匹配 ──
	// 纯简拼：bzd → allSyllables=["b","z","d"] → abbrev="bzd"
	// 混合简拼：nizm → allSyllables=["ni","z","m"] → abbrev="nzm"
	// 混输模式下可通过 SkipAbbrev 关闭简拼匹配以减少噪声
	// 简拼匹配：仅在全部音节都是 partial 时运行（纯简拼模式，如 "bzd"/"nh"）。
	// 混合输入（如 "lwai"=l+wai）中，全拼音节的首字母不应被当作简拼处理，
	// 否则 "lw" 会匹配"龙王"(long+wang) 但实际输入的 "wai" ≠ "w"，导致 "ai" 丢弃。
	// TODO: 实现简拼+全拼混合匹配策略（如 "ldao" → 匹配第二音节为 "dao" 的词组）
	isPureAbbrev := syllableCount == 0 && len(allSyllables) >= 2
	if isPureAbbrev && !(e.config != nil && e.config.SkipAbbrev) {
		var abbrevBuilder strings.Builder
		for _, s := range allSyllables {
			abbrevBuilder.WriteByte(s[0])
		}
		abbrevCode := abbrevBuilder.String()
		totalSyllables := len(allSyllables)

		{
			abbrevResults := e.dict.LookupAbbrev(abbrevCode, 30)
			for _, cand := range abbrevResults {
				c := cand
				charCount := len([]rune(c.Text))
				// 简拼候选的字数必须等于音节数，确保完整覆盖所有音节。
				// 例如 lwai(2音节) 的简拼 "lw" 只接受 2 字词（如"两位"），
				// 但 ConsumedLength 必须覆盖全部输入，否则会丢弃未匹配的部分。
				// 字数不等于音节数时跳过，避免选中后丢弃编码。
				if charCount != totalSyllables {
					continue
				}
				// 步骤 6：简拼匹配
				// 纯简拼（syllableCount=0）initialQuality=3.0，有完整音节时=1.0
				iq := 3.0
				if syllableCount > 0 {
					iq = 1.0
				}
				// 简拼匹配全部音节首字母，coverage=1.0
				c.Weight = e.rimeScore(c.Text, float64(c.Weight), iq, 1.0, charCount)
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
	// 混输模式下由外层 MixedEngine 统一应用，此处跳过避免干扰。
	if e.config == nil || !e.config.SkipShadow {
		result.Candidates = e.applyShadowRules(input, result.Candidates)
	}

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

	// 9. 添加编码提示
	codeHintStart := time.Now()
	e.addCodeHints(result.Candidates)
	e.logger.Debug("codeHints", "elapsed", time.Since(codeHintStart))

	// 10. 双拼后处理：回映 ConsumedLength + 替换预编辑显示
	if spResult != nil {
		e.shuangpinPostprocess(result, spResult, originalInput)
	}

	e.logger.Debug("final", "candidates", len(result.Candidates), "isEmpty", result.IsEmpty, "elapsed", time.Since(convertStart))

	return result
}

// shuangpinConvertResult 双拼预处理的内部结果
type shuangpinConvertResult struct {
	raw            *shuangpin.ConvertResult
	fullPinyin     string
	preeditDisplay string
	hasPartial     bool
}

// shuangpinPreprocess 双拼→全拼预处理
func (e *Engine) shuangpinPreprocess(input string) *shuangpinConvertResult {
	raw := e.spConverter.Convert(input)
	return &shuangpinConvertResult{
		raw:            raw,
		fullPinyin:     raw.FullPinyin,
		preeditDisplay: raw.PreeditDisplay,
		hasPartial:     raw.HasPartial,
	}
}

// shuangpinPostprocess 双拼后处理：回映 ConsumedLength，替换预编辑显示
func (e *Engine) shuangpinPostprocess(result *PinyinConvertResult, spResult *shuangpinConvertResult, originalInput string) {
	// 替换预编辑显示为双拼转换后的全拼显示
	result.PreeditDisplay = spResult.preeditDisplay

	// 回映所有候选的 ConsumedLength（全拼位置→双拼位置）
	for i := range result.Candidates {
		fpConsumed := result.Candidates[i].ConsumedLength
		result.Candidates[i].ConsumedLength = spResult.raw.MapConsumedLength(fpConsumed)
	}
}

// applyShadowRules 在拼音引擎最终排序后应用 Shadow 拦截器（pin + delete）。
// 拼音只支持置顶（pin position=0）和删除，不支持前移/后移。
// 统一使用 dict.ApplyShadowPins，不修改 weight。
func (e *Engine) applyShadowRules(input string, candidates []candidate.Candidate) []candidate.Candidate {
	if e.dictManager == nil {
		return candidates
	}
	shadowLayer := e.dictManager.GetShadowLayer()
	if shadowLayer == nil {
		return candidates
	}

	// 只查当前 input 编码的规则（不再遍历所有候选 Code，避免误删）
	rules := shadowLayer.GetShadowRules(input)
	return dict.ApplyShadowPins(candidates, rules)
}
