package schema

import (
	"log/slog"
	"time"

	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/pkg/encoding"
)

// autoPhraseTimeout 连续单字之间的最大间隔，超过则重置缓冲区
const autoPhraseTimeout = 10 * time.Second

// LearningStrategy 学习策略接口（只负责造词）
// 调频由 dict.FreqHandler 独立处理
type LearningStrategy interface {
	// OnWordCommitted 用户提交词时的造词回调
	OnWordCommitted(code, text string)

	// Reset 重置学习状态
	Reset()
}

// SystemWordChecker 系统词库检查接口
// 用于判断一个 code+text 是否已在系统词库中存在
type SystemWordChecker interface {
	ExistsInSystemDict(code, text string) bool
}

// ManualLearning 手动学习策略
// 不自动造词，用户通过快捷键手动加词
type ManualLearning struct{}

func (m *ManualLearning) OnWordCommitted(code, text string) {
	// 手动模式不自动学词
}

func (m *ManualLearning) Reset() {}

// AutoLearning 自动学习策略
// 选词即学，优先写入临时词库（如有），达标后晋升到用户词库
// 系统词库已有的词不会重复写入
type AutoLearning struct {
	userLayer     *dict.StoreUserLayer
	tempLayer     *dict.StoreTempLayer
	systemChecker SystemWordChecker
	config        AutoLearnSpec
}

// NewAutoLearning 创建自动学习策略
func NewAutoLearning(userLayer *dict.StoreUserLayer, config AutoLearnSpec) *AutoLearning {
	return &AutoLearning{
		userLayer: userLayer,
		config:    config,
	}
}

// SetTempLayer 设置临时词库层（自动学习优先写入临时词库）
func (a *AutoLearning) SetTempLayer(tl *dict.StoreTempLayer) {
	a.tempLayer = tl
}

// SetSystemChecker 设置系统词库检查器
func (a *AutoLearning) SetSystemChecker(checker SystemWordChecker) {
	a.systemChecker = checker
}

func (a *AutoLearning) OnWordCommitted(code, text string) {
	if len([]rune(text)) < a.config.MinWordLength {
		return
	}

	// 系统词库已有该词，跳过造词（词频由 FreqHandler 单独处理）
	if a.systemChecker != nil && a.systemChecker.ExistsInSystemDict(code, text) {
		return
	}

	// 优先写入临时词库
	if a.tempLayer != nil {
		promoted := a.tempLayer.LearnWord(code, text, a.config.WeightDelta)
		if promoted {
			a.tempLayer.PromoteWord(code, text)
		}
		return
	}

	// 没有临时词库时，直接写入用户词库（带误选保护）
	if a.userLayer != nil {
		a.userLayer.OnWordSelected(code, text,
			a.config.AddWeight, a.config.WeightDelta, a.config.CountThreshold)
	}
}

func (a *AutoLearning) Reset() {}

// NewLearningStrategy 根据方案配置创建学习策略
func NewLearningStrategy(ls *LearningSpec, userLayer *dict.StoreUserLayer) LearningStrategy {
	if !ls.IsAutoLearnEnabled() {
		return &ManualLearning{}
	}
	config := ls.GetAutoLearnConfig()
	return NewAutoLearning(userLayer, config)
}

// PhraseTerminator 短语终止接口（可选扩展）
// 当输入标点、选择词组、按回车或失去焦点时由 Coordinator 调用
type PhraseTerminator interface {
	OnPhraseTerminated()
}

// WordCodeCalculator 词编码计算接口
// 根据反向索引和编码规则，为多字词计算编码
type WordCodeCalculator interface {
	CalcWordCode(word string) string
}

// CodeTableAutoPhrase 码表自动造词策略
// 追踪连续上屏的单字序列，遇到终止符时将序列中所有 minLen~maxLen
// 长度的连续子序列组词并写入临时词库，由晋升机制过滤低频组合。
type CodeTableAutoPhrase struct {
	charBuffer    []rune    // 连续单字缓冲区
	lastCharTime  time.Time // 上一个单字上屏的时间
	config        AutoPhraseSpec
	wordCodeCalc  WordCodeCalculator
	userLayer     *dict.StoreUserLayer
	tempLayer     *dict.StoreTempLayer
	systemChecker SystemWordChecker
	logger        *slog.Logger
}

// NewCodeTableAutoPhrase 创建码表自动造词策略
func NewCodeTableAutoPhrase(config AutoPhraseSpec, logger *slog.Logger) *CodeTableAutoPhrase {
	return &CodeTableAutoPhrase{
		config: config,
		logger: logger,
	}
}

// SetWordCodeCalculator 设置词编码计算器
func (p *CodeTableAutoPhrase) SetWordCodeCalculator(calc WordCodeCalculator) {
	p.wordCodeCalc = calc
}

// SetUserLayer 设置用户词库层
func (p *CodeTableAutoPhrase) SetUserLayer(ul *dict.StoreUserLayer) {
	p.userLayer = ul
}

// SetTempLayer 设置临时词库层
func (p *CodeTableAutoPhrase) SetTempLayer(tl *dict.StoreTempLayer) {
	p.tempLayer = tl
}

// SetSystemChecker 设置系统词库检查器
func (p *CodeTableAutoPhrase) SetSystemChecker(checker SystemWordChecker) {
	p.systemChecker = checker
}

// OnWordCommitted 用户上屏回调
// 单字 → 追加到缓冲区；多字词 → 终止当前序列（多字词本身已在词库中）
func (p *CodeTableAutoPhrase) OnWordCommitted(code, text string) {
	runes := []rune(text)
	now := time.Now()

	if len(runes) == 1 {
		// 距上一个单字间隔超过阈值，先清空旧序列再开始新序列
		if len(p.charBuffer) > 0 && now.Sub(p.lastCharTime) > autoPhraseTimeout {
			p.charBuffer = p.charBuffer[:0]
		}
		p.charBuffer = append(p.charBuffer, runes[0])
		p.lastCharTime = now
		return
	}
	// 多字词上屏 = 终止符
	p.flush()
}

// OnPhraseTerminated 终止信号（标点、回车、焦点切换）
func (p *CodeTableAutoPhrase) OnPhraseTerminated() {
	p.flush()
}

// Reset 重置状态
func (p *CodeTableAutoPhrase) Reset() {
	p.charBuffer = p.charBuffer[:0]
}

// flush 将缓冲区中的连续单字序列作为一个词写入词库
// 若序列超过 maxPhraseLen，则只取最后 maxPhraseLen 个字
func (p *CodeTableAutoPhrase) flush() {
	defer func() { p.charBuffer = p.charBuffer[:0] }()

	bufLen := len(p.charBuffer)
	if bufLen < p.config.MinPhraseLen {
		return
	}
	if p.wordCodeCalc == nil {
		return
	}

	// 超过最大长度时截取末尾部分
	start := 0
	if bufLen > p.config.MaxPhraseLen {
		start = bufLen - p.config.MaxPhraseLen
	}

	word := string(p.charBuffer[start:])
	code := p.wordCodeCalc.CalcWordCode(word)
	if code == "" {
		return
	}

	// 系统词库已有则跳过
	if p.systemChecker != nil && p.systemChecker.ExistsInSystemDict(code, word) {
		return
	}

	// 用户词库已有则跳过
	if p.userLayer != nil {
		for _, c := range p.userLayer.Search(code, 0) {
			if c.Text == word {
				return
			}
		}
	}

	p.learnWord(code, word)
}

// learnWord 将词写入临时词库
// 暂不自动晋升到用户词库，后续增加配置选项后再处理
func (p *CodeTableAutoPhrase) learnWord(code, word string) {
	if p.tempLayer != nil {
		p.tempLayer.LearnWord(code, word, p.config.WeightDelta)
		return
	}
	// 无临时词库时回退到用户词库（带误选保护）
	if p.userLayer != nil {
		p.userLayer.OnWordSelected(code, word,
			p.config.AddWeight, p.config.WeightDelta, p.config.CountThreshold)
	}
}

// NewCodeTableLearningStrategy 根据方案配置创建码表自动造词策略
func NewCodeTableLearningStrategy(ls *LearningSpec, logger *slog.Logger) *CodeTableAutoPhrase {
	config := ls.GetAutoPhraseConfig()
	return NewCodeTableAutoPhrase(config, logger)
}

// --- WordCodeCalculator 实现 ---

// EncoderWordCodeCalc 基于反向索引和编码规则计算词编码
// 反向索引在首次调用 CalcWordCode 时惰性构建，避免未使用时的额外开销
type EncoderWordCodeCalc struct {
	rules        []encoding.Rule
	codeTable    *dict.CodeTable
	reverseIndex map[string][]string // 惰性构建
}

// NewEncoderWordCodeCalc 创建编码计算器
func NewEncoderWordCodeCalc(schemaRules []EncoderRule, codeTable *dict.CodeTable) *EncoderWordCodeCalc {
	rules := make([]encoding.Rule, len(schemaRules))
	for i, sr := range schemaRules {
		rules[i] = encoding.Rule{
			LengthEqual: sr.LengthEqual,
			Formula:     sr.Formula,
		}
		if len(sr.LengthInRange) == 2 {
			rules[i].LengthRange = [2]int{sr.LengthInRange[0], sr.LengthInRange[1]}
		}
	}
	return &EncoderWordCodeCalc{
		rules:     rules,
		codeTable: codeTable,
	}
}

// CalcWordCode 计算词的编码
func (c *EncoderWordCodeCalc) CalcWordCode(word string) string {
	if len(c.rules) == 0 || c.codeTable == nil {
		return ""
	}

	// 惰性构建反向索引
	if c.reverseIndex == nil {
		c.reverseIndex = c.codeTable.BuildReverseIndex()
	}

	// 为每个字查找全码（取最长编码）
	charCodes := make(map[string]string)
	for _, ch := range word {
		charStr := string(ch)
		if _, ok := charCodes[charStr]; ok {
			continue
		}
		codes, found := c.reverseIndex[charStr]
		if !found || len(codes) == 0 {
			return ""
		}
		best := codes[0]
		for _, code := range codes {
			if len(code) > len(best) {
				best = code
			}
		}
		charCodes[charStr] = best
	}

	code, err := encoding.CalcWordCode(word, charCodes, c.rules)
	if err != nil {
		return ""
	}
	return code
}
