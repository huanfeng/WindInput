// Package codetable 提供码表输入法引擎
package codetable

import (
	"log/slog"
	"sort"
	"strings"
	"sync"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
)

const (
	// PrefixWeightPenalty 前缀匹配固定降权值（ConvertRaw 使用）
	PrefixWeightPenalty = 2000000
	// PrefixWeightPenaltyPerKey 前缀匹配每剩余键降权值（ConvertEx 使用）
	PrefixWeightPenaltyPerKey = 1000000
)

// LearningStrategy 造词策略接口（避免引擎直接依赖 schema 包）
type LearningStrategy interface {
	OnWordCommitted(code, text string)
}

// Config 码表引擎配置
type Config struct {
	MaxCodeLength      int    // 最大码长，默认4
	AutoCommitAt4      bool   // 四码唯一时自动上屏
	ClearOnEmptyAt4    bool   // 四码为空时清空
	TopCodeCommit      bool   // 五码顶字上屏
	PunctCommit        bool   // 标点顶字上屏
	FilterMode         string // 候选过滤模式
	ShowCodeHint       bool   // 是否显示编码提示
	SingleCodeInput    bool   // 逐字键入模式（关闭前缀匹配）
	DedupCandidates    bool   // 候选去重（内部开关，未来可能开放给用户）
	CandidateSortMode  string // 候选排序模式：frequency（词频）、natural（自然顺序）
	ProtectTopN        int    // 首选保护：前 N 位锁定码表原始顺序
	SkipShadow         bool   // 跳过 Shadow 规则应用（混输模式下由外层统一应用）
	SkipSingleCharFreq bool   // 单字不自动调频
}

// DefaultConfig 返回默认配置
func DefaultConfig() *Config {
	return &Config{
		MaxCodeLength:      4,
		AutoCommitAt4:      false,
		ClearOnEmptyAt4:    false,
		TopCodeCommit:      true,
		PunctCommit:        true,
		FilterMode:         "smart",
		ShowCodeHint:       true,
		DedupCandidates:    true,
		SkipSingleCharFreq: true,
	}
}

// Engine 码表输入引擎
type Engine struct {
	codeTable        *dict.CodeTable // 主码表
	config           *Config
	dictManager      *dict.DictManager // 词库管理器（可选，用于查询用户词和短语）
	freqHandler      *dict.FreqHandler // 词频记录处理器（可选，调频用）
	learningStrategy LearningStrategy  // 造词策略（可选）
	logger           *slog.Logger
}

// NewEngine 创建码表引擎
func NewEngine(config *Config, logger *slog.Logger) *Engine {
	if config == nil {
		config = DefaultConfig()
	}
	if logger == nil {
		logger = slog.Default()
	}
	return &Engine{
		config: config,
		logger: logger,
	}
}

// LoadCodeTable 加载主码表（文本格式）
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

// LoadCodeTableBinary 加载二进制格式码表（mmap 模式）
func (e *Engine) LoadCodeTableBinary(wdbPath string) error {
	ct := dict.NewCodeTable()
	if err := ct.LoadBinary(wdbPath); err != nil {
		return err
	}
	e.codeTable = ct
	return nil
}

// RestoreCodeTableHeader 从 meta 信息恢复 CodeTable 的 Header
func (e *Engine) RestoreCodeTableHeader(header dict.CodeTableHeader) {
	if e.codeTable == nil {
		return
	}
	e.codeTable.Header = header
	if header.CodeLength > 0 && header.CodeLength < e.config.MaxCodeLength {
		e.config.MaxCodeLength = header.CodeLength
	}
}

// GetCodeTable 获取码表（供外部注册到 CompositeDict）
func (e *Engine) GetCodeTable() *dict.CodeTable {
	return e.codeTable
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
	exactCandidates := make([]candidate.Candidate, 0, 32)
	if e.dictManager != nil {
		if phraseLayer := e.dictManager.GetPhraseLayer(); phraseLayer != nil {
			exactCandidates = append(exactCandidates, phraseLayer.Search(input, 0)...)
			exactCandidates = append(exactCandidates, phraseLayer.SearchCommand(input, 0)...)
		}
		if userLayer := e.dictManager.GetStoreUserLayer(); userLayer != nil {
			exactCandidates = append(exactCandidates, userLayer.Search(input, 0)...)
		}
	}
	exactCandidates = append(exactCandidates, e.codeTable.Lookup(input)...)

	// Phase 2: 收集前缀匹配
	prefixCandidates := make([]candidate.Candidate, 0, 64)
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
			if userLayer := e.dictManager.GetStoreUserLayer(); userLayer != nil {
				for _, c := range userLayer.SearchPrefix(input, 0) {
					if c.Code != input {
						prefixCandidates = append(prefixCandidates, c)
					}
				}
			}
		}
		prefixCandidates = append(prefixCandidates, e.codeTable.LookupPrefixExcludeExact(input, 0)...)
	}

	// Phase 3: 处理前缀候选
	for i := range prefixCandidates {
		if e.config.ShowCodeHint && len(prefixCandidates[i].Code) > inputLen {
			prefixCandidates[i].Comment = prefixCandidates[i].Code[inputLen:]
		}
		prefixCandidates[i].Weight -= PrefixWeightPenalty
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
	comparator := candidate.Better
	if e.config != nil && e.config.CandidateSortMode == string(candidate.SortByNatural) {
		comparator = candidate.BetterNatural
	}
	sort.SliceStable(allCandidates, func(i, j int) bool {
		return comparator(allCandidates[i], allCandidates[j])
	})
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
	exactCandidates := make([]candidate.Candidate, 0, 32)

	if e.dictManager != nil {
		// 通过 CompositeDict 查询（包含短语、用户词、系统码表，Shadow 已自动应用）
		compositeDict := e.dictManager.GetCompositeDict()
		exactCandidates = append(exactCandidates, compositeDict.Search(input, 0)...)
		exactCandidates = append(exactCandidates, compositeDict.LookupCommand(input)...)
	}

	// 降级路径：仅当无 DictManager 时直接查询 codeTable（测试场景）
	// 有 DictManager 时系统码表已作为 layer 注册在 CompositeDict 中，无需重复查询
	if e.codeTable != nil && e.dictManager == nil {
		exactCandidates = append(exactCandidates, e.codeTable.Lookup(input)...)
	}

	// ========== Phase 2: 收集前缀匹配 ==========
	prefixCandidates := make([]candidate.Candidate, 0, 64)
	prefixEnabled := !e.config.SingleCodeInput && inputLen >= 1 && inputLen < e.config.MaxCodeLength
	if prefixEnabled {
		if e.dictManager != nil {
			compositeDict := e.dictManager.GetCompositeDict()
			for _, c := range compositeDict.SearchPrefix(input, 0) {
				if c.Code != input {
					prefixCandidates = append(prefixCandidates, c)
				}
			}
		}
		// 降级路径：仅当无 DictManager 时直接查询 codeTable（测试场景）
		if e.codeTable != nil && e.dictManager == nil {
			prefixCandidates = append(prefixCandidates, e.codeTable.LookupPrefixExcludeExact(input, 0)...)
		}
	}

	// ========== Phase 3: 处理前缀候选（code hint + 按剩余码长分层降权）==========
	// 参考 RIME table_translator: 前缀候选 (completion) 整体排在精确匹配之后。
	// 在此基础上按剩余码长分层：剩余1键 > 剩余2键 > 剩余3键，
	// 同层内保持原始 weight 排序。
	for i := range prefixCandidates {
		if e.config.ShowCodeHint && len(prefixCandidates[i].Code) > inputLen {
			prefixCandidates[i].Comment = prefixCandidates[i].Code[inputLen:]
		}
		remaining := len(prefixCandidates[i].Code) - inputLen
		prefixCandidates[i].Weight -= remaining * PrefixWeightPenaltyPerKey
	}

	// ========== Phase 4: 合并 + 去重（Shadow top/delete 已由 CompositeDict 处理）==========
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
	// 排序前记住精确匹配的原始 top-N（用于 ProtectTopN 锁定）
	protectN := 0
	if e.config != nil {
		protectN = e.config.ProtectTopN
	}
	var protectedCandidates []candidate.Candidate
	if protectN > 0 && len(exactCandidates) > 0 {
		n := protectN
		if n > len(exactCandidates) {
			n = len(exactCandidates)
		}
		protectedCandidates = make([]candidate.Candidate, n)
		copy(protectedCandidates, exactCandidates[:n])
	}

	comparator := candidate.Better
	if e.config != nil && e.config.CandidateSortMode == string(candidate.SortByNatural) {
		comparator = candidate.BetterNatural
	}
	sort.SliceStable(allCandidates, func(i, j int) bool {
		return comparator(allCandidates[i], allCandidates[j])
	})

	// ProtectTopN：将原始 top-N 候选回填到固定位置
	// 记录词频但不改变它们的排序位置，保护五笔用户的肌肉记忆
	if len(protectedCandidates) > 0 && len(allCandidates) > 0 {
		allCandidates = applyProtectTopN(allCandidates, protectedCandidates)
	}

	// ========== Phase 6: Shadow 拦截器（pin + delete） ==========
	// 在引擎最终排序后统一应用，不修改 weight，只做呈现层位置覆盖和过滤。
	// 混输模式下由外层 MixedEngine 统一应用，此处跳过避免干扰。
	if !e.config.SkipShadow && e.dictManager != nil {
		if shadowLayer := e.dictManager.GetShadowProvider(); shadowLayer != nil {
			rules := shadowLayer.GetShadowRules(input)
			allCandidates = dict.ApplyShadowPins(allCandidates, rules)
		}
	}

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

// applyProtectTopN 将受保护的候选回填到排序结果的固定位置
// protected 中的候选按原始顺序占据前 N 个位置，其余候选按排序结果填充剩余位置
func applyProtectTopN(sorted, protected []candidate.Candidate) []candidate.Candidate {
	// 构建受保护候选的集合（按 Text 匹配，因为同一词可能权重已变）
	protectedSet := make(map[string]bool, len(protected))
	for _, p := range protected {
		protectedSet[p.Text] = true
	}

	// 从排序结果中提取非受保护的候选（保持排序后的顺序）
	rest := make([]candidate.Candidate, 0, len(sorted))
	for _, c := range sorted {
		if !protectedSet[c.Text] {
			rest = append(rest, c)
		}
	}

	// 合并：受保护候选在前，其余按排序填充
	result := make([]candidate.Candidate, 0, len(sorted))
	result = append(result, protected...)
	result = append(result, rest...)
	return result
}

var seenPool = sync.Pool{New: func() any { return make(map[string]struct{}, 64) }}

// dedup 按 text 去重，保留先出现的（精确匹配优先）
func dedup(candidates []candidate.Candidate) []candidate.Candidate {
	seen := seenPool.Get().(map[string]struct{})
	// 清空复用的 map
	for k := range seen {
		delete(seen, k)
	}
	result := make([]candidate.Candidate, 0, len(candidates))
	for _, c := range candidates {
		if _, ok := seen[c.Text]; !ok {
			seen[c.Text] = struct{}{}
			result = append(result, c)
		}
	}
	seenPool.Put(seen)
	return result
}

// checkAutoCommit 检查是否满足自动上屏条件
func (e *Engine) checkAutoCommit(result *ConvertResult, input string, candidates []candidate.Candidate) {
	if len(candidates) == 0 {
		return
	}

	inputLen := len(input)
	e.logger.Debug("checkAutoCommit", "input", input, "len", inputLen, "candidates", len(candidates), "autoCommitAt4", e.config.AutoCommitAt4, "maxCode", e.config.MaxCodeLength)

	// 达到最大码长且唯一时自动上屏
	if e.config.AutoCommitAt4 && inputLen >= e.config.MaxCodeLength && len(candidates) == 1 {
		result.ShouldCommit = true
		result.CommitText = candidates[0].Text
		e.logger.Debug("AutoCommitAt4 triggered", "text", result.CommitText)
	} else if e.config.AutoCommitAt4 {
		e.logger.Debug("AutoCommitAt4 NOT triggered", "inputLen", inputLen, "maxCode", e.config.MaxCodeLength, "lenGEmax", inputLen >= e.config.MaxCodeLength, "candidates", len(candidates))
	}
}

// HandleTopCode 处理顶码（超过最大码长时顶字）
// 当输入超过最大码长时，自动上屏首选并将多余的码作为新输入
// 通过 ConvertEx 走完整候选流水线，确保顶码结果与用户看到的首选一致
func (e *Engine) HandleTopCode(input string) (commitText string, newInput string, shouldCommit bool) {
	e.logger.Debug("HandleTopCode", "input", input, "topCodeCommit", e.config.TopCodeCommit, "maxCodeLength", e.config.MaxCodeLength)

	if !e.config.TopCodeCommit {
		e.logger.Debug("HandleTopCode: TopCodeCommit is disabled")
		return "", input, false
	}

	if len(input) <= e.config.MaxCodeLength {
		e.logger.Debug("HandleTopCode: input too short, skipping", "inputLen", len(input), "maxCodeLength", e.config.MaxCodeLength)
		return "", input, false
	}

	// 取前 N 码（最大码长），走完整候选流水线（包括用户词、短语、Shadow 规则）
	prefix := input[:e.config.MaxCodeLength]
	result := e.ConvertEx(prefix, 1)

	e.logger.Debug("HandleTopCode", "prefix", prefix, "candidates", len(result.Candidates))

	if len(result.Candidates) > 0 {
		e.logger.Debug("HandleTopCode commit", "commit", result.Candidates[0].Text, "newInput", input[e.config.MaxCodeLength:])
		return result.Candidates[0].Text, input[e.config.MaxCodeLength:], true
	}

	e.logger.Debug("HandleTopCode: no candidates found", "prefix", prefix)
	return "", input, false
}

// Reset 重置引擎状态
func (e *Engine) Reset() {
	// 码表引擎无状态，无需重置
}

// OnCandidateSelected 用户选词回调
// 前置过滤（码表特有） → 调频（FreqHandler） → 造词（LearningStrategy）
func (e *Engine) OnCandidateSelected(code, text string) {
	if e.freqHandler == nil && e.learningStrategy == nil {
		return
	}

	// 前置过滤（码表特有）：单字不调频
	if e.config != nil && e.config.SkipSingleCharFreq && len([]rune(text)) <= 1 {
		return
	}

	// 调频
	if e.freqHandler != nil {
		e.freqHandler.Record(code, text)
	}

	// 造词
	if e.learningStrategy != nil {
		e.learningStrategy.OnWordCommitted(code, text)
	}
}

// Type 返回引擎类型
func (e *Engine) Type() string {
	return "codetable"
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

// SetFreqHandler 设置词频记录处理器
func (e *Engine) SetFreqHandler(h *dict.FreqHandler) {
	e.freqHandler = h
}

// SetLearningStrategy 设置造词策略
func (e *Engine) SetLearningStrategy(ls LearningStrategy) {
	e.learningStrategy = ls
}
