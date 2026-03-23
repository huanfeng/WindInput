// Package mixed 提供五笔拼音混合输入引擎
package mixed

import (
	"log"
	"sort"
	"strings"
	"sync"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
	"github.com/huanfeng/wind_input/internal/engine/wubi"
)

// Config 混输引擎配置
type Config struct {
	MinPinyinLength int  // 拼音最小触发长度，默认2
	WubiWeightBoost int  // 五笔候选权重提升基线，默认10000000
	ShowSourceHint  bool // 是否在 Hint 中标记来源
}

// DefaultConfig 返回默认配置
func DefaultConfig() *Config {
	return &Config{
		MinPinyinLength: 2,
		WubiWeightBoost: 10000000,
		ShowSourceHint:  true,
	}
}

// ConvertResult 混输转换结果
type ConvertResult struct {
	Candidates   []candidate.Candidate
	ShouldCommit bool   // 是否应该自动上屏（来自五笔侧）
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

// Engine 五笔拼音混合输入引擎
// 内部持有独立的五笔引擎和拼音引擎，并行查询后合并候选词。
type Engine struct {
	wubiEngine   *wubi.Engine
	pinyinEngine *pinyin.Engine
	config       *Config
	maxCodeLen   int // 五笔最大码长（通常为4）
}

// NewEngine 创建混输引擎
func NewEngine(wubiEng *wubi.Engine, pinyinEng *pinyin.Engine, config *Config) *Engine {
	if config == nil {
		config = DefaultConfig()
	}
	maxCodeLen := 4
	if wubiEng != nil && wubiEng.GetConfig() != nil {
		maxCodeLen = wubiEng.GetConfig().MaxCodeLength
	}
	return &Engine{
		wubiEngine:   wubiEng,
		pinyinEngine: pinyinEng,
		config:       config,
		maxCodeLen:   maxCodeLen,
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
	if e.wubiEngine != nil {
		e.wubiEngine.Reset()
	}
	if e.pinyinEngine != nil {
		e.pinyinEngine.Reset()
	}
}

// --- ExtendedEngine 接口实现 ---

// GetMaxCodeLength 获取最大码长（取五笔的最大码长）
func (e *Engine) GetMaxCodeLength() int {
	return e.maxCodeLen
}

// ShouldAutoCommit 检查是否应该自动上屏
// 混输模式下由 ConvertEx 内部的五笔引擎 checkAutoCommit 处理，此方法供接口兼容
func (e *Engine) ShouldAutoCommit(input string, candidates []candidate.Candidate) (bool, string) {
	// 五笔的自动上屏逻辑在 wubi.ConvertEx 内部处理（checkAutoCommit），
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
	// 短编码时委托给五笔的空码处理逻辑
	if e.wubiEngine != nil && e.wubiEngine.GetConfig() != nil {
		cfg := e.wubiEngine.GetConfig()
		if cfg.ClearOnEmptyAt4 && len(input) >= cfg.MaxCodeLength {
			return true, false, ""
		}
	}
	return false, false, ""
}

// HandleTopCode 处理顶码
// 混输模式下禁用五笔顶字：超过 maxCodeLen 的输入统一由 ConvertEx 降级为拼音查询，
// 而非触发五笔顶字上屏（用户可能在输入拼音，如 "buyao" → "不要"）。
func (e *Engine) HandleTopCode(input string) (commitText string, newInput string, shouldCommit bool) {
	return "", input, false
}

// --- 核心转换逻辑 ---

// ConvertEx 混输核心转换方法
// 根据输入长度选择查询策略：
//   - 1码：仅查五笔
//   - 2~maxCodeLen码：并行查五笔+拼音，五笔优先
//   - >maxCodeLen码：降级为纯拼音
func (e *Engine) ConvertEx(input string, maxCandidates int) *ConvertResult {
	result := &ConvertResult{}

	if input == "" {
		return result
	}

	input = strings.ToLower(input)
	inputLen := len(input)

	// === 策略分支 ===

	if inputLen > e.maxCodeLen {
		// 超过最大码长：降级为纯拼音
		return e.convertPinyinFallback(input, maxCandidates)
	}

	if inputLen < e.config.MinPinyinLength {
		// 低于拼音触发长度：仅查五笔
		return e.convertWubiOnly(input, maxCandidates)
	}

	// 2~maxCodeLen码：并行查五笔+拼音
	return e.convertMixed(input, maxCandidates)
}

// convertWubiOnly 仅查五笔引擎
func (e *Engine) convertWubiOnly(input string, maxCandidates int) *ConvertResult {
	if e.wubiEngine == nil {
		return &ConvertResult{IsEmpty: true}
	}

	wubiResult := e.wubiEngine.ConvertEx(input, maxCandidates)

	// 标记来源
	for i := range wubiResult.Candidates {
		wubiResult.Candidates[i].Source = candidate.SourceWubi
	}

	return &ConvertResult{
		Candidates:   wubiResult.Candidates,
		ShouldCommit: wubiResult.ShouldCommit,
		CommitText:   wubiResult.CommitText,
		IsEmpty:      wubiResult.IsEmpty,
		ShouldClear:  wubiResult.ShouldClear,
		ToEnglish:    wubiResult.ToEnglish,
	}
}

// convertPinyinFallback 降级为纯拼音查询（输入超过最大码长时）
func (e *Engine) convertPinyinFallback(input string, maxCandidates int) *ConvertResult {
	if e.pinyinEngine == nil {
		return &ConvertResult{IsEmpty: true}
	}

	pinyinResult := e.pinyinEngine.ConvertEx(input, maxCandidates)

	// 标记来源
	for i := range pinyinResult.Candidates {
		pinyinResult.Candidates[i].Source = candidate.SourcePinyin
	}

	result := &ConvertResult{
		Candidates:       pinyinResult.Candidates,
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

// convertMixed 并行查询五笔+拼音，合并候选词
func (e *Engine) convertMixed(input string, maxCandidates int) *ConvertResult {
	var wubiCandidates []candidate.Candidate
	var pinyinCandidates []candidate.Candidate
	var wubiResult *wubi.ConvertResult

	var wg sync.WaitGroup

	// 并行查询五笔
	if e.wubiEngine != nil {
		wg.Add(1)
		go func() {
			defer wg.Done()
			wubiResult = e.wubiEngine.ConvertEx(input, maxCandidates)
			wubiCandidates = wubiResult.Candidates
		}()
	}

	// 并行查询拼音
	if e.pinyinEngine != nil {
		wg.Add(1)
		go func() {
			defer wg.Done()
			pinyinResult := e.pinyinEngine.ConvertEx(input, maxCandidates)
			pinyinCandidates = pinyinResult.Candidates
		}()
	}

	wg.Wait()

	// === 双向夹击权重策略 ===
	//
	// 五笔侧（提权）：
	//   精确匹配(code==input): +10M — 绝对第一层
	//   前缀匹配(code>input):  +6M  — 跨越拼音简拼的 ~4.5M 天花板
	//
	// 拼音侧（纯辅音简拼降权）：
	//   2码简拼: 保持原值 — 高频救急场景（bg→不过, ds→但是）
	//   3码简拼: -2M       — 五笔意图远大于拼音（sfg→上翻盖 降为 ~2.5M）
	//   4码简拼: -3.5M     — 纯噪声压制（wfht→... 降为 ~1M）
	//   含元音输入: 保持原值 — 正常混输
	wubiPrefixBoost := e.config.WubiWeightBoost * 6 / 10 // 6M
	for i := range wubiCandidates {
		wubiCandidates[i].Source = candidate.SourceWubi
		if wubiCandidates[i].Code == input {
			wubiCandidates[i].Weight += e.config.WubiWeightBoost // +10M
		} else {
			wubiCandidates[i].Weight += wubiPrefixBoost // +6M
		}
	}

	hasVowel := containsVowel(input)
	inputLen := len(input)
	for i := range pinyinCandidates {
		pinyinCandidates[i].Source = candidate.SourcePinyin
		// 纯辅音输入时，简拼按长度递减降权
		if !hasVowel && inputLen >= 3 {
			switch {
			case inputLen == 3:
				pinyinCandidates[i].Weight -= 2000000 // 3码简拼 ~4.5M→~2.5M
			default:
				pinyinCandidates[i].Weight -= 3500000 // 4码简拼 ~4.5M→~1M
			}
		}
	}

	// 合并：五笔在前，拼音在后
	merged := make([]candidate.Candidate, 0, len(wubiCandidates)+len(pinyinCandidates))
	merged = append(merged, wubiCandidates...)
	merged = append(merged, pinyinCandidates...)

	// 按权重排序
	sort.Slice(merged, func(i, j int) bool {
		return candidate.Better(merged[i], merged[j])
	})

	// 按文本去重（保留先出现的，即权重高的）
	merged = dedupByText(merged)

	// 截断
	if maxCandidates > 0 && len(merged) > maxCandidates {
		merged = merged[:maxCandidates]
	}

	// 构建结果
	result := &ConvertResult{
		Candidates: merged,
		IsEmpty:    len(merged) == 0,
	}

	// 继承五笔侧的自动上屏状态
	if wubiResult != nil {
		result.ShouldCommit = wubiResult.ShouldCommit
		result.CommitText = wubiResult.CommitText
	}

	// 如果五笔空码但拼音有结果，不标记为空码
	if result.IsEmpty && e.wubiEngine != nil {
		wubiEmpty := wubiResult != nil && wubiResult.IsEmpty
		if wubiEmpty {
			result.ShouldClear = false // 不清空，拼音兜底
		}
	}

	if e.config.ShowSourceHint {
		addSourceHints(result.Candidates)
	}

	log.Printf("[Mixed] input=%s, wubi=%d, pinyin=%d, merged=%d",
		input, len(wubiCandidates), len(pinyinCandidates), len(merged))

	return result
}

// --- 辅助函数 ---

// containsVowel 检查输入是否包含元音字母（a/e/i/o/u/v）
// 有效的拼音输入一定包含元音，纯辅音序列（如 sfg）是五笔编码，无需查拼音。
// v 作为 ü 的替代也算元音（如 nv=女, lv=绿）。
func containsVowel(input string) bool {
	for _, c := range input {
		switch c {
		case 'a', 'e', 'i', 'o', 'u', 'v':
			return true
		}
	}
	return false
}

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
	case candidate.SourceWubi:
		if e.wubiEngine != nil {
			e.wubiEngine.OnCandidateSelected(code, text)
		}
	case candidate.SourcePinyin:
		if e.pinyinEngine != nil {
			e.pinyinEngine.OnCandidateSelected(code, text)
		}
	default:
		// 未标记来源时，默认路由到五笔
		if e.wubiEngine != nil {
			e.wubiEngine.OnCandidateSelected(code, text)
		}
	}
}

// --- Getter ---

// GetWubiEngine 获取内部五笔引擎（供 manager 使用）
func (e *Engine) GetWubiEngine() *wubi.Engine {
	return e.wubiEngine
}

// GetPinyinEngine 获取内部拼音引擎（供 manager 使用）
func (e *Engine) GetPinyinEngine() *pinyin.Engine {
	return e.pinyinEngine
}

// GetConfig 获取混输配置
func (e *Engine) GetConfig() *Config {
	return e.config
}
