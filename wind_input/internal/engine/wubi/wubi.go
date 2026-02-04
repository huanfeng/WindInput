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

	// 精确匹配
	candidates := e.codeTable.Lookup(input)

	// 如果精确匹配为空，尝试前缀匹配
	if len(candidates) == 0 {
		candidates = e.codeTable.LookupPrefix(input)
	}

	if len(candidates) == 0 {
		return nil, nil
	}

	// 排序（按权重降序）
	sort.Sort(candidate.CandidateList(candidates))

	// 限制数量
	if maxCandidates > 0 && len(candidates) > maxCandidates {
		candidates = candidates[:maxCandidates]
	}

	return candidates, nil
}

// ConvertEx 扩展转换，返回更多信息
func (e *Engine) ConvertEx(input string, maxCandidates int) *ConvertResult {
	result := &ConvertResult{}

	if input == "" {
		return result
	}

	input = strings.ToLower(input)
	inputLen := len(input)

	var candidates []candidate.Candidate

	// 查询策略：
	// 1. 先查上层词库（短语、用户词）- 通过 DictManager 的上层
	// 2. 再查五笔系统码表 - 直接使用 codeTable
	// 这样避免了 DictManager 中其他引擎的系统词库干扰

	// Step 1: 查询上层词库（短语、用户词）
	if e.dictManager != nil {
		// 只查询上层（PhraseLayer、UserDict），不查系统词库
		if phraseLayer := e.dictManager.GetPhraseLayer(); phraseLayer != nil {
			candidates = append(candidates, phraseLayer.Search(input, 0)...)
		}
		if userDict := e.dictManager.GetUserDict(); userDict != nil {
			candidates = append(candidates, userDict.Search(input, 0)...)
		}
	}

	// Step 2: 查询五笔系统码表
	if e.codeTable != nil {
		systemCandidates := e.codeTable.Lookup(input)
		if len(systemCandidates) == 0 {
			systemCandidates = e.codeTable.LookupPrefix(input)
		}
		candidates = append(candidates, systemCandidates...)
	}

	// 空码处理
	if len(candidates) == 0 {
		result.IsEmpty = true
		// 四码为空时清空
		if e.config.ClearOnEmptyAt4 && inputLen >= e.config.MaxCodeLength {
			result.ShouldClear = true
		}
		return result
	}

	// 排序（按权重降序）
	sort.Sort(candidate.CandidateList(candidates))

	filterMode := "smart"
	if e.config != nil && e.config.FilterMode != "" {
		filterMode = e.config.FilterMode
	}
	candidates = candidate.FilterCandidates(candidates, filterMode)

	// 限制数量
	if maxCandidates > 0 && len(candidates) > maxCandidates {
		candidates = candidates[:maxCandidates]
	}

	result.Candidates = candidates

	// 检查自动上屏条件
	e.checkAutoCommit(result, input, candidates)

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
