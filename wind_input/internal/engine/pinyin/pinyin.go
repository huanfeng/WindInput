package pinyin

import (
	"sort"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
)

// Config 拼音引擎配置
type Config struct {
	ShowWubiHint bool   // 显示五笔编码提示
	FilterMode   string // 候选过滤模式
}

// Engine 拼音引擎
type Engine struct {
	dict        dict.Dict
	wubiTable   *dict.CodeTable     // 五笔码表（用于反查）
	wubiReverse map[string][]string // 汉字 -> 五笔编码（反向索引）
	config      *Config
}

// NewEngine 创建拼音引擎
func NewEngine(d dict.Dict) *Engine {
	return &Engine{
		dict:   d,
		config: &Config{ShowWubiHint: false, FilterMode: "smart"},
	}
}

// NewEngineWithConfig 创建带配置的拼音引擎
func NewEngineWithConfig(d dict.Dict, config *Config) *Engine {
	if config == nil {
		config = &Config{ShowWubiHint: false, FilterMode: "smart"}
	}
	return &Engine{
		dict:   d,
		config: config,
	}
}

// SetConfig 设置配置
func (e *Engine) SetConfig(config *Config) {
	e.config = config
}

// GetConfig 获取配置
func (e *Engine) GetConfig() *Config {
	return e.config
}

// LoadWubiTable 加载五笔码表（用于反查）
func (e *Engine) LoadWubiTable(path string) error {
	ct, err := dict.LoadCodeTable(path)
	if err != nil {
		return err
	}
	e.wubiTable = ct
	// 构建反向索引
	e.wubiReverse = ct.BuildReverseIndex()
	return nil
}

// lookupWubiCode 查找汉字的五笔编码
func (e *Engine) lookupWubiCode(text string) string {
	if e.wubiReverse == nil {
		return ""
	}

	// 对于词组，查找每个字的编码并拼接
	runes := []rune(text)
	if len(runes) == 0 {
		return ""
	}

	// 单字：直接返回编码
	if len(runes) == 1 {
		codes := e.wubiReverse[text]
		if len(codes) > 0 {
			return codes[0] // 返回第一个编码
		}
		return ""
	}

	// 词组：返回每个字的首码组合（简码提示）
	var result string
	for _, r := range runes {
		codes := e.wubiReverse[string(r)]
		if len(codes) > 0 && len(codes[0]) > 0 {
			result += string(codes[0][0]) // 取首码
		}
	}
	return result
}

// Convert 转换拼音为候选词
func (e *Engine) Convert(input string, maxCandidates int) ([]candidate.Candidate, error) {
	if len(input) == 0 {
		return nil, nil
	}

	// 解析音节
	syllablesList := ParseSyllables(input)
	if len(syllablesList) == 0 {
		return nil, nil
	}

	// 收集所有候选词
	candidatesMap := make(map[string]*candidate.Candidate)

	// 对每一种音节分割方式
	for _, syllables := range syllablesList {
		// 尝试查找完整短语
		phraseCandidates := e.dict.LookupPhrase(syllables)
		for _, cand := range phraseCandidates {
			if existing, ok := candidatesMap[cand.Text]; ok {
				// 如果已存在，保留权重较高的
				if cand.Weight > existing.Weight {
					*existing = cand
				}
			} else {
				c := cand
				candidatesMap[cand.Text] = &c
			}
		}

		// 如果是单个音节，直接查找
		if len(syllables) == 1 {
			singleCandidates := e.dict.Lookup(syllables[0])
			for _, cand := range singleCandidates {
				if existing, ok := candidatesMap[cand.Text]; ok {
					if cand.Weight > existing.Weight {
						*existing = cand
					}
				} else {
					c := cand
					candidatesMap[cand.Text] = &c
				}
			}
		} else {
			// 多音节：尝试组合查找
			// 这里简化处理，实际可以实现更复杂的组合逻辑
			for _, syllable := range syllables {
				singleCandidates := e.dict.Lookup(syllable)
				for _, cand := range singleCandidates {
					// 降低组合词的权重
					cand.Weight = cand.Weight / 2
					if existing, ok := candidatesMap[cand.Text]; ok {
						if cand.Weight > existing.Weight {
							*existing = cand
						}
					} else {
						c := cand
						candidatesMap[cand.Text] = &c
					}
				}
			}
		}
	}

	// 转换为列表并排序
	candidates := make(candidate.CandidateList, 0, len(candidatesMap))
	for _, cand := range candidatesMap {
		candidates = append(candidates, *cand)
	}

	sort.Sort(candidates)

	filterMode := "smart"
	if e.config != nil && e.config.FilterMode != "" {
		filterMode = e.config.FilterMode
	}
	candidates = candidate.FilterCandidates(candidates, filterMode)

	// 限制返回数量
	if maxCandidates > 0 && len(candidates) > maxCandidates {
		candidates = candidates[:maxCandidates]
	}

	// 添加五笔编码提示
	if e.config != nil && e.config.ShowWubiHint && e.wubiReverse != nil {
		for i := range candidates {
			wubiCode := e.lookupWubiCode(candidates[i].Text)
			if wubiCode != "" {
				candidates[i].Hint = wubiCode
			}
		}
	}

	return candidates, nil
}

// ConvertRaw 转换拼音为候选词（不应用过滤，用于测试）
func (e *Engine) ConvertRaw(input string, maxCandidates int) ([]candidate.Candidate, error) {
	if len(input) == 0 {
		return nil, nil
	}

	// 解析音节
	syllablesList := ParseSyllables(input)
	if len(syllablesList) == 0 {
		return nil, nil
	}

	// 收集所有候选词
	candidatesMap := make(map[string]*candidate.Candidate)

	// 对每一种音节分割方式
	for _, syllables := range syllablesList {
		// 尝试查找完整短语
		phraseCandidates := e.dict.LookupPhrase(syllables)
		for _, cand := range phraseCandidates {
			if existing, ok := candidatesMap[cand.Text]; ok {
				if cand.Weight > existing.Weight {
					*existing = cand
				}
			} else {
				c := cand
				candidatesMap[cand.Text] = &c
			}
		}

		// 如果是单个音节，直接查找
		if len(syllables) == 1 {
			singleCandidates := e.dict.Lookup(syllables[0])
			for _, cand := range singleCandidates {
				if existing, ok := candidatesMap[cand.Text]; ok {
					if cand.Weight > existing.Weight {
						*existing = cand
					}
				} else {
					c := cand
					candidatesMap[cand.Text] = &c
				}
			}
		} else {
			// 多音节：尝试组合查找
			for _, syllable := range syllables {
				singleCandidates := e.dict.Lookup(syllable)
				for _, cand := range singleCandidates {
					cand.Weight = cand.Weight / 2
					if existing, ok := candidatesMap[cand.Text]; ok {
						if cand.Weight > existing.Weight {
							*existing = cand
						}
					} else {
						c := cand
						candidatesMap[cand.Text] = &c
					}
				}
			}
		}
	}

	// 转换为列表并排序
	candidates := make(candidate.CandidateList, 0, len(candidatesMap))
	for _, cand := range candidatesMap {
		candidates = append(candidates, *cand)
	}

	sort.Sort(candidates)

	// 不应用过滤！

	// 限制返回数量
	if maxCandidates > 0 && len(candidates) > maxCandidates {
		candidates = candidates[:maxCandidates]
	}

	// 添加五笔编码提示
	if e.config != nil && e.config.ShowWubiHint && e.wubiReverse != nil {
		for i := range candidates {
			wubiCode := e.lookupWubiCode(candidates[i].Text)
			if wubiCode != "" {
				candidates[i].Hint = wubiCode
			}
		}
	}

	return candidates, nil
}

// Reset 重置引擎状态
func (e *Engine) Reset() {
	// 拼音引擎目前无状态，无需重置
}

// Type 返回引擎类型
func (e *Engine) Type() string {
	return "pinyin"
}
