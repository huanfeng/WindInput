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
	charPinyinIdx *pinyinIndex // 懒构建：汉字 → 全拼音节（池化存储），用于自动生成用户词编码
}

// pinyinIndex 池化的"汉字 → 代表读音"反向索引。
// 拼音音节是封闭集（标准约 410 个），用 uint16 索引 + 共享音节池
// 替代每个 rune 各存一份 string，可显著降低重复 string 头部开销，
// 并为后续扩展为"多读音列表"（map[rune][]uint16）铺路。
type pinyinIndex struct {
	pool []string        // 池中索引即音节 ID（构建后不可变）
	char map[rune]uint16 // 汉字 → 代表读音的池索引（按词典权重择优）
}

// syllable 返回池中索引对应的音节字符串，越界返回空串。
func (p *pinyinIndex) syllable(id uint16) string {
	if int(id) >= len(p.pool) {
		return ""
	}
	return p.pool[id]
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
	//
	// 优先级：
	//  1) 用 spConverter 切分用户的实际按键（与本次输入完全一致，多音字天然正确）
	//  2) 兜底：从 text 反查代表读音（已按词典权重择优，多音字也尽量选常用读音）
	if e.spConverter != nil {
		converted := false
		if r := e.spConverter.Convert(code); r != nil && !r.HasPartial && r.FullPinyin != "" {
			code = r.FullPinyin
			converted = true
		}
		if !converted {
			if fp := e.GenerateWordPinyin(text); fp != "" {
				code = fp
			}
		}
	}

	// 写入前反查校验：生成的 code 必须能回查到 text，否则放弃学习。
	// 这能拦截"双拼多义切分错位/反向索引猜错读音"等边界 case，
	// 避免产生"写得进、查不出"的幽灵词条。
	if !e.codeMatchesText(code, text) {
		e.logger.Debug("learning skipped: code cannot reverse-lookup text",
			"code", code, "textLen", len([]rune(text)))
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

// codeMatchesText 检查 code 与 text 是否在"逐字段"层面合理匹配。
//
// 不要求整词反查（用户造的新词本来就不在词典里），而是：
//  1. 把 code 切成 N 个音节，要求 N == len([]rune(text))
//  2. 每个 (音节, 字) 配对必须在词典中存在（该音节下能查到该字）
//
// 这样既能放行"用户造新词"的合法路径（每个字-音节单独都合理），
// 又能拦截"切分错位 / 反向索引猜错读音"（如 费→bi）的幽灵词条。
//
// 切分用 SyllableTrie 做带回溯的 DP，因此 "xian" 可被切为 ["xian"] 或 ["xi","an"]，
// 选择能与 text 字数匹配的那个切分。
func (e *Engine) codeMatchesText(code, text string) bool {
	if code == "" || text == "" {
		return false
	}
	if e.dict == nil || e.syllableTrie == nil {
		// 无词典/无 trie（测试等）：放行，避免阻塞合法路径
		return true
	}
	runes := []rune(text)
	if syls, ok := e.splitCodeToN(code, len(runes)); ok {
		for i, syl := range syls {
			matched := false
			for _, c := range e.dict.Lookup(syl) {
				cr := []rune(c.Text)
				if len(cr) == 1 && cr[0] == runes[i] {
					matched = true
					break
				}
			}
			if !matched {
				return false
			}
		}
		return true
	}
	// 切不出与字数匹配的音节序列：再做一次整词反查兜底（
	// 覆盖 code 含残留字符或词典里整词已存在的场景）
	for _, c := range e.dict.Lookup(code) {
		if c.Text == text {
			return true
		}
	}
	return false
}

// splitCodeToN 把 code 切分成恰好 n 个音节。返回切分结果与是否成功。
// 用回溯：每个位置尝试 MatchAt 给出的所有音节（已按长到短排序），
// 找到第一个能切出 n 段的方案即返回。
func (e *Engine) splitCodeToN(code string, n int) ([]string, bool) {
	if n <= 0 || code == "" {
		return nil, false
	}
	buf := make([]string, 0, n)
	var dfs func(pos int) bool
	dfs = func(pos int) bool {
		if pos == len(code) {
			return len(buf) == n
		}
		if len(buf) >= n {
			return false
		}
		for _, syl := range e.syllableTrie.MatchAt(code, pos) {
			buf = append(buf, syl)
			if dfs(pos + len(syl)) {
				return true
			}
			buf = buf[:len(buf)-1]
		}
		return false
	}
	if dfs(0) {
		out := make([]string, len(buf))
		copy(out, buf)
		return out, true
	}
	return nil, false
}

// buildCharPinyinIndex 构建汉字→代表读音的反向索引（池化存储）。
//
// 遍历全部 ~400 个标准拼音音节，对每个音节做单字精确查询，
// 为每个汉字保留**权重最高**的读音作为代表读音。
// 这样可正确处理多音字：例如"费"在 "fei" 下权重远高于 "bi"，
// 选 fei 作为代表读音；"强" 在 "qiang" 下高于 "jiang"，选 qiang。
//
// 历史问题：旧实现按 allSyllables 顺序"先到先得"，对多音字会被
// 字母序较前的生僻读音"占位"（如 费→bi、强→jiang），导致
// 自动生成的用户词编码与查询时 key 不一致，词条进得去出不来。
//
// 结果缓存于 e.charPinyinIdx，仅在首次调用时执行。
func (e *Engine) buildCharPinyinIndex() {
	// pool: 池索引即音节 ID，0 保留为"未填充"哨兵以简化判定
	pool := make([]string, 0, len(allSyllables)+1)
	pool = append(pool, "") // index 0 占位
	sylID := make(map[string]uint16, len(allSyllables))

	type best struct {
		id     uint16
		weight int
	}
	pick := make(map[rune]best, 6000)

	for _, syl := range allSyllables {
		cands := e.dict.Lookup(syl)
		if len(cands) == 0 {
			continue
		}
		id, ok := sylID[syl]
		if !ok {
			if len(pool) >= 1<<16 {
				// 理论上不会发生（标准音节远少于 65535），保护性跳过
				continue
			}
			id = uint16(len(pool))
			pool = append(pool, syl)
			sylID[syl] = id
		}
		for _, c := range cands {
			runes := []rune(c.Text)
			if len(runes) != 1 {
				continue
			}
			r := runes[0]
			if cur, exists := pick[r]; !exists || c.Weight > cur.weight {
				pick[r] = best{id: id, weight: c.Weight}
			}
		}
	}

	idx := &pinyinIndex{
		pool: pool,
		char: make(map[rune]uint16, len(pick)),
	}
	for r, b := range pick {
		idx.char[r] = b.id
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
	b.Grow(len(runes) * 4)
	for _, r := range runes {
		id, ok := e.charPinyinIdx.char[r]
		if !ok {
			return ""
		}
		b.WriteString(e.charPinyinIdx.syllable(id))
	}
	return b.String()
}
