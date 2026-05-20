// Package mixed 提供码表拼音混合输入引擎
package mixed

import (
	"log/slog"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine/codetable"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
	"github.com/huanfeng/wind_input/pkg/encoding"
)

const (
	// AbbrevPenalty3 纯简拼3码降权值
	AbbrevPenalty3 = 2000000
	// AbbrevPenalty4Plus 纯简拼4码及以上降权值
	AbbrevPenalty4Plus = 3500000
	// CodetablePrefixBoostRatio 码表前缀匹配提权比例（相对于 CodetableWeightBoost）
	CodetablePrefixBoostRatio = 6 // 即 60%
	// PhraseWeightBoost 短语候选独立 tier 的 boost 基线。
	//
	// 设计 (docs/design/command-bar-followup.md §2.2):
	//   Codetable tier  +10,000,000  码表词 (Code==input 的精确匹配)
	//   Phrase tier     + 1,000,000  短语 / cmdbar 命令 (本常量)
	//   Partial tier    +   500,000  拆分组合候选 (Code!=input, 见 PartialMatchBoost)
	//   Pinyin tier              0  拼音候选
	//
	// PhraseLayer 候选会被 compositeDict 混入 codetableCandidates 切片;
	// mixed engine 在应用 codetable boost 之前按 IsPhrase 把它们分离, 仅加
	// PhraseWeightBoost, 让短语永远 > 拼音、永远 < 码表词。这样 phrase
	// weight 本身 (默认 1000) 只决定短语 tier 内部的相对顺序, 不参与跨 tier 比较。
	PhraseWeightBoost = 1000000

	// PartialMatchBoost 拆分组合候选 (Code!=input 的码表前缀匹配) 的 boost。
	//
	// 设计 (2026-05-17 引入, fix(mixed) 拆分组合 tier):
	//
	//   原则: 拆分组合可信度 < 原生候选。
	//   "Code==input" 的精确匹配是用户最确定意图, 走 Codetable tier (+10M);
	//   "Code!=input" 即输入需要再拆分才能命中码表前缀的候选 (典型场景:
	//   用户输入 "date" 没有完整码表 / 短语命中, 引擎拿"d→大"作为前缀
	//   补全), 这类候选可信度低于 phrase 精确命中, 因此独立到一个介于
	//   pinyin 与 phrase 之间的 tier (+500K), 让 phrase tier (+1M) 优先。
	//
	// 兼容性: 原来这个分支用 codetablePrefixBoost (≈6M) 把它送进 codetable
	// tier 顶部, 导致 "date" 输入下 phrase 永远抢不过拆分出来的 "大"。
	// 改为独立低 tier 后短语全码匹配自然优先。
	PartialMatchBoost = 500000

	// PinyinTierScale 拼音候选 weight 归一化系数 (2026-05-17 fix Bug 3)。
	//
	// 拼音引擎内部 weight = rimeScore × 1_000_000 (0~10M), scale 跟混输
	// tier 设计冲突 — 直接合并会让拼音 partial (典型 3M) 远超 phrase tier
	// (1M+) 和 partial tier (500K+), 短语全码匹配永远抢不过拼音 partial。
	//
	// mixed engine 在合并前 ÷ PinyinTierScale, 把拼音 weight 归一化到 0~100K
	// (pinyin tier), 与 phrase / codetable tier 严格隔离, 同时保留拼音内部
	// 相对排序 (精度损失仅最低两位)。
	//
	// 完整 tier 设计:
	//   Codetable tier  +10,000,000 ~ +10,010,000
	//   Phrase tier     + 1,000,000 ~ + 1,010,000
	//   Partial tier    +   500,000 ~ +   510,000
	//   Pinyin tier              0 ~     100,000  (拼音 weight / 100)
	PinyinTierScale = 100
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

// mixedTiming 混输引擎各阶段耗时。
// 并行子引擎记录墙钟时间，而非内部细分——并行执行时总耗时 ≈ max(码表, 拼音)。
type mixedTiming struct {
	Convert   time.Duration // ConvertEx 总耗时
	Codetable time.Duration // 码表子引擎墙钟
	Pinyin    time.Duration // 拼音子引擎墙钟
	Merge     time.Duration // 合并 + 去重 + 排序
	Shadow    time.Duration // Shadow 规则应用
}

// TimingFields 暴露 timing 字段给上层（manager 用于回填到 engine.EngineTiming）。
// 混输引擎将 Codetable→Exact、Pinyin→Prefix 映射，方便前端横向对比。
func (t *mixedTiming) TimingFields() (convert, exact, prefix, weight, sortDur, shadow, filter time.Duration) {
	if t == nil {
		return
	}
	return t.Convert, t.Codetable, t.Pinyin, 0, t.Merge, t.Shadow, 0
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
	FullPinyinInput    string   // 双拼模式下的全拼字符串（用于 preedit 校验）

	// 性能埋点（详见 mixedTiming，由 ConvertEx 各阶段填充）
	Timing *mixedTiming
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
	pinyinParser    *pinyin.PinyinParser // 拼音解析器（用于顶码时判断输入是否为合法拼音序列）

	// 英文候选
	enableEnglish   bool
	englishSearchFn func(prefix string, limit int) []candidate.Candidate

	// 编码反查：从主码表懒构建的单字反向索引（汉字→编码），用于给拼音候选添加主编码提示。
	// reverseIndex 只存单字，多字词编码通过 encoderRules 在线推导并经词库验证后使用。
	reverseIndex    map[string][]string
	encoderRules    []encoding.Rule          // 主码表编码规则，由 manager 在引擎创建后注入
	codeHintEncoder *encoding.ReverseEncoder // 懒构建：首次使用反查时创建
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
	if codetableEng != nil && codetableEng.GetConfig() != nil &&
		codetableEng.GetConfig().MaxCodeLength > 0 {
		maxCodeLen = codetableEng.GetConfig().MaxCodeLength
	}
	return &Engine{
		codetableEngine: codetableEng,
		pinyinEngine:    pinyinEng,
		config:          config,
		maxCodeLen:      maxCodeLen,
		logger:          logger,
		pinyinParser:    pinyin.NewPinyinParser(),
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
	// 短编码时委托给码表的空码处理逻辑（带 HasLongerCode 守护，避免吞掉长码精确匹配）
	if e.codetableEngine != nil && e.codetableEngine.GetConfig() != nil {
		cfg := e.codetableEngine.GetConfig()
		if cfg.ClearOnEmptyAt4 && len(input) >= cfg.MaxCodeLength &&
			!e.codetableEngine.HasLongerCode(input) {
			return true, false, ""
		}
	}
	return false, false, ""
}

// HandleTopCode 处理顶码
// 混输模式下：先检查前 maxCodeLen 码是否构成合法拼音序列，
// 若是则抑制顶码（用户可能在输入拼音，如 yans→yan+se=颜色）；
// 若不是合法拼音（如 rcqn）则委托码表引擎执行顶码上屏。
func (e *Engine) HandleTopCode(input string) (commitText string, newInput string, shouldCommit bool) {
	if len(input) <= e.maxCodeLen {
		return "", input, false
	}

	// 检查前 N 码是否为合法拼音序列：如果是，抑制顶码
	if e.isPossiblePinyinSequence(input[:e.maxCodeLen]) {
		e.logger.Debug("HandleTopCode: prefix is valid pinyin, suppress top-code",
			"prefix", input[:e.maxCodeLen])
		return "", input, false
	}

	// 非拼音序列，委托码表引擎执行顶码
	if e.codetableEngine != nil {
		return e.codetableEngine.HandleTopCode(input)
	}
	return "", input, false
}

// isPossiblePinyinSequence 判断输入是否构成合法的拼音序列。
// 判定条件（满足任一即为 true）：
//  1. 整个输入是某个合法音节的前缀（如 "zhon" 是 "zhong" 的前缀），且长度 >= 2
//  2. 从起始位置有连续的完整拼音音节（首音节长度 >= 2，过滤单字母简拼），
//     且剩余尾部字符是合法的音节前缀
//
// 例如：
//   - "yans" → yan(完整) + s(前缀) → true
//   - "zhon" → 整体是 zhong 的前缀 → true
//   - "rcqn" → 无完整音节，也非音节前缀 → false
//   - "wang" → wang(完整) → true
//   - "gggg" → 无完整音节（g 不是合法拼音） → false
func (e *Engine) isPossiblePinyinSequence(prefix string) bool {
	if e.pinyinParser == nil {
		return false
	}

	trie := e.pinyinParser.GetSyllableTrie()

	// 条件1：整个输入本身是某个合法音节的前缀（如 zhon→zhong）
	// 长度 >= 2 过滤单字母前缀（如 "a"、"g"）
	if len(prefix) >= 2 && trie.HasPrefix(prefix) {
		return true
	}

	// 条件2：连续完整音节 + 合法尾部前缀
	parsed := e.pinyinParser.Parse(prefix)
	if len(parsed.Syllables) == 0 {
		return false
	}

	completedSyllables, endPos := parsed.ContiguousCompletedFromStart()
	if len(completedSyllables) == 0 {
		return false
	}
	if len(completedSyllables[0]) < 2 {
		// 首音节为单字母（如 a/e/o），不算有效的拼音序列
		return false
	}

	// 完整音节已覆盖全部输入
	if endPos >= len(prefix) {
		return true
	}

	// 剩余部分必须是合法的音节前缀（如 "s" 可续写为 se/si/su 等）
	remainder := prefix[endPos:]
	return trie.HasPrefix(remainder)
}

// --- 核心转换逻辑 ---

// ConvertEx 混输核心转换方法
// 根据输入长度选择查询策略：
//   - 1码：仅查码表
//   - 2~maxCodeLen码：并行查码表+拼音，码表优先
//   - >maxCodeLen码：码表用前 maxCodeLen 码查询 + 拼音用完整输入查询
func (e *Engine) ConvertEx(input string, maxCandidates int) *ConvertResult {
	convertStart := time.Now()
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
			result = e.convertPinyinOnly(input, maxCandidates)
		} else {
			// 超过最大码长：码表取前 maxCodeLen 码 + 拼音取完整输入
			result = e.convertMixedOverflow(input, maxCandidates)
		}
	} else if inputLen < e.config.MinPinyinLength {
		// 低于拼音触发长度：仅查五笔
		result = e.convertCodetableOnly(input, maxCandidates)
	} else {
		// 2~maxCodeLen码：并行查码表+拼音
		result = e.convertMixed(input, maxCandidates)
	}

	if result.Timing != nil {
		result.Timing.Convert = time.Since(convertStart)
	} else {
		result.Timing = &mixedTiming{Convert: time.Since(convertStart)}
	}
	return result
}

// convertCodetableOnly 仅查码表引擎
func (e *Engine) convertCodetableOnly(input string, maxCandidates int) *ConvertResult {
	if e.codetableEngine == nil {
		return &ConvertResult{IsEmpty: true}
	}

	ctStart := time.Now()
	codetableResult := e.codetableEngine.ConvertEx(input, maxCandidates)
	ctElapsed := time.Since(ctStart)

	// 标记来源
	for i := range codetableResult.Candidates {
		codetableResult.Candidates[i].Source = candidate.SourceCodetable
	}

	candidates := codetableResult.Candidates

	// 英文候选（码表模式下也可显示英文）
	if e.enableEnglish && e.englishSearchFn != nil {
		englishCandidates := e.englishSearchFn(input, maxCandidates)
		for i := range englishCandidates {
			englishCandidates[i].Source = candidate.SourceEnglish
		}
		candidates = append(candidates, englishCandidates...)
	}

	// 应用 Shadow 规则（置顶/删除）
	shadowStart := time.Now()
	if e.dictManager != nil {
		if shadowLayer := e.dictManager.GetShadowProvider(); shadowLayer != nil {
			rules := shadowLayer.GetShadowRules(input)
			candidates = dict.ApplyShadowPins(candidates, rules)
		}
	}
	shadowElapsed := time.Since(shadowStart)

	// Shadow 可能删词，需在应用后重新评估自动上屏条件（纯码表路径无拼音候选）
	shouldCommit, commitText := e.recheckAutoCommit(input, candidates, false)

	return &ConvertResult{
		Candidates:   candidates,
		ShouldCommit: shouldCommit,
		CommitText:   commitText,
		IsEmpty:      codetableResult.IsEmpty,
		ShouldClear:  codetableResult.ShouldClear,
		ToEnglish:    codetableResult.ToEnglish,
		Timing:       &mixedTiming{Codetable: ctElapsed, Shadow: shadowElapsed},
	}
}

// convertPinyinOnly 超过最大码长时仅查拼音（主流混输行为）。
// 长码场景特例：若完整 input 在码表中有精确匹配或更长后继（如 abcde→乙），
// 仍用完整 input 查码表并合并，避免长码候选被吞掉。
func (e *Engine) convertPinyinOnly(input string, maxCandidates int) *ConvertResult {
	if e.pinyinEngine == nil {
		return &ConvertResult{IsEmpty: true}
	}

	pyStart := time.Now()
	pinyinResult := e.pinyinEngine.ConvertEx(input, maxCandidates)
	pyElapsed := time.Since(pyStart)

	for i := range pinyinResult.Candidates {
		pinyinResult.Candidates[i].Source = candidate.SourcePinyin
	}

	candidates := pinyinResult.Candidates

	// 长码场景：完整 input 在码表中有精确匹配或更长后继时，追加码表候选。
	// 此时需要把拼音 weight 归一化到 pinyin tier，避免与码表 tier 重叠。
	if e.codetableEngine != nil &&
		(e.codetableEngine.HasFullInputMatch(input) || e.codetableEngine.HasLongerCode(input)) {
		for i := range candidates {
			candidates[i].Weight /= PinyinTierScale
			if candidates[i].Weight < 0 {
				candidates[i].Weight = 0
			}
		}
		if ctResult := e.codetableEngine.ConvertEx(input, maxCandidates); ctResult != nil {
			for i := range ctResult.Candidates {
				if ctResult.Candidates[i].IsPhrase {
					ctResult.Candidates[i].Source = candidate.SourcePhrase
					ctResult.Candidates[i].Weight += PhraseWeightBoost
				} else {
					ctResult.Candidates[i].Source = candidate.SourceCodetable
					if ctResult.Candidates[i].Code == input {
						ctResult.Candidates[i].Weight += e.config.CodetableWeightBoost
					} else {
						ctResult.Candidates[i].Weight += PartialMatchBoost
					}
				}
			}
			// 码表候选并入，后续 sort 会按 weight 归位
			candidates = append(ctResult.Candidates, candidates...)
			sort.SliceStable(candidates, func(i, j int) bool {
				return candidate.Better(candidates[i], candidates[j])
			})
			candidates = dedupByText(candidates)
		}
	}

	shadowStart := time.Now()
	if e.dictManager != nil {
		if shadowLayer := e.dictManager.GetShadowProvider(); shadowLayer != nil {
			rules := shadowLayer.GetShadowRules(input)
			candidates = dict.ApplyShadowPins(candidates, rules)
		}
	}
	shadowElapsed := time.Since(shadowStart)

	result := &ConvertResult{
		Candidates:       candidates,
		IsEmpty:          len(candidates) == 0,
		IsPinyinFallback: true,
		PreeditDisplay:   pinyinResult.PreeditDisplay,
		Timing:           &mixedTiming{Pinyin: pyElapsed, Shadow: shadowElapsed},
	}
	if pinyinResult.Composition != nil {
		result.CompletedSyllables = pinyinResult.Composition.CompletedSyllables
		result.PartialSyllable = pinyinResult.Composition.PartialSyllable
		result.HasPartial = pinyinResult.Composition.HasPartial()
	}

	// 超长输入也需要全码自动顶屏判定（长码精确唯一无后继时上屏）
	result.ShouldCommit, result.CommitText = e.recheckAutoCommit(input, candidates, len(pinyinResult.Candidates) > 0)

	e.addCodeHintsFromCodetable(result.Candidates)
	if e.config.ShowSourceHint {
		addSourceHints(result.Candidates)
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
	var ctElapsed, pyElapsed time.Duration

	var wg sync.WaitGroup

	// 码表引擎：默认用前 maxCodeLen 码查询；
	// 若完整 input 存在精确匹配或更长后继（长码场景，如 abcde→乙），
	// 额外用完整 input 查询并合并，避免长码候选被前 N 码截断吞掉。
	if e.codetableEngine != nil {
		wg.Add(1)
		go func() {
			defer wg.Done()
			ctStart := time.Now()
			prefix := input[:e.maxCodeLen]
			codetableResult := e.codetableEngine.ConvertEx(prefix, maxCandidates)
			codetableCandidates = codetableResult.Candidates

			// 长码场景：完整 input 在码表中有精确匹配或更长后继时，追加用完整 input 查询
			if e.codetableEngine.HasFullInputMatch(input) || e.codetableEngine.HasLongerCode(input) {
				if fullResult := e.codetableEngine.ConvertEx(input, maxCandidates); fullResult != nil {
					codetableCandidates = append(codetableCandidates, fullResult.Candidates...)
				}
			}
			ctElapsed = time.Since(ctStart)
		}()
	}

	// 拼音引擎：用完整输入查询
	if e.pinyinEngine != nil {
		wg.Add(1)
		go func() {
			defer wg.Done()
			pyStart := time.Now()
			pinyinResult = e.pinyinEngine.ConvertEx(input, maxCandidates)
			pinyinCandidates = pinyinResult.Candidates
			pyElapsed = time.Since(pyStart)
		}()
	}

	wg.Wait()

	// 码表候选提权（与 convertMixed 相同的策略）。
	// 短语候选 (IsPhrase) 走独立 phrase tier, 仅 +PhraseWeightBoost (1M),
	// 让它永远 < 码表词且 > 拼音, 详见 docs/design/command-bar-followup.md §2.2。
	//
	// 拆分组合可信度 < 原生候选 (2026-05-17): Code!=input[:maxCodeLen] 的
	// 前缀补全候选独立到 PartialMatchBoost (500K) tier, 让 phrase (+1M)
	// 与 codetable 精确 (+10M) 都优先于拆分组合。
	prefixKey := input[:e.maxCodeLen]
	for i := range codetableCandidates {
		if codetableCandidates[i].IsPhrase {
			codetableCandidates[i].Source = candidate.SourcePhrase
			codetableCandidates[i].Weight += PhraseWeightBoost
			continue
		}
		codetableCandidates[i].Source = candidate.SourceCodetable
		// 完整 input 精确命中（长码场景）或前 N 码精确命中都视为 Codetable tier
		if codetableCandidates[i].Code == input || codetableCandidates[i].Code == prefixKey {
			codetableCandidates[i].Weight += e.config.CodetableWeightBoost
		} else {
			codetableCandidates[i].Weight += PartialMatchBoost
		}
	}

	// 拼音候选标记来源 + tier 归一化 (2026-05-17 fix)。
	//
	// 拼音引擎内部 weight = rimeScore × 1_000_000 (0~10M), 直接合入会跟
	// codetable tier (10M+) 重叠且远超 phrase tier (1M+), 让短语全码匹配
	// 永远抢不过拼音 partial。这里 ÷ PinyinTierScale (100) 把拼音 weight
	// 归一化到 0~100K (pinyin tier), 与 phrase tier 1M+ 严格隔离, 同时
	// /100 保留拼音内部相对顺序 (精度损失仅最低两位)。
	for i := range pinyinCandidates {
		pinyinCandidates[i].Source = candidate.SourcePinyin
		pinyinCandidates[i].Weight /= PinyinTierScale
		if pinyinCandidates[i].Weight < 0 {
			pinyinCandidates[i].Weight = 0
		}
	}

	// 合并
	mergeStart := time.Now()
	merged := make([]candidate.Candidate, 0, len(codetableCandidates)+len(pinyinCandidates))
	merged = append(merged, codetableCandidates...)
	merged = append(merged, pinyinCandidates...)

	sort.SliceStable(merged, func(i, j int) bool {
		return candidate.Better(merged[i], merged[j])
	})
	merged = dedupByText(merged)
	mergeElapsed := time.Since(mergeStart)

	// 应用 Shadow 规则
	shadowStart := time.Now()
	if e.dictManager != nil {
		if shadowLayer := e.dictManager.GetShadowProvider(); shadowLayer != nil {
			rules := shadowLayer.GetShadowRules(input)
			merged = dict.ApplyShadowPins(merged, rules)
		}
	}
	shadowElapsed := time.Since(shadowStart)

	if maxCandidates > 0 && len(merged) > maxCandidates {
		merged = merged[:maxCandidates]
	}

	result := &ConvertResult{
		Candidates: merged,
		IsEmpty:    len(merged) == 0,
		Timing:     &mixedTiming{Codetable: ctElapsed, Pinyin: pyElapsed, Merge: mergeElapsed, Shadow: shadowElapsed},
	}

	// 如果拼音有完整音节，标记为拼音降级模式（预编辑区显示拼音分词）
	if pinyinResult != nil && pinyinResult.HasFullSyllable {
		result.IsPinyinFallback = true
		result.PreeditDisplay = pinyinResult.PreeditDisplay
		result.FullPinyinInput = pinyinResult.FullPinyinInput
		if pinyinResult.Composition != nil {
			result.CompletedSyllables = pinyinResult.Composition.CompletedSyllables
			result.PartialSyllable = pinyinResult.Composition.PartialSyllable
			result.HasPartial = pinyinResult.Composition.HasPartial()
		}
	}

	// 超长输入也需要全码自动顶屏判定（长码精确唯一无后继时上屏）
	result.ShouldCommit, result.CommitText = e.recheckAutoCommit(input, merged, len(pinyinCandidates) > 0)

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
	var ctElapsed, pyElapsed time.Duration

	var wg sync.WaitGroup

	// 并行查询码表
	if e.codetableEngine != nil {
		wg.Add(1)
		go func() {
			defer wg.Done()
			ctStart := time.Now()
			codetableResult = e.codetableEngine.ConvertEx(input, maxCandidates)
			codetableCandidates = codetableResult.Candidates
			ctElapsed = time.Since(ctStart)
		}()
	}

	// 并行查询拼音
	var pinyinHasFullSyllable bool
	if e.pinyinEngine != nil {
		wg.Add(1)
		go func() {
			defer wg.Done()
			pyStart := time.Now()
			pinyinResult := e.pinyinEngine.ConvertEx(input, maxCandidates)
			pinyinCandidates = pinyinResult.Candidates
			pinyinHasFullSyllable = pinyinResult.HasFullSyllable
			pyElapsed = time.Since(pyStart)
		}()
	}

	wg.Wait()

	// 查询英文候选
	var englishCandidates []candidate.Candidate
	if e.enableEnglish && e.englishSearchFn != nil {
		englishCandidates = e.englishSearchFn(input, maxCandidates)
		for i := range englishCandidates {
			englishCandidates[i].Source = candidate.SourceEnglish
			// 英文候选权重：低于码表精确匹配(+10M)，但高于拼音
			// 精确匹配的英文词提权更多
			if strings.ToLower(englishCandidates[i].Text) == input {
				englishCandidates[i].Weight += e.config.CodetableWeightBoost / 2 // +5M
			} else {
				englishCandidates[i].Weight += 1000000 // +1M，略高于拼音降权后
			}
		}
	}

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
	// 短语候选 (IsPhrase) 走独立 phrase tier, 仅 +PhraseWeightBoost (1M),
	// 让它永远 > 拼音、永远 < 码表词 (含简拼降权后), 详见
	// docs/design/command-bar-followup.md §2.2。短码场景下短语
	// 自然让位给码表常用词, 不再霸占首位。
	//
	// 拆分组合可信度 < 原生候选 (2026-05-17): Code!=input 的码表前缀补全
	// (典型场景: 用户输入 "date" 没有精确命中, 码表用 "d→大" 提示) 独立到
	// Partial tier (+500K), 让 phrase 全码命中 (+1M) 排在拆分组合之前。
	for i := range codetableCandidates {
		if codetableCandidates[i].IsPhrase {
			codetableCandidates[i].Source = candidate.SourcePhrase
			codetableCandidates[i].Weight += PhraseWeightBoost // +1M
			continue
		}
		codetableCandidates[i].Source = candidate.SourceCodetable
		if codetableCandidates[i].Code == input {
			codetableCandidates[i].Weight += e.config.CodetableWeightBoost // +10M
		} else {
			codetableCandidates[i].Weight += PartialMatchBoost // +500K
		}
	}

	inputLen := len(input)
	for i := range pinyinCandidates {
		pinyinCandidates[i].Source = candidate.SourcePinyin
		// 拼音无完整音节时（纯简拼），按长度递减降权 (原始 scale, 归一化前完成)
		if !pinyinHasFullSyllable && inputLen >= 3 {
			switch {
			case inputLen == 3:
				pinyinCandidates[i].Weight -= AbbrevPenalty3 // 3码简拼 ~4.5M→~2.5M
			default:
				pinyinCandidates[i].Weight -= AbbrevPenalty4Plus // 4码简拼 ~4.5M→~1M
			}
		}
		// 归一化到 pinyin tier (0~100K), 与 phrase tier (1M+) / codetable tier
		// (10M+) 严格隔离。详见 PinyinTierScale 注释及 convertMixedOverflow 中的
		// 同款处理。
		pinyinCandidates[i].Weight /= PinyinTierScale
		if pinyinCandidates[i].Weight < 0 {
			pinyinCandidates[i].Weight = 0
		}
	}

	// 合并：码表在前，拼音在后，英文在最后
	mergeStart := time.Now()
	merged := make([]candidate.Candidate, 0, len(codetableCandidates)+len(pinyinCandidates)+len(englishCandidates))
	merged = append(merged, codetableCandidates...)
	merged = append(merged, pinyinCandidates...)
	merged = append(merged, englishCandidates...)

	// 按权重排序
	sort.SliceStable(merged, func(i, j int) bool {
		return candidate.Better(merged[i], merged[j])
	})

	// 按文本去重（保留先出现的，即权重高的）
	merged = dedupByText(merged)
	mergeElapsed := time.Since(mergeStart)

	// 统一应用 Shadow 规则（置顶/删除）
	// 子引擎内部各自应用了 Shadow，但合并+重排序后位置被打乱，需要在最终列表上重新应用。
	// ApplyShadowPins 是幂等的：先移除 deleted 词，再按 pin position 分配槽位。
	shadowStart := time.Now()
	if e.dictManager != nil {
		if shadowLayer := e.dictManager.GetShadowProvider(); shadowLayer != nil {
			rules := shadowLayer.GetShadowRules(input)
			merged = dict.ApplyShadowPins(merged, rules)
		}
	}
	shadowElapsed := time.Since(shadowStart)

	// 截断
	if maxCandidates > 0 && len(merged) > maxCandidates {
		merged = merged[:maxCandidates]
	}

	// 构建结果
	result := &ConvertResult{
		Candidates: merged,
		IsEmpty:    len(merged) == 0,
		Timing:     &mixedTiming{Codetable: ctElapsed, Pinyin: pyElapsed, Merge: mergeElapsed, Shadow: shadowElapsed},
	}

	// Shadow 可能删词，需在应用后重新评估自动上屏条件（不能直接继承子引擎的 ShouldCommit）
	if codetableResult != nil {
		result.ShouldCommit, result.CommitText = e.recheckAutoCommit(input, merged, len(pinyinCandidates) > 0)
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

// recheckAutoCommit 在外层 Shadow 应用后重新评估全码自动上屏条件。
// 混输模式下 Shadow 由 MixedEngine 统一应用，子引擎的 ShouldCommit 不可直接继承，
// 需在合并候选上重新判定：精确唯一 + 无更长后继 + len >= MinAutoCommitLen。
// 当 hasPinyinCandidate=true 且输入是完整音节时，否决全码顶屏，避免与拼音冲突。
func (e *Engine) recheckAutoCommit(input string, candidates []candidate.Candidate, hasPinyinCandidate bool) (shouldCommit bool, commitText string) {
	ct := e.codetableEngine
	if ct == nil {
		return false, ""
	}
	cfg := ct.GetConfig()
	if !cfg.AutoCommitAtFull || len(input) < cfg.MinAutoCommitLen {
		return false, ""
	}
	// 混输守护：输入构成合法拼音序列（含多音节/合法尾部前缀） + 拼音有候选 → 否决。
	// 使用 isPossiblePinyinSequence 覆盖 "woai" 这种多音节场景；旧版 trie.Contains
	// 只判单音节，对 "woai"/"nizh" 等失效。
	if cfg.AutoCommitBlockOnPinyin && hasPinyinCandidate && e.isPossiblePinyinSequence(input) {
		return false, ""
	}
	// 来源白名单：只有码表 / 短语来源的精确命中才允许触发全码顶屏。
	// 拼音来源即便 Code==input 也不算（拼音引擎的码格式偶尔与 input 一致不应触发码表顶屏行为）。
	var hit candidate.Candidate
	n := 0
	for _, c := range candidates {
		if c.Code != input {
			continue
		}
		if c.Source != candidate.SourceCodetable && c.Source != candidate.SourcePhrase {
			continue
		}
		n++
		hit = c
	}
	if n != 1 {
		return false, ""
	}
	if ct.HasLongerCode(input) {
		return false, ""
	}
	return true, hit.Text
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

// SetEncoderRules 注入主码表编码规则，供多字词编码提示推导使用。
// 由 engine.Manager 在创建混输引擎后调用。
func (e *Engine) SetEncoderRules(rules []encoding.Rule) {
	e.encoderRules = rules
	e.codeHintEncoder = nil // 下次使用时重建
}

// addCodeHintsFromCodetable 使用主码表的单字反向索引为拼音候选添加主编码提示。
//
// 单字候选：直接查单字反查表。
// 多字候选：通过编码规则在线推导编码，再经 CodeTable.Lookup 验证存在后填充。
// 懒构建反向索引和编码器，避免在引擎创建时额外加载反查码表。
func (e *Engine) addCodeHintsFromCodetable(candidates []candidate.Candidate) {
	if e.codetableEngine == nil {
		return
	}
	if pe := e.pinyinEngine; pe != nil {
		if cfg := pe.GetConfig(); cfg != nil && !cfg.ShowCodeHint {
			return
		}
	}

	ct := e.codetableEngine.GetCodeTable()
	if ct == nil {
		return
	}

	// 懒构建单字反向索引（仅首次调用时执行）
	if e.reverseIndex == nil {
		e.reverseIndex = ct.BuildSingleCharReverseIndex()
		if len(e.encoderRules) > 0 {
			e.codeHintEncoder = encoding.NewReverseEncoder(e.reverseIndex, e.encoderRules)
		}
	}

	for i := range candidates {
		if candidates[i].Source != candidate.SourcePinyin || candidates[i].Comment != "" {
			continue
		}
		text := candidates[i].Text
		if len([]rune(text)) == 1 {
			if codes := e.reverseIndex[text]; len(codes) > 0 {
				candidates[i].Comment = codes[0]
			}
		} else if e.codeHintEncoder != nil {
			if code, err := e.codeHintEncoder.Encode(text); err == nil {
				if mixedCodeTableContainsText(ct, code, text) {
					candidates[i].Comment = code
				}
			}
		}
	}
}

// mixedCodeTableContainsText 检查码表中指定编码下是否包含目标文本的词条
func mixedCodeTableContainsText(ct *dict.CodeTable, code, text string) bool {
	for _, e := range ct.Lookup(code) {
		if e.Text == text {
			return true
		}
	}
	return false
}

// addSourceHints 为混输候选添加来源标记提示
// 仅在拼音候选的 Comment 中添加 "拼" 前缀，帮助用户区分
func addSourceHints(candidates []candidate.Candidate) {
	for i := range candidates {
		if candidates[i].Source == candidate.SourcePinyin {
			if candidates[i].Comment == "" {
				candidates[i].Comment = "拼"
			} else {
				candidates[i].Comment = "拼|" + candidates[i].Comment
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
		// 同步通知码表造词策略，维持跨来源的 charBuffer 连续性：
		// 拼音单字 → 追加到 charBuffer（code 由 CalcWordCode 在 flush 时重算）
		// 拼音多字词 → 终止当前序列，触发 flush
		if e.codetableEngine != nil {
			if ls := e.codetableEngine.GetLearningStrategy(); ls != nil {
				if len([]rune(text)) == 1 {
					ls.OnWordCommitted("", text)
				} else {
					e.codetableEngine.OnPhraseTerminated()
				}
			}
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

// SetEnglishSearch 设置英文词库查询函数
func (e *Engine) SetEnglishSearch(fn func(prefix string, limit int) []candidate.Candidate) {
	e.englishSearchFn = fn
	e.enableEnglish = fn != nil
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
