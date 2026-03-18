package pinyin

import (
	"log"
	"os"
	"strings"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
)

// DebugLog 控制是否输出调试日志（每次按键触发的高频信息）
// 仅在开发调试时开启，生产环境默认关闭
var DebugLog = false

func logDebug(format string, args ...interface{}) {
	if DebugLog {
		log.Printf(format, args...)
	}
}

// Config 拼音引擎配置
type Config struct {
	ShowWubiHint    bool         // 显示五笔编码提示
	FilterMode      string       // 候选过滤模式
	UseSmartCompose bool         // 启用智能组句（Viterbi）
	CandidateOrder  string       // 候选排序模式：char_first(单字优先)/phrase_first(词组优先)/smart(智能混排)
	Fuzzy           *FuzzyConfig // 模糊拼音配置（nil 表示不启用）
}

// Engine 拼音引擎
type Engine struct {
	dict         *dict.CompositeDict
	syllableTrie *SyllableTrie       // 音节 Trie
	unigram      UnigramLookup       // Unigram 语言模型（接口：支持内存模式和 mmap 模式）
	bigram       *BigramModel        // Bigram 语言模型（可选）
	wubiTable    *dict.CodeTable     // 五笔码表（用于反查）
	wubiReverse  map[string][]string // 汉字 -> 五笔编码（反向索引）
	config       *Config
	dictManager  *dict.DictManager // 词库管理器（用于用户词频学习）
	scorer       *Scorer           // 统一候选评分器
}

// NewEngine 创建拼音引擎
func NewEngine(d *dict.CompositeDict) *Engine {
	return &Engine{
		dict:         d,
		syllableTrie: NewSyllableTrie(),
		config:       &Config{ShowWubiHint: false, FilterMode: "smart"},
		scorer:       NewScorer(nil, nil),
	}
}

// NewEngineWithConfig 创建带配置的拼音引擎
func NewEngineWithConfig(d *dict.CompositeDict, config *Config) *Engine {
	if config == nil {
		config = &Config{ShowWubiHint: false, FilterMode: "smart"}
	}
	return &Engine{
		dict:         d,
		syllableTrie: NewSyllableTrie(),
		config:       config,
		scorer:       NewScorer(nil, nil),
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

// LoadUnigram 加载 Unigram 语言模型
// 优先尝试同目录下的 unigram.wdb，不存在则 fallback 到文本文件
func (e *Engine) LoadUnigram(path string) error {
	// 尝试加载二进制版本
	wdbPath := strings.TrimSuffix(path, ".txt") + ".wdb"
	if _, err := os.Stat(wdbPath); err == nil {
		bm, err := NewBinaryUnigramModel(wdbPath)
		if err == nil {
			e.unigram = bm
			log.Printf("[PinyinEngine] Unigram 模型(二进制)加载成功: %d 词条", bm.Size())
			return nil
		}
		log.Printf("[PinyinEngine] 加载二进制 Unigram 失败，fallback 到文本: %v", err)
	}

	// Fallback 到文本格式
	m := NewUnigramModel()
	if err := m.Load(path); err != nil {
		return err
	}
	e.unigram = m
	e.scorer = NewScorer(e.unigram, e.bigram)
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
	return nil
}

// SetUnigram 直接设置 Unigram 模型（接口类型）
func (e *Engine) SetUnigram(m UnigramLookup) {
	e.unigram = m
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
// 	e.wubiTable = ct
// 	e.wubiReverse = nil // 延迟构建
// 	return nil
// }

// LoadWubiTableBinary 加载五笔码表的 wdb 二进制格式（mmap 模式，几乎不占堆内存）
func (e *Engine) LoadWubiTableBinary(wdbPath string) error {
	ct := dict.NewCodeTable()
	if err := ct.LoadBinary(wdbPath); err != nil {
		return err
	}
	e.wubiTable = ct
	e.wubiReverse = nil // 延迟构建
	return nil
}

// ReleaseWubiHint 释放五笔反查资源
func (e *Engine) ReleaseWubiHint() {
	e.wubiReverse = nil
	log.Printf("[PinyinEngine] 五笔反向索引已释放")
}

// lookupWubiCode 查找汉字的五笔编码
func (e *Engine) lookupWubiCode(text string) string {
	// 懒构建反向索引
	if e.wubiReverse == nil && e.wubiTable != nil {
		log.Printf("[PinyinEngine] 懒构建五笔反向索引...")
		e.wubiReverse = e.wubiTable.BuildReverseIndex()
		log.Printf("[PinyinEngine] 五笔反向索引构建完成")
	}
	if e.wubiReverse == nil {
		return ""
	}

	runes := []rune(text)
	if len(runes) == 0 {
		return ""
	}

	// 单字：直接返回编码
	if len(runes) == 1 {
		codes := e.wubiReverse[text]
		if len(codes) > 0 {
			return codes[0]
		}
		return ""
	}

	// 词组：只有五笔码表中真实存在该词组时才返回编码
	codes := e.wubiReverse[text]
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

// addWubiHints 添加五笔编码提示
func (e *Engine) addWubiHints(candidates []candidate.Candidate) {
	if e.config == nil || !e.config.ShowWubiHint || e.wubiTable == nil {
		return
	}
	for i := range candidates {
		wubiCode := e.lookupWubiCode(candidates[i].Text)
		if wubiCode != "" {
			candidates[i].Hint = wubiCode
		}
	}
}

// AddWubiHintsForced 强制添加五笔编码提示（不检查 ShowWubiHint 配置）
// 用于临时拼音模式，无论用户是否开启了五笔提示都强制显示
func (e *Engine) AddWubiHintsForced(candidates []candidate.Candidate) {
	if e.wubiReverse == nil && e.wubiTable == nil {
		return
	}
	for i := range candidates {
		wubiCode := e.lookupWubiCode(candidates[i].Text)
		if wubiCode != "" {
			candidates[i].Hint = wubiCode
		}
	}
}

// SetDictManager 设置词库管理器（用于用户词频学习）
func (e *Engine) SetDictManager(dm *dict.DictManager) {
	e.dictManager = dm
}

// OnCandidateSelected 用户选词回调
// 记录用户选择，用于词频学习
func (e *Engine) OnCandidateSelected(code, text string) {
	if e.dictManager == nil {
		return
	}

	userDict := e.dictManager.GetUserDict()
	if userDict == nil {
		return
	}

	// 查询用户词典中是否已存在该词条
	existing := userDict.Search(code, 0)
	found := false
	for _, c := range existing {
		if c.Text == text {
			found = true
			break
		}
	}

	if found {
		// 已存在：增加权重
		userDict.IncreaseWeight(code, text, 10)
		logDebug("[PinyinEngine] 用户词频提升: code=%q text=%q +10", code, text)
	} else {
		// 不存在：添加到用户词典
		// 只对多字词或非系统词添加（避免单字污染）
		runes := []rune(text)
		if len(runes) >= 2 {
			userDict.Add(code, text, 100)
			logDebug("[PinyinEngine] 用户词典添加: code=%q text=%q weight=100", code, text)
		}
	}

	// 更新 Unigram 用户频率
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
