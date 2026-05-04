package pinyin

import (
	"log/slog"
	"os"
	"strings"
	"sync/atomic"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine/pinyin/shuangpin"
)

// Config 拼音引擎配置
type Config struct {
	ShowCodeHint    bool         // 显示编码提示
	FilterMode      string       // 候选过滤模式
	UseSmartCompose bool         // 启用智能组句（Viterbi）
	CandidateOrder  string       // 候选排序模式：char_first(单字优先)/phrase_first(词组优先)/smart(智能混排)
	Fuzzy           *FuzzyConfig // 模糊拼音配置（nil 表示不启用）
	SkipShadow      bool         // 跳过 Shadow 规则应用（混输模式下由外层统一应用）
	SkipAbbrev      bool         // 跳过简拼匹配（混输模式下减少噪声）
}

// LearningStrategy 造词策略接口（避免引擎直接依赖 schema 包）
type LearningStrategy interface {
	OnWordCommitted(code, text string)
}

// Engine 拼音引擎
type Engine struct {
	dict             *dict.CompositeDict
	syllableTrie     *SyllableTrie       // 音节 Trie
	unigram          UnigramLookup       // Unigram 语言模型（接口：支持内存模式和 mmap 模式）
	bigram           *BigramModel        // Bigram 语言模型（可选）
	codeHintTable    *dict.CodeTable     // 编码反查码表
	codeHintReverse  map[string][]string // 汉字 -> 编码（反向索引）
	config           *Config
	fuzzyPtr         atomic.Pointer[FuzzyConfig] // 线程安全的模糊音配置（热更新时原子写入，查询时原子读取）
	dictManager      *dict.DictManager           // 词库管理器（用于用户词频学习）
	freqHandler      *dict.FreqHandler           // 词频记录处理器（可选，调频用）
	learningStrategy LearningStrategy            // 造词策略（可选）
	scorer           *Scorer                     // 统一候选评分器（deprecated，保留供五笔引擎引用）
	rimeScorer       *RimeScorer                 // Rime 风格连续评分器
	logger           *slog.Logger

	// 双拼支持
	spConverter *shuangpin.Converter // 双拼转换器（nil 表示全拼模式）

	// 造词辅助
	charPinyinIdx map[rune]string // 懒构建：汉字 → 全拼音节，用于自动生成用户词编码
}

// NewEngine 创建拼音引擎
func NewEngine(d *dict.CompositeDict, logger *slog.Logger) *Engine {
	if logger == nil {
		logger = slog.Default()
	}
	return &Engine{
		dict:         d,
		syllableTrie: NewSyllableTrie(),
		config:       &Config{ShowCodeHint: false, FilterMode: "smart"},
		scorer:       NewScorer(nil, nil),
		rimeScorer:   NewRimeScorer(nil, nil),
		logger:       logger,
	}
}

// NewEngineWithConfig 创建带配置的拼音引擎
func NewEngineWithConfig(d *dict.CompositeDict, config *Config, logger *slog.Logger) *Engine {
	if config == nil {
		config = &Config{ShowCodeHint: false, FilterMode: "smart"}
	}
	if logger == nil {
		logger = slog.Default()
	}
	e := &Engine{
		dict:         d,
		syllableTrie: NewSyllableTrie(),
		config:       config,
		scorer:       NewScorer(nil, nil),
		rimeScorer:   NewRimeScorer(nil, nil),
		logger:       logger,
	}
	if config.Fuzzy != nil {
		e.fuzzyPtr.Store(config.Fuzzy)
	}
	return e
}

// SetConfig 设置配置
func (e *Engine) SetConfig(config *Config) {
	e.config = config
	if config != nil {
		e.fuzzyPtr.Store(config.Fuzzy)
	} else {
		e.fuzzyPtr.Store(nil)
	}
}

// SetFuzzyConfig 原子更新模糊拼音配置（线程安全，供热更新调用）
func (e *Engine) SetFuzzyConfig(fc *FuzzyConfig) {
	e.fuzzyPtr.Store(fc)
	if e.config != nil {
		e.config.Fuzzy = fc
	}
}

// GetConfig 获取配置
func (e *Engine) GetConfig() *Config {
	return e.config
}

// LoadUnigram 加载 Unigram 语言模型
// 优先尝试同目录下的 unigram.wdb，不存在则 fallback 到文本文件
func (e *Engine) LoadUnigram(path string) error {
	// 尝试加载二进制版本
	wdbPath := strings.TrimSuffix(path, ".txt") + ".wdb"
	if _, err := os.Stat(wdbPath); err == nil {
		bm, err := NewBinaryUnigramModel(wdbPath)
		if err == nil {
			e.unigram = bm
			e.scorer = NewScorer(e.unigram, e.bigram)
			e.rimeScorer = NewRimeScorer(e.unigram, e.bigram)
			e.logger.Info("Unigram 模型(二进制)加载成功", "count", bm.Size())
			return nil
		}
		e.logger.Info("加载二进制 Unigram 失败，fallback 到文本", "err", err)
	}

	// Fallback 到文本格式
	m := NewUnigramModel()
	if err := m.Load(path); err != nil {
		return err
	}
	e.unigram = m
	e.scorer = NewScorer(e.unigram, e.bigram)
	e.rimeScorer = NewRimeScorer(e.unigram, e.bigram)
	return nil
}

// LoadBigram 加载 Bigram 语言模型
func (e *Engine) LoadBigram(path string) error {
	if e.unigram == nil {
		return nil // Bigram 需要 Unigram 作为回退
	}
	m := NewBigramModel(e.unigram)
	if err := m.Load(path); err != nil {
		return err
	}
	e.bigram = m
	e.scorer = NewScorer(e.unigram, e.bigram)
	e.rimeScorer = NewRimeScorer(e.unigram, e.bigram)
	return nil
}

// SetUnigram 直接设置 Unigram 模型（接口类型）
func (e *Engine) SetUnigram(m UnigramLookup) {
	e.unigram = m
	e.scorer = NewScorer(e.unigram, e.bigram)
	e.rimeScorer = NewRimeScorer(e.unigram, e.bigram)
}

// GetUnigram 获取 Unigram 模型（接口类型）
func (e *Engine) GetUnigram() UnigramLookup {
	return e.unigram
}

// GetUnigramModel 获取内存模式的 UnigramModel（用于用户词频管理等）
// 如果不是内存模式则返回 nil
func (e *Engine) GetUnigramModel() *UnigramModel {
	if m, ok := e.unigram.(*UnigramModel); ok {
		return m
	}
	return nil
}

// GetBinaryUnigramModel 获取二进制模式的 BinaryUnigramModel
// 如果不是二进制模式则返回 nil
func (e *Engine) GetBinaryUnigramModel() *BinaryUnigramModel {
	if m, ok := e.unigram.(*BinaryUnigramModel); ok {
		return m
	}
	return nil
}

// // LoadWubiTable 加载五笔码表（用于反查，文本模式 — 会占用较多堆内存）
// // 不再立即构建反向索引，改为首次查询时懒构建
// func (e *Engine) LoadWubiTable(path string) error {
// 	ct, err := dict.LoadCodeTable(path)
// 	if err != nil {
// 		return err
// 	}
// 	e.codeHintTable = ct
// 	e.codeHintReverse = nil // 延迟构建
// 	return nil
// }

// LoadCodeHintTableBinary 加载编码反查码表的 wdb 二进制格式（mmap 模式，几乎不占堆内存）
func (e *Engine) LoadCodeHintTableBinary(wdbPath string) error {
	ct := dict.NewCodeTable()
	if err := ct.LoadBinary(wdbPath); err != nil {
		return err
	}
	e.codeHintTable = ct
	e.codeHintReverse = nil // 延迟构建
	return nil
}

// ReleaseCodeHint 释放编码反查资源
func (e *Engine) ReleaseCodeHint() {
	e.codeHintReverse = nil
	e.logger.Info("编码反查索引已释放")
}

// lookupCodeHint 查找汉字的编码提示
func (e *Engine) lookupCodeHint(text string) string {
	// 懒构建反向索引
	if e.codeHintReverse == nil && e.codeHintTable != nil {
		e.logger.Debug("懒构建编码反查索引")
		e.codeHintReverse = e.codeHintTable.BuildReverseIndex()
		e.logger.Debug("编码反查索引构建完成")
	}
	if e.codeHintReverse == nil {
		return ""
	}

	runes := []rune(text)
	if len(runes) == 0 {
		return ""
	}

	// 单字：直接返回编码
	if len(runes) == 1 {
		codes := e.codeHintReverse[text]
		if len(codes) > 0 {
			return codes[0]
		}
		return ""
	}

	// 词组：只有码表中真实存在该词组时才返回编码
	codes := e.codeHintReverse[text]
	if len(codes) > 0 {
		return codes[0]
	}
	return ""
}

// Convert 转换拼音为候选词（实现 Engine 接口）
func (e *Engine) Convert(input string, maxCandidates int) ([]candidate.Candidate, error) {
	result := e.convertCore(input, maxCandidates, false)
	return result.Candidates, nil
}

// ConvertRaw 转换拼音为候选词（不应用过滤，用于测试）
func (e *Engine) ConvertRaw(input string, maxCandidates int) ([]candidate.Candidate, error) {
	result := e.convertCore(input, maxCandidates, true)
	return result.Candidates, nil
}

// addCodeHints 添加编码提示
func (e *Engine) addCodeHints(candidates []candidate.Candidate) {
	if e.config == nil || !e.config.ShowCodeHint || e.codeHintTable == nil {
		return
	}
	for i := range candidates {
		codeHint := e.lookupCodeHint(candidates[i].Text)
		if codeHint != "" {
			candidates[i].Comment = codeHint
		}
	}
}

// AddCodeHintsForced 强制添加编码提示（不检查 ShowCodeHint 配置）
// 用于临时拼音模式，无论用户是否开启了编码提示都强制显示
func (e *Engine) AddCodeHintsForced(candidates []candidate.Candidate) {
	if e.codeHintReverse == nil && e.codeHintTable == nil {
		return
	}
	for i := range candidates {
		codeHint := e.lookupCodeHint(candidates[i].Text)
		if codeHint != "" {
			candidates[i].Comment = codeHint
		}
	}
}

// SetShuangpinConverter 设置双拼转换器（nil 表示全拼模式）
func (e *Engine) SetShuangpinConverter(conv *shuangpin.Converter) {
	e.spConverter = conv
}

// GetShuangpinConverter 获取双拼转换器
func (e *Engine) GetShuangpinConverter() *shuangpin.Converter {
	return e.spConverter
}

// IsShuangpin 是否为双拼模式
func (e *Engine) IsShuangpin() bool {
	return e.spConverter != nil
}

// SetDictManager 设置词库管理器（用于用户词频学习）
func (e *Engine) SetDictManager(dm *dict.DictManager) {
	e.dictManager = dm
}

// SetFreqHandler 设置词频记录处理器
func (e *Engine) SetFreqHandler(h *dict.FreqHandler) {
	e.freqHandler = h
}

// SetLearningStrategy 设置造词策略
func (e *Engine) SetLearningStrategy(ls LearningStrategy) {
	e.learningStrategy = ls
}

// OnCandidateSelected 用户选词回调
// 前置过滤（拼音特有） → 调频（FreqHandler） → 造词（LearningStrategy） → Unigram boost
func (e *Engine) OnCandidateSelected(code, text string) {
	if e.freqHandler == nil && e.learningStrategy == nil {
		return
	}

	// 前置过滤：单字仅 boost LM，不记录词频/造词
	if len([]rune(text)) < 2 {
		if e.unigram != nil {
			e.unigram.BoostUserFreq(text, 1)
		}
		return
	}

	// 双拼模式：将 code 统一归一化为全拼，确保临时词库/用户词库/调频记录的索引
	// 与 PinyinDict 查询时使用的全拼 key 保持一致。
	// 不归一化时，拆分输入产生的 code 为原始双拼键序列（如 "nihk"），
	// 而引擎查询始终使用全拼（"nihao"），导致写入的临时词条永远无法被检索到。
	if e.spConverter != nil {
		if fp := e.GenerateWordPinyin(text); fp != "" {
			code = fp
		}
	}

	// 调频
	if e.freqHandler != nil {
		e.freqHandler.Record(code, text)
	}

	// 造词
	if e.learningStrategy != nil {
		e.learningStrategy.OnWordCommitted(code, text)
	}

	// 后置：更新 Unigram 用户频率（拼音特有）
	if e.unigram != nil {
		e.unigram.BoostUserFreq(text, 1)
	}
}

// Reset 重置引擎状态
func (e *Engine) Reset() {
	// 拼音引擎目前无状态，无需重置
}

// Type 返回引擎类型
func (e *Engine) Type() string {
	return "pinyin"
}

// buildCharPinyinIndex 构建汉字→全拼音节的反向索引。
// 遍历全部 ~400 个标准拼音音节，对每个音节做单字精确查询，
// 将首次出现的音节作为该字的代表读音（通常为最常用读音）。
// 结果缓存于 e.charPinyinIdx，仅在首次调用时执行。
func (e *Engine) buildCharPinyinIndex() {
	idx := make(map[rune]string, 6000)
	for _, syl := range allSyllables {
		cands := e.dict.Lookup(syl)
		for _, c := range cands {
			runes := []rune(c.Text)
			if len(runes) == 1 {
				if _, exists := idx[runes[0]]; !exists {
					idx[runes[0]] = syl
				}
			}
		}
	}
	e.charPinyinIdx = idx
}

// GenerateWordPinyin 为词语生成全拼编码（如"你好" → "nihao"）。
// 用于用户手动添加词库时自动生成拼音编码，无需手动输入。
// 若词语中含无法确定读音的字符，返回空串。
func (e *Engine) GenerateWordPinyin(word string) string {
	if e.charPinyinIdx == nil {
		e.buildCharPinyinIndex()
	}
	runes := []rune(word)
	if len(runes) == 0 {
		return ""
	}
	var b strings.Builder
	for _, r := range runes {
		syl, ok := e.charPinyinIdx[r]
		if !ok {
			return ""
		}
		b.WriteString(syl)
	}
	return b.String()
}
