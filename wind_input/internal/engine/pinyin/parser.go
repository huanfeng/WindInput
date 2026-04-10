package pinyin

import "strings"

// ============================================================
// PinyinParser 拼音音节解析器
// 负责将拼音字符串解析为音节序列，支持未完成音节识别
// ============================================================

// PinyinParser 拼音解析器
type PinyinParser struct {
	syllableTrie *SyllableTrie
}

// NewPinyinParser 创建拼音解析器
func NewPinyinParser() *PinyinParser {
	return &PinyinParser{
		syllableTrie: NewSyllableTrie(),
	}
}

// NewPinyinParserWithTrie 使用指定的 Trie 创建解析器
func NewPinyinParserWithTrie(st *SyllableTrie) *PinyinParser {
	return &PinyinParser{
		syllableTrie: st,
	}
}

// Parse 解析拼音输入
// 返回解析结果，包含完整音节和可能的未完成音节
// 支持 ' 显式分隔符，如 "xi'an" 强制在 ' 处断开产出 ["xi", "an"]
func (p *PinyinParser) Parse(input string) *ParseResult {
	if len(input) == 0 {
		return &ParseResult{Input: input}
	}

	input = strings.ToLower(input)
	result := &ParseResult{
		Input:     input,
		Syllables: make([]ParsedSyllable, 0),
	}

	if strings.Contains(input, "'") {
		p.parseWithSeparator(input, result)
		return result
	}

	p.parseSegment(input, 0, result)
	p.checkLastSyllableContinuations(result)

	return result
}

// parseWithSeparator 按 ' 分割输入后逐段解析
func (p *PinyinParser) parseWithSeparator(input string, result *ParseResult) {
	segments := strings.Split(input, "'")
	offset := 0
	for i, seg := range segments {
		if seg == "" {
			if i < len(segments)-1 {
				offset++ // 跳过 ' 字符
			}
			continue
		}
		p.parseSegment(seg, offset, result)
		offset += len(seg)
		if i < len(segments)-1 {
			offset++ // 跳过 ' 字符
		}
	}
	p.checkLastSyllableContinuations(result)
}

// parseSegment 对单个片段执行 DAG 最大匹配并处理尾部字符
func (p *PinyinParser) parseSegment(segment string, baseOffset int, result *ParseResult) {
	dag := BuildDAG(segment, p.syllableTrie)
	mainPath := dag.MaximumMatch()

	coveredEnd := 0
	for _, syllable := range mainPath {
		result.Syllables = append(result.Syllables, ParsedSyllable{
			Text:  syllable,
			Type:  SyllableExact,
			Start: baseOffset + coveredEnd,
			End:   baseOffset + coveredEnd + len(syllable),
		})
		coveredEnd += len(syllable)
	}

	// 迭代处理未被 MaximumMatch 覆盖的字符
	pos := coveredEnd
	for pos < len(segment) {
		// 尝试从当前位置开始用 DP MaximumMatch 处理剩余部分，
		// 避免贪心 MatchPrefixAt 导致歧义切分（如 "hen" 吞掉 "he+ni"）。
		remainder := segment[pos:]
		remainDAG := BuildDAG(remainder, p.syllableTrie)
		remainPath := remainDAG.MaximumMatch()
		if len(remainPath) > 0 {
			for _, syllable := range remainPath {
				result.Syllables = append(result.Syllables, ParsedSyllable{
					Text:  syllable,
					Type:  SyllableExact,
					Start: baseOffset + pos,
					End:   baseOffset + pos + len(syllable),
				})
				pos += len(syllable)
			}
			continue
		}

		// DP 也无法处理（当前位置不是有效音节起点），用 MatchPrefixAt 处理单个音节/字符
		prefix, isComplete, possible := p.syllableTrie.MatchPrefixAt(segment, pos)

		if prefix != "" {
			syllableType := SyllablePartial
			if isComplete {
				syllableType = SyllableExact
			}
			result.Syllables = append(result.Syllables, ParsedSyllable{
				Text:     prefix,
				Type:     syllableType,
				Start:    baseOffset + pos,
				End:      baseOffset + pos + len(prefix),
				Possible: possible,
			})
			pos += len(prefix)
		} else {
			// 单个字符无法匹配任何前缀，作为 partial 音节保留
			result.Syllables = append(result.Syllables, ParsedSyllable{
				Text:  string(segment[pos]),
				Type:  SyllablePartial,
				Start: baseOffset + pos,
				End:   baseOffset + pos + 1,
			})
			pos++
		}
	}
}

// checkLastSyllableContinuations 检查最后一个音节是否有可能的续写
// 这对于如 "ni" 这种音节很重要，因为它可以续写为 "nian", "niang" 等
func (p *PinyinParser) checkLastSyllableContinuations(result *ParseResult) {
	if len(result.Syllables) == 0 {
		return
	}
	lastIdx := len(result.Syllables) - 1
	last := &result.Syllables[lastIdx]
	if last.Type == SyllableExact && len(last.Possible) == 0 {
		possible := p.syllableTrie.GetPossibleSyllables(last.Text)
		var continuations []string
		for _, ps := range possible {
			if ps != last.Text {
				suffix := ps[len(last.Text):]
				if suffix != "" {
					continuations = append(continuations, suffix)
				}
			}
		}
		last.Possible = continuations
	}
}

// ParseWithDetail 解析拼音输入并返回详细信息
// 支持识别多种切分方案
func (p *PinyinParser) ParseWithDetail(input string, maxSegmentations int) *ParseDetailResult {
	if len(input) == 0 {
		return &ParseDetailResult{
			Input:         input,
			Best:          &ParseResult{Input: input},
			Alternatives:  nil,
			PartialSuffix: "",
		}
	}

	input = strings.ToLower(input)
	result := &ParseDetailResult{
		Input:        input,
		Alternatives: make([]*ParseResult, 0),
	}

	// 使用 DAG 进行音节切分
	dag := BuildDAG(input, p.syllableTrie)

	// 获取所有可能的切分路径
	allPaths := dag.AllPaths(maxSegmentations)

	// 计算每条路径覆盖到的位置
	bestCoverage := 0
	var bestPath []string

	for _, path := range allPaths {
		coverage := 0
		for _, s := range path {
			coverage += len(s)
		}
		if coverage > bestCoverage {
			bestCoverage = coverage
			bestPath = path
		}
	}

	// 构建最佳解析结果
	best := &ParseResult{
		Input:     input,
		Syllables: make([]ParsedSyllable, 0),
	}

	pos := 0
	for _, syllable := range bestPath {
		best.Syllables = append(best.Syllables, ParsedSyllable{
			Text:  syllable,
			Type:  SyllableExact,
			Start: pos,
			End:   pos + len(syllable),
		})
		pos += len(syllable)
	}

	// 迭代处理所有未覆盖的尾部字符
	pos = bestCoverage
	for pos < len(input) {
		prefix, isComplete, possible := p.syllableTrie.MatchPrefixAt(input, pos)

		if prefix != "" {
			syllableType := SyllablePartial
			if isComplete {
				syllableType = SyllableExact
			}
			best.Syllables = append(best.Syllables, ParsedSyllable{
				Text:     prefix,
				Type:     syllableType,
				Start:    pos,
				End:      pos + len(prefix),
				Possible: possible,
			})
			result.PartialSuffix = prefix
			pos += len(prefix)
		} else {
			// 单个字符无法匹配任何前缀，作为 partial 音节保留
			best.Syllables = append(best.Syllables, ParsedSyllable{
				Text:  string(input[pos]),
				Type:  SyllablePartial,
				Start: pos,
				End:   pos + 1,
			})
			result.PartialSuffix = string(input[pos])
			pos++
		}
	}

	result.Best = best

	// 构建备选解析结果
	for _, path := range allPaths {
		if pathEqual(path, bestPath) {
			continue
		}
		alt := &ParseResult{
			Input:     input,
			Syllables: make([]ParsedSyllable, 0),
		}
		pos := 0
		for _, syllable := range path {
			alt.Syllables = append(alt.Syllables, ParsedSyllable{
				Text:  syllable,
				Type:  SyllableExact,
				Start: pos,
				End:   pos + len(syllable),
			})
			pos += len(syllable)
		}
		result.Alternatives = append(result.Alternatives, alt)
	}

	return result
}

// ParseDetailResult 详细解析结果
type ParseDetailResult struct {
	Input         string         // 原始输入
	Best          *ParseResult   // 最佳切分方案
	Alternatives  []*ParseResult // 备选切分方案
	PartialSuffix string         // 未完成的后缀部分
}

// pathEqual 比较两个路径是否相同
func pathEqual(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

// QuickParse 快速解析，只返回最佳切分的音节文本列表
// 适用于不需要详细信息的场景
func (p *PinyinParser) QuickParse(input string) []string {
	result := p.Parse(input)
	return result.SyllableTexts()
}

// GetSyllableTrie 获取底层的音节 Trie（用于其他模块共享）
func (p *PinyinParser) GetSyllableTrie() *SyllableTrie {
	return p.syllableTrie
}
