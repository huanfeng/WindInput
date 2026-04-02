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
	EnableUserFreq     bool   // 启用用户词频学习
	FrequencyOnly      bool   // 仅调频模式：不创建新词，只调整已有词条权重
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
	codeTable   *dict.CodeTable // 主码表
	config      *Config
	dictManager *dict.DictManager // 词库管理器（可选，用于查询用户词和短语）
	logger      *slog.Logger
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
		if userDict := e.dictManager.GetUserDict(); userDict != nil {
			exactCandidates = append(exactCandidates, userDict.Search(input, 0)...)
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
			if userDict := e.dictManager.GetUserDict(); userDict != nil {
				for _, c := range userDict.SearchPrefix(input, 0) {
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
			prefixCandidates[i].Hint = prefixCandidates[i].Code[inputLen:]
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
	sort.Slice(allCandidates, func(i, j int) bool {
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

	// 始终直接查询 codeTable（确保测试和无 CompositeDict 注册场景正常）
	if e.codeTable != nil {
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
		if e.codeTable != nil {
			prefixCandidates = append(prefixCandidates, e.codeTable.LookupPrefixExcludeExact(input, 0)...)
		}
	}

	// ========== Phase 3: 处理前缀候选（code hint + 按剩余码长分层降权）==========
	// 参考 RIME table_translator: 前缀候选 (completion) 整体排在精确匹配之后。
	// 在此基础上按剩余码长分层：剩余1键 > 剩余2键 > 剩余3键，
	// 同层内保持原始 weight 排序。
	for i := range prefixCandidates {
		if e.config.ShowCodeHint && len(prefixCandidates[i].Code) > inputLen {
			prefixCandidates[i].Hint = prefixCandidates[i].Code[inputLen:]
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
	comparator := candidate.Better
	if e.config != nil && e.config.CandidateSortMode == string(candidate.SortByNatural) {
		comparator = candidate.BetterNatural
	}
	sort.Slice(allCandidates, func(i, j int) bool {
		return comparator(allCandidates[i], allCandidates[j])
	})

	// ========== Phase 6: Shadow 拦截器（pin + delete） ==========
	// 在引擎最终排序后统一应用，不修改 weight，只做呈现层位置覆盖和过滤。
	// 混输模式下由外层 MixedEngine 统一应用，此处跳过避免干扰。
	if !e.config.SkipShadow && e.dictManager != nil {
		if shadowLayer := e.dictManager.GetShadowLayer(); shadowLayer != nil {
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

// OnCandidateSelected 用户选词回调（词频学习）
func (e *Engine) OnCandidateSelected(code, text string) {
	if e.config == nil || !e.config.EnableUserFreq {
		return
	}
	if e.dictManager == nil {
		return
	}

	userDict := e.dictManager.GetUserDict()
	if userDict == nil {
		return
	}

	// 单字不自动调频（码表单字靠码表固定顺序）
	if e.config.SkipSingleCharFreq && len([]rune(text)) <= 1 {
		return
	}

	// 首选保护：检查该词在码表中的原始排名
	if e.config.ProtectTopN > 0 && e.codeTable != nil {
		originalRank := e.getOriginalRank(code, text)
		if originalRank >= 0 && originalRank < e.config.ProtectTopN {
			// 码表前 N 位，不需要调频
			return
		}
	}

	if e.config.FrequencyOnly {
		// 仅调频模式：只调整已有词条权重，不创建新词
		userDict.IncreaseWeight(code, text, 20)
		return
	}

	// 自动学习模式：优先使用临时词库
	tempDict := e.dictManager.GetTempDict()
	if tempDict != nil {
		promoted := tempDict.LearnWord(code, text, 20)
		if promoted {
			tempDict.PromoteWord(code, text)
		}
	} else {
		// 没有临时词库时回退到直接写入用户词库
		userDict.OnWordSelected(code, text, 800, 20, 3)
	}
}

// getOriginalRank 获取词在码表中的原始排名（0-based）
// 返回 -1 表示不在码表中
func (e *Engine) getOriginalRank(code, text string) int {
	if e.codeTable == nil {
		return -1
	}
	entries := e.codeTable.Lookup(code)
	for i, entry := range entries {
		if entry.Text == text {
			return i
		}
	}
	return -1
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
