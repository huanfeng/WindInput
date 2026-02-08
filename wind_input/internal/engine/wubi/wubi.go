// Package wubi 提供五笔输入法引擎
package wubi

import (
	"log"
	"sort"
	"strings"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
)

// Config 五笔引擎配置
type Config struct {
	MaxCodeLength   int    // 最大码长，默认4
	AutoCommitAt4   bool   // 四码唯一时自动上屏
	ClearOnEmptyAt4 bool   // 四码为空时清空
	TopCodeCommit   bool   // 五码顶字上屏
	PunctCommit     bool   // 标点顶字上屏
	FilterMode      string // 候选过滤模式
	ShowCodeHint    bool   // 是否显示编码提示
	SingleCodeInput bool   // 逐字键入模式（关闭前缀匹配）
	DedupCandidates bool   // 候选去重（内部开关，未来可能开放给用户）
}

// DefaultConfig 返回默认配置
func DefaultConfig() *Config {
	return &Config{
		MaxCodeLength:   4,
		AutoCommitAt4:   false,
		ClearOnEmptyAt4: false,
		TopCodeCommit:   true,
		PunctCommit:     true,
		FilterMode:      "smart",
		ShowCodeHint:    true,
		DedupCandidates: true,
	}
}

// Engine 五笔输入引擎
type Engine struct {
	codeTable   *dict.CodeTable // 主码表
	config      *Config
	dictManager *dict.DictManager // 词库管理器（可选，用于查询用户词和短语）
}

// NewEngine 创建五笔引擎
func NewEngine(config *Config) *Engine {
	if config == nil {
		config = DefaultConfig()
	}
	return &Engine{
		config: config,
	}
}

// LoadCodeTable 加载主码表
func (e *Engine) LoadCodeTable(path string) error {
	ct, err := dict.LoadCodeTable(path)
	if err != nil {
		return err
	}
	e.codeTable = ct

	// 如果码表指定了最大码长，使用码表的设置
	if ct.GetMaxCodeLength() > 0 && ct.GetMaxCodeLength() < e.config.MaxCodeLength {
		e.config.MaxCodeLength = ct.GetMaxCodeLength()
	}

	return nil
}

// ConvertResult 转换结果
type ConvertResult struct {
	Candidates   []candidate.Candidate
	ShouldCommit bool   // 是否应该自动上屏
	CommitText   string // 自动上屏的文字
	IsEmpty      bool   // 是否空码
	ShouldClear  bool   // 是否应该清空
	ToEnglish    bool   // 是否转为英文
}

// Convert 转换输入为候选词
func (e *Engine) Convert(input string, maxCandidates int) ([]candidate.Candidate, error) {
	result := e.ConvertEx(input, maxCandidates)
	return result.Candidates, nil
}

// ConvertRaw 转换输入为候选词（不应用过滤，用于测试）
func (e *Engine) ConvertRaw(input string, maxCandidates int) ([]candidate.Candidate, error) {
	if e.codeTable == nil || input == "" {
		return nil, nil
	}

	input = strings.ToLower(input)
	inputLen := len(input)

	// Phase 1: 收集精确匹配
	var exactCandidates []candidate.Candidate
	if e.dictManager != nil {
		if phraseLayer := e.dictManager.GetPhraseLayer(); phraseLayer != nil {
			// 查询普通短语
			exactCandidates = append(exactCandidates, phraseLayer.Search(input, 0)...)
			// 查询命令（uuid, date 等）——命令仅通过 SearchCommand 访问
			exactCandidates = append(exactCandidates, phraseLayer.SearchCommand(input, 0)...)
		}
		if userDict := e.dictManager.GetUserDict(); userDict != nil {
			exactCandidates = append(exactCandidates, userDict.Search(input, 0)...)
		}
	}
	exactCandidates = append(exactCandidates, e.codeTable.Lookup(input)...)

	// Phase 2: 收集前缀匹配
	var prefixCandidates []candidate.Candidate
	prefixEnabled := !e.config.SingleCodeInput && inputLen >= 1 && inputLen < e.config.MaxCodeLength
	if prefixEnabled {
		if e.dictManager != nil {
			if phraseLayer := e.dictManager.GetPhraseLayer(); phraseLayer != nil {
				for _, c := range phraseLayer.SearchPrefix(input, 0) {
					if c.Code != input {
						prefixCandidates = append(prefixCandidates, c)
					}
				}
			}
			if userDict := e.dictManager.GetUserDict(); userDict != nil {
				for _, c := range userDict.SearchPrefix(input, 0) {
					if c.Code != input {
						prefixCandidates = append(prefixCandidates, c)
					}
				}
			}
		}
		prefixCandidates = append(prefixCandidates, e.codeTable.LookupPrefixExcludeExact(input, 50)...)
	}

	// Phase 3: 处理前缀候选
	for i := range prefixCandidates {
		if e.config.ShowCodeHint && len(prefixCandidates[i].Code) > inputLen {
			prefixCandidates[i].Hint = prefixCandidates[i].Code[inputLen:]
		}
		prefixCandidates[i].Weight -= 2000000
	}

	// Phase 4: 合并 + 去重
	allCandidates := append(exactCandidates, prefixCandidates...)
	if e.config.DedupCandidates {
		allCandidates = dedup(allCandidates)
	}

	if len(allCandidates) == 0 {
		return nil, nil
	}

	// Phase 5: 排序 + 截断
	sort.Sort(candidate.CandidateList(allCandidates))
	if maxCandidates > 0 && len(allCandidates) > maxCandidates {
		allCandidates = allCandidates[:maxCandidates]
	}

	return allCandidates, nil
}

// ConvertEx 扩展转换，返回更多信息
func (e *Engine) ConvertEx(input string, maxCandidates int) *ConvertResult {
	result := &ConvertResult{}

	if input == "" {
		return result
	}

	input = strings.ToLower(input)
	inputLen := len(input)

	// ========== Phase 1: 收集精确匹配 ==========
	var exactCandidates []candidate.Candidate

	if e.dictManager != nil {
		if phraseLayer := e.dictManager.GetPhraseLayer(); phraseLayer != nil {
			// 查询普通短语
			exactCandidates = append(exactCandidates, phraseLayer.Search(input, 0)...)
			// 查询命令（uuid, date 等）——命令仅通过 SearchCommand 访问
			exactCandidates = append(exactCandidates, phraseLayer.SearchCommand(input, 0)...)
		}
		if userDict := e.dictManager.GetUserDict(); userDict != nil {
			exactCandidates = append(exactCandidates, userDict.Search(input, 0)...)
		}
	}

	if e.codeTable != nil {
		exactCandidates = append(exactCandidates, e.codeTable.Lookup(input)...)
	}

	// ========== Phase 2: 收集前缀匹配 ==========
	var prefixCandidates []candidate.Candidate
	prefixEnabled := !e.config.SingleCodeInput && inputLen >= 1 && inputLen < e.config.MaxCodeLength
	if prefixEnabled {
		if e.dictManager != nil {
			if phraseLayer := e.dictManager.GetPhraseLayer(); phraseLayer != nil {
				for _, c := range phraseLayer.SearchPrefix(input, 0) {
					if c.Code != input {
						prefixCandidates = append(prefixCandidates, c)
					}
				}
			}
			if userDict := e.dictManager.GetUserDict(); userDict != nil {
				for _, c := range userDict.SearchPrefix(input, 0) {
					if c.Code != input {
						prefixCandidates = append(prefixCandidates, c)
					}
				}
			}
		}
		if e.codeTable != nil {
			prefixCandidates = append(prefixCandidates, e.codeTable.LookupPrefixExcludeExact(input, 50)...)
		}
	}

	// ========== Phase 3: 处理前缀候选 ==========
	for i := range prefixCandidates {
		if e.config.ShowCodeHint && len(prefixCandidates[i].Code) > inputLen {
			prefixCandidates[i].Hint = prefixCandidates[i].Code[inputLen:]
		}
		prefixCandidates[i].Weight -= 2000000
	}

	// ========== Phase 4: 合并 + 去重 ==========
	allCandidates := append(exactCandidates, prefixCandidates...)
	if e.config.DedupCandidates {
		allCandidates = dedup(allCandidates)
	}

	// 空码处理
	if len(allCandidates) == 0 {
		result.IsEmpty = true
		if e.config.ClearOnEmptyAt4 && inputLen >= e.config.MaxCodeLength {
			result.ShouldClear = true
		}
		return result
	}

	// ========== Phase 5: 排序 + 过滤 + 截断 ==========
	sort.Sort(candidate.CandidateList(allCandidates))

	filterMode := "smart"
	if e.config != nil && e.config.FilterMode != "" {
		filterMode = e.config.FilterMode
	}
	allCandidates = candidate.FilterCandidates(allCandidates, filterMode)

	if maxCandidates > 0 && len(allCandidates) > maxCandidates {
		allCandidates = allCandidates[:maxCandidates]
	}

	result.Candidates = allCandidates

	// 自动上屏检查仅考虑精确匹配数量
	e.checkAutoCommit(result, input, exactCandidates)

	return result
}

// dedup 按 text 去重，保留先出现的（精确匹配优先）
func dedup(candidates []candidate.Candidate) []candidate.Candidate {
	seen := make(map[string]struct{})
	result := make([]candidate.Candidate, 0, len(candidates))
	for _, c := range candidates {
		if _, ok := seen[c.Text]; !ok {
			seen[c.Text] = struct{}{}
			result = append(result, c)
		}
	}
	return result
}

// checkAutoCommit 检查是否满足自动上屏条件
func (e *Engine) checkAutoCommit(result *ConvertResult, input string, candidates []candidate.Candidate) {
	if len(candidates) == 0 {
		return
	}

	inputLen := len(input)
	log.Printf("[Wubi] checkAutoCommit: input=%s, len=%d, candidates=%d, autoCommitAt4=%v, maxCode=%d",
		input, inputLen, len(candidates), e.config.AutoCommitAt4, e.config.MaxCodeLength)

	// 四码唯一时自动上屏
	if e.config.AutoCommitAt4 && inputLen >= e.config.MaxCodeLength && len(candidates) == 1 {
		result.ShouldCommit = true
		result.CommitText = candidates[0].Text
		log.Printf("[Wubi] AutoCommitAt4 triggered: text=%s", result.CommitText)
	} else if e.config.AutoCommitAt4 {
		log.Printf("[Wubi] AutoCommitAt4 NOT triggered: inputLen(%d) >= maxCode(%d)=%v, candidates=%d",
			inputLen, e.config.MaxCodeLength, inputLen >= e.config.MaxCodeLength, len(candidates))
	}
}

// HandleTopCode 处理顶码（五码顶字）
// 当输入第五码时，自动上屏首选并将第五码作为新输入
func (e *Engine) HandleTopCode(input string) (commitText string, newInput string, shouldCommit bool) {
	log.Printf("[Wubi] HandleTopCode: input=%s, topCodeCommit=%v, maxCodeLength=%d",
		input, e.config.TopCodeCommit, e.config.MaxCodeLength)

	if !e.config.TopCodeCommit {
		log.Printf("[Wubi] HandleTopCode: TopCodeCommit is disabled")
		return "", input, false
	}

	if len(input) <= e.config.MaxCodeLength {
		log.Printf("[Wubi] HandleTopCode: input length %d <= maxCodeLength %d, skipping",
			len(input), e.config.MaxCodeLength)
		return "", input, false
	}

	// 取前四码的首选
	prefix := input[:e.config.MaxCodeLength]
	candidates := e.codeTable.Lookup(prefix)
	if len(candidates) == 0 {
		candidates = e.codeTable.LookupPrefix(prefix)
	}

	log.Printf("[Wubi] HandleTopCode: prefix=%s, candidates=%d", prefix, len(candidates))

	if len(candidates) > 0 {
		sort.Sort(candidate.CandidateList(candidates))
		log.Printf("[Wubi] HandleTopCode: commit=%s, newInput=%s", candidates[0].Text, input[e.config.MaxCodeLength:])
		return candidates[0].Text, input[e.config.MaxCodeLength:], true
	}

	log.Printf("[Wubi] HandleTopCode: no candidates found for prefix %s", prefix)
	return "", input, false
}

// Reset 重置引擎状态
func (e *Engine) Reset() {
	// 五笔引擎无状态，无需重置
}

// Type 返回引擎类型
func (e *Engine) Type() string {
	return "wubi"
}

// GetConfig 获取配置
func (e *Engine) GetConfig() *Config {
	return e.config
}

// SetConfig 设置配置
func (e *Engine) SetConfig(config *Config) {
	e.config = config
}

// GetCodeTableInfo 获取码表信息
func (e *Engine) GetCodeTableInfo() *dict.CodeTableHeader {
	if e.codeTable == nil {
		return nil
	}
	header := e.codeTable.Header
	return &header
}

// GetEntryCount 获取词条数量
func (e *Engine) GetEntryCount() int {
	if e.codeTable == nil {
		return 0
	}
	return e.codeTable.EntryCount()
}

// SetDictManager 设置词库管理器
func (e *Engine) SetDictManager(dm *dict.DictManager) {
	e.dictManager = dm
}

// GetDictManager 获取词库管理器
func (e *Engine) GetDictManager() *dict.DictManager {
	return e.dictManager
}
