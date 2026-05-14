// Package codetable 提供码表输入法引擎
package codetable

import (
	"log/slog"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
)

const (
	// PrefixWeightPenalty 前缀匹配统一降权值
	PrefixWeightPenalty = 2000000
	// shortCodePerDepthPenalty ShortCodeFirst 模式下每多 1 码追加的降权值
	shortCodePerDepthPenalty = 1000
	// fullCodePhraseBoost 全码词组优先模式下给词组加的权重特权
	fullCodePhraseBoost = 5000000
	// defaultBucketLimit BFS 分桶扫描每层默认上限
	defaultBucketLimit = 30
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
	SingleCodeInput    bool   // 精确匹配模式（关闭前缀匹配）
	SingleCodeComplete bool   // 精确匹配空码补全：精确匹配模式下无候选时，从更长编码中取首个候选
	DedupCandidates    bool   // 候选去重（内部开关，未来可能开放给用户）
	CandidateSortMode  string // 候选排序模式：frequency（词频）、natural（自然顺序）
	ProtectTopN        int    // 首选保护：前 N 位锁定码表原始顺序
	SkipShadow         bool   // 跳过 Shadow 规则应用（混输模式下由外层统一应用）
	SkipSingleCharFreq bool   // 单字不自动调频
	WeightAsOrder      bool   // 权重仅表示同码内排序，前缀匹配时抹平权重差异

	// ---------------- 新增架构字段 ---------------- //
	LoadMode          string // 加载模式: "mmap" (默认), "memory" (全内存，高性能)
	PrefixMode        string // 前缀查找模式: "none" (关闭), "sequential" (顺序扫描), "bfs_bucket" (分层扫描，推荐)
	BucketLimit       int    // 分桶扫描时每层的候选上限
	WeightMode        string // 权重语义: "global_freq" (全局权重), "inner_order" (同码内排序), "auto" (自动探测 HasWeight)
	ShortCodeFirst    bool   // 前缀提示时，对长码施加惩罚，短码优先
	CharsetPreference string // 字符集偏好: "none" (默认), "single_first" (单字优先), "phrase_first" (词组优先), "full_code_phrase_first" (全码词组优先)
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
		SingleCodeComplete: true,
		LoadMode:           "mmap",
		PrefixMode:         "bfs_bucket",
		BucketLimit:        30,
		WeightMode:         "auto",
		ShortCodeFirst:     false,
		CharsetPreference:  "none",
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
	var err error

	if e.config != nil && e.config.LoadMode == "memory" {
		e.logger.Info("使用全内存模式加载码表", "path", wdbPath)
		err = ct.LoadBinaryMemory(wdbPath)
	} else {
		e.logger.Info("使用 mmap 模式加载码表", "path", wdbPath)
		err = ct.LoadBinary(wdbPath)
	}

	if err != nil {
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

	// 性能埋点（详见 engine.EngineTiming，由 ConvertEx 各 Phase 填充）
	Timing *engineTiming
}

// engineTiming 与 engine.EngineTiming 对齐（codetable 包定义本地副本以避免反向依赖）
type engineTiming struct {
	Convert time.Duration
	Exact   time.Duration
	Prefix  time.Duration
	Weight  time.Duration
	Sort    time.Duration
	Shadow  time.Duration
	Filter  time.Duration
}

// TimingFields 暴露 timing 字段给上层（manager 用于回填到 engine.EngineTiming）。
func (t *engineTiming) TimingFields() (convert, exact, prefix, weight, sortDur, shadow, filter time.Duration) {
	if t == nil {
		return
	}
	return t.Convert, t.Exact, t.Prefix, t.Weight, t.Sort, t.Shadow, t.Filter
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
	prefixEnabled := !e.config.SingleCodeInput && e.config.PrefixMode != "none" && inputLen >= 1 && inputLen < e.config.MaxCodeLength
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

		// 判断使用的是旧的 sequential 还是新的 bfs_bucket
		if e.config.PrefixMode == "sequential" {
			prefixCandidates = append(prefixCandidates, e.codeTable.LookupPrefixExcludeExact(input, 0)...)
		} else { // 默认为 bfs_bucket
			limit := e.config.BucketLimit
			if limit <= 0 {
				limit = defaultBucketLimit
			}
			maxDepth := e.config.MaxCodeLength - inputLen
			prefixCandidates = append(prefixCandidates, e.codeTable.LookupPrefixBFS(input, limit, maxDepth)...)
		}
	}

	// Phase 3: 处理前缀候选
	weightMode := e.resolveWeightMode()
	if weightMode == "inner_order" {
		reorderPrefixForInnerOrder(prefixCandidates)
	}
	e.applyPrefixWeights(prefixCandidates, inputLen, weightMode)

	// Phase 3.5: 精确匹配空码补全
	if e.config.SingleCodeInput && e.config.SingleCodeComplete && len(exactCandidates) == 0 && inputLen < e.config.MaxCodeLength {
		var completionCandidates []candidate.Candidate
		if e.dictManager != nil {
			if phraseLayer := e.dictManager.GetPhraseLayer(); phraseLayer != nil {
				for _, c := range phraseLayer.SearchPrefix(input, 1) {
					if c.Code != input {
						completionCandidates = append(completionCandidates, c)
						break
					}
				}
			}
			if len(completionCandidates) == 0 {
				if userLayer := e.dictManager.GetStoreUserLayer(); userLayer != nil {
					for _, c := range userLayer.SearchPrefix(input, 1) {
						if c.Code != input {
							completionCandidates = append(completionCandidates, c)
							break
						}
					}
				}
			}
		}
		if len(completionCandidates) == 0 && e.codeTable != nil {
			completionCandidates = e.codeTable.LookupPrefixExcludeExact(input, 1)
			if len(completionCandidates) > 1 {
				completionCandidates = completionCandidates[:1]
			}
		}
		for i := range completionCandidates {
			if len(completionCandidates[i].Code) > inputLen {
				completionCandidates[i].Comment = completionCandidates[i].Code[inputLen:]
			}
			if weightMode == "inner_order" {
				completionCandidates[i].Weight = -PrefixWeightPenalty
			} else {
				completionCandidates[i].Weight -= PrefixWeightPenalty
			}
		}
		prefixCandidates = append(prefixCandidates, completionCandidates...)
	}

	// Phase 4: 合并 + 去重 + 字符集偏好特权
	allCandidates := append(exactCandidates, prefixCandidates...)
	if e.config.DedupCandidates {
		allCandidates = dedup(allCandidates)
	}
	e.applyCharsetPreference(allCandidates, inputLen)

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
	timing := &engineTiming{}
	result.Timing = timing
	convertStart := time.Now()
	defer func() { timing.Convert = time.Since(convertStart) }()

	if input == "" {
		return result
	}

	input = strings.ToLower(input)
	inputLen := len(input)

	// ========== Phase 1: 收集精确匹配 ==========
	phaseStart := time.Now()
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
	timing.Exact = time.Since(phaseStart)

	// ========== Phase 2: 收集前缀匹配 ==========
	phaseStart = time.Now()
	prefixCandidates := make([]candidate.Candidate, 0, 64)
	prefixEnabled := !e.config.SingleCodeInput && e.config.PrefixMode != "none" && inputLen >= 1 && inputLen < e.config.MaxCodeLength
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
			if e.config.PrefixMode == "sequential" {
				prefixCandidates = append(prefixCandidates, e.codeTable.LookupPrefixExcludeExact(input, 0)...)
			} else { // 默认为 bfs_bucket
				limit := e.config.BucketLimit
				if limit <= 0 {
					limit = defaultBucketLimit
				}
				maxDepth := e.config.MaxCodeLength - inputLen
				prefixCandidates = append(prefixCandidates, e.codeTable.LookupPrefixBFS(input, limit, maxDepth)...)
			}
		}
	}
	timing.Prefix = time.Since(phaseStart)

	// ========== Phase 3: 处理前缀候选（code hint + 统一降权）==========
	// 前缀候选整体排在精确匹配之后，统一降权而不按剩余码长分层。
	// 码表类输入法中编码长度不代表「接近完成」，分层会覆盖词库原始排序信号。
	phaseStart = time.Now()
	weightMode := e.resolveWeightMode()
	if weightMode == "inner_order" {
		reorderPrefixForInnerOrder(prefixCandidates)
	}
	e.applyPrefixWeights(prefixCandidates, inputLen, weightMode)

	// ========== Phase 3.5: 精确匹配空码补全 ==========
	// 精确匹配模式下无候选时，从更长编码中取首个候选作为补全提示
	if e.config.SingleCodeInput && e.config.SingleCodeComplete && len(exactCandidates) == 0 && inputLen < e.config.MaxCodeLength {
		var completionCandidates []candidate.Candidate
		if e.dictManager != nil {
			compositeDict := e.dictManager.GetCompositeDict()
			for _, c := range compositeDict.SearchPrefix(input, 1) {
				if c.Code != input {
					completionCandidates = append(completionCandidates, c)
					break
				}
			}
		}
		if len(completionCandidates) == 0 && e.codeTable != nil && e.dictManager == nil {
			completionCandidates = e.codeTable.LookupPrefixExcludeExact(input, 1)
			if len(completionCandidates) > 1 {
				completionCandidates = completionCandidates[:1]
			}
		}
		// 为补全候选添加编码提示
		for i := range completionCandidates {
			if len(completionCandidates[i].Code) > inputLen {
				completionCandidates[i].Comment = completionCandidates[i].Code[inputLen:]
			}
			if weightMode == "inner_order" {
				completionCandidates[i].Weight = -PrefixWeightPenalty
			} else {
				completionCandidates[i].Weight -= PrefixWeightPenalty
			}
		}
		prefixCandidates = append(prefixCandidates, completionCandidates...)
	}
	timing.Weight = time.Since(phaseStart)

	// ========== Phase 4: 合并 + 去重 + 字符集偏好特权 ==========
	allCandidates := append(exactCandidates, prefixCandidates...)
	if e.config.DedupCandidates {
		allCandidates = dedup(allCandidates)
	}
	e.applyCharsetPreference(allCandidates, inputLen)

	// 空码处理
	if len(allCandidates) == 0 {
		result.IsEmpty = true
		if e.config.ClearOnEmptyAt4 && inputLen >= e.config.MaxCodeLength {
			result.ShouldClear = true
		}
		return result
	}

	// ========== Phase 5: 排序 + 过滤 + 截断 ==========
	phaseStart = time.Now()
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
	timing.Sort = time.Since(phaseStart)

	// ========== Phase 6: Shadow 拦截器（pin + delete） ==========
	// 在引擎最终排序后统一应用，不修改 weight，只做呈现层位置覆盖和过滤。
	// 混输模式下由外层 MixedEngine 统一应用，此处跳过避免干扰。
	phaseStart = time.Now()
	if !e.config.SkipShadow && e.dictManager != nil {
		if shadowLayer := e.dictManager.GetShadowProvider(); shadowLayer != nil {
			rules := shadowLayer.GetShadowRules(input)
			allCandidates = dict.ApplyShadowPins(allCandidates, rules)
		}
	}
	timing.Shadow = time.Since(phaseStart)

	phaseStart = time.Now()
	filterMode := "smart"
	if e.config != nil && e.config.FilterMode != "" {
		filterMode = e.config.FilterMode
	}
	allCandidates = candidate.FilterCandidates(allCandidates, filterMode)

	if maxCandidates > 0 && len(allCandidates) > maxCandidates {
		allCandidates = allCandidates[:maxCandidates]
	}
	timing.Filter = time.Since(phaseStart)

	result.Candidates = allCandidates

	// 自动上屏检查：对精确匹配也应用过滤模式，确保智能模式下生僻字不影响计数
	filteredExact := candidate.FilterCandidates(exactCandidates, filterMode)
	e.checkAutoCommit(result, input, filteredExact)

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

// resolveWeightMode 解析最终生效的 WeightMode：
// 1. WeightAsOrder=true 视为强制 inner_order（向后兼容）。
// 2. WeightMode="auto" 时根据词库 HasWeight 标记自动选择 inner_order 或 global_freq。
// 3. 其它显式值（"global_freq" / "inner_order"）按字面意义返回。
func (e *Engine) resolveWeightMode() string {
	if e.config.WeightAsOrder {
		return "inner_order"
	}
	mode := e.config.WeightMode
	if mode == "auto" || mode == "" {
		if e.codeTable != nil && !e.codeTable.Header.HasWeight {
			return "inner_order"
		}
		return "global_freq"
	}
	return mode
}

// applyPrefixWeights 对前缀候选统一施加降权与可选的短码梯度惩罚。
func (e *Engine) applyPrefixWeights(prefixCandidates []candidate.Candidate, inputLen int, weightMode string) {
	for i := range prefixCandidates {
		if e.config.ShowCodeHint && len(prefixCandidates[i].Code) > inputLen {
			prefixCandidates[i].Comment = prefixCandidates[i].Code[inputLen:]
		}
		if weightMode == "inner_order" {
			prefixCandidates[i].Weight = -PrefixWeightPenalty
		} else {
			prefixCandidates[i].Weight -= PrefixWeightPenalty
		}
		if e.config.ShortCodeFirst {
			depth := len(prefixCandidates[i].Code) - inputLen
			if depth > 0 {
				prefixCandidates[i].Weight -= depth * shortCodePerDepthPenalty
			}
		}
	}
}

// applyCharsetPreference 在最终合并候选上应用字符集偏好特权。
//
// 支持三种模式：
//   - single_first: 任意码长下，单字权重抬高，确保排在词组之前。
//   - phrase_first: 任意码长下，词组权重抬高，确保排在单字之前。
//   - full_code_phrase_first: 仅在满码（inputLen == MaxCodeLength）时，词组权重抬高。
func (e *Engine) applyCharsetPreference(candidates []candidate.Candidate, inputLen int) {
	switch e.config.CharsetPreference {
	case "single_first":
		for i := range candidates {
			if len([]rune(candidates[i].Text)) == 1 {
				candidates[i].Weight += fullCodePhraseBoost
			}
		}
	case "phrase_first":
		for i := range candidates {
			if len([]rune(candidates[i].Text)) > 1 {
				candidates[i].Weight += fullCodePhraseBoost
			}
		}
	case "full_code_phrase_first":
		if inputLen != e.config.MaxCodeLength {
			return
		}
		for i := range candidates {
			if len([]rune(candidates[i].Text)) > 1 {
				candidates[i].Weight += fullCodePhraseBoost
			}
		}
	}
}

// reorderPrefixForInnerOrder 在 inner_order 语义下原地重排前缀候选：
// 同 code 内按 Weight desc（重码序号大者优先），跨 code 按 group 最小 NaturalOrder asc（即词库文件顺序）。
// 同时把每个候选的 NaturalOrder 改写为递增整数，确保后续 Better/BetterNatural
// 在 Weight 被统一降权后仍能稳定输出此顺序。
func reorderPrefixForInnerOrder(candidates []candidate.Candidate) {
	if len(candidates) <= 1 {
		return
	}
	type groupInfo struct {
		minNO int
		idxs  []int
	}
	groupMap := make(map[string]*groupInfo, 16)
	groups := make([]*groupInfo, 0, 16)
	for i, c := range candidates {
		g, ok := groupMap[c.Code]
		if !ok {
			g = &groupInfo{minNO: c.NaturalOrder}
			groupMap[c.Code] = g
			groups = append(groups, g)
		} else if c.NaturalOrder < g.minNO {
			g.minNO = c.NaturalOrder
		}
		g.idxs = append(g.idxs, i)
	}
	sort.SliceStable(groups, func(i, j int) bool {
		return groups[i].minNO < groups[j].minNO
	})
	reordered := make([]candidate.Candidate, 0, len(candidates))
	nextNO := 0
	for _, g := range groups {
		items := make([]candidate.Candidate, len(g.idxs))
		for j, idx := range g.idxs {
			items[j] = candidates[idx]
		}
		sort.SliceStable(items, func(i, j int) bool {
			if items[i].Weight != items[j].Weight {
				return items[i].Weight > items[j].Weight
			}
			return items[i].NaturalOrder < items[j].NaturalOrder
		})
		for _, it := range items {
			it.NaturalOrder = nextNO
			nextNO++
			reordered = append(reordered, it)
		}
	}
	copy(candidates, reordered)
}

var seenPool = sync.Pool{New: func() any { return make(map[string]struct{}, 64) }}

// dedup 按 text 去重，保留先出现的。
// 调用前提：exactCandidates 必须在 prefixCandidates 之前合入，
// 这样"先出现"天然等价于"精确匹配优先于前缀匹配"，无需额外替换分支。
func dedup(candidates []candidate.Candidate) []candidate.Candidate {
	seen := seenPool.Get().(map[string]struct{})
	for k := range seen {
		delete(seen, k)
	}
	result := make([]candidate.Candidate, 0, len(candidates))
	for _, c := range candidates {
		if _, ok := seen[c.Text]; ok {
			continue
		}
		seen[c.Text] = struct{}{}
		result = append(result, c)
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

	skipFreq := e.config != nil && e.config.SkipSingleCharFreq && len([]rune(text)) <= 1

	// 调频（单字可配置跳过）
	if e.freqHandler != nil && !skipFreq {
		e.freqHandler.Record(code, text)
	}

	// 造词（不受 SkipSingleCharFreq 影响，自动造词需要追踪单字序列）
	if e.learningStrategy != nil {
		e.learningStrategy.OnWordCommitted(code, text)
	}
}

// OnPhraseTerminated 短语终止信号，转发给造词策略（如果支持）
func (e *Engine) OnPhraseTerminated() {
	if e.learningStrategy == nil {
		return
	}
	type phraseTerminator interface {
		OnPhraseTerminated()
	}
	if pt, ok := e.learningStrategy.(phraseTerminator); ok {
		pt.OnPhraseTerminated()
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
