// Package mixed 提供码表拼音混合输入引擎
package mixed

import (
	"log/slog"
	"sort"
	"strings"
	"sync"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine/codetable"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
)

const (
	// AbbrevPenalty3 纯简拼3码降权值
	AbbrevPenalty3 = 2000000
	// AbbrevPenalty4Plus 纯简拼4码及以上降权值
	AbbrevPenalty4Plus = 3500000
	// CodetablePrefixBoostRatio 码表前缀匹配提权比例（相对于 CodetableWeightBoost）
	CodetablePrefixBoostRatio = 6 // 即 60%
)

// Config 混输引擎配置
type Config struct {
	MinPinyinLength      int  // 拼音最小触发长度，默认2
	CodetableWeightBoost int  // 码表候选权重提升基线，默认10000000
	ShowSourceHint       bool // 是否在 Hint 中标记来源
	PinyinOnlyOverflow   bool // 超过最大码长时仅查拼音（不查码表前缀），默认 true
}

// DefaultConfig 返回默认配置
func DefaultConfig() *Config {
	return &Config{
		MinPinyinLength:      2,
		CodetableWeightBoost: 10000000,
		ShowSourceHint:       true,
		PinyinOnlyOverflow:   true,
	}
}

// ConvertResult 混输转换结果
type ConvertResult struct {
	Candidates   []candidate.Candidate
	ShouldCommit bool   // 是否应该自动上屏（来自码表侧）
	CommitText   string // 自动上屏的文字
	IsEmpty      bool   // 是否空码
	ShouldClear  bool   // 是否应该清空
	ToEnglish    bool   // 是否转为英文
	NewInput     string // 新的输入（顶码场景）

	// 拼音降级时填充
	PreeditDisplay     string   // 预编辑区显示文本
	CompletedSyllables []string // 已完成的音节
	PartialSyllable    string   // 未完成的音节
	HasPartial         bool     // 是否有未完成音节
	IsPinyinFallback   bool     // 是否为拼音降级模式（>maxCodeLen 时）
}

// Engine 码表拼音混合输入引擎
// 内部持有独立的码表引擎和拼音引擎，并行查询后合并候选词。
type Engine struct {
	codetableEngine *codetable.Engine
	pinyinEngine    *pinyin.Engine
	config          *Config
	maxCodeLen      int               // 码表最大码长（通常为4）
	dictManager     *dict.DictManager // 词库管理器（用于 Shadow 规则访问）
	logger          *slog.Logger

	// 编码反查：从主码表懒构建的反向索引（汉字→编码），用于给拼音候选添加主编码提示
	reverseIndex map[string][]string
}

// NewEngine 创建混输引擎
func NewEngine(codetableEng *codetable.Engine, pinyinEng *pinyin.Engine, config *Config, logger *slog.Logger) *Engine {
	if config == nil {
		config = DefaultConfig()
	}
	if logger == nil {
		logger = slog.Default()
	}
	maxCodeLen := 4
	if codetableEng != nil && codetableEng.GetConfig() != nil {
		maxCodeLen = codetableEng.GetConfig().MaxCodeLength
	}
	return &Engine{
		codetableEngine: codetableEng,
		pinyinEngine:    pinyinEng,
		config:          config,
		maxCodeLen:      maxCodeLen,
		logger:          logger,
	}
}

// --- Engine 接口实现 ---

// Type 返回引擎类型
func (e *Engine) Type() string {
	return "mixed"
}

// Convert 转换输入为候选词（Engine 接口）
func (e *Engine) Convert(input string, maxCandidates int) ([]candidate.Candidate, error) {
	result := e.ConvertEx(input, maxCandidates)
	return result.Candidates, nil
}

// Reset 重置引擎状态
func (e *Engine) Reset() {
	if e.codetableEngine != nil {
		e.codetableEngine.Reset()
	}
	if e.pinyinEngine != nil {
		e.pinyinEngine.Reset()
	}
}

// --- ExtendedEngine 接口实现 ---

// GetMaxCodeLength 获取最大码长（取码表的最大码长）
func (e *Engine) GetMaxCodeLength() int {
	return e.maxCodeLen
}

// ShouldAutoCommit 检查是否应该自动上屏
// 混输模式下由 ConvertEx 内部的五笔引擎 checkAutoCommit 处理，此方法供接口兼容
func (e *Engine) ShouldAutoCommit(input string, candidates []candidate.Candidate) (bool, string) {
	// 码表的自动上屏逻辑在 codetable.ConvertEx 内部处理（checkAutoCommit），
	// 结果通过 ConvertResult.ShouldCommit 返回，无需在此重复
	return false, ""
}

// HandleEmptyCode 处理空码
// 混输模式下，如果输入长度 >= 拼音触发长度，不清空（拼音可能有结果）
func (e *Engine) HandleEmptyCode(input string) (shouldClear bool, toEnglish bool, englishText string) {
	// 如果拼音可能提供候选，不清空
	if len(input) >= e.config.MinPinyinLength {
		return false, false, ""
	}
	// 短编码时委托给码表的空码处理逻辑
	if e.codetableEngine != nil && e.codetableEngine.GetConfig() != nil {
		cfg := e.codetableEngine.GetConfig()
		if cfg.ClearOnEmptyAt4 && len(input) >= cfg.MaxCodeLength {
			return true, false, ""
		}
	}
	return false, false, ""
}

// HandleTopCode 处理顶码
// 混输模式下根据拼音解析质量智能决定是否触发顶码：
//   - 拼音能解析出完整音节（如 "buyao"）→ 不触发顶码，走 ConvertEx 的混合查询
//   - 拼音无法解析为完整音节（纯声母如 "sfght"）→ 触发码表顶码
func (e *Engine) HandleTopCode(input string) (commitText string, newInput string, shouldCommit bool) {
	if len(input) <= e.maxCodeLen {
		return "", input, false
	}

	// 检查拼音引擎能否解析出完整音节
	if e.pinyinEngine != nil {
		pinyinResult := e.pinyinEngine.ConvertEx(input, 1)
		if pinyinResult.HasFullSyllable {
			// 拼音能解析 → 不触发顶码，交给 ConvertEx 处理
			return "", input, false
		}
	}

	// 拼音无法解析（纯声母）→ 委托码表引擎处理顶码
	if e.codetableEngine != nil {
		return e.codetableEngine.HandleTopCode(input)
	}
	return "", input, false
}

// --- 核心转换逻辑 ---

// ConvertEx 混输核心转换方法
// 根据输入长度选择查询策略：
//   - 1码：仅查码表
//   - 2~maxCodeLen码：并行查码表+拼音，码表优先
//   - >maxCodeLen码：码表用前 maxCodeLen 码查询 + 拼音用完整输入查询
func (e *Engine) ConvertEx(input string, maxCandidates int) *ConvertResult {
	result := &ConvertResult{}

	if input == "" {
		return result
	}

	input = strings.ToLower(input)
	inputLen := len(input)

	// === 策略分支 ===

	if inputLen > e.maxCodeLen {
		if e.config.PinyinOnlyOverflow {
			// 超过最大码长：仅查拼音（主流混输行为）
			return e.convertPinyinOnly(input, maxCandidates)
		}
		// 超过最大码长：码表取前 maxCodeLen 码 + 拼音取完整输入
		return e.convertMixedOverflow(input, maxCandidates)
	}

	if inputLen < e.config.MinPinyinLength {
		// 低于拼音触发长度：仅查五笔
		return e.convertCodetableOnly(input, maxCandidates)
	}

	// 2~maxCodeLen码：并行查码表+拼音
	return e.convertMixed(input, maxCandidates)
}

// convertCodetableOnly 仅查码表引擎
func (e *Engine) convertCodetableOnly(input string, maxCandidates int) *ConvertResult {
	if e.codetableEngine == nil {
		return &ConvertResult{IsEmpty: true}
	}

	codetableResult := e.codetableEngine.ConvertEx(input, maxCandidates)

	// 标记来源
	for i := range codetableResult.Candidates {
		codetableResult.Candidates[i].Source = candidate.SourceCodetable
	}

	candidates := codetableResult.Candidates

	// 应用 Shadow 规则（置顶/删除）
	if e.dictManager != nil {
		if shadowLayer := e.dictManager.GetShadowLayer(); shadowLayer != nil {
			rules := shadowLayer.GetShadowRules(input)
			candidates = dict.ApplyShadowPins(candidates, rules)
		}
	}

	return &ConvertResult{
		Candidates:   candidates,
		ShouldCommit: codetableResult.ShouldCommit,
		CommitText:   codetableResult.CommitText,
		IsEmpty:      codetableResult.IsEmpty,
		ShouldClear:  codetableResult.ShouldClear,
		ToEnglish:    codetableResult.ToEnglish,
	}
}

// convertPinyinOnly 超过最大码长时仅查拼音（主流混输行为）
func (e *Engine) convertPinyinOnly(input string, maxCandidates int) *ConvertResult {
	if e.pinyinEngine == nil {
		return &ConvertResult{IsEmpty: true}
	}

	pinyinResult := e.pinyinEngine.ConvertEx(input, maxCandidates)

	for i := range pinyinResult.Candidates {
		pinyinResult.Candidates[i].Source = candidate.SourcePinyin
	}

	candidates := pinyinResult.Candidates

	if e.dictManager != nil {
		if shadowLayer := e.dictManager.GetShadowLayer(); shadowLayer != nil {
			rules := shadowLayer.GetShadowRules(input)
			candidates = dict.ApplyShadowPins(candidates, rules)
		}
	}

	result := &ConvertResult{
		Candidates:       candidates,
		IsEmpty:          pinyinResult.IsEmpty,
		IsPinyinFallback: true,
		PreeditDisplay:   pinyinResult.PreeditDisplay,
	}
	if pinyinResult.Composition != nil {
		result.CompletedSyllables = pinyinResult.Composition.CompletedSyllables
		result.PartialSyllable = pinyinResult.Composition.PartialSyllable
		result.HasPartial = pinyinResult.Composition.HasPartial()
	}
	return result
}

// convertMixedOverflow 超过最大码长时的混合查询
// 码表用前 maxCodeLen 码查询（顶码候选），拼音用完整输入查询，合并竞争。
// 如果拼音有完整音节匹配，标记为拼音降级模式以显示拼音预编辑区。
func (e *Engine) convertMixedOverflow(input string, maxCandidates int) *ConvertResult {
	var codetableCandidates []candidate.Candidate
	var pinyinCandidates []candidate.Candidate
	var pinyinResult *pinyin.PinyinConvertResult

	var wg sync.WaitGroup

	// 码表引擎：用前 maxCodeLen 码查询
	if e.codetableEngine != nil {
		wg.Add(1)
		go func() {
			defer wg.Done()
			prefix := input[:e.maxCodeLen]
			codetableResult := e.codetableEngine.ConvertEx(prefix, maxCandidates)
			codetableCandidates = codetableResult.Candidates
		}()
	}

	// 拼音引擎：用完整输入查询
	if e.pinyinEngine != nil {
		wg.Add(1)
		go func() {
			defer wg.Done()
			pinyinResult = e.pinyinEngine.ConvertEx(input, maxCandidates)
			pinyinCandidates = pinyinResult.Candidates
		}()
	}

	wg.Wait()

	// 码表候选提权（与 convertMixed 相同的策略）
	codetablePrefixBoost := e.config.CodetableWeightBoost * CodetablePrefixBoostRatio / 10
	for i := range codetableCandidates {
		codetableCandidates[i].Source = candidate.SourceCodetable
		if codetableCandidates[i].Code == input[:e.maxCodeLen] {
			codetableCandidates[i].Weight += e.config.CodetableWeightBoost
		} else {
			codetableCandidates[i].Weight += codetablePrefixBoost
		}
	}

	// 拼音候选标记来源
	for i := range pinyinCandidates {
		pinyinCandidates[i].Source = candidate.SourcePinyin
	}

	// 合并
	merged := make([]candidate.Candidate, 0, len(codetableCandidates)+len(pinyinCandidates))
	merged = append(merged, codetableCandidates...)
	merged = append(merged, pinyinCandidates...)

	sort.Slice(merged, func(i, j int) bool {
		return candidate.Better(merged[i], merged[j])
	})
	merged = dedupByText(merged)

	// 应用 Shadow 规则
	if e.dictManager != nil {
		if shadowLayer := e.dictManager.GetShadowLayer(); shadowLayer != nil {
			rules := shadowLayer.GetShadowRules(input)
			merged = dict.ApplyShadowPins(merged, rules)
		}
	}

	if maxCandidates > 0 && len(merged) > maxCandidates {
		merged = merged[:maxCandidates]
	}

	result := &ConvertResult{
		Candidates: merged,
		IsEmpty:    len(merged) == 0,
	}

	// 如果拼音有完整音节，标记为拼音降级模式（预编辑区显示拼音分词）
	if pinyinResult != nil && pinyinResult.HasFullSyllable {
		result.IsPinyinFallback = true
		result.PreeditDisplay = pinyinResult.PreeditDisplay
		if pinyinResult.Composition != nil {
			result.CompletedSyllables = pinyinResult.Composition.CompletedSyllables
			result.PartialSyllable = pinyinResult.Composition.PartialSyllable
			result.HasPartial = pinyinResult.Composition.HasPartial()
		}
	}

	e.addCodeHintsFromCodetable(result.Candidates)
	if e.config.ShowSourceHint {
		addSourceHints(result.Candidates)
	}

	e.logger.Debug("convertMixedOverflow", "input", input, "codetable", len(codetableCandidates), "pinyin", len(pinyinCandidates), "merged", len(merged), "isPinyinFallback", result.IsPinyinFallback)

	return result
}

// convertMixed 并行查询码表+拼音，合并候选词
func (e *Engine) convertMixed(input string, maxCandidates int) *ConvertResult {
	var codetableCandidates []candidate.Candidate
	var pinyinCandidates []candidate.Candidate
	var codetableResult *codetable.ConvertResult

	var wg sync.WaitGroup

	// 并行查询码表
	if e.codetableEngine != nil {
		wg.Add(1)
		go func() {
			defer wg.Done()
			codetableResult = e.codetableEngine.ConvertEx(input, maxCandidates)
			codetableCandidates = codetableResult.Candidates
		}()
	}

	// 并行查询拼音
	var pinyinHasFullSyllable bool
	if e.pinyinEngine != nil {
		wg.Add(1)
		go func() {
			defer wg.Done()
			pinyinResult := e.pinyinEngine.ConvertEx(input, maxCandidates)
			pinyinCandidates = pinyinResult.Candidates
			pinyinHasFullSyllable = pinyinResult.HasFullSyllable
		}()
	}

	wg.Wait()

	// === 双向夹击权重策略 ===
	//
	// 码表侧（提权）：
	//   精确匹配(code==input): +10M — 绝对第一层
	//   前缀匹配(code>input):  +6M  — 跨越拼音简拼的 ~4.5M 天花板
	//
	// 拼音侧（基于解析质量降权）：
	//   拼音含完整音节（如 shi、bao）: 保持原值 — 可能是有效拼音输入
	//   纯简拼（无完整音节，如 sfg、wfht）:
	//     2码: 保持原值 — 高频救急场景（bg→不过, ds→但是）
	//     3码: -2M     — 码表意图远大于拼音（sfg 降为 ~2.5M）
	//     4码: -3.5M   — 纯噪声压制（wfht 降为 ~1M）
	codetablePrefixBoost := e.config.CodetableWeightBoost * CodetablePrefixBoostRatio / 10 // 6M
	for i := range codetableCandidates {
		codetableCandidates[i].Source = candidate.SourceCodetable
		if codetableCandidates[i].Code == input {
			codetableCandidates[i].Weight += e.config.CodetableWeightBoost // +10M
		} else {
			codetableCandidates[i].Weight += codetablePrefixBoost // +6M
		}
	}

	inputLen := len(input)
	for i := range pinyinCandidates {
		pinyinCandidates[i].Source = candidate.SourcePinyin
		// 拼音无完整音节时（纯简拼），按长度递减降权
		if !pinyinHasFullSyllable && inputLen >= 3 {
			switch {
			case inputLen == 3:
				pinyinCandidates[i].Weight -= AbbrevPenalty3 // 3码简拼 ~4.5M→~2.5M
			default:
				pinyinCandidates[i].Weight -= AbbrevPenalty4Plus // 4码简拼 ~4.5M→~1M
			}
		}
	}

	// 合并：码表在前，拼音在后
	merged := make([]candidate.Candidate, 0, len(codetableCandidates)+len(pinyinCandidates))
	merged = append(merged, codetableCandidates...)
	merged = append(merged, pinyinCandidates...)

	// 按权重排序
	sort.Slice(merged, func(i, j int) bool {
		return candidate.Better(merged[i], merged[j])
	})

	// 按文本去重（保留先出现的，即权重高的）
	merged = dedupByText(merged)

	// 统一应用 Shadow 规则（置顶/删除）
	// 子引擎内部各自应用了 Shadow，但合并+重排序后位置被打乱，需要在最终列表上重新应用。
	// ApplyShadowPins 是幂等的：先移除 deleted 词，再按 pin position 分配槽位。
	if e.dictManager != nil {
		if shadowLayer := e.dictManager.GetShadowLayer(); shadowLayer != nil {
			rules := shadowLayer.GetShadowRules(input)
			merged = dict.ApplyShadowPins(merged, rules)
		}
	}

	// 截断
	if maxCandidates > 0 && len(merged) > maxCandidates {
		merged = merged[:maxCandidates]
	}

	// 构建结果
	result := &ConvertResult{
		Candidates: merged,
		IsEmpty:    len(merged) == 0,
	}

	// 继承码表侧的自动上屏状态
	if codetableResult != nil {
		result.ShouldCommit = codetableResult.ShouldCommit
		result.CommitText = codetableResult.CommitText
	}

	// 如果码表空码但拼音有结果，不标记为空码
	if result.IsEmpty && e.codetableEngine != nil {
		codetableEmpty := codetableResult != nil && codetableResult.IsEmpty
		if codetableEmpty {
			result.ShouldClear = false // 不清空，拼音兜底
		}
	}

	e.addCodeHintsFromCodetable(result.Candidates)
	if e.config.ShowSourceHint {
		addSourceHints(result.Candidates)
	}

	e.logger.Debug("convertMixed", "input", input, "codetable", len(codetableCandidates), "pinyin", len(pinyinCandidates), "merged", len(merged))

	return result
}

// --- 辅助函数 ---

var seenPool = sync.Pool{New: func() any { return make(map[string]struct{}, 64) }}

// dedupByText 按候选文本去重，保留先出现的（权重高的优先）
func dedupByText(candidates []candidate.Candidate) []candidate.Candidate {
	seen := seenPool.Get().(map[string]struct{})
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

// addCodeHintsFromCodetable 使用主码表的反向索引为拼音候选添加主编码提示
// 懒构建反向索引，避免在引擎创建时额外加载反查码表
func (e *Engine) addCodeHintsFromCodetable(candidates []candidate.Candidate) {
	if e.codetableEngine == nil {
		return
	}
	// 懒构建反向索引
	if e.reverseIndex == nil {
		ct := e.codetableEngine.GetCodeTable()
		if ct == nil {
			return
		}
		e.reverseIndex = ct.BuildReverseIndex()
	}
	for i := range candidates {
		if candidates[i].Source != candidate.SourcePinyin {
			continue
		}
		codes := e.reverseIndex[candidates[i].Text]
		if len(codes) > 0 {
			candidates[i].Hint = codes[0]
		}
	}
}

// addSourceHints 为混输候选添加来源标记提示
// 仅在拼音候选的 Hint 中添加 "[拼]" 前缀，帮助用户区分
func addSourceHints(candidates []candidate.Candidate) {
	for i := range candidates {
		if candidates[i].Source == candidate.SourcePinyin {
			if candidates[i].Hint == "" {
				candidates[i].Hint = "拼"
			} else {
				candidates[i].Hint = "拼|" + candidates[i].Hint
			}
		}
	}
}

// --- 学习路由 ---

// OnCandidateSelected 选词回调，按来源路由到对应引擎
func (e *Engine) OnCandidateSelected(code, text string, source candidate.CandidateSource) {
	switch source {
	case candidate.SourceCodetable:
		if e.codetableEngine != nil {
			e.codetableEngine.OnCandidateSelected(code, text)
		}
	case candidate.SourcePinyin:
		if e.pinyinEngine != nil {
			e.pinyinEngine.OnCandidateSelected(code, text)
		}
	default:
		// 未标记来源时，默认路由到码表
		if e.codetableEngine != nil {
			e.codetableEngine.OnCandidateSelected(code, text)
		}
	}
}

// --- DictManager ---

// SetDictManager 设置词库管理器（用于 Shadow 规则访问）
func (e *Engine) SetDictManager(dm *dict.DictManager) {
	e.dictManager = dm
}

// --- Getter ---

// GetCodetableEngine 获取内部码表引擎（供 manager 使用）
func (e *Engine) GetCodetableEngine() *codetable.Engine {
	return e.codetableEngine
}

// GetPinyinEngine 获取内部拼音引擎（供 manager 使用）
func (e *Engine) GetPinyinEngine() *pinyin.Engine {
	return e.pinyinEngine
}

// GetConfig 获取混输配置
func (e *Engine) GetConfig() *Config {
	return e.config
}
