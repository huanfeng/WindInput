package pinyin

import (
	"log"

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
	dict         dict.Dict
	syllableTrie *SyllableTrie       // 音节 Trie
	unigram      *UnigramModel       // Unigram 语言模型
	bigram       *BigramModel        // Bigram 语言模型（可选）
	wubiTable    *dict.CodeTable     // 五笔码表（用于反查）
	wubiReverse  map[string][]string // 汉字 -> 五笔编码（反向索引）
	config       *Config
	dictManager  *dict.DictManager // 词库管理器（用于用户词频学习）
}

// NewEngine 创建拼音引擎
func NewEngine(d dict.Dict) *Engine {
	return &Engine{
		dict:         d,
		syllableTrie: NewSyllableTrie(),
		config:       &Config{ShowWubiHint: false, FilterMode: "smart"},
	}
}

// NewEngineWithConfig 创建带配置的拼音引擎
func NewEngineWithConfig(d dict.Dict, config *Config) *Engine {
	if config == nil {
		config = &Config{ShowWubiHint: false, FilterMode: "smart"}
	}
	return &Engine{
		dict:         d,
		syllableTrie: NewSyllableTrie(),
		config:       config,
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
func (e *Engine) LoadUnigram(path string) error {
	m := NewUnigramModel()
	if err := m.Load(path); err != nil {
		return err
	}
	e.unigram = m
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
	return nil
}

// SetUnigram 直接设置 Unigram 模型
func (e *Engine) SetUnigram(m *UnigramModel) {
	e.unigram = m
}

// GetUnigram 获取 Unigram 模型
func (e *Engine) GetUnigram() *UnigramModel {
	return e.unigram
}

// LoadWubiTable 加载五笔码表（用于反查）
func (e *Engine) LoadWubiTable(path string) error {
	ct, err := dict.LoadCodeTable(path)
	if err != nil {
		return err
	}
	e.wubiTable = ct
	e.wubiReverse = ct.BuildReverseIndex()
	return nil
}

// lookupWubiCode 查找汉字的五笔编码
func (e *Engine) lookupWubiCode(text string) string {
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

// smartComposeThreshold 智能组句的输入长度阈值
const smartComposeThreshold = 4

// addWubiHints 添加五笔编码提示
func (e *Engine) addWubiHints(candidates []candidate.Candidate) {
	if e.config == nil || !e.config.ShowWubiHint || e.wubiReverse == nil {
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
